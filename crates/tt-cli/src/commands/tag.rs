//! Tag command implementation.

use anyhow::{Context, Result, bail};
use tt_db::Database;

/// Run the tag command.
///
/// Adds a tag to a stream, identified by ID or name.
pub fn run(db: &Database, stream: &str, tag: &str) -> Result<()> {
    // Resolve stream by ID or name
    let resolved = db
        .resolve_stream(stream)
        .context("failed to query streams")?;

    let Some(resolved) = resolved else {
        bail!(
            "Stream '{stream}' not found.\n\nHint: Use 'tt streams' to see available stream IDs."
        );
    };

    // Add the tag
    db.add_tag(&resolved.id, tag).context("failed to add tag")?;

    // Get all tags for confirmation output
    let tags = db.get_tags(&resolved.id).context("failed to get tags")?;

    // Print confirmation
    let stream_name = resolved.name.as_deref().unwrap_or("<unnamed>");
    println!(
        "Tagged stream {} ({}) as \"{}\"",
        resolved.id, stream_name, tag
    );
    println!("Tags: {}", tags.join(", "));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tag_stream_by_id() {
        let db = Database::open_in_memory().unwrap();

        // Create a stream
        let now = chrono::Utc::now();
        let stream = tt_db::Stream {
            id: "test-stream-123".to_string(),
            name: Some("project-x".to_string()),
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        };
        db.insert_stream(&stream).unwrap();

        // Tag by ID
        run(&db, "test-stream-123", "acme-webapp").unwrap();

        // Verify tag was added
        let tags = db.get_tags("test-stream-123").unwrap();
        assert_eq!(tags, vec!["acme-webapp"]);
    }

    #[test]
    fn test_tag_stream_by_name() {
        let db = Database::open_in_memory().unwrap();

        // Create a stream
        let now = chrono::Utc::now();
        let stream = tt_db::Stream {
            id: "test-stream-456".to_string(),
            name: Some("my-project".to_string()),
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        };
        db.insert_stream(&stream).unwrap();

        // Tag by name
        run(&db, "my-project", "internal").unwrap();

        // Verify tag was added
        let tags = db.get_tags("test-stream-456").unwrap();
        assert_eq!(tags, vec!["internal"]);
    }

    #[test]
    fn test_tag_nonexistent_stream() {
        let db = Database::open_in_memory().unwrap();

        let result = run(&db, "nonexistent", "some-tag");
        assert!(result.is_err());

        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
        assert!(err.contains("tt streams"));
    }
}
