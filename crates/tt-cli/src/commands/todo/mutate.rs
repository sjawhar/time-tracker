use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use tt_core::todos::{Todo, TodoFileItem};

use super::ids::mint_todo_id;
use super::order_edit::insert_todo_by_rank;
use super::raw::parse_raw_todo;
use super::{AddOptions, RankOptions};
use crate::Config;
use crate::todo_store::{LoadedTodoStore, load_mutating, write_todos};

pub fn run_add(config: &Config, db: Option<&tt_db::Database>, options: AddOptions) -> Result<()> {
    if let Some(slug) = options.stream.as_deref() {
        let db = db.context("--stream requires the database")?;
        if db
            .get_stream_by_slug(slug)
            .context("failed to look up stream slug")?
            .is_none()
        {
            bail!(
                "no stream with slug '{slug}'; create it via classification or set one with: tt streams slug <stream> {slug}"
            );
        }
    }
    let when = parse_optional_date(options.when.as_deref(), "--when")?;
    let due = parse_optional_date(options.due.as_deref(), "--due")?;
    let mut loaded = load_mutating(config)?;
    let mut existing = existing_todo_ids(&loaded);
    let id = mint_todo_id(&existing);
    existing.insert(id.clone());
    let todo = Todo {
        id,
        text: options.text,
        priority: options.priority,
        stream: options.stream,
        when,
        due,
        pin: options.pin,
        quick: options.quick,
        done: false,
        block: None,
        sessions: Vec::new(),
    };
    insert_todo_by_rank(&mut loaded, todo);
    write_todos(config, &loaded.store.todos)
}

pub fn run_done(config: &Config, id: &str) -> Result<()> {
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    todo.done = true;
    write_todos(config, &loaded.store.todos)
}

pub fn run_defer(config: &Config, id: &str, date: &str) -> Result<()> {
    let when = parse_date(date, "date")?;
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    todo.when = Some(when);
    write_todos(config, &loaded.store.todos)
}

pub fn run_block(config: &Config, id: &str, reason: &str) -> Result<()> {
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("block reason must not be empty");
    }
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    if todo.done {
        bail!("cannot block a done todo '{id}'");
    }
    todo.block = Some(reason.to_string());
    write_todos(config, &loaded.store.todos)
}

pub fn run_unblock(config: &Config, id: &str) -> Result<()> {
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    todo.block = None;
    write_todos(config, &loaded.store.todos)
}

/// Sets or clears the stream a todo serves.
///
/// The alignment panel needs todo → stream → priority, and nothing could set the middle
/// link on a todo that already existed, so it sat at 3% populated and `aligned` stayed
/// null. This is the surface that sets it: the human names the todo and the stream, and
/// nothing here infers either.
///
/// A reference matching no stream is reported rather than minted, the same discipline as
/// `tt todo link`. Readers resolve `todo.stream` as a slug, so the resolved stream's slug
/// is what lands in the file — a stream carrying none is refused rather than given one.
pub fn run_set_stream(
    config: &Config,
    db: Option<&tt_db::Database>,
    id: &str,
    stream: Option<&str>,
) -> Result<()> {
    // Resolved before the store is loaded so an unresolvable reference never reaches a write.
    let slug = stream
        .map(|reference| resolve_stream_slug(db, reference))
        .transpose()?;
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    todo.stream = slug;
    write_todos(config, &loaded.store.todos)
}

fn resolve_stream_slug(db: Option<&tt_db::Database>, reference: &str) -> Result<String> {
    let db = db.context("setting a todo's stream requires the database")?;
    let Some(stream) = db
        .resolve_stream(reference)
        .context("failed to look up stream")?
    else {
        bail!("no stream matching '{reference}'; see: tt streams list");
    };
    let Some(slug) = stream.slug else {
        bail!(
            "stream '{reference}' has no slug for a todo to reference; set one with: tt streams slug '{reference}' <slug>"
        );
    };
    Ok(slug)
}

pub fn run_rank(config: &Config, options: &RankOptions) -> Result<()> {
    let mut loaded = load_mutating(config)?;
    validate_rank_target(options)?;
    let source = unique_todo_line_index(&loaded, &options.id)?;
    let destination = rank_destination(&loaded, options)?;
    let line = loaded.store.todos.items.remove(source);
    let adjusted = if source < destination {
        destination.saturating_sub(1)
    } else {
        destination
    };
    loaded.store.todos.items.insert(adjusted, line);
    write_todos(config, &loaded.store.todos)
}

pub fn run_normalize_ids(config: &Config) -> Result<()> {
    let mut loaded = load_mutating(config)?;
    let mut existing = existing_todo_ids(&loaded);
    for line in &mut loaded.store.todos.items {
        let TodoFileItem::Raw(raw) = &line.item else {
            continue;
        };
        let Some(mut todo) = parse_raw_todo(raw) else {
            continue;
        };
        if !todo.id.is_empty() {
            continue;
        }
        let id = mint_todo_id(&existing);
        existing.insert(id.clone());
        todo.id = id;
        line.item = TodoFileItem::Todo(todo);
    }
    write_todos(config, &loaded.store.todos)
}

pub(super) fn unique_todo_line_index(loaded: &LoadedTodoStore, id: &str) -> Result<usize> {
    let matches = loaded
        .store
        .todos
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match &line.item {
            TodoFileItem::Todo(todo) if todo.id == id => Some(index),
            TodoFileItem::Todo(_) | TodoFileItem::Raw(_) => None,
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => bail!("todo '{id}' not found"),
        [_, _, ..] => bail!("todo '{id}' is ambiguous"),
    }
}

fn rank_destination(loaded: &LoadedTodoStore, options: &RankOptions) -> Result<usize> {
    if options.top {
        return Ok(0);
    }
    if let Some(above) = &options.above {
        reject_self_relative_rank(&options.id, above)?;
        return unique_todo_line_index(loaded, above);
    }
    if let Some(below) = &options.below {
        reject_self_relative_rank(&options.id, below)?;
        return unique_todo_line_index(loaded, below).map(|index| index + 1);
    }
    bail!("rank requires --top, --above <id>, or --below <id>")
}

fn validate_rank_target(options: &RankOptions) -> Result<()> {
    let count = usize::from(options.top)
        + usize::from(options.above.is_some())
        + usize::from(options.below.is_some());
    if count == 1 {
        return Ok(());
    }
    bail!("rank requires exactly one of --top, --above <id>, or --below <id>")
}

fn reject_self_relative_rank(id: &str, other: &str) -> Result<()> {
    if id == other {
        bail!("rank target cannot be relative to itself");
    }
    Ok(())
}

fn existing_todo_ids(loaded: &LoadedTodoStore) -> HashSet<String> {
    loaded
        .store
        .todos
        .items
        .iter()
        .filter_map(|line| match &line.item {
            TodoFileItem::Todo(todo) if !todo.id.is_empty() => Some(todo.id.clone()),
            TodoFileItem::Todo(_) => None,
            TodoFileItem::Raw(raw) => parse_raw_todo(raw).and_then(|todo| {
                if todo.id.is_empty() {
                    None
                } else {
                    Some(todo.id)
                }
            }),
        })
        .collect()
}

fn parse_optional_date(value: Option<&str>, label: &str) -> Result<Option<NaiveDate>> {
    value.map(|date| parse_date(date, label)).transpose()
}

fn parse_date(value: &str, label: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("invalid {label} date '{value}', expected YYYY-MM-DD"))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tt_db::{Database, Stream};

    use crate::todo_store::load_read_only;

    use super::*;

    fn fixture() -> (tempfile::TempDir, Config) {
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            database_path: temp.path().join("tt.db"),
            todo_store_path: temp.path().join("todo-store"),
            ..Config::default()
        };
        (temp, config)
    }

    fn add_options(stream: Option<&str>) -> AddOptions {
        AddOptions {
            text: "Add task".to_string(),
            priority: Vec::new(),
            stream: stream.map(str::to_string),
            due: None,
            when: None,
            quick: false,
            pin: false,
        }
    }

    fn fixture_with_todos(todos: &str) -> (tempfile::TempDir, Config) {
        let temp = tempfile::TempDir::new().unwrap();
        let store = temp.path().join("todo-store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("todos.md"), todos).unwrap();
        let config = Config {
            database_path: temp.path().join("tt.db"),
            todo_store_path: store,
            ..Config::default()
        };
        (temp, config)
    }

    /// A store holding the todo under test beside content the command must not touch:
    /// a heading, a second todo already naming a stream, and trailing prose.
    fn store_fixture() -> String {
        concat!(
            "# Todos\n",
            "\n",
            "- [ ] Ship the alignment panel <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[\"alignment\"],\"stream\":null,\"when\":\"2026-08-10\",\"due\":\"2026-08-12\",\"pin\":true,\"quick\":true,\"sessions\":[\"ses_a\"]} -->\n",
            "- [ ] Second task <!-- tt-todo:{\"id\":\"td_2\",\"priority\":[],\"stream\":\"other\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
            "\n",
            "Notes below the list.\n",
        )
        .to_string()
    }

    /// The same store with `td_1` linked; `td_2` is the only other stream field and
    /// already carries a value, so the replacement can only reach the todo under test.
    fn linked_store_fixture() -> String {
        store_fixture().replace("\"stream\":null", "\"stream\":\"proj-x\"")
    }

    fn read_todos(config: &Config) -> String {
        std::fs::read_to_string(config.todo_store_path.join("todos.md")).unwrap()
    }

    fn stream_of(config: &Config, id: &str) -> Option<String> {
        let loaded = load_read_only(config).unwrap();
        loaded
            .store
            .todos
            .items
            .iter()
            .find_map(|line| match &line.item {
                TodoFileItem::Todo(todo) if todo.id == id => Some(todo.stream.clone()),
                TodoFileItem::Todo(_) | TodoFileItem::Raw(_) => None,
            })
            .unwrap()
    }

    fn insert_stream(db: &Database, slug: &str) {
        insert_stream_row(db, "stream-1", "Project X", Some(slug));
    }

    fn insert_stream_row(db: &Database, id: &str, name: &str, slug: Option<&str>) {
        let now = Utc::now();
        db.insert_stream(&Stream {
            id: id.to_string(),
            name: Some(name.to_string()),
            slug: slug.map(str::to_string),
            description: None,
            color: None,
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

    #[test]
    fn add_with_stream_requires_existing_slug() {
        let (_temp, config) = fixture();
        let db = Database::open_in_memory().unwrap();
        insert_stream(&db, "proj-x");

        run_add(&config, Some(&db), add_options(Some("proj-x"))).unwrap();

        let err = run_add(&config, Some(&db), add_options(Some("typo-slug"))).unwrap_err();
        assert!(err.to_string().contains("no stream with slug"));

        run_add(&config, None, add_options(None)).unwrap();
    }

    #[test]
    fn set_stream_writes_the_slug_and_leaves_every_other_byte_alone() {
        // Given: an unlinked todo, beside a heading, another todo and prose.
        let (_temp, config) = fixture_with_todos(&store_fixture());
        let db = Database::open_in_memory().unwrap();
        insert_stream(&db, "proj-x");

        // When: the todo is given a stream.
        run_set_stream(&config, Some(&db), "td_1", Some("proj-x")).unwrap();

        // Then: one field of one line changed and every other byte survived.
        assert_eq!(read_todos(&config), linked_store_fixture());
    }

    #[test]
    fn a_stream_named_by_id_or_display_name_still_writes_its_slug() {
        // Given: a stream whose id and display name are neither of them what a todo
        // references — readers resolve `todo.stream` as a slug.
        for reference in ["stream-1", "Project X"] {
            let (_temp, config) = fixture_with_todos(&store_fixture());
            let db = Database::open_in_memory().unwrap();
            insert_stream(&db, "proj-x");

            // When: the stream is named by that reference.
            run_set_stream(&config, Some(&db), "td_1", Some(reference)).unwrap();

            // Then: the slug is what lands in the file.
            assert_eq!(stream_of(&config, "td_1").as_deref(), Some("proj-x"));
        }
    }

    #[test]
    fn a_stream_reference_matching_nothing_errors_and_writes_nothing() {
        // Given: a store and a reference no stream carries.
        let (_temp, config) = fixture_with_todos(&store_fixture());
        let db = Database::open_in_memory().unwrap();
        insert_stream(&db, "proj-x");

        // When: the todo is pointed at it.
        let err = run_set_stream(&config, Some(&db), "td_1", Some("typo-slug")).unwrap_err();

        // Then: the reference is named back, no stream is minted, the file is untouched.
        assert!(err.to_string().contains("no stream matching 'typo-slug'"));
        assert_eq!(db.get_streams().unwrap().len(), 1);
        assert_eq!(read_todos(&config), store_fixture());
    }

    #[test]
    fn a_stream_with_no_slug_is_refused_rather_than_given_one() {
        // Given: a stream todos have no way to reference.
        let (_temp, config) = fixture_with_todos(&store_fixture());
        let db = Database::open_in_memory().unwrap();
        insert_stream_row(&db, "stream-2", "Slugless", None);

        // When: the todo is pointed at it.
        let err = run_set_stream(&config, Some(&db), "td_1", Some("Slugless")).unwrap_err();

        // Then: the command says so rather than inventing a slug.
        assert!(err.to_string().contains("has no slug"));
        assert_eq!(read_todos(&config), store_fixture());
    }

    #[test]
    fn clearing_removes_the_stream_and_leaves_every_other_byte_alone() {
        // Given: a linked todo.
        let (_temp, config) = fixture_with_todos(&linked_store_fixture());

        // When: the link is cleared, which needs no database.
        run_set_stream(&config, None, "td_1", None).unwrap();

        // Then: the file is back to exactly what it was before the link.
        assert_eq!(read_todos(&config), store_fixture());
    }

    #[test]
    fn an_unknown_todo_id_errors_without_writing() {
        // Given: a store holding no such todo.
        let (_temp, config) = fixture_with_todos(&store_fixture());
        let db = Database::open_in_memory().unwrap();
        insert_stream(&db, "proj-x");

        // When: that id is named.
        let err = run_set_stream(&config, Some(&db), "td_missing", Some("proj-x")).unwrap_err();

        // Then: it is a clean error and the file is untouched.
        assert!(err.to_string().contains("todo 'td_missing' not found"));
        assert_eq!(read_todos(&config), store_fixture());
    }
}
