# ADR-002: Remote Analysis Architecture

## Status

**Accepted**

## Context

The time tracker captures events on remote dev servers (tmux hooks, Claude session logs) and stores them locally for analysis. LLM-powered tag suggestions require understanding what work was done in each session.

**Constraints:**
- **Privacy**: Raw session content (Claude Code logs) may contain sensitive code and should not be transmitted to the local machine by default
- **Analysis location**: LLM-powered tag suggestions run on local (user's laptop)
- **Cost**: LLM API calls are expensive; unnecessary calls should be avoided
- **Simplicity**: MVP should minimize infrastructure complexity

**The gap**: How does local get enough context to make intelligent tag suggestions without receiving raw session content?

**Important assumption**: This design assumes the "80% case" is directory-based inference (`/home/user/acme-webapp/` → "acme-webapp"). This should be validated during MVP before investing in on-demand summarization. If metadata-only + manual tagging proves sufficient, `tt suggest` can be deferred to post-MVP.

## Decision Drivers

1. **Privacy by default** — Raw session content stays on remote unless user explicitly requests analysis
2. **Cost efficiency** — Minimize unnecessary LLM API calls
3. **Good UX** — Fast reports, accurate suggestions, graceful handling of edge cases
4. **Simplicity** — MVP focus on solving the common case

## Options Considered

### Option A: On-Demand Remote Summarization

Local SSHs to remote and calls `tt summarize --session=abc` for each session needing analysis. Remote loads session logs, calls Claude API, returns JSON summary.

**Pros:** Privacy-preserving, lazy evaluation, fresh analysis
**Cons:** Requires API key on remote, latency per session, cost per call, depends on remote being reachable

### Option B: Pre-Computed Summaries During Export

During `tt export`, remote runs LLM summarization and includes summaries as events.

**Pros:** Single sync operation, summaries available offline
**Cons:** Summarizes all sessions (wasteful), higher ongoing cost, slower export, stale summaries

### Option C: Metadata-Only Analysis

Local LLM infers context from event metadata (cwd, tool names, file paths, timestamps). No session content needed.

**Pros:** No privacy concerns, no API costs on remote, simpler architecture, works offline
**Cons:** Less context than full content, may miss semantic meaning, relies on meaningful directory names

### Option D: Hybrid Approach

**Default**: Metadata-only analysis on local using cwd, file paths, and temporal patterns.

**On-demand**: Remote summarization via `tt suggest <stream>` when user requests deeper analysis or metadata is ambiguous.

## Decision

**Option D: Hybrid Approach**

Rationale:
1. **80% case is simple** — Directory-based inference (`/home/user/acme-webapp/` → "acme-webapp") handles most sessions
2. **Cost-effective** — Only call LLM API when needed
3. **Privacy preserved** — Raw content stays remote by default
4. **User control** — On-demand summarization when auto-inference fails
5. **Incremental** — Can enhance metadata inference without architectural changes

## Consequences

### Good

- Privacy by default: raw session content never leaves remote unless explicitly requested
- Lower cost: metadata analysis is free; LLM calls only when needed
- Simpler remote: no background summarization, no API key required for basic operation
- Works offline: metadata-based inference needs no network
- Graceful degradation: if summarization fails, falls back to metadata-only

### Bad

- Manual tagging required when directory-based inference fails
- On-demand analysis requires remote to be reachable and have API key configured
- Summary quality depends on Claude API; cannot improve locally
- API key must be configured on each remote (operational overhead)
- Summaries may inadvertently contain sensitive information (LLM prompt is best-effort)

### Neutral

- Optional remote summarization remains available for power users
- May evolve to pre-computed summaries if cost/UX analysis changes
- `tt suggest` may be deferred to post-MVP if metadata-only proves sufficient

## Implementation Notes

### Remote Host Mapping

Events include a `source` field (e.g., `remote.agent`, `remote.tmux`) but not an explicit host. The remote host is determined from:
1. **Sync tracking**: When `tt sync user@devserver` imports events, the source remote is recorded in the sync state
2. **Implementation**: Store `sync_source` in the events table or a separate mapping table: `session_id → remote_host`

For MVP with single remote, this is implicit. Multi-remote support would require explicit tracking.

### Metadata Fields for Inference

From data-model.md:
- `cwd` — working directory (primary signal for project inference)
- `session_id` — groups events from same agent session
- `file` field in `agent_tool_use` events — edited files (secondary signal)

### Low-Confidence Heuristics

Suggest `tt suggest` when:
- `cwd` is home directory or generic path (`/tmp`, `/var/`)
- `cwd` changed 3+ times during session
- No `agent_tool_use` events with `file` field
- Session duration > 30min with < 5 events (unusual pattern)

### Command Interface

```bash
# User-facing: interactive tag workflow
tt suggest <stream-id>           # Interactive: shows suggestions, prompts for action
tt suggest <stream-id> --json    # Non-interactive: outputs suggestions as JSON

# Internal (called by tt suggest)
tt summarize --session=<id>      # Raw LLM summarization on remote
```

### Interactive Flow

When user runs `tt suggest <stream-id>`:

```
$ tt suggest abc123
Analyzing session abc123 (tmux/dev/session-2)...

Suggested tag: acme-webapp
Confidence: high
Reason: cwd=/home/user/projects/acme-webapp, edited 12 files in src/

Apply this tag? [Y/n] y
Tagged stream abc123 as "acme-webapp"
```

For partial failures (stream has multiple sessions, some fail):

```
$ tt suggest abc123
Analyzed 3/5 sessions (2 failed - remote timeout)
Suggested tag: acme-webapp (based on 3 successful analyses)

Apply this tag? [Y/n]
```

### Report Integration

When metadata-only inference has low confidence, reports show the reason and offer guidance:

```
(untagged)                                1h 15m
  Sessions:
    abc123  tmux/dev/session-2        (45m)  cwd: /home/user
    def789  tmux/staging/session-1    (30m)

  Tip: Know the project? Run 'tt tag <id> <project>'
       Need help? Run 'tt suggest <id>' to analyze session content
```

The `cwd: /home/user` explains why inference failed (generic directory).

### Error Handling

| Error | Message | Recovery |
|-------|---------|----------|
| Remote unreachable | `Error: Cannot reach remote 'devserver' for analysis.` | `Hint: Check connectivity with 'ssh devserver' or tag manually` |
| API key missing | `Error: Remote 'devserver' does not have Claude API key configured.` | `Hint: Set ANTHROPIC_API_KEY on devserver, or tag manually` |
| LLM returns nothing | `Could not determine project from session content.` | `Hint: Tag manually with 'tt tag abc123 <project>'` |
| Stream not found | `Error: Stream 'abc123' not found.` | `Hint: Use 'tt streams' to see available stream IDs.` |
| No remote sessions | `Error: Stream abc123 has no remote sessions to analyze.` | `Hint: This stream only contains local events. Tag manually.` |
| API error (rate limit, 5xx) | `Error: Claude API error: <message>. Retry later.` | Exponential backoff for retries; after 3 failures, give up |

### API Failure Handling

`tt summarize` must:
1. **Validate response schema**: Return error if response is not `{"tags": [...], "summary": "..."}`
2. **Retry transient errors**: 3 retries with exponential backoff for 429, 5xx
3. **Return typed errors**: Distinguish "unreachable", "api_error", "invalid_response", "context_too_long"
4. **Graceful degradation**: If summarization fails, `tt suggest` falls back to metadata-only with warning

### API Key Requirement

Remote summarization (`tt summarize`) requires a Claude API key configured on the remote machine. This is documented as a setup requirement for users who want on-demand analysis.

**Alternative considered**: Route summarization through local (fetch content via SSH, summarize locally, discard). This trades some privacy for simpler key management. The privacy is nearly equivalent (content exists temporarily in memory on either machine). Deferred for MVP—current design is adequate. Can revisit if API key management proves burdensome.

### Summary Caching

For MVP, always re-call LLM on `tt suggest`. Caching adds complexity (where to store? expiration?).

**Note**: If users frequently re-run `tt suggest` due to accidental dismiss or uncertainty, consider adding `--cache` flag that stores the result alongside the stream. When caching is added, invalidate on: event count change, explicit refresh request (`--refresh`).

### Stream-to-Session Mapping

`tt suggest <stream>` operates on streams, but remote summarization is per-session.

**Session discovery**: Sessions are identified by extracting unique `session_id` values from `agent_*` events in the stream.

**Multi-session aggregation**: If a stream has multiple sessions:
1. Call `tt summarize` for each session
2. Merge tag suggestions (union of all suggested tags)
3. If sessions suggest conflicting tags, present all with confidence indicators

**Non-agent streams**: If stream has only `tmux_pane_focus` events (no agent sessions), `tt suggest` falls back to metadata-only inference with message: "No agent sessions to analyze. Suggestion based on file paths and working directories."

### Multi-CWD Sessions

Agent sessions can `cd` to multiple directories. When a session's events span multiple cwds:
- The session belongs to whichever stream its events were assigned to during stream inference
- Stream inference uses the **first** or **most frequent** cwd for clustering
- `tt summarize` sees the full session content regardless of which stream the events were assigned to
- Summary may reference multiple directories; this provides richer context than metadata alone

### Large Session Handling

Sessions with extensive logs may exceed Claude's context window.

**Strategy**: Truncate to most recent N events (configurable, default 1000). Include:
- Session start/end events (always)
- Last N tool_use events
- Last N user_message events

If context still exceeded, return error: `Session too large for analysis. Tag manually.`

### Concurrency

`tt suggest` may be called while `tt sync` is running:

**Design**: `tt suggest` operates on a snapshot of the stream at command start. If `tt sync` imports new events during analysis:
- The suggestion is based on the snapshot (slightly stale)
- No locking required; SQLite handles concurrent reads
- If stream_id becomes invalid (deleted during reorganization), fail gracefully: `Stream changed during analysis. Re-run tt suggest.`

This is acceptable because:
- Suggestions are advisory, not authoritative
- User can re-run if results seem stale
- Avoiding locks keeps the system simple

### Security Note

Summaries may inadvertently contain sensitive information despite prompting the LLM to exclude secrets. The summarization prompt includes: "Do not include secrets, API keys, passwords, or sensitive data in the summary."

**Not guaranteed**: LLM prompts are best-effort. Summaries are transmitted from remote to local and may be displayed in CLI output. Users should be aware that sensitive information could leak through summaries.

Future consideration: Post-process summaries to detect and redact common secret patterns.

## Non-Goals

This ADR does not decide:
- Tag taxonomy or hierarchy
- Multi-remote scenarios (multiple dev servers)
- Summary schema details (prose vs structured)
- Caching strategy for summaries
