# tt-db — SQLite Storage Layer

Single-file monolith (`src/lib.rs`, 2363 lines). All database types and methods in one file.

## Schema (v7)

No migrations. Version mismatch = `DbError::SchemaVersionMismatch` (hard error). Bump `SCHEMA_VERSION` constant + recreate DB.

### Tables

```sql
events (id TEXT PK, timestamp TEXT, type TEXT, source TEXT, schema_version INT,
        cwd TEXT, git_project TEXT, git_workspace TEXT, pane_id TEXT,
        tmux_session TEXT, window_index INT, status TEXT, idle_duration_ms INT,
        action TEXT, session_id TEXT, stream_id TEXT FK, assignment_source TEXT)

streams (id TEXT PK, created_at TEXT, updated_at TEXT, name TEXT,
         time_direct_ms INT, time_delegated_ms INT,
         first_event_at TEXT, last_event_at TEXT, needs_recompute INT)

stream_tags (stream_id TEXT, tag TEXT, PK(stream_id, tag), FK stream_id)

agent_sessions (session_id TEXT PK, source TEXT, parent_session_id TEXT,
                session_type TEXT, project_path TEXT, project_name TEXT,
                start_time TEXT, end_time TEXT, message_count INT,
                summary TEXT, user_prompts TEXT, starting_prompt TEXT,
                assistant_message_count INT, tool_call_count INT)
```

Timestamps: ISO 8601 TEXT (`2024-01-15T10:30:00.000Z`), always UTC, millisecond precision. Lexicographic order = chronological order.

### Indexes

`idx_events_timestamp`, `idx_events_type`, `idx_events_stream`, `idx_events_cwd`, `idx_events_session`, `idx_events_git_project`, `idx_streams_updated`, `idx_stream_tags_tag`, `idx_agent_sessions_start_time`, `idx_agent_sessions_project_path`, `idx_agent_sessions_parent`

## Key Types

- `Database` — wraps `rusqlite::Connection`. `Send` but not `Sync`.
- `StoredEvent` — implements `tt_core::AllocatableEvent` trait
- `Stream` — work unit with computed time fields
- `DbError` — `Sqlite(rusqlite::Error)` | `SchemaVersionMismatch { found, expected }`
- `SourceStatus` — last event timestamp per source

## Method Reference

### Events
| Method | Purpose |
|--------|---------|
| `insert_event` / `insert_events` | Idempotent insert (`INSERT OR IGNORE`) |
| `get_events` | All events, optional time_after/time_before filters |
| `get_events_in_range` | Events between start..end (inclusive) |
| `get_events_by_stream` | Events for a specific stream |
| `get_events_without_stream` | Unassigned events |
| `get_last_event_per_source` | Latest timestamp per source name |

### Streams
| Method | Purpose |
|--------|---------|
| `insert_stream` | Create new stream |
| `get_stream` / `get_streams` | Retrieve by ID or all |
| `streams_in_range` | Streams overlapping a time range |
| `resolve_stream` | Find by ID prefix or name |
| `assign_event_to_stream` / `assign_events_to_stream` | Set stream_id on events |
| `clear_inferred_assignments` | Remove auto-assigned stream_ids |
| `delete_orphaned_streams` | Remove streams with no events |
| `update_stream_times` | Set direct/delegated ms + event timestamps |
| `mark_streams_for_recompute` | Flag streams needing time recalculation |
| `get_streams_needing_recompute` | Streams with needs_recompute=1 |

### Tags
| Method | Purpose |
|--------|---------|
| `add_tag` | Idempotent tag addition |
| `get_tags` | Tags for a stream |
| `delete_tag` | Remove tag from stream |
| `get_all_tags` | All unique tags |
| `get_streams_with_tags` | Streams + their tags (joined) |

### Agent Sessions
| Method | Purpose |
|--------|---------|
| `upsert_agent_session` | Insert or update session metadata |
| `agent_sessions_in_range` | Sessions overlapping a time range |

## Thread Safety

`Database` is `Send` (movable between threads) but NOT `Sync` (no shared access). For multi-threaded use: `Mutex<Database>`, connection pool, or separate instances per thread.

## Testing

Use `Database::open_in_memory()` for all tests. Helper: `make_event(id, timestamp, event_type)` returns `StoredEvent` with sensible defaults. 50+ unit tests covering field persistence, idempotency, range queries, cascading deletes, schema version checks.
