# End-to-End Test Plan

## Task

> Test end-to-end: tmux focus → `events.jsonl` → sync → local SQLite

Validate the complete prototype flow works correctly.

## Approach

Create integration tests that run the actual Rust binary (`tt ingest`, `tt export`) via subprocess and verify the output is correctly consumed by the Python local tools (`tt import`, `tt events`, `tt status`).

### Why NOT Kubernetes

Initial plan proposed Kubernetes pods, but this is over-engineered:
- The SSH layer is not what's being tested - we're testing data format compatibility
- Existing mock-based tests in `test_cli.py` already cover SSH behavior
- Kubernetes adds pod startup latency (~30s), flakiness, and complexity
- Local subprocess tests can validate the same thing in ~100ms

### What We're Actually Testing

**Integration boundary:** Rust binary output → Python CLI input

```
Rust tt binary                    Python tt-local
┌─────────────────────┐          ┌─────────────────────┐
│ tt ingest → file    │ ───────▶ │ N/A (not tested,    │
│ tt export → stdout  │ ───────▶ │ tt import reads)    │
└─────────────────────┘          └─────────────────────┘
                                           │
                                           ▼
                                  ┌─────────────────────┐
                                  │ SQLite              │
                                  │ tt events queries   │
                                  │ tt status queries   │
                                  └─────────────────────┘
```

### Test Flow

```python
# 1. Build Rust binary (once per test session)
cargo build --release

# 2. Run tt ingest (Rust) with temp XDG_DATA_HOME
XDG_DATA_HOME=/tmp/test ./tt ingest pane-focus --pane %1 --cwd /home/test --session main --window 0

# 3. Run tt export (Rust) and capture stdout
output = XDG_DATA_HOME=/tmp/test ./tt export

# 4. Pipe to Python tt import
echo "$output" | tt import --db /tmp/test.db

# 5. Verify with tt events
tt events --db /tmp/test.db

# 6. Verify with tt status
tt status --db /tmp/test.db
```

## Test Cases

### Must Have (validates integration)

1. **Ingest-export-import roundtrip**
   - Run `tt ingest` (Rust) to create an event
   - Run `tt export` (Rust) to output JSONL
   - Pipe to `tt import` (Python)
   - Verify event in local SQLite via `tt events`

2. **Multiple events roundtrip**
   - Ingest 3 events with different panes/cwds
   - Export → import
   - Verify all 3 events with correct data

3. **Re-export idempotency**
   - Export → import → export → import again
   - Verify no duplicates (same event ID = skipped)

### Nice to Have (already covered elsewhere)

- Debouncing: covered by `crates/tt-cli/src/commands/ingest.rs` unit tests
- Import idempotency: covered by `tt-local/tests/test_cli.py`
- SSH error handling: covered by `tt-local/tests/test_cli.py`

### Not Testing

- SSH transport (mocked in existing tests)
- Claude log parsing (has extensive unit tests)
- tmux hook installation (deployment concern, not code)

## Implementation

### File

`tt-local/tests/test_e2e.py`

### Fixture: Build Rust Binary

```python
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent.parent
RUST_BINARY = REPO_ROOT / "target" / "release" / "tt"

@pytest.fixture(scope="module")
def rust_binary():
    """Build the Rust binary once for all tests."""
    subprocess.run(
        ["cargo", "build", "--release"],
        cwd=REPO_ROOT,
        check=True,
    )
    assert RUST_BINARY.exists(), f"Binary not found at {RUST_BINARY}"
    return RUST_BINARY
```

### Test: Roundtrip

```python
def test_ingest_export_import_roundtrip(rust_binary, tmp_path):
    """End-to-end: Rust ingest/export → Python import → query."""

    # Set up temp data directory for Rust binary
    data_dir = tmp_path / "time-tracker"
    env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

    # 1. Run ingest (Rust)
    subprocess.run([
        str(rust_binary), "ingest", "pane-focus",
        "--pane", "%1",
        "--cwd", "/home/test/project",
        "--session", "dev",
        "--window", "0",
    ], env=env, check=True)

    # Verify events.jsonl created
    events_file = data_dir / "events.jsonl"
    assert events_file.exists()

    # 2. Run export (Rust)
    result = subprocess.run(
        [str(rust_binary), "export"],
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    export_output = result.stdout
    assert export_output.strip()  # Not empty

    # 3. Import to Python (via CLI runner to stay in-process)
    db_path = tmp_path / "local.db"
    runner = CliRunner()
    import_result = runner.invoke(main, ["import", "--db", str(db_path)], input=export_output)
    assert import_result.exit_code == 0
    assert "Imported 1 events" in import_result.output

    # 4. Query events
    events_result = runner.invoke(main, ["events", "--db", str(db_path)])
    assert events_result.exit_code == 0
    events = [json.loads(line) for line in events_result.output.strip().split("\n")]
    assert len(events) == 1
    assert events[0]["type"] == "tmux_pane_focus"
    assert events[0]["source"] == "remote.tmux"
    assert events[0]["cwd"] == "/home/test/project"

    # 5. Check status
    status_result = runner.invoke(main, ["status", "--db", str(db_path)])
    assert status_result.exit_code == 0
    assert "remote.tmux" in status_result.output
    assert "1 event" in status_result.output
```

## Acceptance Criteria

- [ ] Test builds Rust binary via `cargo build --release`
- [ ] Test runs `tt ingest` and verifies `events.jsonl` created
- [ ] Test runs `tt export` and captures valid JSONL output
- [ ] Test imports events to local SQLite via Python CLI
- [ ] Test queries events and verifies data integrity
- [ ] Test verifies re-import is idempotent (no duplicates)
- [ ] All tests pass with `pytest tt-local/tests/test_e2e.py`

## Dependencies

- Rust toolchain (for building tt binary)
- Python with tt-local installed (`uv pip install -e tt-local`)
- pytest

No Kubernetes, no Docker, no SSH mocking - just subprocess calls to the actual binaries.
