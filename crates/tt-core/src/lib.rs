//! Core domain logic for the time tracker.
//!
//! This crate contains the fundamental types and logic for:
//! - Events: raw activity signals from various sources
//! - Streams: named collections of events
//! - Time entries: consolidated entries for reporting
//! - Inference: clustering events into streams
//! - Allocation: computing direct/delegated time for streams

mod allocation;
mod event;
pub mod inference;
mod stream;
mod types;

pub use allocation::{
    AllocatableEvent, AllocationConfig, AllocationResult, StreamTime, allocate_time,
};
pub use event::{Event, EventKind};
pub use inference::{
    InferableEvent, InferenceConfig, InferenceResult, InferredStream, StreamAssignment,
    infer_streams,
};
pub use stream::Stream;
pub use types::{EventId, StreamId, ValidationError};
