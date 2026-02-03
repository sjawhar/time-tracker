//! Time tracker CLI library.
//!
//! This crate provides the CLI interface for the time tracker.

mod cli;
pub mod commands;
mod config;

pub use cli::{Cli, Commands, IngestEvent, StreamsAction};
pub use config::Config;
