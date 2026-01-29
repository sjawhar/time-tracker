# Plan: Claude Log Manifest for Incremental Parsing

## Task

From `specs/plan.md`:
> Create Claude log manifest for incremental parsing

## Spec Reference

From `specs/architecture/components.md` (lines 85-96):

```
**Incremental parsing:** A manifest file (`~/.time-tracker/claude-manifest.json`) tracks byte offsets per session file. On each export, only new bytes are read.

{
  "sessions": {
    "/home/user/.claude/projects/abc/sessions/123.jsonl": {"byte_offset": 145632},
    "/home/user/.claude/projects/abc/sessions/456.jsonl": {"byte_offset": 89012}
  }
}

If manifest is lost, falls back to full re-parse (slow but not data loss).
```

## Acceptance Criteria

1. Manifest file tracks byte offsets per Claude session log file
2. `tt export` reads only new bytes (after stored offset) from each file
3. Manifest is updated after successful export
4. Graceful fallback: if manifest is missing/corrupted, full re-parse occurs
5. Deleted files are handled (manifest entries for non-existent files are cleaned up)

## Implementation Approach

### Files to Modify

1. **`crates/tt-cli/src/commands/export.rs`** - Add manifest loading, incremental reading, and saving

### Data Structures

```rust
/// Manifest tracking byte offsets for incremental Claude log parsing.
/// Maps file path to byte offset after last successfully parsed line.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeManifest {
    /// Byte offset per session file path
    pub sessions: HashMap<PathBuf, u64>,
}
```

Note: Using `u64` directly instead of a `SessionState` struct (YAGNI). Can migrate if more state is needed later.

### Key Changes

1. **Add manifest path parameter** to `run_impl()` and `export_claude_events()`
   - Path: `data_dir.join("claude-manifest.json")`
   - For testing: make manifest path injectable (via Option parameter)

2. **Load manifest** at start of `export_claude_events()`
   - If missing or corrupted: use empty manifest (full re-parse), log warning
   - Log: `tracing::warn!("failed to load manifest, performing full re-parse")`

3. **Seek to offset** in `export_single_claude_log()`
   - Before reading, seek to `manifest.sessions[path].byte_offset`
   - BufReader handles partial lines at EOF gracefully (logs debug, continues)
   - Return final stream position after successful parsing

4. **Update manifest** after all files processed
   - Write updated offsets atomically (write to temp file, rename)
   - Remove entries for files that no longer exist
   - If manifest write fails: log warning and continue (export still succeeds, next run re-processes some events)

5. **Handle edge cases**
   - File smaller than recorded offset → re-parse from start (file was truncated/replaced)
   - Partial line at EOF (concurrent writer) → `lines()` returns partial content without error, JSON parse fails and line is skipped. Offset saved at position after last complete line.
   - File deleted → skip, remove from manifest
   - File replaced with different content (same size) → accepted behavior; new session IDs will differ, downstream idempotency handles duplicates

### Implementation Details

**Key insight**: Store the offset *after* the trailing newline of the last successfully parsed line. Then on resume, we're positioned at the start of a complete line - no skip logic needed.

**BufReader behavior**: `lines()` consumes bytes from the underlying reader but may read ahead into its buffer. After exhausting `lines()`, `stream_position()` returns the position in the underlying file (accounting for the buffer). Need to verify with a unit test.

The main change is in reading logic:

```rust
fn export_single_claude_log(
    log_path: &Path,
    seen_sessions: &mut HashSet<String>,
    output: &mut dyn Write,
    start_offset: u64,  // NEW: offset to start from
) -> Result<u64> {      // NEW: returns final byte position (after last newline)
    let file = File::open(log_path)?;
    let file_size = file.metadata()?.len();

    // If offset is beyond file size, file was truncated - restart from 0
    let actual_offset = if start_offset > file_size { 0 } else { start_offset };

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(actual_offset))?;

    // Track position after each successfully parsed line
    let mut last_good_position = actual_offset;

    // ... existing parsing logic ...
    // After each successful line: last_good_position = reader.stream_position()?;

    // Return position after last successfully parsed line
    Ok(last_good_position)
}
```

**Offset storage**: After each line is successfully parsed, the reader position is *after* the newline. When we save the manifest, we use `stream_position()` which gives us this position. On the next run, seeking to this position puts us at the start of the next line.

### Test Cases

1. **Fresh export (no manifest)** - Full parse, manifest created
2. **Incremental export** - Only new lines parsed, manifest updated
3. **Corrupted manifest** - JSON parse error → full re-parse (with warning log)
4. **File truncated** - Offset > file size → restart from 0
5. **File deleted** - Manifest entry removed
6. **Atomic manifest write** - Write to temp, then rename
7. **Multiple files** - Each tracked independently
8. **Empty file** - Handle gracefully
9. **Offset at EOF (no new content)** - No events emitted, offset unchanged
10. **File grows between exports** - Verify only new lines processed (integration test)
11. **Concurrent Claude writing** - Partial line at EOF → JSON parse fails, skipped. Offset saved at last complete line.
12. **Session start deduplication** - Stable IDs ensure downstream idempotency
13. **Duplicate session starts across incremental runs** - Verify same session emits identical event IDs (idempotent)
14. **BufReader stream_position after lines()** - Verify offset semantics with unit test
15. **Manifest write failure** - Export succeeds, warning logged

### Performance Considerations

- Manifest read/write adds minimal overhead (small JSON file)
- Seeking is O(1) for random access
- Main benefit: large session files (common for long Claude sessions) only parse once

## Resolved Questions

1. **Where to store manifest?** `~/.time-tracker/claude-manifest.json` (per spec), derived from `data_dir`
2. **Atomic writes?** Yes, write to `.claude-manifest.json.tmp` then rename
3. **Cleanup stale entries?** Yes, remove entries for files that no longer exist
4. **Handle seen_sessions across incremental runs?** The `seen_sessions` HashSet prevents duplicate session start events within a single export run. Since session starts are emitted deterministically, this works correctly with incremental parsing - we may re-emit a session start if we haven't seen it in this run, but the downstream import is idempotent (deterministic IDs).
5. **Concurrent export runs?** Not handled (would require file locking). Both runs will process overlapping data, but downstream idempotency prevents issues. Document as known limitation.
6. **Offset semantics?** Offset is the byte position *after* the trailing newline of the last successfully parsed line. No skip logic needed on resume.
7. **Duplicate session start events?** Accepted. Across incremental runs, the same session may emit session start multiple times. Event IDs are deterministic (`remote.agent:agent_session:{timestamp}:{session_id}:started`), so downstream import can dedupe on ID.
