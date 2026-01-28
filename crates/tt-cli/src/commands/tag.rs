//! Tag command for adding manual stream tags.

use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::Args;

use tt_db::Database;

use crate::Config;

#[derive(Debug, Args)]
pub struct TagArgs {
    /// Stream ID to tag.
    pub stream_id: String,
    /// Tag to attach to the stream.
    pub tag: String,
}

pub fn run<W: Write>(writer: &mut W, args: &TagArgs, config: &Config) -> Result<()> {
    let tag = args.tag.trim();
    if tag.is_empty() {
        bail!("tag cannot be empty");
    }

    let mut db = Database::open(&config.database_path)
        .with_context(|| format!("failed to open {}", config.database_path.display()))?;
    let stream_exists = db
        .list_streams()?
        .iter()
        .any(|stream| stream.id == args.stream_id);
    if !stream_exists {
        bail!("stream not found: {}", args.stream_id);
    }

    db.add_stream_tag(&args.stream_id, tag)?;
    writeln!(writer, "Tagged stream {} with {}", args.stream_id, tag)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tt_db::EventRecord;

    use insta::assert_snapshot;

    #[test]
    fn tag_adds_stream_tag() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("tt.db");
        let mut db = Database::open(&db_path).unwrap();

        let event = EventRecord {
            id: "event-1".to_string(),
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

        db.insert_events(&[event]).unwrap();
        db.infer_streams(1_800_000).unwrap();
        let streams = db.list_streams().unwrap();
        assert_eq!(streams.len(), 1);
        let stream_id = streams[0].id.clone();

        let config = Config {
            database_path: db_path,
            api_key: None,
        };
        let mut output = Vec::new();
        let args = TagArgs {
            stream_id: stream_id.clone(),
            tag: "project:time-tracker".to_string(),
        };

        run(&mut output, &args, &config).unwrap();

        let tags = db.list_stream_tags().unwrap();
        assert_eq!(
            tags.get(&stream_id).cloned().unwrap_or_default(),
            vec!["project:time-tracker".to_string()]
        );

        let output = String::from_utf8(output).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn tag_rejects_missing_stream() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("tt.db");
        let config = Config {
            database_path: db_path,
            api_key: None,
        };
        let mut output = Vec::new();
        let args = TagArgs {
            stream_id: "missing".to_string(),
            tag: "backend".to_string(),
        };

        let err = run(&mut output, &args, &config).unwrap_err();
        assert!(err.to_string().contains("stream not found"));
    }

    #[test]
    fn tag_rejects_blank_tag() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("tt.db");
        let config = Config {
            database_path: db_path,
            api_key: None,
        };
        let mut output = Vec::new();
        let args = TagArgs {
            stream_id: "stream-1".to_string(),
            tag: "   ".to_string(),
        };

        let err = run(&mut output, &args, &config).unwrap_err();
        assert!(err.to_string().contains("tag cannot be empty"));
    }
}
