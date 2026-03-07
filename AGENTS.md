# Time Tracker (`tt`) - AI Assistant Guide

AI-native time tracker that passively collects activity signals from development tools and uses LLMs to generate accurate timesheets.

## Structure

```
crates/
├── tt-cli/        # CLI binary ("tt"). Clap dispatch, 9 subcommands, anyhow errors
│   ├── src/
│   │   ├── main.rs         # Entry point: parse args → open db → dispatch
│   │   ├── cli.rs          # Clap definitions (Commands, IngestEvent, StreamsAction)
│   │   ├── config.rs       # Figment config loading (defaults → TOML → env)
│   │   ├── machine.rs      # Machine identity (UUID) for multi-machine sync
│   │   └── commands/       # One file per subcommand + snapshots/
│   └── tests/e2e_flow.rs   # Integration tests (spawns actual binary)
├── tt-core/       # Domain logic. See crates/tt-core/AGENTS.md
└── tt-db/         # SQLite storage. See crates/tt-db/AGENTS.md
config/            # tmux-hook.conf (event capture setup)
scripts/           # deploy-remote.sh (binary → remote ~/.local/bin/tt)
specs/             # Architecture docs, design specs, research notes
```

### Crate Dependencies

```
tt-cli ─┬─> tt-core
        └─> tt-db ───> tt-core
```

`tt-llm` is referenced in specs but does not exist yet.

## Where to Look

| Task | Location | Notes |
|------|----------|-------|
| Add CLI subcommand | `tt-cli/src/cli.rs` + `commands/{name}.rs` + `commands/mod.rs` | Follow existing pattern (see `tag.rs` for simple, `report.rs` for complex) |
| Change time algorithm | `tt-core/src/allocation.rs` | 1366-line algo with extensive tests. See `tt-core/AGENTS.md` |
| Add DB table/column | `tt-db/src/lib.rs` | Bump `SCHEMA_VERSION`, add to `init()`. No migrations—schema mismatch = fail-fast |
| Add event type | `tt-db/src/lib.rs` (`StoredEvent`) | Then handle in `allocation.rs` and relevant command |
| Session scanning | `tt-core/src/session.rs` (Claude), `tt-core/src/opencode.rs` (OpenCode) | Claude: parse JSONL session files from `~/.claude/`. OpenCode: query SQLite database via rusqlite |
| Config options | `tt-cli/src/config.rs` | Figment: defaults → `~/.config/tt/config.toml` → `TT_*` env vars |
| Snapshot test update | Run `cargo insta review` | 18 snapshots in `tt-cli/src/commands/snapshots/` |
| Add machine support | `tt-cli/src/machine.rs` | UUID-based identity per machine |
| Multi-machine sync | `tt-cli/src/commands/sync.rs` | SSH-based event pull |
| Deploy binary | `scripts/deploy-remote.sh` | Builds release, copies via SSH, optionally configures tmux hook |

## Commands

```bash
cargo build                     # Build all crates
cargo clippy --all-targets      # Lint (must pass with zero warnings; -D warnings in CI)
cargo fmt --check               # Format check (max_width=100)
cargo test                      # Run all tests (unit + integration + snapshots)
cargo deny check                # Dependency audit (licenses + advisories)
cargo insta review              # Review snapshot test changes
cargo run -- --help             # Show CLI help
tt init --label devbox          # Initialize machine identity
tt sync devbox gpu-server       # Pull events from remote machines
tt machines                     # List known remote machines
tt classify --json              # Show sessions + events for classification
tt classify --apply input.json  # Apply stream assignments from LLM
tt classify --unclassified      # Show only unassigned data
```

CI (`.github/workflows/pr-and-main.yml`): lint job (fmt + deny) + build job (clippy + test). Runs on push to main and PRs.

## Code Conventions

### Error Handling

- **Library crates** (`tt-core`, `tt-db`): `thiserror` typed errors
- **CLI crate** (`tt-cli`): `anyhow` with `.context("message")`

### Lint Suppressions

Use `#[expect(clippy::lint_name, reason = "...")]` — never bare `#[allow]`. Every suppression needs a reason string.

### Workspace Lints (Cargo.toml)

- `unsafe_code = "deny"` — no unsafe anywhere
- `clippy::all`, `pedantic`, `nursery` = warn
- Allowed: `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`

### Async

Sync everywhere except future `tt-llm` (which would use tokio for HTTP). CLI has `#[tokio::main]`-ready but currently all sync. `tt-db` is sync (rusqlite).

### Logging

`tracing` with structured fields: `tracing::info!(count = events.len(), "processed")`. CLI `--verbose` flag sets `debug` level.

### Testing Patterns

- **Unit tests**: `#[cfg(test)] mod tests` at bottom of same file
- **Snapshots**: `insta::assert_snapshot!` for CLI output (report, streams, status)
- **DB tests**: `Database::open_in_memory()` — no filesystem needed
- **Fixture builders**: `make_event()`, `make_test_stream()`, `TestEvent::tmux_focus()` — small helpers with sensible defaults
- **Integration**: `tt-cli/tests/e2e_flow.rs` spawns actual binary via `Command`
- **Temp dirs**: `tempfile::TempDir` for filesystem isolation

### Configuration

Figment layered loading: compiled defaults → `~/.config/tt/config.toml` → `TT_*` env vars. Currently only `database_path` is configurable.

XDG directory layout:
```
Config: ~/.config/tt/config.toml
Data:   ~/.local/share/tt/ (tt.db, events.jsonl, machine.json)
State:  ~/.local/state/tt/ (hook.log, claude-manifest.json)
```

## Anti-Patterns

- **No migrations**: Schema version mismatch = hard error. DB must be recreated on schema change.
- **No `unwrap()` in non-test code** (except compile-time-safe patterns like `LazyLock` regex, hardcoded `NaiveTime`)
- **No `tt-llm` crate yet** — docs reference it but it's unimplemented

## Key Types

| Type | Crate | Role |
|------|-------|------|
| `StoredEvent` | tt-db | Raw activity signal (file save, pane focus, AFK, agent action) |
| `Database` | tt-db | SQLite connection wrapper (80+ methods) |
| `Stream` | tt-db | Coherent unit of work with direct/delegated time |
| `AllocatableEvent` | tt-core | Trait for events that participate in time allocation |
| `AllocationConfig` | tt-core | Tunables: attention_window (1min), agent_timeout (30min) |
| `AgentSession` | tt-core | Parsed Claude/OpenCode session metadata |
| `Config` | tt-cli | App config (database_path) |
| `MachineIdentity` | tt-cli | Persistent UUID + label per machine |
| `Machine` | tt-db | Known remote machine with sync state |
