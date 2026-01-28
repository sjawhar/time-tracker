//! Status command for showing recent activity by source.

use std::io::Write;

use anyhow::{Context, Result};

use tt_db::Database;

use crate::Config;

pub fn run<W: Write>(writer: &mut W, config: &Config) -> Result<()> {
    let db = Database::open(&config.database_path)
        .with_context(|| format!("failed to open {}", config.database_path.display()))?;
    let sources = db.last_event_times_by_source()?;

    writeln!(writer, "Time tracker status")?;
    writeln!(writer, "Database: {}", config.database_path.display())?;

    if sources.is_empty() {
        writeln!(writer, "No events recorded.")?;
        return Ok(());
    }

    writeln!(writer, "Sources:")?;
    for source in sources {
        writeln!(writer, "- {}: {}", source.source, source.last_event)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tt_db::{Database, EventRecord};

    use insta::assert_snapshot;

    #[test]
    fn status_command_outputs_last_event_per_source() {
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
            assignment_source: None,
        };

        db.insert_events(&[event_a, event_b]).unwrap();

        let config = Config {
            database_path: db_path.clone(),
            api_key: None,
        };
        let mut output = Vec::new();
        run(&mut output, &config).unwrap();

        let output = String::from_utf8(output).unwrap();
        let output = output.replace(&db_path.display().to_string(), "[TEMP]/tt.db");
        assert_snapshot!(output);
    }
}
