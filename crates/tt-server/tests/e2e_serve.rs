use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{Value, json};
use tempfile::{NamedTempFile, TempDir};
use tt_core::EventType;
use tt_db::{Database, StoredEvent, Stream};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const SSE_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const RETRY_DELAY: Duration = Duration::from_millis(50);
const STREAM_ID: &str = "e2e-focus-stream";

struct Daemon {
    child: Child,
    port: u16,
}

impl Daemon {
    fn start(database_path: &Path, home: &Path) -> std::io::Result<Self> {
        let mut daemon = Self {
            child: Command::new(env!("CARGO_BIN_EXE_tt-serve"))
                .env_clear()
                .env("HOME", home)
                .env("XDG_CONFIG_HOME", home.join("config"))
                .env("XDG_DATA_HOME", home.join("data"))
                .env("ANTHROPIC_API_KEY", "e2e-test-key")
                .arg("--port")
                .arg("0")
                .arg("--db")
                .arg(database_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?,
            port: 0,
        };
        let stdout = daemon
            .child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("daemon stdout was not piped"))?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let result = reader.read_line(&mut line).map(|_| line);
            let _ = sender.send(result);
        });

        let line = receiver
            .recv_timeout(STARTUP_TIMEOUT)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::TimedOut, error))??;
        let address = line
            .trim()
            .strip_prefix("listening on ")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, line.clone()))?
            .parse::<SocketAddr>()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        daemon.port = address.port();
        Ok(daemon)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn connect(port: u16) -> std::io::Result<TcpStream> {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), POLL_INTERVAL)
}

fn status(port: u16) -> Result<Value, Box<dyn Error>> {
    let mut stream = connect(port)?;
    stream.set_read_timeout(Some(POLL_INTERVAL))?;
    stream
        .write_all(b"GET /api/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("status response has no body"))?;
    if !headers.starts_with("HTTP/1.1 200") {
        return Err(std::io::Error::other(headers.to_owned()).into());
    }
    Ok(serde_json::from_str(body)?)
}

fn wait_for_status(port: u16) -> Result<Value, Box<dyn Error>> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Ok(verdict) = status(port) {
            return Ok(verdict);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "daemon did not serve status within 10 seconds",
            )
            .into());
        }
        thread::sleep(RETRY_DELAY);
    }
}

fn open_sse(port: u16) -> Result<BufReader<TcpStream>, Box<dyn Error>> {
    let mut stream = connect(port)?;
    stream.set_read_timeout(Some(STARTUP_TIMEOUT))?;
    stream.write_all(
        b"GET /api/sse HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n",
    )?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    if !status_line.starts_with("HTTP/1.1 200") {
        return Err(std::io::Error::other(status_line).into());
    }
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header == "\r\n" {
            break;
        }
    }
    reader.get_mut().set_read_timeout(Some(POLL_INTERVAL))?;
    Ok(reader)
}

fn focus_event(sequence: u32) -> StoredEvent {
    StoredEvent {
        id: format!("e2e-focus-{sequence}"),
        timestamp: Utc::now(),
        event_type: EventType::TmuxPaneFocus,
        source: "e2e".to_string(),
        machine_id: None,
        schema_version: 1,
        pane_id: Some("%1".to_string()),
        tmux_session: Some("e2e".to_string()),
        window_index: Some(1),
        git_project: None,
        git_workspace: None,
        status: None,
        idle_duration_ms: None,
        window_app_id: None,
        window_title: None,
        action: None,
        cwd: Some("/tmp/e2e".to_string()),
        session_id: None,
        stream_id: Some(STREAM_ID.to_string()),
        assignment_source: Some("test".to_string()),
        data: json!({}),
    }
}

fn wait_for_events_appended(
    database_path: &Path,
    sse: &mut BufReader<TcpStream>,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + SSE_TIMEOUT;
    let mut sequence = 0;
    loop {
        sequence += 1;
        let db = Database::open(database_path)?;
        assert_eq!(db.insert_events(&[focus_event(sequence)])?, 1);
        drop(db);

        let mut line = String::new();
        match sse.read_line(&mut line) {
            Ok(0) => return Err(std::io::Error::other("SSE connection closed").into()),
            Ok(_) if line.trim() == "event: events_appended" => return Ok(()),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "daemon did not emit events_appended within 5 seconds",
            )
            .into());
        }
    }
}

#[test]
fn serve_reports_status_and_sse_after_external_focus_event() -> Result<(), Box<dyn Error>> {
    // Given a file-backed database with no activity and a daemon bound to an ephemeral port.
    let home = TempDir::new()?;
    let database_file = NamedTempFile::new()?;
    let db = Database::open(database_file.path())?;
    let now = Utc::now();
    db.insert_stream(&Stream {
        id: STREAM_ID.to_string(),
        name: Some("E2E focus stream".to_string()),
        slug: None,
        description: None,
        color: None,
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })?;
    drop(db);
    let daemon = Daemon::start(database_file.path(), home.path())?;

    // When the status API is ready, it returns a parseable empty verdict.
    let initial_verdict = wait_for_status(daemon.port)?;
    assert_eq!(initial_verdict.get("current_stream"), Some(&Value::Null));
    assert_eq!(initial_verdict.get("top_todo"), Some(&Value::Null));
    let mut sse = open_sse(daemon.port)?;
    wait_for_events_appended(database_file.path(), &mut sse)?;

    // Then the same database mutation is visible through the daemon's status API.
    let updated_verdict = status(daemon.port)?;
    assert_eq!(
        updated_verdict
            .get("current_stream")
            .and_then(|stream| stream.get("stream_id"))
            .and_then(Value::as_str),
        Some(STREAM_ID)
    );
    Ok(())
}
