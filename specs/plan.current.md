# Current Task: Implement Stream Inference

**Task from plan.md:** Implement stream inference (directory + temporal clustering)

## Spec Summary

Stream inference clusters events into coherent work streams based on:

1. **Working directory** — Events with the same `cwd` belong together
2. **Temporal proximity** — Events within 30 minutes of each other belong to the same stream

**Note:** Agent session relationships are mentioned in the spec but will be deferred. For MVP, we cluster by cwd + temporal proximity only. Session-based boundaries add complexity (session may span directories, or a directory may have multiple sessions) that isn't needed for the core use case.

### Parameters (from data-model.md)

| Parameter | Default | Description |
|-----------|---------|-------------|
| `stream_gap_threshold_ms` | 1,800,000 (30 min) | Max gap before starting new stream |

### Key Behaviors (from ux-cli.md)

- Streams are computed lazily when needed (during `tt report`, `tt streams`, or `tt status`)
- No separate `tt infer` command
- Events with `assignment_source = 'user'` are preserved during recomputation
- Stream names are auto-generated from the cwd (e.g., the directory basename)

### Schema Requirements (from data-model.md)

Streams have a `needs_recompute` flag for lazy recomputation.

Events have:
- `stream_id` — assigned stream (null = "Uncategorized")
- `assignment_source` — 'inferred' | 'user' | 'imported'

## Implementation Approach

### Algorithm

```python
def run_stream_inference(store: EventStore, gap_threshold_ms: int = 1_800_000) -> None:
    """
    Cluster events into streams based on directory and temporal proximity.

    1. Get all events where assignment_source != 'user' and stream_id IS NULL
    2. Normalize cwd paths (strip trailing slash)
    3. Group events by normalized cwd
    4. Within each group, sort by timestamp
    5. Split into streams when gap > 30 minutes (using > not >=)
    6. Get or create Stream for each cluster
    7. Update event.stream_id (sets assignment_source = 'inferred')
    """
```

### Stream Naming

Auto-generate name from cwd:
- `/home/sami/time-tracker` → `time-tracker`
- `/home/sami/time-tracker/` → `time-tracker` (trailing slash stripped)
- null cwd → `Uncategorized`
- empty string cwd (`""`) → `Uncategorized`

For MVP: Use the basename of the cwd (last path component).

**Temporal splits within same cwd:** Multiple streams can have the same name. The spec shows stream IDs as 7-char prefixes for disambiguation, so name collisions are acceptable.

### Recomputation Strategy

**Simplified for MVP:**
- Process all events where `stream_id IS NULL` and `assignment_source != 'user'`
- No time-range scoping initially (premature optimization)
- Mark streams as `needs_recompute = 0` after recomputation

**When `needs_recompute` becomes true:**
- When new events are imported (future: trigger during `tt import`/`tt sync`)

## Files to Modify

### Modify: `tt_local/db.py`

Add methods directly to `EventStore` (simpler than a separate module; may extract to `inference.py` later if `EventStore` grows too large):

```python
def get_unassigned_events(self) -> list[dict]:
    """Get events where assignment_source != 'user' and stream_id IS NULL."""
    ...

def assign_events_to_stream(self, event_ids: list[str], stream_id: str) -> None:
    """
    Assign multiple events to a stream (sets assignment_source = 'inferred').
    Uses WHERE id IN (...) with batching (500 IDs per query) to stay under SQLite's 999-parameter limit.
    """
    ...

def get_streams(self) -> list[dict]:
    """Get all streams."""
    ...

def run_stream_inference(self, gap_threshold_ms: int = 1_800_000) -> int:
    """
    Run stream inference on all unassigned events.
    Returns the number of events assigned.

    Uses `with self._conn:` for automatic transaction handling (commits on success, rolls back on exception).
    """
    ...
```

**Note:** The existing `create_stream(name=...)` method is used for each temporal cluster. Multiple clusters from the same cwd create separate streams (with the same name but different IDs). Do NOT reuse streams by name lookup.

### No CLI changes for this task

Stream inference is an internal module. The `tt streams`, `tt report`, etc. commands will call `run_stream_inference()` as needed (those commands are separate MVP tasks).

## Test Cases

### Unit Tests: `tests/test_streams.py`

1. **Empty events** — `run_stream_inference()` with no events is a no-op (returns 0)
2. **Single event** — One event creates one stream
3. **Same cwd, within gap** — Two events 15 min apart → one stream
4. **Same cwd, exceeds gap** — Two events 45 min apart → two streams
5. **Gap boundary (exactly 30 min)** — Events exactly 30 min apart → same stream (gap > threshold, not >=)
6. **Different cwd, same time** — Events in different cwds → separate streams
7. **User-assigned events preserved** — Events with `assignment_source='user'` keep their stream_id
8. **Imported events get inferred** — Events with `assignment_source='imported'` are eligible for inference
9. **Null cwd handling** — Events without cwd go to "Uncategorized" stream
10. **Empty string cwd handling** — Events with `cwd=""` go to "Uncategorized" stream (same as null)
11. **Stream naming** — Verify auto-generated names from cwd
12. **Same basename, different cwds** — `/home/a/project` and `/home/b/project` → separate streams (both named "project")
13. **Path normalization** — `/home/sami/project/` and `/home/sami/project` → same stream
14. **Idempotent** — Running inference twice assigns same events to same streams (no duplicates)
15. **Across midnight** — 11:59 PM and 12:01 AM in same cwd → same stream (2 min gap)
16. **Events already assigned (inferred)** — Events with `stream_id` set and `assignment_source='inferred'` are not re-processed
17. **Three events in sequence** — Events at 9:00, 9:15, 9:30 (all within gap) → same stream
18. **Root directory cwd** — `cwd="/"` → stream name is "/" or "root" (document choice)
19. **Unicode paths** — `/home/sami/proyecto-español` → stream name is `proyecto-español`
20. **Deeply nested paths** — `/home/sami/very/deep/structure/project` → stream name is `project`

### Performance Test

**10,000 events in <1s** — Create events, run inference, verify timing

## Acceptance Criteria (from ux-cli.md)

- Events within 30 minutes in same directory → same stream
- First query after sync may be slower (stream computation); subsequent queries use cached results
- Local queries on 10,000 events complete in <1s

## Resolved Questions (from code-architect review)

1. **Gap threshold is `>` not `>=`** — Events exactly 30 min apart belong to same stream
2. **No separate module** — Keep inference logic in `db.py` for simplicity; add comment noting it could be extracted if file grows too large
3. **No time-range scoping for MVP** — Process all unassigned events
4. **Path normalization** — Strip trailing slashes before grouping
5. **`assignment_source='imported'` treated like `'inferred'`** — Both are eligible for re-inference
6. **Multiple streams can have same name** — Use ID prefix for disambiguation; always create new stream per temporal cluster, never reuse by name lookup
7. **Agent session boundaries deferred** — Not needed for MVP, adds complexity
8. **Transaction wrapper** — Use `with self._conn:` for automatic transaction handling (commits on success, rolls back on exception)
9. **Batch updates** — Use `WHERE id IN (...)` with batching (~500 IDs per query) to stay under SQLite's 999-parameter limit
10. **Empty string cwd** — Treat `cwd=""` the same as `cwd=None` (both → "Uncategorized")
11. **Same basename, different cwds** — `/home/a/project` and `/home/b/project` get separate streams with the same name; ID prefix disambiguates
12. **Root directory** — `cwd="/"` should have stream name `"/"` (not "root" or empty string)
13. **Ignore `needs_recompute` flag for MVP** — Always process unassigned events; the flag is premature optimization
14. **Events already assigned (inferred) stay assigned** — Query only processes events where `stream_id IS NULL`

## Dependencies

This task has no dependencies on other MVP tasks. It must be completed before:
- `tt report --week` (needs streams for time aggregation)
- `tt streams` (lists streams)
- Direct/delegated time calculation (uses stream assignments)
