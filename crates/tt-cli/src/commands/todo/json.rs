use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;
use tt_core::todos::{Priority, StreamPriorityLink, Todo, priority_rank};

use crate::commands::todo::view::TodoView;
use crate::todo_store::StoreFile;

#[derive(Serialize)]
struct JsonNext<'a> {
    today: String,
    sections: JsonSections<'a>,
    diagnostics: Vec<JsonDiagnostic<'a>>,
}

#[derive(Serialize)]
struct JsonSections<'a> {
    due: Vec<JsonTodo<'a>>,
    main: Vec<JsonTodo<'a>>,
    later: Vec<JsonTodo<'a>>,
}

#[derive(Serialize)]
struct JsonTodo<'a> {
    id: Option<&'a str>,
    text: &'a str,
    priority: Vec<&'a str>,
    stream: Option<&'a str>,
    rank: Option<i32>,
    when: Option<String>,
    due: Option<String>,
    due_marker: Option<String>,
    pin: bool,
    quick: bool,
}

#[derive(Serialize)]
struct JsonDiagnostic<'a> {
    file: StoreFile,
    line: usize,
    reason: String,
    raw_line: &'a str,
}

pub fn render_next(view: &TodoView<'_>) -> Result<String> {
    let json = JsonNext {
        today: view.today.to_string(),
        sections: JsonSections {
            due: json_todos(&view.due, &view.priorities, &view.stream_links, view.today),
            main: json_todos(&view.main, &view.priorities, &view.stream_links, view.today),
            later: json_todos(
                &view.later,
                &view.priorities,
                &view.stream_links,
                view.today,
            ),
        },
        diagnostics: view
            .loaded
            .diagnostics
            .iter()
            .map(|diagnostic| JsonDiagnostic {
                file: diagnostic.file,
                line: diagnostic.diagnostic.line_number,
                reason: diagnostic.diagnostic.reason.to_string(),
                raw_line: diagnostic.diagnostic.raw_line.as_str(),
            })
            .collect(),
    };
    let mut output =
        serde_json::to_string_pretty(&json).context("failed to serialize todo JSON")?;
    output.push('\n');
    Ok(output)
}

fn json_todos<'a>(
    todos: &'a [Todo],
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
    today: NaiveDate,
) -> Vec<JsonTodo<'a>> {
    todos
        .iter()
        .map(|todo| JsonTodo {
            id: if todo.id.is_empty() {
                None
            } else {
                Some(todo.id.as_str())
            },
            text: todo.text.as_str(),
            priority: todo.priority.iter().map(String::as_str).collect(),
            stream: todo.stream.as_deref(),
            rank: priority_rank(todo, priorities, stream_links),
            when: todo.when.map(|when| when.to_string()),
            due: todo.due.map(|due| due.to_string()),
            due_marker: json_due_marker(todo, today),
            pin: todo.pin,
            quick: todo.quick,
        })
        .collect()
}

fn json_due_marker(todo: &Todo, today: NaiveDate) -> Option<String> {
    let due = todo.due?;
    match due.cmp(&today) {
        std::cmp::Ordering::Less => Some("overdue".to_string()),
        std::cmp::Ordering::Equal => Some("due_today".to_string()),
        std::cmp::Ordering::Greater => Some("future".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::todo::NextOptions;
    use crate::commands::todo::view::TodoView;
    use crate::todo_store::parse_store_contents;

    #[test]
    fn todo_next_json_snapshot() {
        let priorities = "- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n";
        let todos = "- [ ] Due item <!-- tt-todo:{\"id\":\"td_due000001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":\"2026-06-23\",\"pin\":false,\"quick\":true} -->\n- [ ] Later item <!-- tt-todo:{\"id\":\"td_later0001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":\"2026-06-25\",\"due\":null,\"pin\":false,\"quick\":false} -->\n";
        let loaded = parse_store_contents(priorities, todos, "");
        let today = NaiveDate::from_ymd_opt(2026, 6, 23).unwrap();
        let view = TodoView::from_loaded(
            &loaded,
            today,
            NextOptions {
                top: None,
                quick: false,
                json: true,
                by_priority: false,
                later: true,
            },
        );

        insta::assert_snapshot!("todo_next_json", render_next(&view).unwrap());
    }
}
