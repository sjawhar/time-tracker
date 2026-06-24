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

pub fn run_add(config: &Config, options: AddOptions) -> Result<()> {
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

fn unique_todo_line_index(loaded: &LoadedTodoStore, id: &str) -> Result<usize> {
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
