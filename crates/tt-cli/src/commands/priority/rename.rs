use anyhow::{Context, Result, bail};
use tt_core::todos::{PriorityFileItem, StreamFileItem, TodoFileItem};

use super::validate_explicit_slug;
use crate::Config;
use crate::todo_store::{
    LoadedTodoStore, load_mutating, write_priorities, write_streams, write_todos,
};

pub fn run_rename(config: &Config, old_slug: &str, new_slug: &str) -> Result<()> {
    let new_slug = validate_explicit_slug(new_slug)?;
    if old_slug == new_slug {
        bail!("old and new slug are identical: '{old_slug}'");
    }

    let mut loaded = load_mutating(config)?;
    ensure_old_exists(&loaded, old_slug)?;
    ensure_new_absent(&loaded, &new_slug)?;

    rename_priority(&mut loaded, old_slug, &new_slug);
    let todo_count = rename_todo_refs(&mut loaded, old_slug, &new_slug);
    let stream_count = rename_stream_links(&mut loaded, old_slug, &new_slug);

    // Write priorities last so validation remains re-runnable after a partial write failure.
    write_todos(config, &loaded.store.todos).context("failed to write todos.md")?;
    write_streams(config, &loaded.store.streams).context("failed to write streams.md")?;
    write_priorities(config, &loaded.store.priorities).context("failed to write priorities.md")?;

    println!(
        "renamed priority '{old_slug}' -> '{new_slug}' ({todo_count} todo(s), {stream_count} stream link(s) updated)"
    );
    Ok(())
}

fn ensure_old_exists(loaded: &LoadedTodoStore, slug: &str) -> Result<()> {
    let exists = loaded.store.priorities.items.iter().any(
        |line| matches!(&line.item, PriorityFileItem::Priority(priority) if priority.slug == slug),
    );
    if exists {
        Ok(())
    } else {
        bail!("priority '{slug}' not found")
    }
}

fn ensure_new_absent(loaded: &LoadedTodoStore, slug: &str) -> Result<()> {
    let exists = loaded.store.priorities.items.iter().any(
        |line| matches!(&line.item, PriorityFileItem::Priority(priority) if priority.slug == slug),
    );
    if exists {
        bail!("priority '{slug}' already exists");
    }
    Ok(())
}

fn rename_priority(loaded: &mut LoadedTodoStore, old: &str, new: &str) {
    for line in &mut loaded.store.priorities.items {
        if let PriorityFileItem::Priority(priority) = &mut line.item {
            if priority.slug == old {
                priority.slug = new.to_string();
            }
        }
    }
}

fn rename_todo_refs(loaded: &mut LoadedTodoStore, old: &str, new: &str) -> usize {
    let mut count = 0usize;
    for line in &mut loaded.store.todos.items {
        let TodoFileItem::Todo(todo) = &mut line.item else {
            continue;
        };
        let mut changed = false;
        for slug in &mut todo.priority {
            if slug.as_str() == old {
                *slug = new.to_string();
                changed = true;
            }
        }
        if changed {
            count += 1;
        }
    }
    count
}

fn rename_stream_links(loaded: &mut LoadedTodoStore, old: &str, new: &str) -> usize {
    let mut count = 0usize;
    for line in &mut loaded.store.streams.items {
        if let StreamFileItem::Link(link) = &mut line.item {
            if link.priority == old {
                link.priority = new.to_string();
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use crate::todo_store::parse_store_contents;

    use super::{rename_priority, rename_stream_links, rename_todo_refs};

    #[test]
    fn rename_helpers_rewrite_all_three_files() {
        // Given: a store referencing `diversification` in all three files.
        let mut loaded = parse_store_contents(
            "- [ ] Diversification <!-- tt-priority:{\"slug\":\"diversification\",\"value\":4,\"status\":\"active\"} -->\n",
            "- [ ] A <!-- tt-todo:{\"id\":\"td_a00000001\",\"priority\":[\"diversification\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] B <!-- tt-todo:{\"id\":\"td_b00000001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
            "- Sales <!-- tt-stream:{\"priority\":\"diversification\"} -->\n",
        );

        // When: each rewrite helper runs.
        rename_priority(&mut loaded, "diversification", "sales");
        let todo_count = rename_todo_refs(&mut loaded, "diversification", "sales");
        let stream_count = rename_stream_links(&mut loaded, "diversification", "sales");

        // Then: only matching records change, and counts reflect the rewrites.
        assert_eq!(todo_count, 1);
        assert_eq!(stream_count, 1);
        assert!(
            loaded
                .store
                .priorities
                .to_string()
                .contains("\"slug\":\"sales\"")
        );
        assert!(
            loaded
                .store
                .todos
                .to_string()
                .contains("\"priority\":[\"sales\"]")
        );
        assert!(
            loaded
                .store
                .todos
                .to_string()
                .contains("\"priority\":[\"ipi\"]")
        );
        assert!(
            loaded
                .store
                .streams
                .to_string()
                .contains("\"priority\":\"sales\"")
        );
    }
}
