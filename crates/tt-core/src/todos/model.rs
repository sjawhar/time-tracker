use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub(super) const PRIORITY_PREFIX: &str = "- [ ] ";
pub(super) const TODO_OPEN_PREFIX: &str = "- [ ] ";
pub(super) const TODO_DONE_PREFIX: &str = "- [x] ";
pub(super) const STREAM_PREFIX: &str = "- ";
pub(super) const PRIORITY_MARKER: &str = " <!-- tt-priority:";
pub(super) const TODO_MARKER: &str = " <!-- tt-todo:";
pub(super) const STREAM_MARKER: &str = " <!-- tt-stream:";
pub(super) const COMMENT_SUFFIX: &str = " -->";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    None,
    Lf,
    CrLf,
}

impl LineEnding {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLine<T> {
    pub item: T,
    pub line_ending: LineEnding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorityStatus {
    Active,
    Done,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Priority {
    pub slug: String,
    pub value: i32,
    pub status: PriorityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    pub id: String,
    pub text: String,
    pub priority: Vec<String>,
    pub stream: Option<String>,
    pub when: Option<NaiveDate>,
    pub due: Option<NaiveDate>,
    pub pin: bool,
    pub quick: bool,
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPriorityLink {
    pub stream: String,
    pub priority: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorityFileItem {
    Priority(Priority),
    Raw(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoFileItem {
    Todo(Todo),
    Raw(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamFileItem {
    Link(StreamPriorityLink),
    Raw(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityFile {
    pub items: Vec<FileLine<PriorityFileItem>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoFile {
    pub items: Vec<FileLine<TodoFileItem>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFile {
    pub items: Vec<FileLine<StreamFileItem>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoStore {
    pub priorities: PriorityFile,
    pub todos: TodoFile,
    pub streams: StreamFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub line_number: usize,
    pub raw_line: String,
    pub reason: TodoParseError,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TodoParseError {
    #[error("malformed {entry} line: expected {expected}")]
    InvalidGrammar {
        entry: &'static str,
        expected: &'static str,
    },
    #[error("JSON metadata parse error: {0}")]
    InvalidJson(String),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PriorityMetadata {
    pub(super) slug: String,
    pub(super) value: i32,
    pub(super) status: PriorityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) description: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TodoMetadata {
    pub(super) id: String,
    pub(super) priority: Vec<String>,
    pub(super) stream: Option<String>,
    pub(super) when: Option<NaiveDate>,
    pub(super) due: Option<NaiveDate>,
    pub(super) pin: bool,
    pub(super) quick: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) block: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StreamMetadata {
    pub(super) priority: String,
}
