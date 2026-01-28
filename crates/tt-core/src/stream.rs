//! Event streams from various sources.

use serde::{Deserialize, Serialize};

use crate::types::StreamId;

/// A named collection of events from a specific source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stream {
    /// Unique identifier for this stream.
    pub id: StreamId,
    /// Human-readable name.
    pub name: String,
    /// Description of what this stream captures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
