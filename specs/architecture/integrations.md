# Integrations

How `tt` connects to external systems for automatic tagging and data export.

## Design Principles

1. **Deterministic rules complement LLM tagging** — Rules handle predictable patterns; LLM handles semantic analysis
2. **Export is explicit, not automatic** — Users review before pushing to billing systems
3. **Simple first** — No webhooks or API server until demonstrated need

---

## 1. Rules Engine

Deterministic auto-tagging for known patterns. Complements LLM tagging, doesn't replace it.

### Rule Evaluation

Rules evaluate at **stream inference time** (when streams are computed from events). This means:
- Rules run during `tt report`, `tt streams`, or any command that triggers inference
- Rules see complete stream data (aggregated from all events)
- Editing rules affects streams on next inference run (existing streams with `needs_recompute=false` are not re-evaluated unless flagged)

Rules match on **stream-level aggregates**:

| Field | Description |
|-------|-------------|
| `primary_cwd` | Most frequent working directory in the stream |
| `tmux_session` | Any tmux session name involved in the stream |
| `agent` | Any agent type used (e.g., `claude-code`) |

**Note:** `git_branch` matching is deferred until git integration captures branch data in events.

### Rule Format

Rules are defined in the standard config file:

```toml
# ~/.config/tt/config.toml

[[rules.auto_tag]]
name = "Acme projects"
match = { primary_cwd = "~/work/acme/*" }
tags = ["acme", "billable"]

[[rules.auto_tag]]
name = "Personal work"
match = { tmux_session = "personal-*" }
tags = ["personal"]

[[rules.auto_tag]]
name = "Open source contributions"
match = { primary_cwd = "~/oss/*", agent = "claude-code" }
tags = ["oss", "unbillable"]
```

### Matching Semantics

- **First matching rule wins** — Order matters; place specific rules before general ones
- **`~` expands to `$HOME`** — At evaluation time
- **`*` is glob-style** — Matches any characters (not regex)
- **Case-sensitive** — Paths match exactly as stored
- **Multiple conditions are AND** — All conditions in a match block must be true
- **Paths are literal** — Stream `primary_cwd` values are matched literally (glob metacharacters in actual paths are escaped before comparison)

**Rule ordering guidance:** The spec uses first-match semantics for simplicity. Place more specific rules first:
```toml
# CORRECT: Specific before general
[[rules.auto_tag]]
match = { primary_cwd = "~/work/acme/secret-project/*" }
tags = ["acme", "secret", "billable"]

[[rules.auto_tag]]
match = { primary_cwd = "~/work/acme/*" }
tags = ["acme", "billable"]

# WRONG: General rule shadows specific
# The ~/work/acme/* rule would catch everything, ~/work/acme/secret-project/* never fires
```

### Tag Priority and Sources

Tags can come from three sources, tracked in `stream_tags.assignment_source`:

| Source | Value | Description |
|--------|-------|-------------|
| User | `'user'` | Tags added via `tt tag <stream> <tag>` |
| Rule | `'rule'` | Tags applied by matching auto-tag rules |
| LLM | `'llm'` | Tags suggested by LLM analysis |

**Priority:** User > Rule > LLM

When a stream already has tags from a higher-priority source for a given tag name, lower-priority sources don't override it. However, sources can add *additional* tags that don't conflict:
- User tags `personal` → rule can still add `unbillable` if it matches
- If both user and rule want to set the same tag, user wins

**Schema addition required:** The `stream_tags` table needs an `assignment_source` column:
```sql
CREATE TABLE stream_tags (
  stream_id TEXT NOT NULL,
  tag TEXT NOT NULL,
  assignment_source TEXT NOT NULL DEFAULT 'inferred',  -- 'user', 'rule', 'llm'
  PRIMARY KEY (stream_id, tag),
  FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE CASCADE
);
```

### Validation Commands

```bash
# Validate rules file and show all loaded rules
$ tt rules check
config.toml: 3 rules loaded, all valid

# Test which rule matches a specific stream
$ tt rules check --stream a1b2c3d
Stream a1b2c3d: /home/sami/work/acme/webapp
  Current tags: acme (user), billable (rule)
  Matched rule: "Acme projects" (line 5)
  Would apply: billable (already has: acme from user)

# Dry-run: show what rules would apply to all untagged streams
$ tt rules check --dry-run
12 streams without rule-applied tags:
  3 match "Acme projects" → acme, billable
  2 match "Personal work" → personal
  7 unmatched (will use LLM suggestions)
```

### Error Handling

**Unknown field:**
```
Error: config.toml line 7: unknown field 'pathh' in match block
Valid fields: primary_cwd, tmux_session, agent
Did you mean: primary_cwd?
```

**Invalid glob:**
```
Error: config.toml line 9: invalid glob pattern '~/work/[invalid'
  Unclosed bracket in pattern
```

**Missing required field:**
```
Error: config.toml line 12: rule missing required 'tags' field
```

**Config file not found:**
```
Warning: No config file found at ~/.config/tt/config.toml
         Rules disabled; using LLM-only tagging
```

**TOML parse error:**
```
Error: config.toml line 7: TOML parse error
  unexpected character '=' in bare key
```

---

## 2. Push Integrations

Push time data to external billing systems.

**Note:** This is `tt push`, not `tt export`. The existing `tt export` command outputs JSONL for syncing between machines. `tt push` sends processed time entries to billing APIs.

### Supported Destinations

| Destination | Flag | Auth |
|-------------|------|------|
| Toggl Track | `--toggl` | `TOGGL_TOKEN` or config |
| Clockify | `--clockify` | `CLOCKIFY_TOKEN` or config |
| CSV | `--csv` | None |

### Commands

```bash
# Preview what would be pushed (default: this week)
tt push --toggl --dry-run

# Push to Toggl for specific date range
tt push --toggl --since 2025-01-20 --until 2025-01-27

# Push all un-pushed streams
tt push --toggl --all

# Push to CSV file
tt push --csv > timesheet.csv

# Force re-push (updates existing entries)
tt push --toggl --force
```

### Date Range Handling

**Default range:** This week (Monday through today), or "since last push" if tracking exists.

**Date interpretation:** Dates are interpreted as **local time at start of day** and converted to UTC for filtering. This matches user expectations ("push today's work").

```bash
# User in PST runs at 6 PM on Jan 28
tt push --toggl --since 2025-01-28
# Filters: streams with first_event_at >= 2025-01-28T08:00:00Z (midnight PST in UTC)
```

**Accepted formats:**
- `YYYY-MM-DD` — Start of that day in local time
- `YYYY-MM-DDTHH:MM:SS` — Specific local time
- `YYYY-MM-DDTHH:MM:SSZ` — Explicit UTC

### Dry Run Output

```bash
$ tt push --toggl --dry-run
Would push 12 streams as time entries:

  Jan 28  2h 15m  acme-webapp     "Auth service implementation"    [acme, billable]
  Jan 28  1h 30m  personal        "Side project experimentation"   [personal]
  Jan 27  3h 45m  acme-api        "API endpoint refactoring"       [acme, billable]
  ...

Run without --dry-run to push.
```

### Push Execution

```bash
$ tt push --toggl
Pushing to Toggl: 12 streams
  [################----] 8/12

Pushed 8 streams successfully.
4 failed:
  - a1b2c3d: Toggl 400: project_id 12345 not found
    Hint: Verify project exists in Toggl, or set default_project_id in config
  - e4f5g6h: Toggl 400: workspace_id required
    Hint: Add workspace_id to [push.toggl] in config.toml
  - i7j8k9l: Toggl 429: rate limited
    Hint: Waiting 60s before retry... (re-run to retry failed entries)
  - m0n1o2p: Network error: connection timeout
    Hint: Check network connection and try again

Fix the issues and re-run to retry failed entries.
```

### Rate Limiting

Push commands respect external API rate limits:
- **Proactive throttling:** 500ms delay between requests (Toggl limit is 1 req/sec)
- **Reactive backoff:** On 429 response, wait the requested duration (from `Retry-After` header)
- **User feedback:** Show countdown during rate limit waits

### Idempotency

Pushes are tracked in the database to prevent duplicates:

```sql
CREATE TABLE push_log (
  stream_id TEXT NOT NULL,
  destination TEXT NOT NULL,  -- 'toggl', 'clockify', etc.
  pushed_at TEXT NOT NULL,
  stream_updated_at TEXT NOT NULL,  -- snapshot for smart re-push
  external_id TEXT,           -- ID in external system (for updates)
  PRIMARY KEY (stream_id, destination)
);
```

**Behavior:**
- Already-pushed streams are skipped if `stream.updated_at <= push_log.stream_updated_at`
- Streams updated since last push are re-pushed automatically
- `--force` flag pushes everything, updating existing entries
- Failed pushes are not logged, so re-running retries them

### Streams Spanning Multiple Days

When a stream spans multiple calendar days (e.g., late-night work session from 10 PM to 2 AM):

**Default behavior:** Stream is pushed as a single entry dated at `first_event_at`.

**Rationale:** Splitting requires arbitrary time allocation decisions. Users who need per-day accuracy should tag/manage streams accordingly or manually adjust in the external system.

**Future consideration:** A `--split-days` flag could split streams at midnight boundaries, but this is deferred.

### Field Mapping

| tt field | Toggl | Clockify | CSV |
|----------|-------|----------|-----|
| Duration (see below) | `duration` (seconds) | `timeInterval.duration` (ISO 8601) | `duration_hours` |
| `stream.first_event_at` | `start` | `timeInterval.start` | `start` |
| `stream.last_event_at` | `stop` | `timeInterval.end` | `end` |
| `stream.name` | `description` | `description` | `description` |
| `stream.tags` | `tags` | `tagIds` (requires lookup) | `tags` |
| `stream.primary_cwd` | — | — | `working_directory` |

**Duration semantics:** Duration equals `time_direct_ms + time_delegated_ms` (total time, not elapsed time). This is the actual work time, which may be less than `last_event_at - first_event_at` if the user was AFK.

### Credential Management

**Environment variables (recommended):**
```bash
export TOGGL_TOKEN="your-api-token"
export CLOCKIFY_TOKEN="your-api-key"
```

**Config file:**
```toml
# ~/.config/tt/config.toml

[push.toggl]
api_token = "..."  # WARNING: Prefer env var for security
workspace_id = "12345"
default_project_id = "67890"  # Optional fallback

[push.clockify]
api_key = "..."
workspace_id = "..."
```

**Security notes:**
- Environment variables are preferred over config file
- Config files with credentials should have 600 permissions
- Implementation should warn if config file is world-readable

### Project Mapping (Future)

For users with project-based billing, tag-to-project mapping is a common need:

```toml
# Deferred - document as future enhancement
[push.toggl.project_mapping]
"acme" = "123456"     # Toggl project ID
"personal" = "789012"
```

This is not in the initial spec but noted as a likely user request.

### CSV Format

```csv
start,end,duration_hours,description,tags,working_directory
2025-01-28T10:00:00Z,2025-01-28T12:15:00Z,2.25,"Auth service implementation","acme,billable",/home/sami/work/acme/webapp
2025-01-28T14:00:00Z,2025-01-28T15:30:00Z,1.5,"Side project experimentation","personal",/home/sami/projects/side
```

**CSV safety:** Values starting with `=`, `+`, `-`, `@` are prefixed with a single quote to prevent formula injection when imported into spreadsheet software.

---

## 3. Deferred Features

These features are not included in this spec. Documented here for future reference.

### Webhooks

**Why deferred:**
- No demonstrated user need
- Push workflow requires human review anyway
- Adds operational complexity (retry queues, delivery tracking)

**If needed later:**
- Start with fire-and-forget (no retries, just logging)
- Single event type: `report.generated`
- HMAC signature for authenticity
- Add retry/delivery guarantees only when users hit limits

### API Server

**Why deferred:**
- TUI queries SQLite directly (no HTTP needed)
- Custom watchers use `tt ingest` (CLI, not HTTP)
- Running a daemon adds operational burden

**If needed later:**
- Consider ephemeral server: `tt dashboard --web` (starts server, opens browser)
- Not a persistent daemon
- REST endpoints for read operations only

---

## Implementation Notes

### Path Handling

- **Symlinks:** Paths should be canonicalized (symlinks resolved) before storage to ensure consistent matching
- **Unicode:** Normalize paths to NFC form before storage and comparison (macOS uses NFD)
- **Glob metacharacters in paths:** When matching a stream's `primary_cwd` against a rule pattern, escape any glob metacharacters in the stream value

### Concurrency

- **Single user assumption:** No concurrent push protection for MVP
- **If needed:** Use file lock or database advisory lock to prevent simultaneous pushes

### External API Compatibility

- Target Toggl Track API v9
- Target Clockify API v1
- Include `User-Agent: tt/<version>` header
- Document API versions in release notes

---

## Acceptance Criteria

### Rules Engine

- [ ] Rules defined in `~/.config/tt/config.toml` under `[[rules.auto_tag]]`
- [ ] Rules evaluate at stream inference time
- [ ] First matching rule wins
- [ ] `~` expands to `$HOME`, `*` is glob-style
- [ ] `tt rules check` validates rules and reports errors with line numbers
- [ ] `tt rules check --stream <id>` shows which rule matched and tag sources
- [ ] `tt rules check --dry-run` shows summary by rule, not per-stream
- [ ] Unknown fields produce helpful "did you mean?" errors
- [ ] Missing config file produces warning, not error
- [ ] Tags from rules have `assignment_source = 'rule'`
- [ ] User tags are never overwritten by rules
- [ ] `stream_tags` table has `assignment_source` column

### Push Integrations

- [ ] `tt push --toggl` pushes to Toggl Track API
- [ ] `tt push --clockify` pushes to Clockify API
- [ ] `tt push --csv` outputs CSV to stdout
- [ ] Default date range is "this week" or "since last push"
- [ ] `--dry-run` shows what would be pushed without pushing
- [ ] `--since` and `--until` filter by date (local time interpreted)
- [ ] `--all` pushes all un-pushed streams
- [ ] Credentials from env vars (`TOGGL_TOKEN`, `CLOCKIFY_TOKEN`)
- [ ] Credentials from config file as fallback
- [ ] Push log tracks what has been pushed with `stream_updated_at`
- [ ] Updated streams are re-pushed automatically
- [ ] `--force` re-pushes everything
- [ ] Partial failures reported with actionable hints
- [ ] Re-running retries failed pushes
- [ ] Rate limiting: 500ms delay between requests
- [ ] Rate limit backoff on 429 response
- [ ] CSV values escaped to prevent formula injection

---

## Related Documents

- [Data Model](../design/data-model.md) — Event schema, stream structure
- [CLI UX](../design/ux-cli.md) — Command interface patterns
- [Architecture Overview](overview.md) — System context
