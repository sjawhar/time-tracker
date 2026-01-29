//! Core domain logic for the time tracker.
//!
//! This crate contains the fundamental types and logic for:
//! - Events: raw activity signals from various sources
//! - Streams: named collections of events
//! - Time entries: consolidated entries for reporting
//! - Inference: clustering events into streams

mod event;
pub mod inference;
mod stream;
mod types;

pub use event::{Event, EventKind};
pub use inference::{
    InferableEvent, InferenceConfig, InferenceResult, InferredStream, StreamAssignment,
    infer_streams,
};
pub use stream::Stream;
pub use types::{EventId, StreamId, ValidationError};
