use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use tt_core::todos::{Priority, StreamPriorityLink, Todo, TodoFileItem};
use tt_db::Database;

use crate::Config;
use crate::commands::report::Period;
use crate::drift::TopTodo;
use crate::todo_store::load_read_only;

mod check;
mod drift;
mod ids;
mod json;
mod link;
mod mutate;
mod order_edit;
mod raw;
mod render;
mod stream_links;
mod view;

pub use link::{run_link, run_unlink};

#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "options mirror independent read-only CLI flags"
)]
pub struct NextOptions {
    pub top: Option<usize>,
    pub quick: bool,
    pub json: bool,
    pub by_priority: bool,
    pub later: bool,
}

#[derive(Debug, Clone)]
pub struct AddOptions {
    pub text: String,
    pub priority: Vec<String>,
    pub stream: Option<String>,
    pub due: Option<String>,
    pub when: Option<String>,
    pub quick: bool,
    pub pin: bool,
}

#[derive(Debug, Clone)]
pub struct RankOptions {
    pub id: String,
    pub top: bool,
    pub above: Option<String>,
    pub below: Option<String>,
}

pub struct TopTodoView {
    pub top: Option<TopTodo>,
    pub priorities: Vec<Priority>,
    pub stream_links: Vec<StreamPriorityLink>,
    pub ranked_todos: Vec<RankedTodo>,
    pub linked_todo_texts_by_session: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RankedTodo {
    pub todo: Todo,
    pub section: TodoNextSection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoNextSection {
    Due,
    Main,
    Blocked,
}

impl TodoNextSection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Due => "due",
            Self::Main => "main",
            Self::Blocked => "blocked",
        }
    }
}

pub fn top_todo_view(config: &Config, today: NaiveDate) -> Result<TopTodoView> {
    let loaded = load_read_only(config).context("failed to load todo store")?;
    let view = view::TodoView::from_loaded(
        &loaded,
        today,
        NextOptions {
            top: None,
            quick: false,
            json: false,
            by_priority: false,
            later: false,
        },
    );
    let top = view.main.first().map(|todo| TopTodo {
        id: todo.id.clone(),
        text: todo.text.clone(),
        stream_slug: todo.stream.clone(),
    });
    let ranked_todos = view
        .due
        .iter()
        .cloned()
        .map(|todo| RankedTodo {
            todo,
            section: TodoNextSection::Due,
        })
        .chain(view.main.iter().cloned().map(|todo| RankedTodo {
            todo,
            section: TodoNextSection::Main,
        }))
        .chain(view.blocked.iter().cloned().map(|todo| RankedTodo {
            todo,
            section: TodoNextSection::Blocked,
        }))
        .collect();
    let mut linked_todo_texts_by_session = HashMap::new();
    for line in &view.loaded.store.todos.items {
        let TodoFileItem::Todo(todo) = &line.item else {
            continue;
        };
        if todo.done {
            continue;
        }
        for session_id in &todo.sessions {
            linked_todo_texts_by_session
                .entry(session_id.clone())
                .or_insert_with(|| todo.text.clone());
        }
    }
    Ok(TopTodoView {
        top,
        priorities: view.priorities,
        stream_links: view.stream_links,
        ranked_todos,
        linked_todo_texts_by_session,
    })
}

pub fn run_next(config: &Config, options: NextOptions) -> Result<()> {
    let loaded = load_read_only(config)?;
    let today = Local::now().date_naive();
    let view = view::TodoView::from_loaded(&loaded, today, options);
    if options.json {
        print!("{}", json::render_next(&view)?);
    } else {
        print!("{}", render::render_next(&view)?);
    }
    Ok(())
}

pub fn run_ls(config: &Config) -> Result<()> {
    let loaded = load_read_only(config)?;
    let view = view::TodoListView::from_loaded(&loaded);
    print!("{}", render::render_ls(&view)?);
    Ok(())
}

pub fn run_add(config: &Config, db: Option<&Database>, options: AddOptions) -> Result<()> {
    mutate::run_add(config, db, options)
}

pub fn run_set_stream(
    config: &Config,
    db: Option<&Database>,
    id: &str,
    stream: Option<&str>,
) -> Result<()> {
    mutate::run_set_stream(config, db, id, stream)
}

pub fn run_done(config: &Config, id: &str) -> Result<()> {
    mutate::run_done(config, id)
}

pub fn run_defer(config: &Config, id: &str, date: &str) -> Result<()> {
    mutate::run_defer(config, id, date)
}

pub fn run_block(config: &Config, id: &str, reason: &str) -> Result<()> {
    mutate::run_block(config, id, reason)
}

pub fn run_unblock(config: &Config, id: &str) -> Result<()> {
    mutate::run_unblock(config, id)
}

pub fn run_rank(config: &Config, options: &RankOptions) -> Result<()> {
    mutate::run_rank(config, options)
}

pub fn run_normalize_ids(config: &Config) -> Result<()> {
    mutate::run_normalize_ids(config)
}

pub fn run_check(config: &Config, json: bool) -> Result<()> {
    let loaded = load_read_only(config)?;
    print!("{}", check::render_check(&loaded, json)?);
    Ok(())
}

pub fn run_drift(db: &Database, config: &Config, period: Period, json: bool) -> Result<()> {
    drift::run(db, config, period, json)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::ids;

    #[test]
    fn mint_todo_id_retries_when_candidate_collides() {
        // Given: the first generated id already exists and the second one does not.
        let existing = HashSet::from(["td_0000000000".to_string()]);
        let byte_batches = [
            [0_u8; 16],
            [
                0x08, 0x86, 0x42, 0x98, 0xe8, 0x4a, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x11, 0x22,
                0x33, 0x44,
            ],
        ];
        let mut index = 0usize;

        // When: an id is minted with a deterministic byte source.
        let id = ids::mint_todo_id_with(&existing, || {
            let bytes = byte_batches[index];
            index += 1;
            bytes
        });

        // Then: the collision is skipped and the next candidate is returned.
        assert_eq!(id, "td_123456789a");
    }
}
