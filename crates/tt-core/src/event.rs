//! Raw activity events from various sources.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{EventId, StreamId};

/// A raw activity signal captured from a development tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique identifier for this event.
    pub id: EventId,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// The type of activity.
    pub kind: EventKind,
    /// The stream this event belongs to.
    pub stream: StreamId,
    /// Optional additional context as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// The type of activity captured.
///
/// # Path Safety
///
/// The `path` field in `FileSave` is stored as-is from the event source.
/// Consumers should validate and normalize paths before performing any
/// filesystem operations to prevent path traversal attacks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// A file was saved or modified.
    FileSave {
        /// The file path. Should be validated before use in filesystem operations.
        path: String,
    },
    /// A command was executed.
    Command {
        /// The command string. Should be sanitized before display in terminals.
        command: String,
    },
    /// A generic heartbeat/presence signal.
    Heartbeat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serialization_roundtrip() {
        let event = Event {
            id: EventId::new("test-1").unwrap(),
            timestamp: Utc::now(),
            kind: EventKind::FileSave {
                path: "/src/main.rs".into(),
            },
            stream: StreamId::new("editor").unwrap(),
            metadata: None,
        };

        let json = serde_json::to_string(&event).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, event.id);
        assert_eq!(parsed.stream, event.stream);
    }

    #[test]
    fn event_rejects_empty_ids() {
        let json = r#"{
            "id": "",
            "timestamp": "2024-01-01T00:00:00Z",
            "kind": {"type": "heartbeat"},
            "stream": "editor"
        }"#;
        let result: Result<Event, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
