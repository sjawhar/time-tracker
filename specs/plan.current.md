# Plan: Implement `tt import` Command

## Task

Implement `tt import` command that reads events from stdin and inserts to SQLite.

**From `specs/plan.md`:** `- [ ] Implement `tt import` command (reads events from stdin, inserts to SQLite)`

## Acceptance Criteria

From `specs/architecture/components.md`:
- `tt import` — Import events from stdin (used by sync)
- Events piped from `tt export` output to local SQLite store
- Deduplication: Events have UUIDs; import is idempotent

From `specs/design/data-model.md`:
- ID generation is deterministic hash of content: `id = hash(source + type + timestamp + data)`
- Same logical event always produces same ID, making imports idempotent

## Implementation Approach

### 1. Add `Import` Command to CLI

**File:** `crates/tt-cli/src/cli.rs`

Add new command variant:
```rust
/// Import events from stdin into local SQLite database.
///
/// Events are expected as JSONL (one JSON object per line).
/// Duplicate events (same ID) are silently ignored.
Import,
```

### 2. Create Import Module

**File:** `crates/tt-cli/src/commands/import.rs`

**Responsibilities:**
- Read JSONL from stdin line by line (streaming, not collect all)
- Parse each line directly as `tt_db::StoredEvent`
- Insert events to database using `Database::insert_events()` in batches
- Report counts: total read, successfully inserted, duplicates skipped

**Key considerations:**
1. **Simple deserialization:** Deserialize directly to `StoredEvent`. Serde's `#[serde(default)]` attributes handle missing optional fields (`cwd`, `session_id` → `None`, `schema_version` → 1)
2. **Streaming:** Process stdin as a stream to handle large imports without OOM
3. **Batch processing:** Buffer 1000 events, insert, clear, repeat
4. **Error handling:** Skip malformed JSON with warnings (don't fail entire import); database errors fail import
5. **Output:** Report summary to stderr (not stdout, to allow piping)

### 3. Wire Up in Main

**File:** `crates/tt-cli/src/main.rs`

Add handler for `Import` command that:
1. Loads config (needs database path)
2. Opens database
3. Calls `import::run()`

### 4. Update Module Exports

**File:** `crates/tt-cli/src/commands/mod.rs`

Add `pub mod import;`

## Test Cases

1. **Empty stdin** — Returns success, reports 0 events
2. **Valid JSONL** — All events inserted, reports correct counts
3. **Malformed lines** — Skipped with warning, other events still imported
4. **Duplicate events** — Idempotent: re-importing same events succeeds, reports as duplicates
5. **Mixed valid/invalid** — Partial success, reports both counts

## Files to Create/Modify

| File | Action |
|------|--------|
| `crates/tt-cli/src/commands/import.rs` | Create |
| `crates/tt-cli/src/commands/mod.rs` | Modify (add `pub mod import`) |
| `crates/tt-cli/src/cli.rs` | Modify (add `Import` variant) |
| `crates/tt-cli/src/main.rs` | Modify (add `Import` handler) |

## Implementation Notes

### Event Format Compatibility

`tt export` outputs:
```json
{"id":"...","timestamp":"2025-01-29T12:00:00Z","source":"...","type":"...","data":{...}}
```

`tt-db::StoredEvent` has serde attributes that handle the mismatch:
- `#[serde(rename = "type")]` maps JSON `type` to `event_type`
- `#[serde(default = "default_schema_version")]` defaults missing `schema_version` to 1
- `#[serde(default)]` on `cwd` and `session_id` defaults them to `None`

This means we can deserialize export output directly to `StoredEvent` without transformation.

**Note:** Currently `cwd`/`session_id` won't be indexed for imported events since export doesn't include them at top level. Future improvement: add these fields to `ExportEvent` for lossless round-trip.

### Streaming Processing

```rust
const BATCH_SIZE: usize = 1000;
let mut batch = Vec::with_capacity(BATCH_SIZE);

for line_result in stdin.lock().lines() {
    let line = line_result?;
    match serde_json::from_str::<StoredEvent>(&line) {
        Ok(event) => {
            batch.push(event);
            if batch.len() >= BATCH_SIZE {
                db.insert_events(&batch)?;
                batch.clear();
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "malformed JSON, skipping line");
            malformed_count += 1;
        }
    }
}
// Flush remaining
db.insert_events(&batch)?;
```

### Error Categories

- **Malformed JSON:** Skip with warning, continue import
- **Valid JSON, missing required fields:** Serde will fail to parse, handled as malformed
- **Database errors:** Fail the import (indicates systemic problem)
