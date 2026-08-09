//! The log filter every tt binary starts with.
//!
//! Shared because all three had the same hole. `EnvFilter::from_default_env()` carries
//! `LevelFilter::ERROR` as its default directive, so with `RUST_LOG` unset — which is
//! how `tt` is run interactively and how `config/tt-serve.service` runs the daemon —
//! not one `warn!` in this workspace reached a terminal. The classifier logs every
//! failed session at `warn!` with its id, and a live pass counted 315 of them while
//! printing nothing about which ones; the count said something was wrong and the only
//! record of *what* was being filtered away at the subscriber.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

/// Builds the filter for a binary invoked with `verbose` repetitions of `-v`.
///
/// Three layers, most specific first: `-v` forces `debug`, otherwise `RUST_LOG` is
/// honoured as written, otherwise the floor is `WARN`. A warning is a line about work
/// that did not happen, so it is on by default; anything chattier still has to be
/// asked for.
#[must_use]
pub fn filter(verbose: u8) -> EnvFilter {
    if verbose > 0 {
        return EnvFilter::new("debug");
    }
    EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .from_env_lossy()
}

/// Renders an `anyhow` error with every cause beneath it.
///
/// `tracing::warn!(%error, …)` formats with `Display`, and `Display` on an
/// `anyhow::Error` prints **only the outermost context**. Every cause below it is
/// dropped silently, so a daemon that logs `session ingest failed error=failed to
/// attribute events to their own classified session's stream` every 30 seconds names
/// the symptom and withholds the defect — the `database is locked` underneath it never
/// reaches the journal. That is the hole this module's own doc comment describes, one
/// level down: there the subscriber filtered the record away, here the formatter throws
/// half of it away before the subscriber ever sees it.
///
/// Call it as `error = %logging::chain(&error)` at any site that logs an
/// `anyhow::Error`. It allocates, which is why it is on the error path only.
#[must_use]
pub fn chain(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

#[cfg(test)]
mod tests {
    use super::chain;

    #[test]
    fn chain_keeps_the_cause_that_plain_display_throws_away() {
        let error = anyhow::anyhow!("database is locked")
            .context("claim unassigned events")
            .context("failed to attribute events");

        // What the daemon logged before: the outermost context, and nothing else.
        assert_eq!(format!("{error}"), "failed to attribute events");

        // What it logs now: the whole chain, innermost cause included.
        let rendered = chain(&error);
        assert!(
            rendered.contains("failed to attribute events"),
            "{rendered}"
        );
        assert!(rendered.contains("claim unassigned events"), "{rendered}");
        assert!(rendered.contains("database is locked"), "{rendered}");
    }
}
