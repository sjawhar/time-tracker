# Implementation Plan: Stream Inference

**Task:** Implement stream inference (directory + temporal clustering)
**Source:** `specs/plan.md` MVP Implementation, item 1

## Overview

Stream inference clusters events into coherent work units based on:
1. **Working directory** (`cwd`) - Events in the same directory belong to the same stream
2. **Temporal proximity** - Events separated by >30min gap start a new stream

This is the foundation for time attribution. Without streams, we cannot compute direct/delegated time or generate reports.

## Spec References

- **Algorithm:** `architecture/overview.md` §Attention Allocation Algorithm
- **Parameters:** `design/data-model.md` §Stream Inference
- **Schema:** `design/data-model.md` §Storage

## Current State

**Ready:**
- `events` table has `stream_id` and `assignment_source` columns (not yet populated)
- Indexes on `cwd`, `timestamp`, `session_id` already exist
- `StoredEvent` struct exposes `cwd`, `timestamp`, `session_id`

**Missing:**
- `streams` table in database
- Stream inference algorithm
- Methods to assign events to streams
- Query methods for streams

## Implementation Approach

### Step 1: Add streams table to tt-db

Create the `streams` table per the data model spec:

```sql
CREATE TABLE streams (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  name TEXT,
  time_direct_ms INTEGER DEFAULT 0,
  time_delegated_ms INTEGER DEFAULT 0,
  first_event_at TEXT,
  last_event_at TEXT,
  needs_recompute INTEGER DEFAULT 0
);

CREATE INDEX idx_streams_updated ON streams(updated_at);
```

**Note:** `stream_tags` table deferred until tagging feature is implemented. The schema supports adding it later.

**Schema version:** Bump from 2 to 3.

**Files:** `crates/tt-db/src/lib.rs`

### Step 2: Add Stream domain type to tt-core

The existing `Stream` struct in `stream.rs` is minimal (id, name, description). Replace with the full schema:

```rust
pub struct Stream {
    pub id: StreamId,
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub first_event_at: Option<DateTime<Utc>>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub needs_recompute: bool,
}
```

**Changes:**
- Remove `description` field (not in database schema)
- Add `created_at`, `updated_at` timestamps
- Add time tracking fields (`time_direct_ms`, `time_delegated_ms`)
- Add `first_event_at`, `last_event_at` for range tracking
- Add `needs_recompute` flag for lazy recomputation
- Make `name` optional (auto-generated from cwd)

**Files:** `crates/tt-core/src/stream.rs`

### Step 3: Implement stream inference algorithm in tt-core

Create a new module `crates/tt-core/src/inference.rs`:

```rust
pub struct InferenceConfig {
    pub gap_threshold_ms: i64,  // Default: 1_800_000 (30 min)
}

pub struct StreamAssignment {
    pub event_id: EventId,
    pub stream_id: StreamId,
}

/// Infer stream assignments for a set of events.
///
/// Algorithm:
/// 1. Normalize all cwd paths (trailing slash, canonicalization)
/// 2. Group events by normalized cwd
/// 3. Within each cwd group (including null-cwd), sort by timestamp
/// 4. Start a new stream when gap > threshold
/// 5. Return (new_streams, event_assignments)
pub fn infer_streams(
    events: &[StoredEvent],
    config: &InferenceConfig,
) -> (Vec<Stream>, Vec<StreamAssignment>)
```

**Key behaviors:**

1. **Path normalization:** Before grouping, normalize all paths:
   - Remove trailing slash (`/home/user/project/` → `/home/user/project`)
   - No symlink resolution (too expensive, may cause surprises)

2. **User corrections preserved:** Events with `assignment_source = 'user'` are skipped entirely

3. **Null-cwd events:** Events with `cwd = NULL` are grouped together BUT still split by temporal gaps. Each resulting stream is named "Uncategorized" with a suffix if there are multiples.

4. **Stream naming:**
   - Default name from directory basename (e.g., `/home/user/time-tracker` → "time-tracker")
   - Name collisions resolved with parent directory: "project-a/time-tracker" vs "project-b/time-tracker"
   - Null-cwd streams: "Uncategorized", "Uncategorized (2)", etc.

5. **Stream IDs:** Generated as UUIDs (not deterministic hashes). Simpler and avoids edge cases with changing first events.

6. **Re-inference behavior:** Only processes events where `stream_id IS NULL`. Already-inferred events are not touched. Use `--force` flag to clear all inferred assignments first.

**Files:** `crates/tt-core/src/inference.rs`, `crates/tt-core/src/lib.rs`

### Step 4: Add database methods for streams

Add to `Database`:

```rust
// Stream CRUD
pub fn insert_stream(&self, stream: &Stream) -> Result<(), DbError>;
pub fn get_stream(&self, id: &StreamId) -> Result<Option<Stream>, DbError>;
pub fn get_streams(&self) -> Result<Vec<Stream>, DbError>;
pub fn get_streams_in_range(&self, after: DateTime<Utc>, before: DateTime<Utc>) -> Result<Vec<Stream>, DbError>;

// Stream assignment
pub fn assign_event_to_stream(&self, event_id: &EventId, stream_id: &StreamId, source: &str) -> Result<(), DbError>;
pub fn assign_events_to_stream(&self, assignments: &[(EventId, StreamId)], source: &str) -> Result<u64, DbError>;

// Query events by stream
pub fn get_events_by_stream(&self, stream_id: &StreamId) -> Result<Vec<StoredEvent>, DbError>;
pub fn get_events_without_stream(&self) -> Result<Vec<StoredEvent>, DbError>;
```

**Files:** `crates/tt-db/src/lib.rs`

### Step 5: Create CLI command for triggering inference

For MVP, inference is triggered manually (lazy recomputation is post-MVP):

```bash
tt infer [--force]  # Run inference on unassigned events (--force to re-infer all)
```

This command:
1. If `--force`: Clear all `stream_id` where `assignment_source = 'inferred'`
2. Query events where `stream_id IS NULL`
3. Run inference algorithm
4. Create new streams in database
5. Assign events to streams (all in single transaction for consistency)

**Alternative:** Run inference automatically on `tt sync` completion. Prefer explicit command for MVP (simpler debugging).

**Files:** `crates/tt-cli/src/commands/infer.rs`, `crates/tt-cli/src/commands/mod.rs`, `crates/tt-cli/src/main.rs`

## Test Cases

### Unit tests (tt-core/src/inference.rs)

1. **Single directory, continuous work**
   - 5 events in `/project-a` with 5min gaps
   - Expect: 1 stream, all events assigned

2. **Single directory, gap creates new stream**
   - Events at t=0, t=5m, t=60m (31min gap after t=5m)
   - Expect: 2 streams

3. **Multiple directories**
   - Events in `/project-a` and `/project-b` interleaved
   - Expect: 2 streams (one per directory)

4. **Null cwd events with temporal gaps**
   - Events without `cwd` at t=0, t=5m, t=60m
   - Expect: 2 "Uncategorized" streams (split by gap)

5. **User corrections preserved**
   - Events with `assignment_source = 'user'` already set
   - Expect: Not reassigned by inference

6. **Path normalization**
   - Events in `/project-a/` and `/project-a` (trailing slash difference)
   - Expect: Same stream

7. **Stream name collisions**
   - Events in `/work/a/project` and `/work/b/project`
   - Expect: Names like "a/project" and "b/project"

8. **Already-assigned events skipped**
   - Events with existing `stream_id` (inferred)
   - Expect: Not re-processed without `--force`

### Integration tests (tt-cli)

1. **End-to-end inference**
   - Import events via `tt import`
   - Run `tt infer`
   - Query streams via database
   - Verify correct clustering

2. **Force re-inference**
   - Run `tt infer`, verify streams created
   - Run `tt infer --force`, verify streams recreated

## Acceptance Criteria

Per `architecture/overview.md`:

1. ✅ Events with same `cwd` belong to same stream (within temporal window)
2. ✅ Gap > 30min starts new stream
3. ✅ User corrections (`assignment_source = 'user'`) preserved
4. ✅ Streams have computed `first_event_at` and `last_event_at`
5. ✅ Stream names derived from directory basename

## Dependencies

- No external dependencies needed
- Uses existing: `rusqlite`, `chrono`, `uuid` (for stream ID generation)

## Open Questions

**Resolved:**

1. **Should inference run automatically?** No for MVP. Explicit `tt infer` command.
2. **How to handle events that span stream boundaries?** Each event belongs to exactly one stream based on its timestamp and cwd.
3. **What about agent sessions spanning multiple directories?** Agent sessions are assigned to the stream matching their `cwd`. The `session_id` field enables later correlation but doesn't affect stream assignment.
4. **What happens to already-inferred events?** They're skipped by default; use `--force` to re-infer.
5. **Should null-cwd events split by gap?** Yes, they follow the same temporal gap rules.
6. **How to handle stream name collisions?** Include parent directory in name.

## Risks

1. **Performance with many events**: Stream inference processes all unassigned events. For large datasets, may need batching or incremental processing. Monitor and add if needed.

2. **Directory path variations**: Addressed via normalization (trailing slash removal). Symlinks not resolved (out of scope).

## Implementation Order

1. Schema changes (tt-db) - blocking for everything else
2. Domain types (tt-core) - needed for inference
3. Inference algorithm (tt-core) - core logic
4. Database methods (tt-db) - persistence
5. CLI command (tt-cli) - user interface
6. Tests throughout

## Estimated Changes

| File | Changes |
|------|---------|
| `crates/tt-db/src/lib.rs` | +150 lines (schema, methods) |
| `crates/tt-core/src/stream.rs` | +50 lines (extended type) |
| `crates/tt-core/src/inference.rs` | +150 lines (new file) |
| `crates/tt-core/src/lib.rs` | +5 lines (module export) |
| `crates/tt-cli/src/commands/infer.rs` | +80 lines (new file) |
| `crates/tt-cli/src/commands/mod.rs` | +2 lines |
| `crates/tt-cli/src/main.rs` | +5 lines |

Total: ~450 lines new code
