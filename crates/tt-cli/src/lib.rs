//! Time tracker CLI library.
//!
//! This crate provides the CLI interface for the time tracker.

mod cli;
mod config;

pub use cli::{Cli, Commands};
pub use config::Config;
