//! Import command for ingesting events into the local `SQLite` store.

use std::io::{self, BufRead};

use anyhow::{Context, Result};
use clap::Args;
use serde::Deserialize;

use tt_db::{Database, EventRecord};

use crate::Config;

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Default source to apply when incoming events omit `source`.
    #[arg(long)]
    pub source: Option<String>,
}

pub fn run(args: &ImportArgs, config: &Config) -> Result<usize> {
    let stdin = io::stdin();
    let events = parse_events(stdin.lock(), args.source.as_deref())?;

    let mut db = Database::open(&config.database_path)
        .with_context(|| format!("failed to open {}", config.database_path.display()))?;
    let inserted = db.insert_events(&events)?;
    Ok(inserted)
}

fn parse_events<R: BufRead>(reader: R, default_source: Option<&str>) -> Result<Vec<EventRecord>> {
    let mut events = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed to read line {}", idx + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: ImportEvent = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSON on line {}", idx + 1))?;
        let record = parsed
            .into_record(default_source)
            .with_context(|| format!("invalid event on line {}", idx + 1))?;
        events.push(record);
    }
    Ok(events)
}

#[derive(Debug, Deserialize)]
struct ImportEvent {
    id: String,
    timestamp: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    data: serde_json::Value,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    stream_id: Option<String>,
    #[serde(default)]
    assignment_source: Option<String>,
}

const fn default_schema_version() -> u32 {
    1
}

impl ImportEvent {
    fn into_record(self, default_source: Option<&str>) -> Result<EventRecord> {
        let source = match self.source {
            Some(source) if !source.trim().is_empty() => source,
            _ => default_source
                .map(str::to_string)
                .filter(|val| !val.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("missing source"))?,
        };
        if self.id.trim().is_empty() {
            return Err(anyhow::anyhow!("missing id"));
        }
        if self.timestamp.trim().is_empty() {
            return Err(anyhow::anyhow!("missing timestamp"));
        }
        if self.kind.trim().is_empty() {
            return Err(anyhow::anyhow!("missing type"));
        }
        let data = serde_json::to_string(&self.data).context("failed to encode data")?;
        let assignment_source = self.assignment_source.and_then(|value| {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
        Ok(EventRecord {
            id: self.id,
            timestamp: self.timestamp,
            kind: self.kind,
            source,
            schema_version: i64::from(self.schema_version),
            data,
            cwd: self.cwd,
            session_id: self.session_id,
            stream_id: self.stream_id,
            assignment_source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;

    #[test]
    fn parse_events_uses_default_source() {
        let input = r#"{"id":"1","timestamp":"2025-01-01T00:00:00Z","type":"tmux_pane_focus","schema_version":1,"data":{"pane_id":"%1"}}"#;
        let events = parse_events(Cursor::new(input), Some("remote.tmux")).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, "remote.tmux");
    }

    #[test]
    fn parse_events_rejects_missing_source() {
        let input = r#"{"id":"1","timestamp":"2025-01-01T00:00:00Z","type":"tmux_pane_focus","schema_version":1,"data":{"pane_id":"%1"}}"#;
        let err = parse_events(Cursor::new(input), None).unwrap_err();
        assert!(err.to_string().contains("invalid event on line 1"));
    }
}
