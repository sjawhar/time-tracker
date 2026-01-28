//! Remote ingest command for appending events to a JSONL buffer.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::{Args, Subcommand};
use fs2::FileExt;
use serde::Serialize;
use uuid::Uuid;

const EVENT_TYPE_TMUX_PANE_FOCUS: &str = "tmux_pane_focus";
const SOURCE_REMOTE_TMUX: &str = "remote.tmux";
const SCHEMA_VERSION: u32 = 1;
const DEBOUNCE_WINDOW_MS: i64 = 500;

#[derive(Debug, Args)]
pub struct IngestArgs {
    #[command(subcommand)]
    pub command: IngestCommand,
}

#[derive(Debug, Subcommand)]
pub enum IngestCommand {
    /// Record a tmux pane focus event.
    PaneFocus(PaneFocusArgs),
}

#[derive(Debug, Args)]
pub struct PaneFocusArgs {
    /// tmux pane id (e.g. %3).
    #[arg(long)]
    pub pane: String,
    /// Current working directory for the pane.
    #[arg(long)]
    pub cwd: String,
    /// tmux session name.
    #[arg(long)]
    pub session: String,
    /// Optional tmux window index.
    #[arg(long)]
    pub window_index: Option<i64>,
}

pub fn run(args: IngestArgs) -> Result<()> {
    let home = dirs::home_dir().context("failed to determine home directory")?;
    let base_dir = home.join(".time-tracker");
    run_with_base_dir(args, &base_dir)
}

pub(crate) fn run_with_base_dir(args: IngestArgs, base_dir: &Path) -> Result<()> {
    let writer = EventWriter::from_base_dir(base_dir);
    match args.command {
        IngestCommand::PaneFocus(pane_args) => {
            let now = Utc::now();
            writer.append_pane_focus_at(&pane_args, now)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
struct EventWriter {
    events_file: PathBuf,
    debounce_file: PathBuf,
    lock_file: PathBuf,
}

impl EventWriter {
    fn from_base_dir(base_dir: &Path) -> Self {
        Self {
            events_file: base_dir.join("events.jsonl"),
            debounce_file: base_dir.join("ingest-debounce.json"),
            lock_file: base_dir.join("events.lock"),
        }
    }

    fn append_pane_focus_at(&self, args: &PaneFocusArgs, now: DateTime<Utc>) -> Result<()> {
        self.ensure_parent_dir()?;
        let lock_file = self.open_lock_file()?;
        lock_file
            .lock_exclusive()
            .context("failed to acquire ingest lock")?;

        let mut debounce_state = self.load_debounce_state()?;
        if debounce_state.should_debounce(&args.pane, now) {
            return Ok(());
        }

        let timestamp = format_timestamp(now);
        let data = TmuxPaneFocusData {
            pane_id: &args.pane,
            session_name: &args.session,
            window_index: args.window_index,
            cwd: &args.cwd,
        };
        let data_json = serde_json::to_string(&data).context("failed to serialize tmux data")?;
        let id = deterministic_event_id(
            SOURCE_REMOTE_TMUX,
            EVENT_TYPE_TMUX_PANE_FOCUS,
            &timestamp,
            &data_json,
        );
        let event = Event {
            id,
            timestamp: &timestamp,
            kind: EVENT_TYPE_TMUX_PANE_FOCUS,
            source: SOURCE_REMOTE_TMUX,
            schema_version: SCHEMA_VERSION,
            data,
            cwd: Some(&args.cwd),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        };
        let event_json = serde_json::to_string(&event).context("failed to serialize event")?;

        self.append_event_line(&event_json)?;
        debounce_state.update(&args.pane, now);
        self.save_debounce_state(&debounce_state)?;

        Ok(())
    }

    fn ensure_parent_dir(&self) -> Result<()> {
        if let Some(parent) = self.events_file.parent() {
            fs::create_dir_all(parent).context("failed to create events directory")?;
        }
        Ok(())
    }

    fn open_lock_file(&self) -> Result<File> {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.lock_file)
            .with_context(|| format!("failed to open lock file {}", self.lock_file.display()))
    }

    fn append_event_line(&self, line: &str) -> Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_file)
            .with_context(|| {
                format!("failed to open events file {}", self.events_file.display())
            })?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(line.as_bytes())
            .context("failed to write event")?;
        writer.write_all(b"\n").context("failed to write newline")?;
        writer.flush().context("failed to flush event")?;
        Ok(())
    }

    fn load_debounce_state(&self) -> Result<DebounceState> {
        if !self.debounce_file.exists() {
            return Ok(DebounceState::default());
        }
        let contents = fs::read_to_string(&self.debounce_file)
            .with_context(|| format!("failed to read {}", self.debounce_file.display()))?;
        let parsed = serde_json::from_str(&contents);
        Ok(parsed.unwrap_or_default())
    }

    fn save_debounce_state(&self, state: &DebounceState) -> Result<()> {
        let json = serde_json::to_string(state).context("failed to serialize debounce state")?;
        fs::write(&self.debounce_file, json)
            .with_context(|| format!("failed to write {}", self.debounce_file.display()))?;
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, serde::Deserialize)]
struct DebounceState {
    panes: HashMap<String, i64>,
}

impl DebounceState {
    fn should_debounce(&self, pane_id: &str, now: DateTime<Utc>) -> bool {
        let Some(last_ms) = self.panes.get(pane_id) else {
            return false;
        };
        let now_ms = now.timestamp_millis();
        now_ms - *last_ms < DEBOUNCE_WINDOW_MS
    }

    fn update(&mut self, pane_id: &str, now: DateTime<Utc>) {
        self.panes
            .insert(pane_id.to_string(), now.timestamp_millis());
    }
}

#[derive(Debug, Serialize)]
struct TmuxPaneFocusData<'a> {
    pane_id: &'a str,
    session_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_index: Option<i64>,
    cwd: &'a str,
}

#[derive(Debug, Serialize)]
struct Event<'a, D> {
    id: String,
    timestamp: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    source: &'a str,
    schema_version: u32,
    data: D,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignment_source: Option<&'a str>,
}

fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn deterministic_event_id(source: &str, event_type: &str, timestamp: &str, data: &str) -> String {
    let content = format!("{source}|{event_type}|{timestamp}|{data}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, content.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn writer_with_temp_dir() -> (TempDir, EventWriter) {
        let temp_dir = TempDir::new().expect("temp dir");
        let writer = EventWriter::from_base_dir(temp_dir.path());
        (temp_dir, writer)
    }

    fn pane_args(pane: &str) -> PaneFocusArgs {
        PaneFocusArgs {
            pane: pane.to_string(),
            cwd: "/tmp/project".to_string(),
            session: "dev".to_string(),
            window_index: Some(1),
        }
    }

    #[test]
    fn writes_pane_focus_event_jsonl() {
        let (_temp_dir, writer) = writer_with_temp_dir();
        let args = pane_args("%1");
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 123_000_000).unwrap();

        writer.append_pane_focus_at(&args, now).unwrap();

        let contents = fs::read_to_string(&writer.events_file).unwrap();
        let mut lines = contents.lines();
        let line = lines.next().unwrap();
        assert!(lines.next().is_none());
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["type"], EVENT_TYPE_TMUX_PANE_FOCUS);
        assert_eq!(value["source"], SOURCE_REMOTE_TMUX);
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["data"]["pane_id"], "%1");
        assert_eq!(value["data"]["session_name"], "dev");
        assert_eq!(value["data"]["window_index"], 1);
        assert_eq!(value["data"]["cwd"], "/tmp/project");
        assert_eq!(value["cwd"], "/tmp/project");
        assert!(value["id"].as_str().is_some());
    }

    #[test]
    fn debounces_same_pane_within_window() {
        let (_temp_dir, writer) = writer_with_temp_dir();
        let args = pane_args("%1");
        let first = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let second = DateTime::<Utc>::from_timestamp(1_700_000_000, 100_000_000).unwrap();

        writer.append_pane_focus_at(&args, first).unwrap();
        writer.append_pane_focus_at(&args, second).unwrap();

        let contents = fs::read_to_string(&writer.events_file).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }

    #[test]
    fn allows_different_pane_within_window() {
        let (_temp_dir, writer) = writer_with_temp_dir();
        let first = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let second = DateTime::<Utc>::from_timestamp(1_700_000_000, 100_000_000).unwrap();

        writer
            .append_pane_focus_at(&pane_args("%1"), first)
            .unwrap();
        writer
            .append_pane_focus_at(&pane_args("%2"), second)
            .unwrap();

        let contents = fs::read_to_string(&writer.events_file).unwrap();
        assert_eq!(contents.lines().count(), 2);
    }

    #[test]
    fn allows_same_pane_after_window() {
        let (_temp_dir, writer) = writer_with_temp_dir();
        let first = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let second = DateTime::<Utc>::from_timestamp(1_700_000_000, 600_000_000).unwrap();

        writer
            .append_pane_focus_at(&pane_args("%1"), first)
            .unwrap();
        writer
            .append_pane_focus_at(&pane_args("%1"), second)
            .unwrap();

        let contents = fs::read_to_string(&writer.events_file).unwrap();
        assert_eq!(contents.lines().count(), 2);
    }
}
