# Time Tracker (`tt`) - AI Assistant Guide

AI-native time tracker that passively collects activity signals from development tools and uses LLMs to generate accurate timesheets.

## Quick Commands

```bash
cargo build                     # Build all crates
cargo clippy --all-targets      # Lint (should pass with no warnings)
cargo fmt --check               # Check formatting
cargo test                      # Run all tests
cargo run -- --help             # Show CLI help
cargo deny check                # Check dependencies (licenses, advisories)
```

## Project Structure

```
crates/
├── tt-cli/     # Main CLI binary (clap-based)
├── tt-core/    # Domain logic: events, streams, time entries
├── tt-db/      # SQLite storage layer (rusqlite)
└── tt-llm/     # Claude API integration
```

### Crate Dependencies

```
tt-cli ─┬─> tt-core
        ├─> tt-db ───> tt-core
        └─> tt-llm ──> tt-core
```

## Code Style

### Error Handling

- **Library crates** (`tt-core`, `tt-db`, `tt-llm`): Use `thiserror` for typed errors
- **CLI crate** (`tt-cli`): Use `anyhow` for ergonomic error propagation

```rust
// In library crates
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database not found: {0}")]
    NotFound(PathBuf),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

// In CLI
fn main() -> anyhow::Result<()> {
    // Use context() for adding error context
    let db = Database::open(&path).context("failed to open database")?;
}
```

### Async

- Use `tokio` only where needed (HTTP calls in `tt-llm`, file watching)
- Prefer sync code in `tt-db` (rusqlite is sync)
- CLI uses `#[tokio::main]` but most operations are sync

### Logging

Use `tracing` macros with structured fields:

```rust
tracing::info!(event_count = events.len(), "processed events");
tracing::debug!(?event, "received event");
```

### Testing

- Use `insta` for snapshot testing CLI output and complex data structures
- Use `tempfile` for database tests
- Tests live in the same file as the code they test (`#[cfg(test)]` modules)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;

    #[test]
    fn test_format_output() {
        let output = format_entries(&entries);
        assert_snapshot!(output);
    }
}
```

## Key Types

### `tt-core`

- `Event` - Raw activity signal (file save, command run, etc.)
- `Stream` - Named collection of events from a source
- `TimeEntry` - Consolidated time entry for reporting

### `tt-db`

- `Database` - SQLite connection wrapper
- Event and time entry persistence

### `tt-llm`

- `Client` - Claude API client
- Prompt construction and response parsing

## Configuration

Uses `figment` for layered configuration:

1. Defaults (compiled in)
2. Config file (`~/.config/tt/config.toml`)
3. Environment variables (`TT_*`)

## Architecture Notes

- **Event sourcing**: Raw events are immutable, stored in SQLite
- **Lazy aggregation**: Time entries computed on demand from events
- **Offline-first**: All data local, LLM calls optional
- **Pluggable collectors**: Each tool (editor, terminal) has its own event stream
