use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use tt_core::todos::{Priority, StreamPriorityLink, Todo};

use crate::commands::todo::view::{TodoListView, TodoView, priority_groups, todo_metadata};

pub fn render_next(view: &TodoView<'_>) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "TODO NEXT ({})", view.today).context("failed to format todo next header")?;

    if view.due.is_empty()
        && view.main.is_empty()
        && view.blocked.is_empty()
        && (!view.show_later || view.later.is_empty())
    {
        writeln!(output, "No todos yet.").context("failed to format empty todo next")?;
    } else {
        render_section(
            &mut output,
            "Due",
            &view.due,
            &view.priorities,
            &view.stream_links,
            view.today,
        )?;
        if view.by_priority {
            render_main_by_priority(&mut output, view)?;
        } else {
            render_section(
                &mut output,
                "Main",
                &view.main,
                &view.priorities,
                &view.stream_links,
                view.today,
            )?;
        }
        render_section(
            &mut output,
            "Blocked",
            &view.blocked,
            &view.priorities,
            &view.stream_links,
            view.today,
        )?;
        if view.show_later {
            render_section(
                &mut output,
                "Later",
                &view.later,
                &view.priorities,
                &view.stream_links,
                view.today,
            )?;
        }
    }

    render_diagnostics(&mut output, view.loaded)?;
    Ok(output)
}

pub fn render_ls(view: &TodoListView<'_>) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "TODOS").context("failed to format todo ls header")?;
    if view.todos.is_empty() {
        writeln!(output, "No todos yet.").context("failed to format empty todo ls")?;
    } else {
        for todo in &view.todos {
            writeln!(
                output,
                "- [{}] {} {}",
                if todo.done { "x" } else { " " },
                todo.text,
                todo_metadata(todo, &view.priorities, &view.stream_links, None)
            )
            .context("failed to format todo ls row")?;
        }
    }
    render_diagnostics(&mut output, view.loaded)?;
    Ok(output)
}

fn render_section(
    output: &mut String,
    title: &str,
    todos: &[Todo],
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
    today: NaiveDate,
) -> Result<()> {
    if todos.is_empty() {
        return Ok(());
    }
    writeln!(output).context("failed to format todo section spacer")?;
    writeln!(output, "{title}").context("failed to format todo section header")?;
    for todo in todos {
        writeln!(
            output,
            "- {} {}",
            todo.text,
            todo_metadata(todo, priorities, stream_links, Some(today))
        )
        .context("failed to format todo row")?;
    }
    Ok(())
}

fn render_main_by_priority(output: &mut String, view: &TodoView<'_>) -> Result<()> {
    if view.main.is_empty() {
        return Ok(());
    }
    writeln!(output).context("failed to format grouped main spacer")?;
    writeln!(output, "Main by priority").context("failed to format grouped main header")?;
    for group in priority_groups(&view.main, &view.priorities, &view.stream_links) {
        writeln!(output, "  {}", group.label).context("failed to format priority group")?;
        for todo in group.todos {
            writeln!(
                output,
                "  - {} {}",
                todo.text,
                todo_metadata(todo, &view.priorities, &view.stream_links, Some(view.today))
            )
            .context("failed to format priority group todo")?;
        }
    }
    Ok(())
}

fn render_diagnostics(
    output: &mut String,
    loaded: &crate::todo_store::LoadedTodoStore,
) -> Result<()> {
    if loaded.diagnostics.is_empty() {
        return Ok(());
    }
    writeln!(output).context("failed to format diagnostics spacer")?;
    writeln!(output, "DIAGNOSTICS").context("failed to format diagnostics header")?;
    for diagnostic in &loaded.diagnostics {
        writeln!(
            output,
            "- {} line {}: {} | {}",
            diagnostic.file.label(),
            diagnostic.diagnostic.line_number,
            diagnostic.diagnostic.reason,
            diagnostic.diagnostic.raw_line
        )
        .context("failed to format diagnostic")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
