//! Core domain logic for the time tracker.
//!
//! This crate contains the fundamental types and logic for:
//! - Allocation: computing direct/delegated time for streams
//! - Session scanning: discovering Claude and `OpenCode` sessions
//! - Project identification: extracting project names from git remotes

mod allocation;
pub mod event_type;
pub mod opencode;
pub mod project;
pub mod session;

pub use allocation::{
    AllocatableEvent, AllocationConfig, AllocationResult, StreamTime, allocate_time,
};
pub use event_type::{EventType, UnknownEventType};
pub use opencode::scan_opencode_sessions;
pub use session::{AgentSession, SessionSource, SessionType};
