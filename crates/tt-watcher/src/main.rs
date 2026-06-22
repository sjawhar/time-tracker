//! `tt-watcher` — COSMIC window/idle watcher daemon.
//!
//! Captures active-window focus and AFK/idle transitions from a COSMIC Wayland
//! session and writes them directly to the shared `tt` `SQLite` database. Runs
//! as a systemd user service (see `config/tt-watcher.service`).

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "tt-watcher",
    version,
    about = "Watch the COSMIC desktop for active-window and idle events"
)]
struct Args {
    /// Path to a custom configuration TOML file (defaults to standard XDG locations).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Idle timeout in seconds before emitting an idle event.
    #[arg(long)]
    idle_timeout: Option<u64>,

    /// Poll interval in milliseconds.
    #[arg(long)]
    poll_ms: Option<u64>,

    /// Print events as JSONL instead of writing to `SQLite` (useful for debugging).
    #[arg(long)]
    no_write: bool,

    /// Poll once, emit/write any resulting events, then exit.
    #[arg(long)]
    once: bool,

    /// Increase logging verbosity (-v debug, -vv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let filter = if args.verbose > 0 {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

    tt_watcher::run(
        args.config.as_deref(),
        args.idle_timeout,
        args.poll_ms,
        args.no_write,
        args.once,
    )
}
