//! Core domain logic for the time tracker.
//!
//! This crate contains the fundamental types and logic for:
//! - Allocation: computing direct/delegated time for streams
//! - Session scanning: discovering Claude and `OpenCode` sessions
//! - Project identification: extracting project names from git remotes
//! - Attribution: resolving window focus that carries no `cwd`

mod allocation;
pub mod attribution;
pub mod classification;
pub mod event_type;
pub mod injection;
pub mod opencode;
pub mod project;
pub mod session;
pub mod slug;
pub mod todos;

pub use allocation::{
    AllocatableEvent, AllocationConfig, AllocationResult, Allocator, Interval, StreamTime,
    allocate_time,
};
pub use classification::{
    MisnamedReason, is_misnamed_stream, is_structurally_junk, normalize_stream_name,
};
pub use event_type::{EventType, UnknownEventType};
pub use injection::{INJECTION_MARKERS, human_message, is_injected};
pub use opencode::{scan_opencode_sessions, scan_opencode_sessions_incremental};
pub use session::{AgentSession, ScanOutcome, SessionSource, SessionType};
