//! Core domain logic for the time tracker.
//!
//! This crate contains the fundamental types and logic for:
//! - Events: raw activity signals from various sources
//! - Streams: named collections of events
//! - Time entries: consolidated entries for reporting
//! - Inference: clustering events into streams
//! - Allocation: computing direct/delegated time for streams
//! - Suggestion: tag suggestion from event metadata

mod allocation;
mod event;
pub mod inference;
pub mod opencode;
pub mod project;
pub mod session;
mod stream;
mod suggest;
mod types;

pub use allocation::{
    AllocatableEvent, AllocationConfig, AllocationResult, StreamTime, allocate_time,
};
pub use event::{Event, EventKind};
pub use inference::{
    InferableEvent, InferenceConfig, InferenceResult, InferredStream, StreamAssignment,
    infer_streams,
};
pub use opencode::scan_opencode_sessions;
pub use session::{AgentSession, SessionSource, SessionType};
pub use stream::Stream;
pub use suggest::{Suggestion, is_metadata_ambiguous, suggest_from_metadata};
pub use types::{AssignmentSource, Confidence, EventId, SessionId, StreamId, ValidationError};
