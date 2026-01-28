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

/// Minimal event format for JSONL storage (remote collection).
///
/// This is the wire format used for `events.jsonl` on remote machines.
/// Stream assignment fields are added during local import.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawEvent {
    /// Unique identifier (deterministic hash of content).
    pub id: String,
    /// When the event occurred (UTC).
    pub timestamp: DateTime<Utc>,
    /// Event type string (e.g., `tmux_pane_focus`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// Event source (e.g., "remote.tmux").
    pub source: String,
    /// Type-specific payload.
    pub data: serde_json::Value,
    /// Working directory, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
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
    /// tmux pane received focus.
    TmuxPaneFocus {
        /// The pane ID (e.g., "%3").
        pane_id: String,
        /// The tmux session name.
        session_name: String,
        /// The window index within the session.
        window_index: u32,
    },
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

    #[test]
    fn tmux_pane_focus_serialization() {
        let kind = EventKind::TmuxPaneFocus {
            pane_id: "%3".into(),
            session_name: "dev".into(),
            window_index: 1,
        };

        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains(r#""type":"tmux_pane_focus""#));
        assert!(json.contains(r#""pane_id":"%3""#));

        let parsed: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn raw_event_serialization_roundtrip() {
        let raw = RawEvent {
            id: "abc123".into(),
            timestamp: DateTime::parse_from_rfc3339("2025-01-25T14:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            event_type: "tmux_pane_focus".into(),
            source: "remote.tmux".into(),
            data: serde_json::json!({
                "pane_id": "%3",
                "session_name": "dev",
                "window_index": 1
            }),
            cwd: Some("/home/sami/project".into()),
        };

        let json = serde_json::to_string(&raw).unwrap();
        let parsed: RawEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, raw);
    }

    #[test]
    fn raw_event_json_format() {
        let raw = RawEvent {
            id: "abc123".into(),
            timestamp: DateTime::parse_from_rfc3339("2025-01-25T14:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            event_type: "tmux_pane_focus".into(),
            source: "remote.tmux".into(),
            data: serde_json::json!({
                "pane_id": "%3",
                "session_name": "dev",
                "window_index": 1
            }),
            cwd: Some("/home/sami/project".into()),
        };

        let json = serde_json::to_string(&raw).unwrap();
        // Verify field names match spec
        assert!(json.contains(r#""type":"tmux_pane_focus""#));
        assert!(json.contains(r#""source":"remote.tmux""#));
        assert!(json.contains(r#""cwd":"/home/sami/project""#));
    }

    #[test]
    fn raw_event_omits_null_cwd() {
        let raw = RawEvent {
            id: "abc123".into(),
            timestamp: Utc::now(),
            event_type: "test".into(),
            source: "test".into(),
            data: serde_json::json!({}),
            cwd: None,
        };

        let json = serde_json::to_string(&raw).unwrap();
        assert!(!json.contains("cwd"));
    }
}
