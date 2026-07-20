use anyhow::{Context, Result, bail};
use tt_core::slug::validate_slug;
use tt_db::Database;

/// Sets a stream's slug. `stream_ref` may be an ID, an existing slug, or an exact name.
pub fn set_slug(db: &Database, stream_ref: &str, slug: &str) -> Result<()> {
    validate_slug(slug).context("invalid stream slug")?;
    let Some(stream) = db
        .resolve_stream(stream_ref)
        .context("failed to resolve stream")?
    else {
        bail!("no stream matching '{stream_ref}' (tried id, slug, exact name)");
    };
    db.set_stream_slug(&stream.id, slug)
        .map_err(|error| anyhow::anyhow!("failed to set slug on stream {}: {error}", stream.id))?;
    let name = stream.name.as_deref().unwrap_or("(unnamed)");
    println!(
        "Set slug '{slug}' on stream {name} ({})",
        &stream.id[..8.min(stream.id.len())]
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tt_db::Database;

    fn db_with_stream(id: &str, name: &str) -> Database {
        let db = Database::open_in_memory().unwrap();
        let stream = tt_db::Stream {
            id: id.to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            name: Some(name.to_string()),
            slug: None,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        };
        db.insert_stream(&stream).unwrap();
        db
    }

    #[test]
    fn sets_slug_by_stream_id() {
        let db = db_with_stream("s1", "agent-c: eval-3 moto");
        set_slug(&db, "s1", "eval3-moto").unwrap();
        assert_eq!(
            db.get_stream_by_slug("eval3-moto").unwrap().unwrap().id,
            "s1"
        );
    }

    #[test]
    fn rejects_invalid_slug_format() {
        let db = db_with_stream("s1", "x");
        let err = set_slug(&db, "s1", "Bad Slug").unwrap_err();
        assert!(err.to_string().contains("invalid"));
    }

    #[test]
    fn rejects_unknown_stream() {
        let db = Database::open_in_memory().unwrap();
        let err = set_slug(&db, "nope", "fine-slug").unwrap_err();
        assert!(err.to_string().contains("no stream"));
    }

    #[test]
    fn rejects_taken_slug() {
        let db = db_with_stream("s1", "x");
        let stream2 = tt_db::Stream {
            id: "s2".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            name: Some("y".to_string()),
            slug: None,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        };
        db.insert_stream(&stream2).unwrap();
        set_slug(&db, "s1", "taken").unwrap();
        let err = set_slug(&db, "s2", "taken").unwrap_err();
        assert!(err.to_string().contains("already in use"));
    }
}
