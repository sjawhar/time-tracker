# Tech Stack

## Architecture Context

The system has two deployment targets with different constraints:

| Component | Runs on | Startup critical? | LLM calls? | SQLite? |
|-----------|---------|-------------------|------------|---------|
| **Remote CLI** | Dev server | Yes (<50ms) | MVP only (summarization) | No |
| **Local CLI** | Laptop | No | Yes (tagging) | Yes |

See [components.md](../architecture/components.md) for full architecture.

## Constraints by Component

### Remote CLI (`tt ingest`, `tt events`, `tt summarize`)

| Constraint | Requirement | Rationale |
|------------|-------------|-----------|
| **Startup time** | <50ms | tmux hooks fire on every pane focus |
| **Distribution** | Single binary | Deploy via `scp`; no runtime deps |
| **Storage** | JSONL append only | No SQLite needed; just buffer events |
| **LLM (MVP)** | Call Claude API | Summarize sessions without sending raw content |

### Local CLI (`tt sync`, `tt report`, `tt tag`, etc.)

| Constraint | Requirement | Rationale |
|------------|-------------|-----------|
| **Startup time** | Not critical | User-initiated commands, not hooks |
| **Distribution** | Single binary preferred | But local install is manageable |
| **Storage** | SQLite | Full event store + materialized views |
| **LLM (MVP)** | Call Claude API | Tag suggestions, report summaries |

## Required Capabilities

| Capability | Remote | Local |
|------------|--------|-------|
| JSON parsing | Yes | Yes |
| UUID generation | Yes | Yes |
| DateTime handling | Yes | Yes |
| JSONL file append | Yes | No |
| File watching | Yes (session logs) | No |
| SQLite | No | Yes |
| HTTP/LLM client | MVP | Yes |

## Decision

**Rust everywhere.**

A single Rust codebase serves both remote and local deployments with different feature flags or compile-time configuration.

### Rationale

1. **Single binary deployment.** Cross-compile for remote server and local laptop. No runtime dependencies.

2. **Fast startup everywhere.** Rust binaries start in 1-5ms, well under the 50ms constraint for tmux hooks.

3. **Code sharing.** Event types, serialization, and core logic shared between remote and local variants. No duplication.

4. **Type safety.** Strong types for events eliminate the "two languages defining the same types" problem.

5. **LLM ecosystem has matured.** Raw HTTP to Claude API is straightforward with `reqwest`. No SDK needed.

6. **Consistency.** One toolchain, one test suite, one CI pipeline.

### Trade-offs Accepted

- **Longer compile times** than interpreted languages. Mitigated by incremental compilation and workspace structure.
- **Slower iteration** on LLM prompts. Mitigated by keeping prompts in config files or const strings that don't require recompilation.

---

## Project Structure

```
crates/
├── tt-cli/     # Main CLI binary (clap-based)
├── tt-core/    # Domain logic: events, streams, time entries
├── tt-db/      # SQLite storage layer (rusqlite)
└── tt-llm/     # Claude API integration (reqwest)
```

### Crate Dependencies

```
tt-cli ─┬─> tt-core
        ├─> tt-db ───> tt-core
        └─> tt-llm ──> tt-core
```

## Tooling

| Tool | Choice |
|------|--------|
| Rust version | 1.85+ (Rust 2024 edition) |
| CLI framework | `clap` (derive macros) |
| Async runtime | `tokio` (HTTP, file watching) |
| Serialization | `serde` + `serde_json` |
| SQLite | `rusqlite` (bundled) |
| HTTP client | `reqwest` (rustls) |
| DateTime | `chrono` |
| Configuration | `figment` (TOML + env vars) |
| Error handling | `thiserror` (libraries), `anyhow` (CLI) |
| Logging | `tracing` |
| Testing | built-in + `insta` (snapshots) + `tempfile` |

## Distribution

| Environment | Method |
|-------------|--------|
| Remote | Cross-compile, deploy via `scp` |
| Local | `cargo install` or download release binary |

## Remote vs Local Variants

Both remote and local use the same binary. Command availability differs:

| Command | Remote | Local | Notes |
|---------|--------|-------|-------|
| `tt ingest` | Yes | — | Append events to JSONL buffer |
| `tt export` | Yes | — | Emit events for sync |
| `tt summarize` | MVP | — | LLM summarization (privacy) |
| `tt import` | — | Yes | Receive events from stdin |
| `tt sync` | — | Yes | Pull from remote via SSH |
| `tt events` | Yes | Yes | Query events (JSONL or SQLite) |
| `tt status` | Yes | Yes | Health check |
| `tt streams` | — | MVP | List inferred streams |
| `tt tag` | — | MVP | Manual tagging |
| `tt report` | — | MVP | Generate time report |

The binary detects its role by:
1. Explicit `--remote` / `--local` flag, or
2. Presence of SQLite database (local) vs JSONL buffer (remote)
