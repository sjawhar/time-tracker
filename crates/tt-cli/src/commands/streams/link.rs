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
    let stream_name = resolve_exact_stream_name(db, &options.stream)?;
    validate_priority_exists(&loaded, &options.priority)?;
    validate_stream_has_no_link(&loaded, stream_name)?;

    loaded.store.streams.items.push(FileLine {
        item: StreamFileItem::Link(StreamPriorityLink {
            stream: stream_name.to_string(),
            priority: options.priority.clone(),
        }),
        line_ending: LineEnding::Lf,
    });
    write_streams(config, &loaded.store.streams)
}

fn resolve_exact_stream_name<'a>(db: &Database, stream_name: &'a str) -> Result<&'a str> {
    let match_count = db
        .get_streams()
        .context("failed to load streams")?
        .into_iter()
        .filter(|stream| stream.name.as_deref() == Some(stream_name))
        .count();

    match match_count {
        0 => bail!("no stream named '{stream_name}'"),
        1 => Ok(stream_name),
        count => bail!("'{stream_name}' is ambiguous: {count} streams share that name"),
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

fn validate_stream_has_no_link(loaded: &LoadedTodoStore, stream_name: &str) -> Result<()> {
    let has_link =
        loaded.store.streams.items.iter().any(
            |line| matches!(&line.item, StreamFileItem::Link(link) if link.stream == stream_name),
        );
    if has_link {
        bail!("stream '{stream_name}' already has a priority link")
    }
    Ok(())
}
