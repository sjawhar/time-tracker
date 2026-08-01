//! `tt streams rename <ref> <new-name>` — set a stream's display name.
//!
//! The first half of repairing a real initiative that was minted once per week:
//! strip the `(Jun14-20)` suffix here, then collapse the rows that now share a name
//! with `tt streams merge`. Names carry no uniqueness constraint, so that
//! intermediate state is legal and expected — but it also makes the name ambiguous
//! as a reference, so a rename that lands on an existing name says so.
//!
//! Which streams deserve renaming, and to what, is a human judgement. This command
//! holds no list of its own.

use std::fmt::Write;

use anyhow::{Context, Result, bail};
use tt_db::Database;

use super::format_stream_label;

/// Renames a stream, returning the confirmation to print.
///
/// `stream_ref` may be an ID, a slug, or an exact display name.
fn rename_to(db: &Database, stream_ref: &str, name: &str) -> Result<String> {
    if name.trim().is_empty() {
        bail!("a stream name cannot be blank");
    }
    let Some(stream) = db
        .resolve_stream(stream_ref)
        .with_context(|| format!("failed to resolve stream '{stream_ref}'"))?
    else {
        bail!("no stream matching '{stream_ref}' (tried id, slug, exact name)");
    };
    let was = format_stream_label(stream.name.as_deref(), &stream.id);
    db.rename_stream(&stream.id, name)
        .with_context(|| format!("failed to rename stream {}", stream.id))?;

    let mut message = format!("Renamed {was} to '{name}'.\n");
    let sharing = db
        .get_streams()
        .context("failed to load streams")?
        .into_iter()
        .filter(|other| other.id != stream.id && other.name.as_deref() == Some(name))
        .count();
    if sharing > 0 {
        write!(
            message,
            "{sharing} other stream(s) now share this name, so it no longer identifies one \
             row.\nCollapse them with 'tt streams merge <from>... --into {}'.\n",
            stream.id
        )?;
    }
    Ok(message)
}

/// Runs the rename command.
pub fn rename(db: &Database, stream_ref: &str, name: &str) -> Result<()> {
    print!("{}", rename_to(db, stream_ref, name)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(id: &str, name: &str, slug: Option<&str>) -> tt_db::Stream {
        let now = chrono::Utc::now();
        tt_db::Stream {
            id: id.to_string(),
            created_at: now,
            updated_at: now,
            name: Some(name.to_string()),
            slug: slug.map(String::from),
            description: None,
            color: None,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        }
    }

    fn db_with_week_buckets() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&stream(
            "wk1",
            "workorder-5: IPI envs (Jun14-20)",
            Some("wo5-wk1"),
        ))
        .unwrap();
        db.insert_stream(&stream("wk2", "workorder-5: IPI envs (Jun21-27)", None))
            .unwrap();
        db
    }

    #[test]
    fn strips_a_week_suffix_by_stream_id() {
        // Given: a real initiative bucketed into a week.
        let db = db_with_week_buckets();

        // When: the suffix is stripped.
        let message = rename_to(&db, "wk1", "workorder-5: IPI envs").unwrap();

        // Then: the new name is stored and confirmed.
        assert_eq!(
            db.get_stream("wk1").unwrap().unwrap().name.as_deref(),
            Some("workorder-5: IPI envs")
        );
        assert!(message.contains("workorder-5: IPI envs"));
    }

    #[test]
    fn resolves_a_reference_by_slug_and_by_exact_name() {
        // Given: two streams, each reachable by a different kind of reference.
        let db = db_with_week_buckets();

        // When: each is renamed through a different reference form.
        rename_to(&db, "wo5-wk1", "one").unwrap();
        rename_to(&db, "workorder-5: IPI envs (Jun21-27)", "two").unwrap();

        // Then: both took the new name.
        assert_eq!(
            db.get_stream("wk1").unwrap().unwrap().name.as_deref(),
            Some("one")
        );
        assert_eq!(
            db.get_stream("wk2").unwrap().unwrap().name.as_deref(),
            Some("two")
        );
    }

    #[test]
    fn reports_when_the_new_name_is_now_shared() {
        // Given: two week buckets of one initiative, the first already renamed.
        let db = db_with_week_buckets();
        rename_to(&db, "wk1", "workorder-5: IPI envs").unwrap();

        // When: the second is renamed to the same thing — the state a merge resolves.
        let message = rename_to(&db, "wk2", "workorder-5: IPI envs").unwrap();

        // Then: the collision is reported, pointing at the target id to merge onto.
        assert!(message.contains("1 other stream(s) now share this name"));
        assert!(message.contains("tt streams merge <from>... --into wk2"));
    }

    #[test]
    fn says_nothing_about_sharing_when_the_name_is_unique() {
        // Given: two differently named streams.
        let db = db_with_week_buckets();

        // When: one is renamed to something nothing else uses.
        let message = rename_to(&db, "wk1", "workorder-5: IPI envs").unwrap();

        // Then: no collision is reported.
        assert!(!message.contains("share this name"));
    }

    #[test]
    fn rejects_an_unknown_reference() {
        // Given: a reference naming nothing.
        let db = db_with_week_buckets();

        // When/Then: the rename is refused.
        let error = rename_to(&db, "nope", "anything").unwrap_err();
        assert!(error.to_string().contains("no stream"));
    }

    #[test]
    fn rejects_a_blank_name() {
        // Given: a stream and a name that names nothing.
        let db = db_with_week_buckets();

        // When/Then: the rename is refused before any write.
        let error = rename_to(&db, "wk1", "   ").unwrap_err();
        assert!(error.to_string().contains("cannot be blank"));
        assert_eq!(
            db.get_stream("wk1").unwrap().unwrap().name.as_deref(),
            Some("workorder-5: IPI envs (Jun14-20)")
        );
    }
}
