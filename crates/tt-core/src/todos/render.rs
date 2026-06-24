use std::fmt;

use super::model::{
    COMMENT_SUFFIX, FileLine, PRIORITY_MARKER, PRIORITY_PREFIX, PriorityFile, PriorityFileItem,
    PriorityMetadata, STREAM_MARKER, STREAM_PREFIX, StreamFile, StreamFileItem, StreamMetadata,
    TODO_DONE_PREFIX, TODO_MARKER, TODO_OPEN_PREFIX, TodoFile, TodoFileItem, TodoMetadata,
};

trait RenderLine {
    fn render_line(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result;
}

fn write_lines<T: RenderLine>(items: &[FileLine<T>], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let last_index = items.len().saturating_sub(1);
    for (index, line) in items.iter().enumerate() {
        line.item.render_line(f)?;
        let line_ending = line.line_ending.as_str();
        f.write_str(if index == last_index || !line_ending.is_empty() {
            line_ending
        } else {
            "\n"
        })?;
    }
    Ok(())
}

impl fmt::Display for PriorityFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_lines(&self.items, f)
    }
}

impl fmt::Display for TodoFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_lines(&self.items, f)
    }
}

impl fmt::Display for StreamFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_lines(&self.items, f)
    }
}

impl RenderLine for PriorityFileItem {
    fn render_line(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Priority(priority) => {
                let metadata = PriorityMetadata {
                    slug: priority.slug.clone(),
                    value: priority.value,
                    status: priority.status,
                    description: priority.description.clone(),
                };
                let json = serde_json::to_string(&metadata).map_err(|_| fmt::Error)?;
                write!(
                    f,
                    "{PRIORITY_PREFIX}{}{PRIORITY_MARKER}{json}{COMMENT_SUFFIX}",
                    priority.slug
                )
            }
            Self::Raw(line) => f.write_str(line),
        }
    }
}

impl RenderLine for TodoFileItem {
    fn render_line(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Todo(todo) => {
                let metadata = TodoMetadata {
                    id: todo.id.clone(),
                    priority: todo.priority.clone(),
                    stream: todo.stream.clone(),
                    when: todo.when,
                    due: todo.due,
                    pin: todo.pin,
                    quick: todo.quick,
                    block: todo.block.clone(),
                };
                let prefix = if todo.done {
                    TODO_DONE_PREFIX
                } else {
                    TODO_OPEN_PREFIX
                };
                let json = serde_json::to_string(&metadata).map_err(|_| fmt::Error)?;
                write!(
                    f,
                    "{prefix}{}{TODO_MARKER}{json}{COMMENT_SUFFIX}",
                    todo.text
                )
            }
            Self::Raw(line) => f.write_str(line),
        }
    }
}

impl RenderLine for StreamFileItem {
    fn render_line(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Link(link) => {
                let metadata = StreamMetadata {
                    priority: link.priority.clone(),
                };
                let json = serde_json::to_string(&metadata).map_err(|_| fmt::Error)?;
                write!(
                    f,
                    "{STREAM_PREFIX}{}{STREAM_MARKER}{json}{COMMENT_SUFFIX}",
                    link.stream
                )
            }
            Self::Raw(line) => f.write_str(line),
        }
    }
}
