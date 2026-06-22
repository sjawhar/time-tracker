use serde::Deserialize;

use super::model::{
    COMMENT_SUFFIX, FileLine, LineEnding, PRIORITY_MARKER, PRIORITY_PREFIX, ParseDiagnostic,
    Priority, PriorityFile, PriorityFileItem, PriorityMetadata, STREAM_MARKER, STREAM_PREFIX,
    StreamFile, StreamFileItem, StreamMetadata, StreamPriorityLink, TODO_DONE_PREFIX, TODO_MARKER,
    TODO_OPEN_PREFIX, Todo, TodoFile, TodoFileItem, TodoMetadata, TodoParseError,
};

struct ParsedFile<T> {
    items: Vec<FileLine<T>>,
    diagnostics: Vec<ParseDiagnostic>,
}

#[derive(Clone, Copy)]
struct LineGrammar {
    prefix: &'static str,
    marker: &'static str,
    entry: &'static str,
}

const PRIORITY_GRAMMAR: LineGrammar = LineGrammar {
    prefix: PRIORITY_PREFIX,
    marker: PRIORITY_MARKER,
    entry: "priority",
};
const TODO_OPEN_GRAMMAR: LineGrammar = LineGrammar {
    prefix: TODO_OPEN_PREFIX,
    marker: TODO_MARKER,
    entry: "todo",
};
const TODO_DONE_GRAMMAR: LineGrammar = LineGrammar {
    prefix: TODO_DONE_PREFIX,
    marker: TODO_MARKER,
    entry: "todo",
};
const STREAM_GRAMMAR: LineGrammar = LineGrammar {
    prefix: STREAM_PREFIX,
    marker: STREAM_MARKER,
    entry: "stream",
};

#[must_use]
pub fn parse_priorities(input: &str) -> (PriorityFile, Vec<ParseDiagnostic>) {
    let parsed = parse_markdown_file(input, parse_priority_line, PriorityFileItem::Raw);
    (
        PriorityFile {
            items: parsed.items,
        },
        parsed.diagnostics,
    )
}

#[must_use]
pub fn parse_todos(input: &str) -> (TodoFile, Vec<ParseDiagnostic>) {
    let parsed = parse_markdown_file(input, parse_todo_line, TodoFileItem::Raw);
    (
        TodoFile {
            items: parsed.items,
        },
        parsed.diagnostics,
    )
}

#[must_use]
pub fn parse_streams(input: &str) -> (StreamFile, Vec<ParseDiagnostic>) {
    let parsed = parse_markdown_file(input, parse_stream_line, StreamFileItem::Raw);
    (
        StreamFile {
            items: parsed.items,
        },
        parsed.diagnostics,
    )
}

#[must_use]
pub fn parse_todo_lenient(line: &str) -> Option<Todo> {
    if !line.contains(TODO_MARKER) {
        return None;
    }
    let (done, grammar) = if line.starts_with(TODO_DONE_PREFIX) {
        (true, TODO_DONE_GRAMMAR)
    } else {
        (false, TODO_OPEN_GRAMMAR)
    };
    let (text, json) = hidden_json(line, grammar).ok()?;
    let metadata = parse_lenient_todo_metadata(json)?;
    Some(todo_from_metadata(text, metadata, done))
}

fn parse_markdown_file<T>(
    input: &str,
    parse_line: fn(&str) -> Result<Option<T>, TodoParseError>,
    raw_item: fn(String) -> T,
) -> ParsedFile<T> {
    let mut items = Vec::new();
    let mut diagnostics = Vec::new();

    for (index, chunk) in input.split_inclusive('\n').enumerate() {
        let (line, ending) = split_line(chunk);
        let item = match parse_line(line) {
            Ok(Some(item)) => item,
            Ok(None) => raw_item(line.to_string()),
            Err(reason) => {
                diagnostics.push(ParseDiagnostic {
                    line_number: index + 1,
                    raw_line: line.to_string(),
                    reason,
                });
                raw_item(line.to_string())
            }
        };
        items.push(FileLine {
            item,
            line_ending: ending,
        });
    }

    ParsedFile { items, diagnostics }
}

fn split_line(chunk: &str) -> (&str, LineEnding) {
    match (chunk.strip_suffix("\r\n"), chunk.strip_suffix('\n')) {
        (Some(line), _) => (line, LineEnding::CrLf),
        (None, Some(line)) => (line, LineEnding::Lf),
        (None, None) => (chunk, LineEnding::None),
    }
}

fn parse_priority_line(line: &str) -> Result<Option<PriorityFileItem>, TodoParseError> {
    if !line.contains(PRIORITY_MARKER) {
        return Ok(None);
    }
    let (title, json) = hidden_json(line, PRIORITY_GRAMMAR)?;
    let metadata = parse_json::<PriorityMetadata>(json)?;
    Ok(Some(PriorityFileItem::Priority(Priority {
        title: title.to_string(),
        slug: metadata.slug,
        value: metadata.value,
        status: metadata.status,
    })))
}

fn parse_todo_line(line: &str) -> Result<Option<TodoFileItem>, TodoParseError> {
    if !line.contains(TODO_MARKER) {
        return Ok(None);
    }
    let (done, grammar) = if line.starts_with(TODO_DONE_PREFIX) {
        (true, TODO_DONE_GRAMMAR)
    } else {
        (false, TODO_OPEN_GRAMMAR)
    };
    let (text, json) = hidden_json(line, grammar)?;
    let metadata = parse_json::<TodoMetadata>(json)?;
    Ok(Some(TodoFileItem::Todo(todo_from_metadata(
        text, metadata, done,
    ))))
}

fn parse_lenient_todo_metadata(json: &str) -> Option<TodoMetadata> {
    let mut value = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let serde_json::Value::Object(object) = &mut value else {
        return None;
    };
    object
        .entry("id")
        .or_insert_with(|| serde_json::Value::String(String::new()));
    object
        .entry("priority")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    object
        .entry("pin")
        .or_insert_with(|| serde_json::Value::Bool(false));
    object
        .entry("quick")
        .or_insert_with(|| serde_json::Value::Bool(false));
    serde_json::from_value(value).ok()
}

fn todo_from_metadata(text: &str, metadata: TodoMetadata, done: bool) -> Todo {
    Todo {
        id: metadata.id,
        text: text.to_string(),
        priority: metadata.priority,
        stream: metadata.stream,
        when: metadata.when,
        due: metadata.due,
        pin: metadata.pin,
        quick: metadata.quick,
        done,
    }
}

fn parse_stream_line(line: &str) -> Result<Option<StreamFileItem>, TodoParseError> {
    if !line.contains(STREAM_MARKER) {
        return Ok(None);
    }
    let (stream, json) = hidden_json(line, STREAM_GRAMMAR)?;
    let metadata = parse_json::<StreamMetadata>(json)?;
    Ok(Some(StreamFileItem::Link(StreamPriorityLink {
        stream: stream.to_string(),
        priority: metadata.priority,
    })))
}

fn hidden_json(line: &str, grammar: LineGrammar) -> Result<(&str, &str), TodoParseError> {
    if !line.starts_with(grammar.prefix) {
        return Err(TodoParseError::InvalidGrammar {
            entry: grammar.entry,
            expected: grammar.prefix,
        });
    }
    let Some((visible, metadata)) = line[grammar.prefix.len()..].split_once(grammar.marker) else {
        return Err(TodoParseError::InvalidGrammar {
            entry: grammar.entry,
            expected: grammar.marker,
        });
    };
    let Some(json) = metadata.strip_suffix(COMMENT_SUFFIX) else {
        return Err(TodoParseError::InvalidGrammar {
            entry: grammar.entry,
            expected: COMMENT_SUFFIX,
        });
    };
    Ok((visible, json))
}

fn parse_json<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T, TodoParseError> {
    serde_json::from_str(json).map_err(|error| TodoParseError::InvalidJson(error.to_string()))
}
