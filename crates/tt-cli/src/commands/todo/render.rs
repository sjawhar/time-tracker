use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use tt_core::todos::{Priority, StreamPriorityLink, Todo};

use crate::commands::todo::view::{TodoListView, TodoView, priority_groups, todo_metadata};

pub fn render_next(view: &TodoView<'_>) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "TODO NEXT ({})", view.today).context("failed to format todo next header")?;

    if view.due.is_empty() && view.main.is_empty() && (!view.show_later || view.later.is_empty()) {
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
mod tests {
    use super::*;
    use crate::commands::todo::NextOptions;
    use crate::commands::todo::view::{TodoListView, TodoView};
    use crate::todo_store::{LoadedTodoStore, parse_store_contents};

    const PRIORITIES: &str = "- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n- [ ] Admin <!-- tt-priority:{\"slug\":\"admin\",\"value\":2,\"status\":\"active\"} -->\n";
    const STREAMS: &str = "- Fable 5 DPI <!-- tt-stream:{\"priority\":\"ipi\"} -->\n";
    const TODOS: &str = "- [ ] Ship overdue fix <!-- tt-todo:{\"id\":\"td_due000001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":\"2026-06-30\",\"due\":\"2026-06-22\",\"pin\":false,\"quick\":false} -->\n- [ ] Quick admin reply <!-- tt-todo:{\"id\":\"td_quick0001\",\"priority\":[\"admin\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":true} -->\n- [ ] Stream-linked work <!-- tt-todo:{\"id\":\"td_stream001\",\"priority\":[],\"stream\":\"Fable 5 DPI\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] Deferred task <!-- tt-todo:{\"id\":\"td_later0001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":\"2026-06-25\",\"due\":null,\"pin\":false,\"quick\":true} -->\n- [x] Done task <!-- tt-todo:{\"id\":\"td_done00001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    const DUE_TODOS: &str = "- [ ] Already late <!-- tt-todo:{\"id\":\"td_late00001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":\"2026-06-22\",\"pin\":false,\"quick\":false} -->\n- [ ] Due today <!-- tt-todo:{\"id\":\"td_today0001\",\"priority\":[\"admin\"],\"stream\":null,\"when\":null,\"due\":\"2026-06-23\",\"pin\":false,\"quick\":false} -->\n- [ ] Future due <!-- tt-todo:{\"id\":\"td_future001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":\"2026-06-24\",\"pin\":false,\"quick\":false} -->\n";

    fn fixture() -> LoadedTodoStore {
        parse_store_contents(PRIORITIES, TODOS, STREAMS)
    }

    fn due_fixture() -> LoadedTodoStore {
        parse_store_contents(PRIORITIES, DUE_TODOS, STREAMS)
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 23).unwrap()
    }

    fn options() -> NextOptions {
        NextOptions {
            top: None,
            quick: false,
            json: false,
            by_priority: false,
            later: false,
        }
    }

    fn next_output(loaded: &LoadedTodoStore, options: NextOptions) -> String {
        let view = TodoView::from_loaded(loaded, today(), options);
        render_next(&view).unwrap()
    }

    #[test]
    fn todo_next_by_priority_orders_high_value_group_first() {
        // Given: the file order puts a lower-value priority todo before a higher-value stream todo.
        let loaded = fixture();

        // When: next is rendered by priority.
        let output = next_output(
            &loaded,
            NextOptions {
                by_priority: true,
                ..options()
            },
        );

        // Then: the high-value priority group renders before the low-value group.
        let high = output.find("priority:ipi(9)").unwrap();
        let low = output.find("priority:admin(2)").unwrap();
        assert!(
            high < low,
            "high priority group should render first:\n{output}"
        );
    }

    #[test]
    fn todo_next_snapshots() {
        let loaded = fixture();
        let due_loaded = due_fixture();

        insta::assert_snapshot!("todo_next_default", next_output(&loaded, options()));
        insta::assert_snapshot!("todo_next_due", next_output(&due_loaded, options()));
        insta::assert_snapshot!(
            "todo_next_quick",
            next_output(
                &loaded,
                NextOptions {
                    quick: true,
                    later: true,
                    ..options()
                }
            )
        );
        insta::assert_snapshot!(
            "todo_next_by_priority",
            next_output(
                &loaded,
                NextOptions {
                    by_priority: true,
                    ..options()
                }
            )
        );
        insta::assert_snapshot!(
            "todo_next_later",
            next_output(
                &loaded,
                NextOptions {
                    later: true,
                    ..options()
                }
            )
        );
    }

    #[test]
    fn todo_ls_malformed_line_diagnostic_snapshot() {
        let todos = "- [ ] Missing id <!-- tt-todo:{\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":true} -->\n- [ ] Broken <!-- tt-todo:{bad json} -->\n";
        let loaded = parse_store_contents(PRIORITIES, todos, STREAMS);
        let view = TodoListView::from_loaded(&loaded);

        insta::assert_snapshot!(render_ls(&view).unwrap());
    }
}
