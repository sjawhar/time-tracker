use anyhow::{Context, Result, bail};
use tt_core::todos::{FileLine, LineEnding, PriorityFileItem, StreamFileItem, StreamPriorityLink};
use tt_db::Database;

use crate::Config;
use crate::todo_store::{LoadedTodoStore, load_mutating, write_streams};

#[derive(Debug, Clone)]
pub struct LinkOptions {
    pub stream: String,
    pub priority: String,
}

pub fn link(db: &Database, config: &Config, options: &LinkOptions) -> Result<()> {
    let mut loaded = load_mutating(config)?;
    let stream_slug = resolve_stream_slug(db, &options.stream)?;
    let stream_name = db
        .get_stream_by_slug(&stream_slug)
        .context("failed to load stream")?
        .and_then(|stream| stream.name);
    validate_priority_exists(&loaded, &options.priority)?;
    validate_stream_has_no_link(&loaded, &stream_slug, stream_name.as_deref())?;

    loaded.store.streams.items.push(FileLine {
        item: StreamFileItem::Link(StreamPriorityLink {
            stream: stream_slug,
            priority: options.priority.clone(),
        }),
        line_ending: LineEnding::Lf,
    });
    write_streams(config, &loaded.store.streams)
}

/// Resolves a stream reference (slug or exact unique name) to the slug that
/// should be written into streams.md.
fn resolve_stream_slug(db: &Database, stream_ref: &str) -> Result<String> {
    if let Some(stream) = db
        .get_stream_by_slug(stream_ref)
        .context("failed to load stream")?
    {
        return stream.slug.ok_or_else(|| {
            anyhow::anyhow!("stream fetched by slug '{stream_ref}' is missing its slug")
        });
    }
    let matches: Vec<_> = db
        .get_streams()
        .context("failed to load streams")?
        .into_iter()
        .filter(|stream| stream.name.as_deref() == Some(stream_ref))
        .collect();

    match matches.as_slice() {
        [] => bail!("no stream with slug or name '{stream_ref}'"),
        [stream] => stream.slug.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "stream '{stream_ref}' has no slug; assign one first: tt streams slug '{stream_ref}' <slug>"
            )
        }),
        _ => bail!("multiple streams named '{stream_ref}'; refer to it by slug instead"),
    }
}

fn validate_priority_exists(loaded: &LoadedTodoStore, slug: &str) -> Result<()> {
    let exists = loaded.store.priorities.items.iter().any(
        |line| matches!(&line.item, PriorityFileItem::Priority(priority) if priority.slug == slug),
    );
    if exists {
        Ok(())
    } else {
        bail!("priority '{slug}' not found")
    }
}

fn validate_stream_has_no_link(
    loaded: &LoadedTodoStore,
    stream_slug: &str,
    stream_name: Option<&str>,
) -> Result<()> {
    let has_link = loaded.store.streams.items.iter().any(|line| {
        matches!(
            &line.item,
            StreamFileItem::Link(link)
                if link.stream == stream_slug || Some(link.stream.as_str()) == stream_name
        )
    });
    if has_link {
        bail!("stream '{stream_slug}' already has a priority link")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tt_db::{Database, Stream};

    use super::*;

    fn fixture() -> (tempfile::TempDir, Database, Config) {
        let temp = tempfile::TempDir::new().unwrap();
        let store = temp.path().join("todo-store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("priorities.md"),
            "- [ ] IPI <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n",
        )
        .unwrap();
        let config = Config {
            database_path: temp.path().join("tt.db"),
            todo_store_path: store,
        };
        let db = Database::open_in_memory().unwrap();
        (temp, db, config)
    }

    fn insert_stream(db: &Database, id: &str, name: &str, slug: Option<&str>) {
        let now = Utc::now();
        db.insert_stream(&Stream {
            id: id.to_string(),
            name: Some(name.to_string()),
            slug: slug.map(String::from),
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        })
        .unwrap();
    }

    fn streams_md(config: &Config) -> String {
        std::fs::read_to_string(config.todo_store_path.join("streams.md")).unwrap()
    }

    #[test]
    fn link_resolves_slug_and_writes_slug() {
        let (_temp, db, config) = fixture();
        insert_stream(&db, "s1", "Project X: the big one", Some("proj-x"));

        link(
            &db,
            &config,
            &LinkOptions {
                stream: "proj-x".to_string(),
                priority: "ipi".to_string(),
            },
        )
        .unwrap();

        assert!(streams_md(&config).contains("- proj-x <!-- tt-stream:{\"priority\":\"ipi\"} -->"));
    }

    #[test]
    fn link_resolves_unique_name_but_writes_slug() {
        let (_temp, db, config) = fixture();
        insert_stream(&db, "s1", "Project X", Some("proj-x"));

        link(
            &db,
            &config,
            &LinkOptions {
                stream: "Project X".to_string(),
                priority: "ipi".to_string(),
            },
        )
        .unwrap();

        let content = streams_md(&config);
        assert!(content.contains("- proj-x "));
        assert!(!content.contains("- Project X "));
    }

    #[test]
    fn link_rejects_stream_without_slug() {
        let (_temp, db, config) = fixture();
        insert_stream(&db, "s1", "Project X", None);

        let err = link(
            &db,
            &config,
            &LinkOptions {
                stream: "Project X".to_string(),
                priority: "ipi".to_string(),
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("tt streams slug"));
    }
}
