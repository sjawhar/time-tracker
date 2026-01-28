//! Core domain logic for the time tracker.
//!
//! This crate contains the fundamental types and logic for:
//! - Events: raw activity signals from various sources
//! - Streams: named collections of events
//! - Time entries: consolidated entries for reporting

mod event;
pub mod id;
mod stream;
mod types;

pub use event::{Event, EventKind, RawEvent};
pub use id::generate_event_id;
pub use stream::Stream;
pub use types::{EventId, StreamId, ValidationError};
