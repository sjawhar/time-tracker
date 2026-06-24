use tt_core::todos::{Todo, parse_todo_lenient};

pub fn parse_raw_todo(line: &str) -> Option<Todo> {
    parse_todo_lenient(line)
}
