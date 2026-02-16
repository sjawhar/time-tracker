//! Event type enum as the single source of truth for event type strings.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Canonical event types for time tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    AgentSession,
    AgentToolUse,
    UserMessage,
    TmuxPaneFocus,
    TmuxScroll,
    AfkChange,
    WindowFocus,
    BrowserTab,
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::AgentSession => "agent_session",
            Self::AgentToolUse => "agent_tool_use",
            Self::UserMessage => "user_message",
            Self::TmuxPaneFocus => "tmux_pane_focus",
            Self::TmuxScroll => "tmux_scroll",
            Self::AfkChange => "afk_change",
            Self::WindowFocus => "window_focus",
            Self::BrowserTab => "browser_tab",
        };
        write!(f, "{s}")
    }
}

impl FromStr for EventType {
    type Err = UnknownEventType;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "agent_session" | "session_start" | "session_end" => Ok(Self::AgentSession),
            "agent_tool_use" => Ok(Self::AgentToolUse),
            "user_message" => Ok(Self::UserMessage),
            "tmux_pane_focus" => Ok(Self::TmuxPaneFocus),
            "tmux_scroll" => Ok(Self::TmuxScroll),
            "afk_change" => Ok(Self::AfkChange),
            "window_focus" => Ok(Self::WindowFocus),
            "browser_tab" => Ok(Self::BrowserTab),
            _ => Err(UnknownEventType(s.to_string())),
        }
    }
}

impl Serialize for EventType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Error type for unknown event type strings.
#[derive(Debug, Clone)]
pub struct UnknownEventType(String);

impl fmt::Display for UnknownEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown event type: {}", self.0)
    }
}

impl std::error::Error for UnknownEventType {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_variants() {
        let variants = [
            EventType::AgentSession,
            EventType::AgentToolUse,
            EventType::UserMessage,
            EventType::TmuxPaneFocus,
            EventType::TmuxScroll,
            EventType::AfkChange,
            EventType::WindowFocus,
            EventType::BrowserTab,
        ];

        for variant in &variants {
            let s = variant.to_string();
            let parsed: EventType = s.parse().expect("should parse");
            assert_eq!(parsed, *variant, "roundtrip failed for {variant:?}");
        }
    }

    #[test]
    fn legacy_aliases_parse() {
        let session_start: EventType = "session_start".parse().expect("should parse");
        assert_eq!(session_start, EventType::AgentSession);

        let session_end: EventType = "session_end".parse().expect("should parse");
        assert_eq!(session_end, EventType::AgentSession);
    }

    #[test]
    fn unknown_type_errors() {
        let result: Result<EventType, _> = "unknown_type".parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.to_string(), "unknown event type: unknown_type");
    }
}
