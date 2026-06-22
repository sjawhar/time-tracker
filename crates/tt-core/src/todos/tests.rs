mod drift;
mod order;
mod support;

use super::{
    FileLine, LineEnding, PriorityFileItem, PriorityStatus, StreamFileItem, Todo, TodoFileItem,
    parse_priorities, parse_streams, parse_todo_lenient, parse_todos,
};

const FULL_PRIORITIES: &str = "# Priorities\n\n- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n- [ ] Archive docs <!-- tt-priority:{\"slug\":\"docs\",\"value\":3,\"status\":\"done\"} -->\n- [ ] Old idea <!-- tt-priority:{\"slug\":\"old\",\"value\":1,\"status\":\"dropped\"} -->\n";
const FULL_TODOS: &str = "## Due\n- [ ] Draft pricing reply <!-- tt-todo:{\"id\":\"td_0123456789\",\"priority\":[\"ipi\",\"docs\"],\"stream\":\"Fable 5 DPI\",\"when\":\"2026-06-18\",\"due\":\"2026-06-19\",\"pin\":true,\"quick\":true} -->\n\n## Later\n- [x] Close old loop <!-- tt-todo:{\"id\":\"td_abcdefghij\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
const FULL_STREAMS: &str = "# Streams\n- Fable 5 DPI <!-- tt-stream:{\"priority\":\"ipi\"} -->\n";

#[test]
fn round_trips_full_fixture() {
    // Given: well-formed markdown fixtures exercising every T2 field.
    let (priorities, priority_diagnostics) = parse_priorities(FULL_PRIORITIES);
    let (todos, todo_diagnostics) = parse_todos(FULL_TODOS);
    let (streams, stream_diagnostics) = parse_streams(FULL_STREAMS);

    // When: each parsed file is serialized and parsed again.
    let serialized_priorities = priorities.to_string();
    let serialized_todos = todos.to_string();
    let serialized_streams = streams.to_string();
    let (priorities_again, priority_diagnostics_again) = parse_priorities(&serialized_priorities);
    let (todos_again, todo_diagnostics_again) = parse_todos(&serialized_todos);
    let (streams_again, stream_diagnostics_again) = parse_streams(&serialized_streams);

    // Then: serialization is identity for well-formed input and models round-trip.
    assert!(priority_diagnostics.is_empty());
    assert!(todo_diagnostics.is_empty());
    assert!(stream_diagnostics.is_empty());
    assert_eq!(serialized_priorities, FULL_PRIORITIES);
    assert_eq!(serialized_todos, FULL_TODOS);
    assert_eq!(serialized_streams, FULL_STREAMS);
    assert_eq!(priorities, priorities_again);
    assert_eq!(todos, todos_again);
    assert_eq!(streams, streams_again);
    assert!(priority_diagnostics_again.is_empty());
    assert!(todo_diagnostics_again.is_empty());
    assert!(stream_diagnostics_again.is_empty());
}

#[test]
fn preserves_malformed_lines() {
    // Given: a malformed JSON todo line surrounded by raw markdown.
    let input = "## Later\n- [ ] Broken <!-- tt-todo:{bad json} -->\nplain note\n";

    // When: the tolerant parser reads and serializes the file.
    let (file, diagnostics) = parse_todos(input);
    let rendered = file.to_string();

    // Then: the malformed line is preserved and reported with its source line.
    assert_eq!(rendered, input);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line_number, 2);
    assert_eq!(
        diagnostics[0].raw_line,
        "- [ ] Broken <!-- tt-todo:{bad json} -->"
    );
    assert!(diagnostics[0].reason.to_string().contains("JSON"));
}

#[test]
fn preserves_unknown_json_key_as_diagnostic() {
    // Given: a hand-edited todo line with a typo in hidden JSON metadata.
    let input = "- [ ] Typo stream <!-- tt-todo:{\"id\":\"td_typostream\",\"priority\":[\"ipi\"],\"strem\":\"Fable 5 DPI\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";

    // When: the tolerant parser reads and serializes the file.
    let (file, diagnostics) = parse_todos(input);
    let rendered = file.to_string();

    // Then: the typo is not silently dropped and the original line is preserved.
    assert_eq!(rendered, input);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line_number, 1);
    assert_eq!(diagnostics[0].raw_line, input.trim_end());
    assert!(diagnostics[0].reason.to_string().contains("unknown field"));
}

#[test]
fn preserves_invalid_grammar_lines() {
    // Given: a line with a todo marker but an unsupported checkbox prefix.
    let input = "- [X] Capital done <!-- tt-todo:{\"id\":\"td_capitalxxx\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";

    // When: the tolerant parser reads and serializes the file.
    let (file, diagnostics) = parse_todos(input);
    let rendered = file.to_string();

    // Then: the line is preserved verbatim and reported as invalid grammar.
    assert_eq!(rendered, input);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line_number, 1);
    assert_eq!(diagnostics[0].raw_line, input.trim_end());
    assert!(
        diagnostics[0]
            .reason
            .to_string()
            .contains("malformed todo line")
    );
}

#[test]
fn parses_exact_hidden_json_grammar() {
    // Given: exact GFM hidden-JSON examples from the approved scope.
    let priorities = "- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n";
    let todos = "- [ ] Draft pricing reply <!-- tt-todo:{\"id\":\"td_0123456789\",\"priority\":[\"ipi\"],\"stream\":\"Fable 5 DPI\",\"when\":null,\"due\":\"2026-06-19\",\"pin\":false,\"quick\":true} -->\n";
    let streams = "- Fable 5 DPI <!-- tt-stream:{\"priority\":\"ipi\"} -->\n";

    // When: each file is parsed.
    let (priority_file, priority_diagnostics) = parse_priorities(priorities);
    let (todo_file, todo_diagnostics) = parse_todos(todos);
    let (stream_file, stream_diagnostics) = parse_streams(streams);

    // Then: visible text and hidden JSON fields are parsed exactly.
    assert!(priority_diagnostics.is_empty());
    assert!(todo_diagnostics.is_empty());
    assert!(stream_diagnostics.is_empty());

    let PriorityFileItem::Priority(priority) = &priority_file.items[0].item else {
        panic!("expected parsed priority");
    };
    assert_eq!(priority.title, "IPI launch");
    assert_eq!(priority.slug, "ipi");
    assert_eq!(priority.value, 9);
    assert_eq!(priority.status, PriorityStatus::Active);

    let TodoFileItem::Todo(todo) = &todo_file.items[0].item else {
        panic!("expected parsed todo");
    };
    assert_eq!(todo.text, "Draft pricing reply");
    assert_eq!(todo.id, "td_0123456789");
    assert_eq!(todo.priority, ["ipi"]);
    assert_eq!(todo.stream.as_deref(), Some("Fable 5 DPI"));
    assert_eq!(todo.when, None);
    assert_eq!(todo.due, chrono::NaiveDate::from_ymd_opt(2026, 6, 19));
    assert!(!todo.pin);
    assert!(todo.quick);
    assert!(!todo.done);

    let StreamFileItem::Link(link) = &stream_file.items[0].item else {
        panic!("expected parsed stream link");
    };
    assert_eq!(link.stream, "Fable 5 DPI");
    assert_eq!(link.priority, "ipi");
}

#[test]
fn serializes_none_line_ending_as_separator_when_line_becomes_non_final() {
    // Given: a parsed file whose original final line had no trailing newline.
    let input = "## Later";
    let (mut file, diagnostics) = parse_todos(input);
    file.items.push(FileLine {
        item: TodoFileItem::Todo(Todo {
            id: "td_0123456789".to_string(),
            text: "New task".to_string(),
            priority: Vec::new(),
            stream: None,
            when: None,
            due: None,
            pin: false,
            quick: false,
            done: false,
        }),
        line_ending: LineEnding::None,
    });

    // When: the mutated file is serialized and parsed again.
    let rendered = file.to_string();
    let (round_trip, round_trip_diagnostics) = parse_todos(&rendered);

    // Then: the formerly-final raw line receives a separator and the file round-trips.
    assert!(diagnostics.is_empty());
    assert_eq!(
        rendered,
        "## Later\n- [ ] New task <!-- tt-todo:{\"id\":\"td_0123456789\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->"
    );
    assert!(round_trip_diagnostics.is_empty());
    assert_eq!(round_trip.items.len(), 2);
    assert_eq!(round_trip.items[0].line_ending, LineEnding::Lf);
    assert_eq!(round_trip.items[1].line_ending, LineEnding::None);
}

#[test]
fn parses_lenient_todo_without_id_when_metadata_omits_id() {
    // Given: a hand-authored raw todo line with complete grammar but no ID metadata.
    let line = "- [ ] Draft parser cleanup <!-- tt-todo:{\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":true} -->";

    // When: the shared lenient todo parser reads the line.
    let todo = parse_todo_lenient(line);

    // Then: the line parses with an empty ID for CLI normalization/display gates.
    assert_eq!(todo.as_ref().map(|item| item.id.as_str()), Some(""));
    assert_eq!(
        todo.as_ref().map(|item| item.text.as_str()),
        Some("Draft parser cleanup")
    );
    assert_eq!(
        todo.as_ref().map(|item| item.priority.as_slice()),
        Some(&["ipi".to_string()][..])
    );
    assert_eq!(todo.as_ref().map(|item| item.quick), Some(true));
    assert_eq!(todo.as_ref().map(|item| item.done), Some(false));
}

#[test]
fn parses_lenient_todo_with_existing_id_for_collision_detection() {
    // Given: a raw todo line that still carries an existing ID.
    let line = "- [x] Done parser cleanup <!-- tt-todo:{\"id\":\"td_existing01\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->";

    // When: the shared lenient todo parser reads the line.
    let todo = parse_todo_lenient(line);

    // Then: the ID and done state remain available for CLI collision detection.
    assert_eq!(
        todo.as_ref().map(|item| item.id.as_str()),
        Some("td_existing01")
    );
    assert_eq!(
        todo.as_ref().map(|item| item.text.as_str()),
        Some("Done parser cleanup")
    );
    assert_eq!(todo.as_ref().map(|item| item.done), Some(true));
}
