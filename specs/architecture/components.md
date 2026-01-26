# Components

_To be designed after architecture overview is established._

## Component List

_What are the major components?_

## Component Details

_For each component: responsibility, interfaces, dependencies._

---

## Preliminary Ideas

> **Note**: The following are preliminary ideas from early brainstorming. They should be validated and refined during architecture design.

### Component Architecture Sketch

```
┌────────────────────────────────────────────────────────────────────┐
│                         DATA COLLECTORS                            │
├────────────────┬─────────────────┬─────────────────┬───────────────┤
│  tmux-watcher  │  agent-watcher  │  git-watcher    │  manual-input │
│  (hooks/poll)  │  (log parser)   │  (hooks)        │  (CLI/TUI)    │
└───────┬────────┴────────┬────────┴────────┬────────┴───────┬───────┘
        │                 │                 │                │
        ▼                 ▼                 ▼                ▼
┌────────────────────────────────────────────────────────────────────┐
│                      EVENT INGESTION LAYER                         │
│  • Deduplication                                                   │
│  • Context inference/resolution                                    │
│  • Event enrichment                                                │
└────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌────────────────────────────────────────────────────────────────────┐
│                         EVENT STORE                                │
│  • Append-only log (SQLite)                                        │
│  • Compaction/archival for old data                               │
└────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌────────────────────────────────────────────────────────────────────┐
│                      QUERY & AGGREGATION                           │
│  • Time window aggregation                                         │
│  • Fractional attribution calculation                              │
│  • Report generation                                               │
└────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌────────────────────────────────────────────────────────────────────┐
│                         INTERFACES                                 │
├─────────────────┬─────────────────┬─────────────────┬──────────────┤
│    CLI          │    TUI          │    API          │   Export     │
│ (tt status)     │ (interactive)   │  (HTTP/JSON)    │ (Toggl CSV)  │
└─────────────────┴─────────────────┴─────────────────┴──────────────┘
```

### Prototype Components (Minimal)

For the data collection prototype, only these are needed:

1. **tmux-watcher** - Receive events from tmux hooks
2. **agent-watcher** - Parse Claude session JSONL files
3. **Event store** - SQLite database
4. **CLI** - `tt ingest` and `tt events` commands

### Open Questions

- Where does stream inference happen? (Ingestion layer vs query time)
- Where does LLM classification run? (Local vs remote)
- How do watchers communicate with the store? (Direct SQLite vs IPC)
