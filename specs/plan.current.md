# Implementation Plan: `tt status` command

**Task:** Implement `tt status` command (show last event time per source)

**From:** `specs/plan.md` - Prototype Implementation (Local)

## Acceptance Criteria

1. `tt status` displays the last event timestamp for each event source
2. Shows database path
3. Human-readable output format (not JSONL)
4. Works with empty databases (shows "no events")

## Files to Modify

### 1. `crates/tt-db/src/lib.rs`

Add a new method to retrieve last event per source:

```rust
/// Returns the most recent event for each source.
pub fn get_last_event_per_source(&self) -> Result<Vec<SourceStatus>, DbError>
```

Where `SourceStatus` is a new struct:

```rust
pub struct SourceStatus {
    pub source: String,
    pub last_timestamp: DateTime<Utc>,
}
```

SQL query:
```sql
SELECT source, MAX(timestamp) as last_timestamp
FROM events
GROUP BY source
ORDER BY last_timestamp DESC
```

### 2. `crates/tt-cli/src/commands/status.rs` (new file)

Create command module following the pattern of `events.rs`:

```rust
pub fn run(db: &Database) -> Result<()> {
    let statuses = db.get_last_event_per_source()?;
    // Format and print output
}
```

Output format:
```
Database: /path/to/events.db

Sources:
  remote.tmux:   2025-01-29T10:30:00Z
  remote.agent:  2025-01-29T10:25:00Z
```

Or if empty:
```
Database: /path/to/events.db

No events recorded yet.
```

### 3. `crates/tt-cli/src/commands/mod.rs`

Add `pub mod status;`

### 4. `crates/tt-cli/src/main.rs`

Update the `Commands::Status` handler to call `status::run(&db)`

## Test Cases

### Unit tests in `tt-db`

1. `test_get_last_event_per_source_empty` - Returns empty vec for empty database
2. `test_get_last_event_per_source_single_source` - Returns one entry for single source
3. `test_get_last_event_per_source_multiple_sources` - Returns correct last timestamp per source
4. `test_get_last_event_per_source_ordered_by_timestamp` - Results ordered by most recent first

### Integration test in `tt-cli`

1. Snapshot test for empty/new database output (verify "no events" message)
2. Snapshot test with multiple sources
3. Verify database path is printed (normalize path in snapshot for reproducibility)

## Implementation Notes

- Reuse timestamp formatting pattern from `events.rs` if needed
- Use `chrono-humanize` or similar for "2 hours ago" relative time? Or keep simple with absolute timestamps for MVP
- Keep output simple and scannable
- No performance concerns - aggregation query is fast even with many events

## Decision: Relative vs Absolute Time

**Option A: Absolute timestamps** (e.g., `2025-01-29T10:30:00Z`)
- Pros: Precise, no timezone confusion
- Cons: Harder to scan quickly

**Option B: Relative time** (e.g., `2 hours ago`)
- Pros: Immediately understandable
- Cons: Requires additional dependency (`chrono-humanize`)

**Decision:** Use absolute timestamps for MVP. Add `chrono-humanize` later if users want relative time. Keeps dependencies minimal and output unambiguous.

## Decision: Event Count

Drop `event_count` from MVP - it's not in the acceptance criteria. Keep the struct simple:

```rust
pub struct SourceStatus {
    pub source: String,
    pub last_timestamp: DateTime<Utc>,
}
```

## Decision: Output Format

Use simple key-value format (no table/box-drawing characters for maximum terminal compatibility):

```
Database: /path/to/events.db

Sources:
  remote.tmux:   2025-01-29T10:30:00Z
  remote.agent:  2025-01-29T10:25:00Z
```

## Notes from Review

1. The existing placeholder in `main.rs` (lines 88-96) will be replaced with a call to `status::run(&db)`
2. Need to add `Database::open` in main.rs for the Status handler (like other commands)
3. No need to add an index on `source` - the query will be fast enough for occasional use
