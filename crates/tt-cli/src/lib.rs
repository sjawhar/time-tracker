//! Time tracker CLI library.
//!
//! This crate provides the CLI interface for the time tracker.

mod cli;
pub mod commands;
mod config;
pub mod machine;

pub use cli::{Cli, Commands, IngestEvent, StreamsAction};
pub use config::{Config, dirs_data_path, dirs_state_path};
