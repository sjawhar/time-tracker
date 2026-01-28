//! Events command for listing local database records.

use std::io::Write;

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;

use tt_db::{Database, EventRecord};

use crate::Config;

#[derive(Debug, Args, Default)]
pub struct EventsArgs {}

pub fn run<W: Write>(writer: &mut W, config: &Config) -> Result<usize> {
    let db = Database::open(&config.database_path)
        .with_context(|| format!("failed to open {}", config.database_path.display()))?;
    let events = db.list_events()?;

    for event in &events {
        let output = OutputEvent::from_record(event)?;
        serde_json::to_writer(&mut *writer, &output).context("failed to encode event")?;
        writeln!(writer).context("failed to write event line")?;
    }

    Ok(events.len())
}

#[derive(Debug, Serialize)]
struct OutputEvent {
    id: String,
    timestamp: String,
    #[serde(rename = "type")]
    kind: String,
    source: String,
    schema_version: i64,
    data: serde_json::Value,
    cwd: Option<String>,
    session_id: Option<String>,
    stream_id: Option<String>,
    assignment_source: Option<String>,
}

impl OutputEvent {
    fn from_record(record: &EventRecord) -> Result<Self> {
        let data = serde_json::from_str(&record.data)
            .with_context(|| format!("failed to parse event data for {}", record.id))?;
        Ok(Self {
            id: record.id.clone(),
            timestamp: record.timestamp.clone(),
            kind: record.kind.clone(),
            source: record.source.clone(),
            schema_version: record.schema_version,
            data,
            cwd: record.cwd.clone(),
            session_id: record.session_id.clone(),
            stream_id: record.stream_id.clone(),
            assignment_source: record.assignment_source.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tt_db::{Database, EventRecord};

    use insta::assert_snapshot;

    #[test]
    fn events_command_outputs_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("tt.db");
        let mut db = Database::open(&db_path).unwrap();

        let event_a = EventRecord {
            id: "event-a".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            kind: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: r#"{"pane_id":"%1"}"#.to_string(),
            cwd: Some("/repo".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        };
        let event_b = EventRecord {
            id: "event-b".to_string(),
            timestamp: "2025-01-01T00:02:00Z".to_string(),
            kind: "agent_session".to_string(),
            source: "remote.agent".to_string(),
            schema_version: 1,
            data: r#"{"action":"started"}"#.to_string(),
            cwd: None,
            session_id: Some("sess-1".to_string()),
            stream_id: None,
            assignment_source: Some("user".to_string()),
        };

        db.insert_events(&[event_b, event_a]).unwrap();

        let config = Config {
            database_path: db_path,
            api_key: None,
        };
        let mut output = Vec::new();
        let count = run(&mut output, &config).unwrap();
        assert_eq!(count, 2);

        let output = String::from_utf8(output).unwrap();
        assert_snapshot!(output);
    }
}
