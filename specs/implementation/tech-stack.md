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

## Options Evaluated

### Option 1: Single Language (Rust everywhere)

| Aspect | Remote | Local |
|--------|--------|-------|
| Startup | ~1-5ms | ~1-5ms |
| LLM client | `anthropic-sdk` (community) or raw HTTP | Same |
| Shared code | Event types, serialization shared | Same |

**Pros**:
- Code sharing between remote and local
- Consistent tooling
- Fastest startup everywhere

**Cons**:
- Rust LLM ecosystem is immature compared to Python
- Longer iteration cycles

### Option 2: Single Language (Go everywhere)

| Aspect | Remote | Local |
|--------|--------|-------|
| Startup | ~10-20ms | ~10-20ms |
| LLM client | Raw HTTP (no official SDK) | Same |
| Shared code | Event types shared | Same |

**Pros**:
- Fast iteration
- Simple concurrency
- Good enough startup time

**Cons**:
- No official Anthropic SDK
- Less expressive types for event variants

### Option 3: Single Language (Python everywhere)

| Aspect | Remote | Local |
|--------|--------|-------|
| Startup | ~100-500ms | ~100-500ms |
| LLM client | `anthropic` official SDK | Same |

**Pros**:
- Best LLM ecosystem
- Fastest prototyping

**Cons**:
- Startup time is a hard blocker for tmux hooks
- Distribution complexity

### Option 4: Hybrid (Rust remote + Python local)

| Aspect | Remote | Local |
|--------|--------|-------|
| Language | Rust | Python |
| Startup | ~1-5ms | Not critical |
| LLM client | None (prototype) / raw HTTP (MVP) | `anthropic` SDK |
| SQLite | None | Built-in |

**Pros**:
- Best of both: fast hooks + rich LLM ecosystem
- Each component uses the right tool

**Cons**:
- Two languages to maintain
- Event types defined twice (or use JSON schema)
- Build/deploy complexity

### Option 5: Hybrid (Shell stub + Python)

| Aspect | Remote | Local |
|--------|--------|-------|
| tmux hook | Shell script (instant) | N/A |
| Event buffer | Append to JSONL via `echo >>` | N/A |
| Everything else | N/A | Python |

**Pros**:
- Shell is instant, already available everywhere
- Python for all "real" logic
- Simplest to iterate on

**Cons**:
- Less type safety on event format

## Decision

**Proposed: Option 5 (Shell stub + Python)**

Rationale:

1. **Prototype speed.** Shell is instant and already available. Python lets us iterate fast on the interesting parts (sync, inference, LLM).

2. **LLM ecosystem.** The official `anthropic` SDK is Python. Fighting with immature Rust/Go clients isn't worth it for MVP.

3. **Startup time solved simply.** The tmux hook is just:
   ```bash
   echo '{"type":"pane_focus","pane":"#{pane_id}",...}' >> ~/.time-tracker/events.jsonl
   ```
   This is instant. No process startup at all.

4. **Claude log parsing.** Python parses Claude session logs on-demand during `tt export`. No daemon needed.

5. **Migration path.** If startup time for `tt summarize` becomes a problem (MVP), we can rewrite just the remote CLI in Rust/Go. The JSONL format is language-agnostic.

**Trade-off accepted:** Less type safety on event format. Mitigated by:
- JSON schema definition
- Pydantic models in Python
- Tests that validate event round-tripping

---

## Alternative: Revisit for MVP

If the shell stub feels too hacky, or if `tt summarize` on remote needs fast startup:

**Fallback: Go for remote, Python for local**
- Go has good enough startup (~15ms)
- No LLM SDK needed on remote for prototype
- For MVP, use raw HTTP to Claude API (simple enough in Go)

## Tooling

### Remote (Shell + Python)

| Component | Implementation |
|-----------|----------------|
| tmux hook | Inline shell in `.tmux.conf` |
| Event buffer | Plain JSONL file, append via `echo >>` |
| Claude log parser | Python, runs on-demand during `tt export` (not a daemon) |
| Manifest | JSON file tracking byte offsets for incremental parsing |

### Local (Python)

| Tool | Choice |
|------|--------|
| Python version | 3.11+ (for `tomllib`, better typing) |
| Package manager | `uv` (fast, reliable) |
| CLI framework | `click` or `typer` |
| SQLite | Built-in `sqlite3` |
| JSON | Built-in `json` |
| Data validation | `pydantic` (event schemas) |
| HTTP/LLM client | `anthropic` SDK |
| DateTime | `datetime` + `zoneinfo` (stdlib) |
| UUID | `uuid` (stdlib) |
| Testing | `pytest` |

### Distribution

| Environment | Method |
|-------------|--------|
| Remote | Copy shell config + Python script; or `pipx install` |
| Local | `pipx install tt` or `uv tool install tt` |

### Future: If Go/Rust needed

If `tt summarize` (MVP) needs fast startup on remote:
- Rewrite remote CLI in Go
- Keep Python for local
- Share event schema via JSON Schema or protobuf
