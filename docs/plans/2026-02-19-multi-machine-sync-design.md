# Multi-Machine Sync Design

**Goal:** Enable time tracking across multiple remote dev servers, with a local laptop as the aggregation point. Each remote generates events independently; the laptop pulls and merges them into a single database for unified reporting.

**Context:** The app currently assumes a single remote. Event IDs can collide across machines, there's no machine provenance in the schema, sync state isn't tracked, and directory paths are inconsistent across XDG conventions. This design addresses all of these.

**User setup:** Multiple remote dev servers accessed via SSH config aliases, one local laptop as the hub. Occasionally parallel work on the same project across machines.

---

## 1. XDG Directory Cleanup

The current directory layout is inconsistent:
- `~/.config/tt/` holds config AND the database (code default)
- `~/.time-tracker/` holds events, logs, and manifests (custom dotfolder)
- `~/.local/share/tt/` is documented in the README but unused by code

Migrate to proper XDG Base Directory conventions:

| Category | Directory | Files |
|----------|-----------|-------|
| Config | `~/.config/tt/` | `config.toml` |
| Data | `~/.local/share/tt/` | `tt.db`, `machine.json`, `events.jsonl` |
| State | `~/.local/state/tt/` | `hook.log`, `claude-manifest.json` |

Implementation:
- Add `dirs_data_path()` and `dirs_state_path()` helpers alongside existing `dirs_config_path()`
- Update `config.rs` default `database_path` to use `dirs_data_path()`
- Update `default_data_dir()` in `ingest.rs` and `export.rs` to use `dirs_data_path()`
- Update `tmux-hook.conf` to log to `~/.local/state/tt/hook.log`
- Update `claude-manifest.json` path in `export.rs` to use `dirs_state_path()`
- Update README to match
- No automatic migration of old `~/.time-tracker/` paths

## 2. Machine Identity

Each machine gets a persistent UUID generated via `tt init`, stored in `~/.local/share/tt/machine.json`:

```json
{
  "machine_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "label": "devbox"
}
```

- **`machine_id`**: UUID v4, generated once, never changes. Stable identity that survives SSH alias renames.
- **`label`**: Optional human-friendly name (`tt init --label devbox`, defaults to system hostname). Used in reports/status output only, never for identity or dedup.
- `tt init` is idempotent: if `machine.json` exists, prints the existing ID. Pass `--label` to update just the label.

Event-producing commands (`ingest`, `export`) require `machine.json` and refuse to run without it:
> `"No machine identity found. Run 'tt init' first."`

Read-only commands (`report`, `status`, `streams`, `context`) do not require machine identity, since the local aggregation laptop may never generate events.

## 3. Event ID Scheme

Prepend `{machine_id}:` to all event IDs, making cross-machine collisions impossible.

Current format:
```
remote.tmux:tmux_pane_focus:2025-01-29T14:32:01.000Z:%3
```

New format:
```
a1b2c3d4-e5f6-7890-abcd-ef1234567890:remote.tmux:tmux_pane_focus:2025-01-29T14:32:01.000Z:%3
```

Change points:
- `IngestEvent::pane_focus()` in `ingest.rs` — reads `machine.json`, prepends UUID
- `emit_session_*()` functions in `export.rs` — same

The JSONL format on disk (`events.jsonl`) includes the full ID, so events are self-describing and carry provenance before import.

### Migrating existing events.jsonl

Existing `events.jsonl` files on remotes need a one-time migration after `tt init`. This is a manual step, not built into `tt`:

```bash
MACHINE_ID=$(jq -r .machine_id ~/.local/share/tt/machine.json)
jq -c ".id = \"${MACHINE_ID}:\" + .id" ~/.time-tracker/events.jsonl > ~/.local/share/tt/events.jsonl
```

## 4. Schema Changes

Schema version 7 → 8. Per existing convention, this is a hard break (fresh DB required).

### `events` table — add `machine_id` column

```sql
machine_id TEXT  -- UUID of the originating machine
```

Denormalized from the event ID for query convenience. Populated during import by parsing the UUID prefix from the event ID.

### New `machines` table

```sql
CREATE TABLE machines (
    machine_id TEXT PRIMARY KEY,  -- UUID
    label TEXT,                   -- human-friendly name (e.g., "devbox")
    last_sync_at TEXT,            -- ISO 8601 timestamp of last successful sync
    last_event_id TEXT            -- last event ID imported from this machine
);
```

Tracks per-remote sync state so `tt sync` can do incremental pulls.

### `agent_sessions` table — add `machine_id` column

```sql
machine_id TEXT  -- UUID of the machine where the session ran
```

Sessions from different machines must be distinguishable.

## 5. Sync Command

Today there's no `tt sync` in the code (README describes it, unimplemented). The new command formalizes the SSH pull workflow:

```bash
tt sync devbox                  # sync from SSH alias
tt sync user@10.0.0.5           # sync from explicit host
tt sync devbox gpu-server       # sync multiple remotes in sequence
```

Flow:
1. SSH to remote, run `tt export --after={last_event_id}` (incremental)
2. Pipe output into local import logic
3. On first sync from a new machine, prompt for label: `"New machine a1b2c3d4. Label? [devbox]:"`
4. Populate/update `machines` table (`last_sync_at`, `last_event_id`)
5. Run `tt ingest sessions` and `tt recompute` after import

New CLI additions:
- `tt sync <remote>...` — new subcommand
- `tt export --after <event_id>` — new flag on existing command (filter events.jsonl to only emit events after the given ID)
- `tt machines` — list known remotes from `machines` table (UUID, label, last sync time)

## 6. Stream Inference

The allocation algorithm in `allocation.rs` currently clusters events by cwd, timestamps, and agent sessions. With multi-machine support:

**Machine as a stream boundary:** Events from different machines are never merged into the same stream, even if they share the same cwd/project. Working on `time-tracker` on devbox and `time-tracker` on gpu-server are separate work sessions.

Implementation: partition events by `machine_id` before applying the existing clustering logic. `machine_id` becomes a grouping key alongside `git_project`.

**Reports:** Tags already handle cross-machine aggregation. A stream on devbox tagged `acme` and a stream on gpu-server tagged `acme` both roll up under the same tag in `tt report`. No changes needed to the report layer.

---

## Non-Goals

- Bidirectional sync (remotes don't need to see each other's data)
- Automatic migration of old `~/.time-tracker/` directory (manual one-liner)
- Built-in migration of existing `events.jsonl` IDs (manual jq command)
- Real-time sync (pull-based on demand is sufficient)
