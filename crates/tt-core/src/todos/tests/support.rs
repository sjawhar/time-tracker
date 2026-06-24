use chrono::NaiveDate;

use super::super::{Priority, PriorityStatus, StreamPriorityLink, StreamTimeInput, Todo};

pub fn priority(slug: &str, value: i32, status: PriorityStatus) -> Priority {
    Priority {
        slug: slug.to_string(),
        value,
        status,
        description: None,
    }
}

pub fn stream_link(stream: &str, priority: &str) -> StreamPriorityLink {
    StreamPriorityLink {
        stream: stream.to_string(),
        priority: priority.to_string(),
    }
}

pub fn stream_time(stream_name: &str, direct_ms: i64, delegated_ms: i64) -> StreamTimeInput {
    StreamTimeInput {
        stream_name: stream_name.to_string(),
        direct_ms,
        delegated_ms,
    }
}

pub fn todo(
    id: &str,
    priorities: &[&str],
    stream: Option<&str>,
    when: Option<NaiveDate>,
    due: Option<NaiveDate>,
    pin: bool,
    done: bool,
) -> Todo {
    Todo {
        id: id.to_string(),
        text: id.to_string(),
        priority: priorities
            .iter()
            .map(|priority| (*priority).to_string())
            .collect(),
        stream: stream.map(str::to_string),
        when,
        due,
        pin,
        quick: false,
        done,
        block: None,
    }
}

pub fn section_ids<'a>(todos: &[&'a Todo]) -> Vec<&'a str> {
    todos.iter().map(|todo| todo.id.as_str()).collect()
}

pub fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < f64::EPSILON * 16.0,
        "actual={actual}, expected={expected}"
    );
}
