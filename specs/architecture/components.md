# Components

## Deployment Model

The system spans two environments:

| Environment | What runs there | Why |
|-------------|-----------------|-----|
| **Remote** (dev server) | Event collectors, optional LLM summarization | Events originate here (tmux, Claude sessions) |
| **Local** (laptop) | SQLite store, LLM analysis, reports, CLI | User's primary interface, persistent storage |

Events flow from remote to local via pull-based sync (SSH) or shared storage. See [ADR-001](decisions/001-event-transport.md).

### Prototype Scope

The prototype proves: **"Can we capture useful data?"**

| What | How | Notes |
|------|-----|-------|
| tmux focus events | Automatic (hooks) | tmux fires hooks on pane focus → append to `events.jsonl` |
| Claude session events | On-demand (sync) | Parsed from existing logs during `tt sync` — no daemon needed |
| Sync to local | Manual | User runs `tt sync`; automation is MVP |

The only running component on remote is the tmux hook (which tmux handles). Everything else runs on-demand.

---

## Component List

| Component | Runs on | Prototype | MVP |
|-----------|---------|-----------|-----|
| `tt` CLI (remote) | Remote | Yes | Yes |
| tmux hooks | Remote | Yes | Yes |
| Claude log parser | Remote (on-demand) | Yes | Yes |
| `tt` CLI (local) | Local | Yes | Yes |
| SQLite store | Local | Yes | Yes |
| Event sync | Local | Yes | Yes |
| Stream inference | Local | No | Yes |
| LLM tagger | Remote + Local | No | Yes |

---

## Component Details

### Remote Components

#### `tt` CLI (remote variant)

**Responsibility:** Receive events from hooks and watchers, write to local buffer.

**Commands:**
- `tt ingest <event-json>` — Append event to buffer file
- `tt events [--since=ID]` — Dump events for sync (used by local)

**Storage:** Events buffered in `~/.time-tracker/events.jsonl` (append-only).

**Implementation notes:**
- Must be fast (<50ms startup) — called on every tmux focus change
- No SQLite on remote; simple JSONL append is sufficient
- Use `flock` for concurrent write safety (multiple hooks/watchers may write simultaneously)
- Deduplication handled by local on import

#### tmux hooks

**Responsibility:** Emit events when pane focus changes.

**Implementation:**
```bash
# In ~/.tmux.conf
set-hook -g pane-focus-in 'run-shell "tt ingest pane-focus --pane=#{pane_id} --cwd=#{pane_current_path} --session=#{session_name}"'
```

**Events emitted:** `tmux_pane_focus`

**Debouncing:** Rapid pane cycling (e.g., keyboard shortcut to switch through 10 panes) creates event storms. Handle in `tt ingest`: ignore focus events for same pane within 500ms of last event.

#### Claude log parser

**Responsibility:** Extract events from Claude Code session logs.

**Not a daemon** — runs on-demand during sync. The logs already have timestamps; no need to watch them continuously.

**Implementation:** `tt export` parses `~/.claude/projects/*/sessions/*.jsonl` and emits events. Called by local during `tt sync`.

**Incremental parsing:** A manifest file (`~/.time-tracker/claude-manifest.json`) tracks byte offsets per session file. On each export, only new bytes are read.

```json
{
  "sessions": {
    "/home/user/.claude/projects/abc/sessions/123.jsonl": {"byte_offset": 145632},
    "/home/user/.claude/projects/abc/sessions/456.jsonl": {"byte_offset": 89012}
  }
}
```

If manifest is lost, falls back to full re-parse (slow but not data loss).

**Events extracted:** `agent_session`, `agent_tool_use`, `user_message`

**Note:** Claude log format is an external dependency. If format changes, parser may silently produce no events. `tt status` should warn if no Claude events seen recently.

#### LLM summarizer (MVP only)

**Responsibility:** Summarize session content for tagging without sending raw content to local.

**Trigger:** On-demand when local requests tags for a session.

**Implementation:**
```bash
# Local calls:
ssh remote "tt summarize --session=abc123"
# Returns: {"tags": ["refactoring", "auth"], "summary": "Working on login flow"}
```

**Privacy:** Raw session content never leaves remote to local. Only summaries/tags transmitted.

**Note:** Session content is sent to the model provider (Anthropic) for summarization. Use a provider with zero data retention if this is a concern. The summarization prompt should explicitly instruct: "Do not include secrets, API keys, passwords, or sensitive data in the summary."

---

### Local Components

#### `tt` CLI (local variant)

**Responsibility:** Primary user interface for all operations.

**Commands (prototype):**
- `tt sync <remote>` — Pull events from remote
- `tt events` — List events in local store
- `tt import` — Import events from stdin (used by sync)
- `tt status` — Show health: last event time per source, sync status (serves as heartbeat check)

**Commands (MVP):**
- `tt streams` — List inferred streams
- `tt tag <stream> <tag>` — Manual tagging
- `tt report [--week]` — Generate time report

#### SQLite store

**Responsibility:** Persistent event store and materialized views.

**Location:** `~/.time-tracker/tracker.db`

**Schema:** See [data-model.md](../design/data-model.md)

#### Event sync

**Responsibility:** Pull events from remotes into local store.

**Implementation:**
1. SSH to remote
2. Run `tt events --since=$LAST_SYNC`
3. Pipe output to `tt import --source=$REMOTE`
4. Update sync position

**Deduplication:** Events have UUIDs; import is idempotent.

#### Stream inference (MVP only)

**Responsibility:** Cluster events into coherent work streams.

**Algorithm:** Directory-based clustering with temporal gaps.

**Trigger:** Lazy recomputation when `needs_recompute` flag is set.

#### LLM tagger (MVP only)

**Responsibility:** Suggest tags for streams based on activity.

**Implementation:**
1. For streams with remote sessions: call `tt summarize` on remote
2. For local-only streams: analyze event patterns
3. Present suggestions to user for confirmation

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                      REMOTE (dev server)                            │
│                                                                     │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────────────────┐ │
│  │ tmux hooks   │────▶│   tt ingest  │────▶│ events.jsonl (buffer)│ │
│  └──────────────┘     └──────────────┘     └──────────────────────┘ │
│                                                      │              │
│  ┌──────────────────────────────────┐                │              │
│  │ ~/.claude/projects/*/sessions/   │                │              │
│  │ (Claude Code logs - already exist)│               │              │
│  └──────────────────────────────────┘                │              │
│                 │                                    │              │
│                 └──────────────┬─────────────────────┘              │
│                                │                                    │
│                                ▼                                    │
│                        ┌──────────────┐                  (SSH)      │
│                        │  tt export   │◀─────────────────────────────
│                        │ (on-demand)  │                             │
│                        └──────────────┘                             │
│                                                                     │
│  ┌──────────────┐                                                   │
│  │LLM summarizer│◀── on-demand from local (MVP only) ───────────────
│  └──────────────┘                                                   │
└─────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────────┐
│                       LOCAL (laptop)                                 │
│                                                                      │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────────────────┐  │
│  │   tt sync    │────▶│  tt import   │────▶│   SQLite store       │  │
│  └──────────────┘     └──────────────┘     └──────────────────────┘  │
│                                                      │               │
│                                                      ▼               │
│                              ┌──────────────────────────────────┐    │
│                              │ stream inference │ LLM tagger    │    │
│                              │ (MVP only)                       │    │
│                              └──────────────────────────────────┘    │
│                                                      │               │
│                                                      ▼               │
│                              ┌──────────────────────────────────┐    │
│                              │ tt streams │ tt report │ tt tag  │    │
│                              └──────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────────────┘
```

---

## Resolved Questions

1. **Where does stream inference happen?** Local, at query time (lazy).
2. **Where does LLM classification run?** Summarization on remote (privacy), tagging decisions on local.
3. **How do watchers communicate with the store?** Remote writes to JSONL buffer; local pulls via SSH and imports to SQLite.
