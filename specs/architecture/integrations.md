# Integrations

This document specifies the integrations architecture for Time Tracker: rules engine, API server, and webhooks.

## Problem Statement

Time Tracker needs to integrate with external systems and provide extensibility:

1. **Rules Engine** — Predictable patterns (e.g., "work in ~/acme always gets the 'acme' tag") shouldn't require LLM calls. Users need a way to define these rules.

2. **API Server** — External tools (TUI dashboard, scripts, future mobile app) need programmatic access to time tracking data.

3. **Webhooks** — Some workflows need push notifications (e.g., Slack alerts, timesheet systems) rather than polling.

## Priority and Phasing

| Feature | Priority | Phase | Rationale |
|---------|----------|-------|-----------|
| Rules engine | High | 1 | Complements LLM tagger, reduces manual work, no external dependencies |
| API server | Medium | 2 | Enables TUI, scripting, external tools |
| Webhooks | Low | 3 | Most users don't need push; can poll API instead |

---

## 1. Rules Engine

### Purpose

Handle predictable tagging patterns without LLM calls. Rules run first; LLM handles ambiguous cases.

### Configuration

Rules are defined in `~/.config/tt/config.toml` using TOML syntax.

```toml
[[rules]]
path = "~/work/acme/*"
tags = ["acme-webapp"]

[[rules]]
path = "~/projects/personal/*"
tags = ["personal"]

[[rules]]
path = "~/work/client-a/**"
tags = ["client-a", "billable"]
```

### Match Conditions

**MVP scope**: Path matching only.

| Condition | Type | Description |
|-----------|------|-------------|
| `path` | glob | Working directory pattern |

**Glob semantics** (using `globset` crate):
- `~` expands to `$HOME` at config load time
- `*` matches any single path component
- `**` matches any number of path components (recursive)
- Patterns are case-sensitive on Linux, case-insensitive on macOS (follows filesystem)
- Trailing slashes are normalized away before matching
- Paths are canonicalized before matching (resolves `..`, symlinks)

**Examples**:
- `~/work/acme/*` — matches `~/work/acme/frontend` but not `~/work/acme/frontend/src`
- `~/work/acme/**` — matches `~/work/acme/frontend/src/components`
- `~/projects/*` — matches any direct child of `~/projects`

**Post-MVP match conditions** (not in initial implementation):
- `git_remote` — Git remote URL pattern
- `git_branch` — Branch name pattern
- `tmux_session` — tmux session name pattern
- `time_after`, `time_before` — Time-of-day rules

### Actions

**MVP scope**: Tags only.

| Action | Type | Description |
|--------|------|-------------|
| `tags` | list | Tags to assign to matching streams |

**Tag validation**: Tags must match `[a-zA-Z0-9_-]+` and be 1-64 characters. Empty tags are rejected.

**Post-MVP actions** (not in initial implementation):
- `project` — Assign to project
- `client` — Assign to client
- `billable` — Set billable flag

### Evaluation Semantics

1. Rules are evaluated in definition order
2. **First match wins** — once a rule matches, evaluation stops
3. Matching occurs during stream inference (lazy, not on every event)
4. When no rules match:
   - Fall back to `suggest_from_metadata()` (existing heuristic)
   - If still no match, invoke LLM for suggestions

**Config merge behavior**: When multiple config files are provided (via `--config`), rules from later files are appended after rules from earlier files. Order within each file is preserved.

**Evaluation trigger**: Rules run when:
- A new stream is created
- `tt suggest` is invoked
- A stream's events are updated and `needs_recompute` is true

### CLI Commands

#### `tt rules list`

Display all loaded rules with their index, patterns, and validation status.

```
$ tt rules list
Rules from ~/.config/tt/config.toml:

  1. path: ~/work/acme/*     → tags: [acme-webapp]
  2. path: ~/projects/oss/*  → tags: [open-source]
  3. path: ~/projects/personal/* → tags: [personal]

3 rules loaded.

# With validation errors:
$ tt rules list
Rules from ~/.config/tt/config.toml:

  1. path: ~/work/acme/*     → tags: [acme-webapp]
  2. [INVALID] path: ~/work/[invalid → Error: Missing closing bracket
  3. path: ~/projects/*      → tags: [personal]

2 rules loaded (1 skipped due to errors).
Warning: Run 'tt rules validate' for details.
```

#### `tt rules validate`

Check rules file for syntax errors and invalid patterns. Rules are also validated automatically at startup.

```
$ tt rules validate
✓ 3 rules validated successfully.

$ tt rules validate
Error at line 12: invalid glob pattern '~/work/[invalid'
  Missing closing bracket in character class

$ tt rules validate
Warning at line 8: unknown field 'project' (ignored)
  Hint: 'project' is not supported in MVP. Use 'tags' instead.
```

**Exit codes**:
- 0: All rules valid
- 1: Errors found (rules won't load)
- 0 with warnings: Rules load but some fields ignored

#### `tt rules test --path <path>`

Test which rule would match a given path.

```
$ tt rules test --path ~/work/acme/frontend
Rule 1 matches: path: ~/work/acme/*
  Would assign tags: [acme-webapp]

$ tt rules test --path ~/random/project
No rules match path: ~/random/project
  Will fall back to metadata analysis, then LLM suggestions.
```

This helps users debug why certain streams get certain tags.

### Observability

When `tt suggest` uses a rule, show the source:

```
$ tt suggest abc123
Stream: abc123 (tmux/dev/session-1)
  Path: ~/work/acme/frontend

Suggested tags: acme-webapp
  Source: Rule 1 (path: ~/work/acme/*)
```

When no rule matches:

```
$ tt suggest def456
Stream: def456 (tmux/dev/session-2)
  Path: ~/projects/experiment

Suggested tags: misc
  Source: LLM analysis
  Confidence: 65%
```

### Error Handling

| Error | Behavior |
|-------|----------|
| TOML syntax error | Startup fails with line number and message |
| Invalid glob pattern | Warning; rule skipped |
| Unknown field | Warning; field ignored, rule still loads |
| Invalid tag format | Warning; rule skipped |
| Empty tags list | Warning; rule skipped |
| Duplicate rule (same pattern) | Warning; both rules load (first wins) |

Example error output:

```
$ tt status
Warning: Rules file has errors, rules disabled.
  Line 5: invalid TOML - expected '=' after key
  Run 'tt rules validate' for details.
```

**Independence**: Invalid rules do not affect other features. If rules fail to parse, the application continues with rules disabled.

### Acceptance Criteria

1. Rules load from `~/.config/tt/config.toml` at startup
2. Path patterns support `~`, `*`, and `**` glob syntax
3. First matching rule's tags are applied to streams
4. `tt rules list` shows all loaded rules with validation status
5. `tt rules validate` catches syntax errors before startup
6. `tt rules test --path <path>` shows which rule would match
7. `tt suggest` output indicates when tags came from rules vs LLM
8. Invalid rules produce warnings but don't crash the application

---

## 2. API Server

### Purpose

Provide programmatic access to time tracking data for external tools (TUI dashboard, scripts, integrations).

### Design Philosophy

- **Read-heavy**: Most operations are queries, not writes
- **Local-only**: Binds to localhost by default; no authentication needed
- **Synchronous**: Uses `spawn_blocking` for SQLite calls (acceptable for local use)

### Starting the Server

```bash
$ tt serve [--port 8080] [--host localhost]
Starting API server on http://localhost:8080

Endpoints:
  GET  /api/v1/health        Lightweight health check
  GET  /api/v1/status        Health and tracking status
  GET  /api/v1/events        Query events (supports filters)
  GET  /api/v1/streams       List streams with time totals
  GET  /api/v1/streams/:id   Stream details
  GET  /api/v1/report        Generate time report

Press Ctrl+C to stop.
```

**Options**:
- `--port` — Port number (default: 8080)
- `--host` — Bind address (default: localhost)

### Database Concurrency

The API server uses `Arc<Mutex<Database>>` for thread-safe access to the SQLite connection. All database operations are wrapped in `tokio::task::spawn_blocking` since rusqlite is synchronous.

**WAL mode**: The database must enable WAL mode (`PRAGMA journal_mode=WAL`) for concurrent reads from CLI and API server.

**Limitation**: This serializes all database access through a single connection. Acceptable for local use with modest traffic, but not designed for high-throughput scenarios.

### Endpoints

#### `GET /api/v1/health`

Lightweight health check for process managers.

**Response**:
```json
{"ok": true}
```

#### `GET /api/v1/status`

Health and tracking status.

**Response**:
```json
{
  "database": "/home/user/.local/share/tt/events.db",
  "event_count": 15432,
  "sources": {
    "remote.agent": "2025-01-29T12:45:00Z",
    "remote.tmux": "2025-01-29T12:43:00Z"
  },
  "sync_status": {
    "devserver": {
      "last_sync": "2025-01-29T12:40:00Z",
      "event_count": 847,
      "stale": false
    }
  },
  "rules_loaded": 3,
  "webhooks_disabled": ["timesheet-sync"]
}
```

Note: All paths in API responses are absolute (no `~` expansion needed by clients).

#### `GET /api/v1/events`

Query events with filters. All queries use parameterized statements (no SQL injection risk).

**Query parameters**:
- `after` — ISO 8601 timestamp, only events after this time
- `before` — ISO 8601 timestamp, only events before this time
- `type` — Event type filter (e.g., `tmux_pane_focus`)
- `stream_id` — Filter by stream ID
- `limit` — Maximum events to return (default: 1000, max: 10000)
- `offset` — Pagination offset

**Validation**:
- `before` must be after `after` (if both provided), else 400
- Date-only timestamps (e.g., `2025-01-29`) treated as midnight UTC
- `limit` over 10000 returns 400 error

**Response**:
```json
{
  "events": [
    {
      "id": "abc123...",
      "type": "tmux_pane_focus",
      "timestamp": "2025-01-29T12:00:00Z",
      "data": {
        "pane_id": "%3",
        "cwd": "/home/user/work/acme"
      },
      "stream_id": "stream-xyz"
    }
  ],
  "total": 15432,
  "has_more": true
}
```

#### `GET /api/v1/streams`

List all streams with time totals.

**Query parameters**:
- `since` — Only streams with activity since this date
- `tag` — Filter by tag

**Response**:
```json
{
  "streams": [
    {
      "id": "abc123",
      "name": "tmux/dev/session-1",
      "time_direct_ms": 8100000,
      "time_direct_formatted": "2h 15m",
      "time_delegated_ms": 16200000,
      "time_delegated_formatted": "4h 30m",
      "tags": ["acme-webapp", "urgent"],
      "last_activity": "2025-01-29T12:45:00Z"
    }
  ]
}
```

#### `GET /api/v1/streams/:id`

Get details for a specific stream.

**Response**:
```json
{
  "id": "abc123",
  "name": "tmux/dev/session-1",
  "time_direct_ms": 8100000,
  "time_direct_formatted": "2h 15m",
  "time_delegated_ms": 16200000,
  "time_delegated_formatted": "4h 30m",
  "tags": ["acme-webapp", "urgent"],
  "last_activity": "2025-01-29T12:45:00Z",
  "event_count": 234,
  "first_event": "2025-01-27T09:00:00Z",
  "paths": ["/home/user/work/acme/frontend"]
}
```

#### `GET /api/v1/report`

Generate a time report.

**Query parameters**:
- `period` — `week`, `last-week`, `day`, `last-day` (default: `week`)

**Validation**: Invalid period values return 400 with message listing valid options.

**Response**:
```json
{
  "period": {
    "start": "2025-01-27T00:00:00Z",
    "end": "2025-02-02T00:00:00Z",
    "label": "Week of Jan 27, 2025"
  },
  "by_tag": {
    "acme-webapp": {
      "time_direct_ms": 9900000,
      "time_direct_formatted": "2h 45m",
      "time_delegated_ms": 14400000,
      "time_delegated_formatted": "4h 00m",
      "streams": ["abc123", "ghi789"]
    }
  },
  "untagged": {
    "time_direct_ms": 2700000,
    "time_direct_formatted": "45m",
    "time_delegated_ms": 1800000,
    "time_delegated_formatted": "30m",
    "streams": ["jkl012"]
  },
  "summary": {
    "total_tracked_ms": 37800000,
    "total_tracked_formatted": "10h 30m",
    "time_direct_ms": 18000000,
    "time_direct_formatted": "5h 00m",
    "time_delegated_ms": 19800000,
    "time_delegated_formatted": "5h 30m"
  }
}
```

### Error Handling

| Scenario | Status | Response |
|----------|--------|----------|
| Invalid query parameter | 400 | `{"error": "Invalid timestamp format for 'after'"}` |
| Invalid period value | 400 | `{"error": "Invalid period 'thisweek'. Valid: week, last-week, day, last-day"}` |
| Limit exceeds max | 400 | `{"error": "Limit cannot exceed 10000"}` |
| before < after | 400 | `{"error": "'before' must be after 'after'"}` |
| Stream not found | 404 | `{"error": "Stream 'xyz' not found"}` |
| Database error | 500 | `{"error": "Database error"}` (no detail in production) |
| Port in use | — | Startup fails with message and hint |

**Port in use error**:
```
Error: Port 8080 is already in use.
Hint: Check what's using it with: lsof -i :8080
      Or try a different port: tt serve --port 8081
```

### Security Considerations

- **No authentication** — Server binds to localhost only by default
- **Read-only** — No mutation endpoints
- If `--host 0.0.0.0` is used, warn about security implications

```
$ tt serve --host 0.0.0.0
Warning: Binding to 0.0.0.0 exposes the API to your network.
         No authentication is configured. Use with caution.

Starting API server on http://0.0.0.0:8080
```

### Acceptance Criteria

1. `tt serve` starts an HTTP server on localhost:8080
2. `/api/v1/health` returns `{"ok": true}` with minimal overhead
3. `/api/v1/status` returns database and sync information
4. `/api/v1/events` supports `after`, `before`, `type`, `limit` filters with max 10000
5. `/api/v1/streams` returns all streams with time totals (both ms and formatted)
6. `/api/v1/report` generates reports matching CLI output
7. Invalid requests return appropriate 4xx errors with helpful messages
8. Database errors return 500 without leaking internal details
9. Port conflicts produce actionable error messages

---

## 3. Webhooks

### Purpose

Push notifications to external systems when significant events occur in Time Tracker.

### Design Philosophy

- **Privacy-first**: Opt-in only; no data sent without explicit configuration
- **Reliable delivery**: At-least-once with retries and backoff
- **Debuggable**: Clear visibility into delivery status and failures
- **Secure**: HTTPS required, HMAC signing for verification

### Configuration

Webhooks are defined in `~/.config/tt/config.toml`:

```toml
[[webhooks]]
name = "slack-notify"
url = "https://hooks.slack.com/services/..."
events = ["sync_completed", "report_generated"]
secret = "$TT_WEBHOOK_SECRET_SLACK"

[[webhooks]]
name = "timesheet-sync"
url = "https://internal.example.com/timesheet/webhook"
events = ["stream_tagged"]
secret = "$TT_WEBHOOK_SECRET_TIMESHEET"
```

**Fields**:
- `name` — Human-readable identifier (required, must be unique)
- `url` — HTTPS endpoint to POST to (required, must be https://)
- `events` — List of event types to subscribe to (required)
- `secret` — Shared secret for HMAC signing (required)

**Secret format**: Secrets can be:
- Environment variable reference: `$VAR_NAME` or `${VAR_NAME}`
- Literal value (not recommended): `"my-secret"`

If an environment variable reference is used but the variable is unset, the webhook fails validation at startup.

**Validation**:
- Names must be unique across all webhooks
- URLs must use HTTPS (HTTP rejected)
- URLs cannot be localhost/127.0.0.1 (warning only, not blocked)

### Webhook State Storage

Webhook delivery state is persisted in SQLite (`webhook_state` table):

```sql
CREATE TABLE webhook_state (
  webhook_name TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 1,
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  total_deliveries INTEGER NOT NULL DEFAULT 0,
  successful_deliveries INTEGER NOT NULL DEFAULT 0,
  last_delivery_at TEXT,
  last_failure_at TEXT,
  last_error TEXT,
  disabled_reason TEXT
);
```

This ensures delivery state survives restarts.

### Supported Events

| Event | Trigger | Payload includes |
|-------|---------|------------------|
| `sync_completed` | `tt sync` finishes successfully | Remote name, event count, duration |
| `stream_tagged` | `tt tag` assigns a tag | Stream ID, tag, all current tags |
| `report_generated` | `tt report` completes | Period, summary totals |

### Payload Format

All payloads follow this structure:

```json
{
  "event_id": "550e8400-e29b-41d4-a716-446655440000",
  "event": "sync_completed",
  "timestamp": "2025-01-29T12:45:00Z",
  "data": {
    "remote": "devserver",
    "events_imported": 42,
    "duration_ms": 1234
  }
}
```

**`event_id`**: Unique UUID for each webhook delivery. Use for deduplication on the receiver side.

**HTTP headers**:
- `Content-Type: application/json`
- `X-TT-Signature: <hmac-sha256-hex>`
- `X-TT-Timestamp: <unix-timestamp>`

**Event-specific data**:

`sync_completed`:
```json
{
  "remote": "devserver",
  "events_imported": 42,
  "duration_ms": 1234
}
```

`stream_tagged`:
```json
{
  "stream_id": "abc123",
  "stream_name": "tmux/dev/session-1",
  "tag_added": "acme-webapp",
  "all_tags": ["acme-webapp", "urgent"]
}
```

`report_generated`:
```json
{
  "period": "week",
  "period_start": "2025-01-27T00:00:00Z",
  "period_end": "2025-02-02T00:00:00Z",
  "total_tracked_ms": 37800000,
  "time_direct_ms": 18000000,
  "time_delegated_ms": 19800000
}
```

### Delivery Semantics

- **Asynchronous**: Webhook delivery does not block the triggering command. If `tt sync` completes, the `sync_completed` webhook is queued for delivery in the background.
- **At-least-once delivery**: Payloads may be delivered multiple times on retry
- **Idempotency**: Receivers should use `event_id` for deduplication

**Retry policy** (with jitter):
1. Initial delivery attempt
2. Retry after 1 minute (± 30 seconds jitter)
3. Retry after 5 minutes (± 1 minute jitter)
4. Retry after 15 minutes (± 3 minutes jitter)
5. After 5 consecutive failures, webhook is disabled

**Pending retries**: Stored in SQLite. Checked on application startup and delivered.

**Success criteria**: HTTP 2xx response within 30 seconds.

**HTTP status handling**:
- 2xx: Success
- 429: Retry (respect `Retry-After` header if present)
- 408: Retry (request timeout)
- Other 4xx: Log error, don't retry (client error)
- 5xx: Retry with backoff

### Security

**HTTPS required**: All webhook URLs must use HTTPS. This protects secrets in transit.

**HMAC-SHA256 signing**:

Each request includes:
- `X-TT-Signature`: HMAC-SHA256 of the request body using the webhook's secret (hex-encoded)
- `X-TT-Timestamp`: Unix timestamp of when the webhook was sent

**Verification example** (Python):
```python
import hmac
import hashlib
import time

def verify_webhook(body: bytes, signature: str, timestamp: str, secret: str) -> bool:
    # Check timestamp is recent (prevent replay attacks)
    # Use 60 second window (ensure NTP sync)
    if abs(time.time() - int(timestamp)) > 60:
        return False

    expected = hmac.new(
        secret.encode(),
        body,
        hashlib.sha256
    ).hexdigest()

    return hmac.compare_digest(signature, expected)
```

**Clock synchronization**: Webhook receivers should have NTP-synchronized clocks. The 60-second window allows for minor clock drift.

### CLI Commands

#### `tt webhooks list [-v]`

Show configured webhooks and their status.

```
$ tt webhooks list
Webhooks:

  slack-notify
    URL: https://hooks.slack.com/services/...
    Events: sync_completed, report_generated
    Status: enabled
    Last delivery: 2025-01-29T12:30:00Z (success)

  timesheet-sync
    URL: https://internal.example.com/timesheet/webhook
    Events: stream_tagged
    Status: disabled (5 consecutive failures)
    Last delivery: 2025-01-28T15:00:00Z (failed: 503)

$ tt webhooks list -v
Webhook: slack-notify
  URL: https://hooks.slack.com/services/...
  Events: sync_completed, report_generated
  Status: enabled
  Total deliveries: 47
  Successful: 47
  Failed: 0
  Last success: 2025-01-29T12:30:00Z

Webhook: timesheet-sync
  URL: https://internal.example.com/timesheet/webhook
  Events: stream_tagged
  Status: disabled
  Reason: 5 consecutive failures
  Total deliveries: 23
  Successful: 18
  Failed: 5
  Last failure: 2025-01-28T15:00:00Z
  Error: HTTP 503 Service Unavailable

  To re-enable: tt webhooks enable timesheet-sync
```

#### `tt webhooks test <name> [--dry-run]`

Send a test payload to verify webhook configuration.

```
$ tt webhooks test slack-notify
Sending test payload to slack-notify...
✓ Delivered successfully (HTTP 200, 234ms)

$ tt webhooks test slack-notify --dry-run
Would send to: https://hooks.slack.com/services/...
Headers:
  Content-Type: application/json
  X-TT-Signature: 8a7d...
  X-TT-Timestamp: 1706529600
Payload:
{
  "event_id": "test-...",
  "event": "test",
  "timestamp": "2025-01-29T12:00:00Z",
  "data": {"message": "Test payload from tt webhooks test"}
}

$ tt webhooks test timesheet-sync
Sending test payload to timesheet-sync...
✗ Delivery failed: HTTP 503 Service Unavailable
```

#### `tt webhooks enable <name>`

Re-enable a disabled webhook.

```
$ tt webhooks enable timesheet-sync
Webhook timesheet-sync re-enabled.
Next event will trigger delivery attempt.
```

#### `tt webhooks disable <name>`

Temporarily disable a webhook.

```
$ tt webhooks disable slack-notify
Webhook slack-notify disabled.
Use 'tt webhooks enable slack-notify' to re-enable.
```

### Error Handling

| Scenario | Behavior |
|----------|----------|
| Invalid URL (not HTTPS) | Config validation error at startup |
| Missing secret | Config validation error at startup |
| Secret env var not set | Config validation error at startup |
| Duplicate webhook name | Config validation error at startup |
| Network error | Retry with backoff |
| HTTP 429 | Retry with backoff (use Retry-After if present) |
| HTTP 408 | Retry with backoff |
| HTTP 4xx (other) | Log error, don't retry |
| HTTP 5xx | Retry with backoff |
| Timeout (>30s) | Treat as failure, retry |
| 5 consecutive failures | Disable webhook, require manual re-enable |

**Independence**: Invalid webhook config does not affect other features. The application continues with webhooks disabled.

### Status in `tt status`

When webhooks are disabled, `tt status` shows a warning:

```
$ tt status
...
Webhooks:
  Warning: 1 webhook disabled due to failures
    timesheet-sync: 5 consecutive failures
    Run 'tt webhooks list -v' for details.
```

### Acceptance Criteria

1. Webhooks load from `config.toml` at startup with HTTPS and unique name validation
2. Secrets can be read from environment variables
3. Configured events trigger webhook delivery asynchronously
4. Payloads include unique `event_id`, correct event data, and HMAC signature
5. Failed deliveries retry with exponential backoff and jitter
6. HTTP 429 is handled with retry (respecting Retry-After)
7. Webhooks disable after 5 consecutive failures
8. `tt webhooks list` shows all webhooks with status
9. `tt webhooks list -v` shows detailed delivery history
10. `tt webhooks test` sends test payload and reports result
11. `tt webhooks test --dry-run` shows payload without sending
12. `tt webhooks enable/disable` toggle webhook state
13. Disabled webhooks appear as warning in `tt status`

---

## Implementation Dependencies

```
Rules Engine  →  (none, can be implemented first)
        ↓
API Server    →  (enables TUI dashboard)
        ↓
Webhooks      →  (can use API server infrastructure)
```

**Database prerequisite**: WAL mode must be enabled in `tt-db` for API server concurrency.

## Not In Scope

- **Authentication**: Local-only API doesn't need auth
- **Multi-user**: Single-user design
- **Cloud deployment**: All components run locally
- **Event ingestion via API**: `tt ingest` handles this
- **Complex webhook filtering**: Start simple, expand based on feedback
- **Webhook management UI**: CLI-only for now
- **Config hot reload**: Requires restart to pick up changes

## Future Considerations

These may be added based on user feedback:

1. **Rule conditions**: Git remote, branch, tmux session, time-of-day
2. **Rule actions**: Project, client, billable flag
3. **API authentication**: If users need network access
4. **More webhook events**: Stream created, tag removed, etc.
5. **Webhook filtering**: Per-tag, per-stream filters
6. **Daemon mode**: `tt serve --daemon` for background operation
7. **Config hot reload**: SIGHUP to reload config without restart
