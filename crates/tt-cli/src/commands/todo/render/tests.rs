use chrono::NaiveDate;

use super::*;
use crate::commands::todo::NextOptions;
use crate::commands::todo::view::{TodoListView, TodoView};
use crate::todo_store::{LoadedTodoStore, parse_store_contents};

const PRIORITIES: &str = "- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n- [ ] Admin <!-- tt-priority:{\"slug\":\"admin\",\"value\":2,\"status\":\"active\"} -->\n";
const STREAMS: &str = "- Fable 5 DPI <!-- tt-stream:{\"priority\":\"ipi\"} -->\n";
const TODOS: &str = "- [ ] Ship overdue fix <!-- tt-todo:{\"id\":\"td_due000001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":\"2026-06-30\",\"due\":\"2026-06-22\",\"pin\":false,\"quick\":false} -->\n- [ ] Quick admin reply <!-- tt-todo:{\"id\":\"td_quick0001\",\"priority\":[\"admin\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":true} -->\n- [ ] Stream-linked work <!-- tt-todo:{\"id\":\"td_stream001\",\"priority\":[],\"stream\":\"Fable 5 DPI\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] Deferred task <!-- tt-todo:{\"id\":\"td_later0001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":\"2026-06-25\",\"due\":null,\"pin\":false,\"quick\":true} -->\n- [x] Done task <!-- tt-todo:{\"id\":\"td_done00001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
const DUE_TODOS: &str = "- [ ] Already late <!-- tt-todo:{\"id\":\"td_late00001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":\"2026-06-22\",\"pin\":false,\"quick\":false} -->\n- [ ] Due today <!-- tt-todo:{\"id\":\"td_today0001\",\"priority\":[\"admin\"],\"stream\":null,\"when\":null,\"due\":\"2026-06-23\",\"pin\":false,\"quick\":false} -->\n- [ ] Future due <!-- tt-todo:{\"id\":\"td_future001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":\"2026-06-24\",\"pin\":false,\"quick\":false} -->\n";
const BLOCKED_TODOS: &str = "- [ ] Ship overdue fix <!-- tt-todo:{\"id\":\"td_due000001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":\"2026-06-22\",\"pin\":false,\"quick\":false} -->\n- [ ] Blocked launch task <!-- tt-todo:{\"id\":\"td_block0001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"block\":\"waiting on Peter\"} -->\n- [ ] Plain admin <!-- tt-todo:{\"id\":\"td_admin0001\",\"priority\":[\"admin\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";

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

fn with_render_snapshots(run: impl FnOnce()) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.bind(run);
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

    with_render_snapshots(|| {
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
    });
}

#[test]
fn todo_ls_malformed_line_diagnostic_snapshot() {
    let todos = "- [ ] Missing id <!-- tt-todo:{\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":true} -->\n- [ ] Broken <!-- tt-todo:{bad json} -->\n";
    let loaded = parse_store_contents(PRIORITIES, todos, STREAMS);
    let view = TodoListView::from_loaded(&loaded);

    with_render_snapshots(|| {
        insta::assert_snapshot!(render_ls(&view).unwrap());
    });
}

#[test]
fn todo_ls_shows_when_defer_date() {
    // Given: a deferred todo that has a `when` date but no `due` date.
    let todos = "- [ ] Deferred only <!-- tt-todo:{\"id\":\"td_deferred01\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":\"2026-07-01\",\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    let loaded = parse_store_contents(PRIORITIES, todos, STREAMS);
    let view = TodoListView::from_loaded(&loaded);

    // When: the read-only list is rendered.
    let output = render_ls(&view).unwrap();

    // Then: the defer date is visible.
    assert!(
        output.contains("when:2026-07-01"),
        "todo ls should show when date:\n{output}"
    );
}

#[test]
fn todo_next_blocked_section_snapshot() {
    // Given: a store with an overdue todo, a blocked todo, and a plain todo.
    let loaded = parse_store_contents(PRIORITIES, BLOCKED_TODOS, STREAMS);

    // When: next is rendered.
    let output = next_output(&loaded, options());

    // Then: the blocked todo is under a Blocked section, not Due/Main.
    with_render_snapshots(|| {
        insta::assert_snapshot!("todo_next_blocked", output);
    });
    assert!(
        output.contains("\nBlocked\n"),
        "missing Blocked section:\n{output}"
    );
    let blocked = output.find("Blocked launch task").unwrap();
    let main = output.find("Plain admin").unwrap();
    assert!(main < blocked, "blocked must render after Main:\n{output}");
}

#[test]
fn todo_ls_shows_block_reason() {
    // Given: a blocked todo.
    let todos = "- [ ] Blocked launch task <!-- tt-todo:{\"id\":\"td_block0001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"block\":\"waiting on Peter\"} -->\n";
    let loaded = parse_store_contents(PRIORITIES, todos, STREAMS);
    let view = TodoListView::from_loaded(&loaded);

    // When: the read-only list renders.
    let output = render_ls(&view).unwrap();

    // Then: the block reason is surfaced.
    assert!(
        output.contains("blocked:\"waiting on Peter\""),
        "todo ls should surface block reason:\n{output}"
    );
}
