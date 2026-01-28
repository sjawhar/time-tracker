//! Core type definitions with validation.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Validation errors for core types.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// The provided value was empty.
    #[error("{field} cannot be empty")]
    Empty { field: &'static str },
}

/// A validated event identifier.
///
/// Event IDs must be non-empty strings. They should be unique within the system,
/// though uniqueness is enforced at the database level.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EventId(String);

impl EventId {
    /// Creates a new event ID after validation.
    pub fn new(id: impl Into<String>) -> Result<Self, ValidationError> {
        let id = id.into();
        if id.is_empty() {
            return Err(ValidationError::Empty { field: "event ID" });
        }
        Ok(Self(id))
    }

    /// Returns the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EventId {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<EventId> for String {
    fn from(id: EventId) -> Self {
        id.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A validated stream identifier.
///
/// Stream IDs must be non-empty strings. They identify the source of events
/// (e.g., "editor", "terminal", "browser").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StreamId(String);

impl StreamId {
    /// Creates a new stream ID after validation.
    pub fn new(id: impl Into<String>) -> Result<Self, ValidationError> {
        let id = id.into();
        if id.is_empty() {
            return Err(ValidationError::Empty { field: "stream ID" });
        }
        Ok(Self(id))
    }

    /// Returns the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for StreamId {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StreamId> for String {
    fn from(id: StreamId) -> Self {
        id.0
    }
}

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_id_rejects_empty() {
        assert!(EventId::new("").is_err());
        assert!(EventId::new("valid-id").is_ok());
    }

    #[test]
    fn stream_id_rejects_empty() {
        assert!(StreamId::new("").is_err());
        assert!(StreamId::new("editor").is_ok());
    }

    #[test]
    fn event_id_serde_roundtrip() {
        let id = EventId::new("test-123").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"test-123\"");
        let parsed: EventId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn event_id_serde_rejects_empty() {
        let result: Result<EventId, _> = serde_json::from_str("\"\"");
        assert!(result.is_err());
    }
}
