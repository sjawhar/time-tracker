# Integrations

## Problem Statement

Time Tracker needs a clear, local-first integrations story beyond MVP. We must define where events come from, how derived data is exported, and how future extensions (rules, webhooks, API server) fit without violating event-sourcing or privacy expectations.

## Principles

- **Local-first by default**: integrations should work without external services.
- **Event-sourced**: raw events are append-only truth; derived outputs are recomputable.
- **Privacy-by-design**: path and metadata filters apply before any export.
- **Minimum viable surface area**: describe extensibility without committing to premature API compatibility.

## Research Findings (External)

- **ActivityWatch** provides a local REST API and export endpoints, emphasizing local-only access as the default security model.
- **WakaTime** exposes privacy controls such as include/exclude path patterns and path/name obfuscation.
- **Toggl** represents running entries with negative duration and expects explicit start/stop times.
- **Clockify** requires UTC timestamps and rejects local time sent as UTC.
- **Webhook best practices** (Stripe, GitHub) stress HMAC signatures, timestamp tolerance, constant-time comparison, and fast 2xx responses.

## Proposed Approach

### Input Integrations (Event Sources)

**Current sources (MVP and earlier)**:
- tmux focus changes
- Agent session logs
- Git hooks
- Manual input

**Future collectors** should emit the same `Event` shape. Each collector is responsible only for producing raw events. No collector writes derived entries.

### Privacy Filter Layer

Introduce a single, optional privacy filter stage that runs **before export or external exposure**:

- **Include/exclude patterns** for paths and tool metadata.
- **Obfuscation options** for project folder names, file paths, branch names.
- **No per-tool configuration** initially; one global filter is applied uniformly.

The filter never modifies stored raw events. It only transforms export outputs.

### Output Integrations

Primary output is **local export + local API**. External API integrations are explicitly deferred.

1. **Export Formats**
   - JSONL and JSON exports for events and derived time entries.
   - Export always in UTC timestamps.

2. **Local API (Deferred implementation)**
   - Read-only endpoints for events, streams, and time entries.
   - Append-only endpoint for custom event ingestion.
   - Bound to localhost by default; explicit opt-in required for remote access.

3. **External APIs (Document mapping only)**
   - **Toggl**: map time entries to start/stop/duration; note running-entry convention (negative duration).
   - **Clockify**: ensure UTC normalization and explicit timestamps.
   - These mappings are descriptive only; no implementation in MVP.

### Webhooks (Post-MVP)

If added, webhooks are delivery-only (no ingestion) and must enforce:

- HMAC signature with shared secret
- Timestamp tolerance to prevent replays
- Constant-time signature comparison
- Fast 2xx response and async processing
- TLS-only delivery

### Rules Engine (Post-MVP)

Rules are a deterministic, declarative mapping from event attributes to tags/projects. The rules engine **must not mutate raw events**; it augments derived time entries at query/export time.

High-level shape (illustrative, not final):

```yaml
rules:
  - match:
      path: "~/work/acme/*"
    assign:
      project: "acme"
      tags: ["client"]
```

Ordering is first-match or explicit priority; conflicts are resolved deterministically.

### API Server (Post-MVP)

A local HTTP server that exposes:

- Read APIs for events, streams, time entries
- Append-only write API for custom events
- Explicit enablement for non-localhost bindings

## Edge Cases & Failure Modes

- **UTC mismatch**: external APIs reject local timestamps; export must normalize to UTC.
- **Partial exports**: export jobs interrupted mid-stream should resume from last event ID.
- **Privacy leaks**: filter must run before any export or API response.
- **Running entry mapping**: external APIs with negative duration require careful conversion.
- **Large exports**: should stream outputs instead of materializing full datasets.

## Acceptance Criteria

- Spec defines input, output, and extension points with local-first defaults.
- Privacy filter is specified as pre-export transform without mutating stored events.
- External API mappings are documented (Toggl, Clockify) but clearly deferred.
- Webhook security requirements are explicit and consistent with best practices.
- Rules engine and API server are scoped as post-MVP with deterministic behavior.
- All timestamps in integration outputs are defined as UTC.
