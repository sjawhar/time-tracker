//! Time tracker CLI library.
//!
//! This crate provides the CLI interface for the time tracker.

mod cli;
pub mod commands;
mod config;
pub mod machine;
pub mod todo_store;

pub use cli::{Cli, Commands, IngestEvent, PriorityAction, StreamsAction, TodoAction};
pub use config::{Config, dirs_data_path, dirs_state_path};
