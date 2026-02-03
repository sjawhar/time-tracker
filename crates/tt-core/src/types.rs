//! Core type definitions with validation.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Validation errors for core types.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum ValidationError {
    /// The provided value was empty.
    #[error("{field} cannot be empty")]
    Empty { field: &'static str },

    /// The confidence value was out of range.
    #[error("confidence must be between 0.0 and 1.0, got {value}")]
    ConfidenceOutOfRange { value: f32 },

    /// Invalid assignment source value.
    #[error("invalid assignment source: {value}")]
    InvalidAssignmentSource { value: String },
}

/// How an event was assigned to a stream.
///
/// This enum encodes the valid assignment sources, preventing invalid string values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssignmentSource {
    /// Assigned by the inference algorithm.
    Inferred,
    /// Manually assigned by the user.
    User,
}

impl AssignmentSource {
    /// String representation for database storage.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Inferred => "inferred",
            Self::User => "user",
        }
    }
}

impl fmt::Display for AssignmentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for AssignmentSource {
    type Err = ValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "inferred" => Ok(Self::Inferred),
            "user" => Ok(Self::User),
            _ => Err(ValidationError::InvalidAssignmentSource {
                value: s.to_string(),
            }),
        }
    }
}

/// Generates a validated string ID newtype with common trait implementations.
macro_rules! define_string_id {
    (
        $(#[$meta:meta])*
        $name:ident, $field_name:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Creates a new ID after validation.
            pub fn new(id: impl Into<String>) -> Result<Self, ValidationError> {
                let id = id.into();
                if id.is_empty() {
                    return Err(ValidationError::Empty { field: $field_name });
                }
                Ok(Self(id))
            }

            /// Returns the ID as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

define_string_id!(
    /// A validated event identifier.
    ///
    /// Event IDs must be non-empty strings. They should be unique within the system,
    /// though uniqueness is enforced at the database level.
    EventId, "event ID"
);

define_string_id!(
    /// A validated stream identifier.
    ///
    /// Stream IDs must be non-empty strings. They identify the source of events
    /// (e.g., "editor", "terminal", "browser").
    StreamId, "stream ID"
);

define_string_id!(
    /// A validated session identifier.
    ///
    /// Session IDs must be non-empty strings. They identify Claude Code sessions
    /// or other agent sessions.
    SessionId, "session ID"
);

/// A confidence score in the range \[0.0, 1.0\].
///
/// Used to indicate how confident the system is in a classification or inference.
/// Values are clamped during deserialization to ensure they stay within bounds.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f32);

impl Confidence {
    /// The maximum confidence value (1.0).
    pub const MAX: Self = Self(1.0);

    /// The minimum confidence value (0.0).
    pub const MIN: Self = Self(0.0);

    /// Creates a new confidence value after validation.
    ///
    /// Returns an error if the value is outside \[0.0, 1.0\] or is NaN.
    pub fn new(value: f32) -> Result<Self, ValidationError> {
        if value.is_nan() || !(0.0..=1.0).contains(&value) {
            return Err(ValidationError::ConfidenceOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Creates a confidence value, clamping to \[0.0, 1.0\].
    ///
    /// NaN values become 0.0. Values outside the range are clamped.
    #[must_use]
    pub const fn clamped(value: f32) -> Self {
        if value.is_nan() || value < 0.0 {
            Self(0.0)
        } else if value > 1.0 {
            Self(1.0)
        } else {
            Self(value)
        }
    }

    /// Returns the inner f32 value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.0
    }
}

impl Default for Confidence {
    fn default() -> Self {
        Self::MAX
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

impl TryFrom<f32> for Confidence {
    type Error = ValidationError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Confidence> for f32 {
    fn from(c: Confidence) -> Self {
        c.0
    }
}

impl Serialize for Confidence {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = f32::deserialize(deserializer)?;
        // Clamp on deserialization to be lenient with external data
        Ok(Self::clamped(value))
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

    #[test]
    fn confidence_validates_range() {
        assert!(Confidence::new(0.0).is_ok());
        assert!(Confidence::new(0.5).is_ok());
        assert!(Confidence::new(1.0).is_ok());
        assert!(Confidence::new(-0.1).is_err());
        assert!(Confidence::new(1.1).is_err());
        assert!(Confidence::new(f32::NAN).is_err());
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "exact equality intended for boundary tests"
    )]
    fn confidence_clamped_handles_edge_cases() {
        assert_eq!(Confidence::clamped(-1.0).value(), 0.0);
        assert_eq!(Confidence::clamped(2.0).value(), 1.0);
        assert_eq!(Confidence::clamped(f32::NAN).value(), 0.0);
        assert_eq!(Confidence::clamped(0.5).value(), 0.5);
    }

    #[test]
    fn confidence_serde_roundtrip() {
        let c = Confidence::new(0.85).unwrap();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "0.85");
        let parsed: Confidence = serde_json::from_str(&json).unwrap();
        assert!((parsed.value() - 0.85).abs() < f32::EPSILON);
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "exact equality intended for boundary tests"
    )]
    fn confidence_serde_clamps_out_of_range() {
        // Deserialization should clamp values outside [0.0, 1.0]
        let parsed: Confidence = serde_json::from_str("1.5").unwrap();
        assert_eq!(parsed.value(), 1.0);

        let parsed: Confidence = serde_json::from_str("-0.5").unwrap();
        assert_eq!(parsed.value(), 0.0);
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "exact equality intended for default value"
    )]
    fn confidence_default_is_max() {
        assert_eq!(Confidence::default().value(), 1.0);
    }

    // ========== SessionId Tests ==========

    #[test]
    fn session_id_rejects_empty() {
        assert!(SessionId::new("").is_err());
        assert!(SessionId::new("valid-session").is_ok());
    }

    #[test]
    fn session_id_serde_roundtrip() {
        let id = SessionId::new("session-abc").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"session-abc\"");
        let parsed: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn session_id_as_ref() {
        let id = SessionId::new("my-session").unwrap();
        let s: &str = id.as_ref();
        assert_eq!(s, "my-session");
    }

    // ========== AssignmentSource Tests ==========

    #[test]
    fn assignment_source_from_str() {
        assert_eq!(
            "inferred".parse::<AssignmentSource>().unwrap(),
            AssignmentSource::Inferred
        );
        assert_eq!(
            "user".parse::<AssignmentSource>().unwrap(),
            AssignmentSource::User
        );
        assert!("invalid".parse::<AssignmentSource>().is_err());
    }

    #[test]
    fn assignment_source_as_str() {
        assert_eq!(AssignmentSource::Inferred.as_str(), "inferred");
        assert_eq!(AssignmentSource::User.as_str(), "user");
    }

    #[test]
    fn assignment_source_serde_roundtrip() {
        let inferred = AssignmentSource::Inferred;
        let json = serde_json::to_string(&inferred).unwrap();
        assert_eq!(json, "\"inferred\"");
        let parsed: AssignmentSource = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, inferred);

        let user = AssignmentSource::User;
        let json = serde_json::to_string(&user).unwrap();
        assert_eq!(json, "\"user\"");
        let parsed: AssignmentSource = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, user);
    }

    // ========== AsRef Tests ==========

    #[test]
    fn event_id_as_ref() {
        let id = EventId::new("event-123").unwrap();
        let s: &str = id.as_ref();
        assert_eq!(s, "event-123");
    }

    #[test]
    fn stream_id_as_ref() {
        let id = StreamId::new("stream-456").unwrap();
        let s: &str = id.as_ref();
        assert_eq!(s, "stream-456");
    }
}
