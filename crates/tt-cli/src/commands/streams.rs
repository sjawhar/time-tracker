//! Streams command for listing inferred streams.

use std::io::Write;

use anyhow::{Context, Result};
use clap::Args;

use tt_db::Database;

use crate::Config;

#[derive(Debug, Args, Default)]
pub struct StreamsArgs {}

pub fn run<W: Write>(writer: &mut W, config: &Config) -> Result<()> {
    let db = Database::open(&config.database_path)
        .with_context(|| format!("failed to open {}", config.database_path.display()))?;
    let streams = db.list_streams()?;

    if streams.is_empty() {
        writeln!(writer, "No streams recorded.")?;
        return Ok(());
    }

    let tags_map = db.list_stream_tags()?;
    let mut rows: Vec<StreamRow> = streams
        .into_iter()
        .map(|stream| {
            let name = stream.name.unwrap_or_else(|| "-".to_string());
            let tags = tags_map
                .get(&stream.id)
                .map(|tags| tags.join(", "))
                .filter(|tags| !tags.is_empty())
                .unwrap_or_else(|| "-".to_string());
            StreamRow {
                id: stream.id,
                name,
                tags,
            }
        })
        .collect();

    rows.sort_by(|a, b| a.id.cmp(&b.id));

    let id_width = rows
        .iter()
        .map(|row| row.id.len())
        .max()
        .unwrap_or(0)
        .max("Stream ID".len());
    let name_width = rows
        .iter()
        .map(|row| row.name.len())
        .max()
        .unwrap_or(0)
        .max("Name".len());

    writeln!(writer, "STREAMS")?;
    writeln!(
        writer,
        "{:<id_width$}  {:<name_width$}  Tags",
        "Stream ID",
        "Name",
        id_width = id_width,
        name_width = name_width
    )?;
    for row in rows {
        writeln!(
            writer,
            "{:<id_width$}  {:<name_width$}  {}",
            row.id,
            row.name,
            row.tags,
            id_width = id_width,
            name_width = name_width
        )?;
    }

    Ok(())
}

struct StreamRow {
    id: String,
    name: String,
    tags: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    use tt_db::{Database, EventRecord};

    use insta::assert_snapshot;

    #[test]
    fn streams_command_outputs_table() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("tt.db");
        let mut db = Database::open(&db_path).unwrap();

        let event_a = EventRecord {
            id: "event-a".to_string(),
            timestamp: "2026-01-28T09:00:00Z".to_string(),
            kind: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: r#"{"cwd":"/repo"}"#.to_string(),
            cwd: Some("/repo".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        };
        let event_b = EventRecord {
            id: "event-b".to_string(),
            timestamp: "2026-01-28T10:05:00Z".to_string(),
            kind: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: r#"{"cwd":"/notes"}"#.to_string(),
            cwd: Some("/notes".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        };

        db.insert_events(&[event_a, event_b]).unwrap();
        db.infer_streams(1_800_000).unwrap();

        let streams = db.list_streams().unwrap();
        let repo_stream_id = streams
            .iter()
            .find(|stream| stream.name.as_deref() == Some("/repo"))
            .expect("/repo stream")
            .id
            .clone();
        db.add_stream_tag(&repo_stream_id, "project:time-tracker")
            .unwrap();

        let config = Config {
            database_path: db_path,
            api_key: None,
        };
        let mut output = Vec::new();
        run(&mut output, &config).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn streams_command_handles_empty_db() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("tt.db");
        let config = Config {
            database_path: db_path,
            api_key: None,
        };
        let mut output = Vec::new();

        run(&mut output, &config).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_snapshot!(output);
    }
}
