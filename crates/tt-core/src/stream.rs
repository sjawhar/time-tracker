//! Event streams - coherent units of work.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::StreamId;

/// A coherent unit of work, grouping related events.
///
/// Streams are materialized for performance but can be recomputed from events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stream {
    /// Unique identifier (UUID).
    pub id: StreamId,

    /// Human-readable name (auto-generated or user-provided).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// When the stream was created.
    pub created_at: DateTime<Utc>,

    /// When the stream was last updated.
    pub updated_at: DateTime<Utc>,

    /// Total human attention time in milliseconds.
    #[serde(default)]
    pub time_direct_ms: i64,

    /// Total agent execution time in milliseconds.
    #[serde(default)]
    pub time_delegated_ms: i64,

    /// Timestamp of the first event in this stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_event_at: Option<DateTime<Utc>>,

    /// Timestamp of the last event in this stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<DateTime<Utc>>,

    /// Flag for lazy recomputation.
    #[serde(default)]
    pub needs_recompute: bool,
}

impl Stream {
    /// Creates a new stream with the given ID and name.
    pub fn new(id: StreamId, name: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id,
            name,
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_new() {
        let id = StreamId::new("test-stream").unwrap();
        let stream = Stream::new(id.clone(), Some("Test Stream".to_string()));

        assert_eq!(stream.id, id);
        assert_eq!(stream.name, Some("Test Stream".to_string()));
        assert_eq!(stream.time_direct_ms, 0);
        assert_eq!(stream.time_delegated_ms, 0);
        assert!(!stream.needs_recompute);
    }

    #[test]
    fn test_stream_serde_roundtrip() {
        let id = StreamId::new("stream-123").unwrap();
        let stream = Stream::new(id, Some("My Project".to_string()));

        let json = serde_json::to_string(&stream).unwrap();
        let parsed: Stream = serde_json::from_str(&json).unwrap();

        assert_eq!(stream.id, parsed.id);
        assert_eq!(stream.name, parsed.name);
    }
}
