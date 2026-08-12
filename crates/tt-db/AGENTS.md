# tt-db — SQLite Storage Layer

Single-file monolith (`src/lib.rs`). All database types and methods live here, plus the `allocate_for_period` allocation entry point.

## Schema (v14)

`init()` migrates supported older versions forward: v8–v13 → v14 via additive `ALTER TABLE` or `CREATE TABLE IF NOT EXISTS` statements inside one transaction (v8 adds `window_app_id`/`window_title`; v≤9 adds `streams.slug`; v≤10 adds `streams.description` + `streams.color`; v≤12 adds `proposals.classifier_generation`; v≤13 adds `pane_session_bindings`). Any other version mismatch (newer-than-expected, or an unsupported older version) = `DbError::SchemaVersionMismatch` (hard error). New tables are declared `CREATE TABLE IF NOT EXISTS`, so they appear on both fresh init and forward migration. To evolve: bump the `SCHEMA_VERSION` constant, add the columns to the `CREATE TABLE`, and add a migration arm in `init()`.

Because the migration `ALTER`s tables rather than creating them, a fixture standing in for an older version has to carry every table the arm touches. **But an arm may only assume what the version guarantees, not what a typical database happens to hold.** The v≤12 arm ALTERed `proposals` unconditionally, and a capture-only machine never runs the classifier so nothing there ever creates that table: devbox sat at schema 10 with six tables and no `proposals`, the migration aborted, and `tt` could not open the database at all. Every migration fixture built a `proposals` table, which is exactly why none of them caught it. The arm now probes `sqlite_master` first and skips the ALTER when the table is absent — the `CREATE TABLE IF NOT EXISTS` block runs after the match and declares the column, so a missing table is created complete rather than patched. `a_capture_only_machine_with_no_proposals_table_still_migrates` pins it.

### Tables

```sql
events (id TEXT PK, timestamp TEXT, type TEXT, source TEXT, machine_id TEXT, schema_version INT,
        cwd TEXT, git_project TEXT, git_workspace TEXT, pane_id TEXT,
        tmux_session TEXT, window_index INT, status TEXT, idle_duration_ms INT,
        action TEXT, session_id TEXT, stream_id TEXT FK, assignment_source TEXT,
        window_app_id TEXT, window_title TEXT)

streams (id TEXT PK, created_at TEXT, updated_at TEXT, name TEXT, slug TEXT,
         description TEXT, color TEXT, time_direct_ms INT, time_delegated_ms INT,
         first_event_at TEXT, last_event_at TEXT, needs_recompute INT)

stream_tags (stream_id TEXT, tag TEXT, PK(stream_id, tag), FK stream_id)

agent_sessions (session_id TEXT PK, source TEXT, parent_session_id TEXT,
                session_type TEXT, project_path TEXT, project_name TEXT,
                start_time TEXT, end_time TEXT, message_count INT,
                summary TEXT, user_prompts TEXT, starting_prompt TEXT,
                assistant_message_count INT, tool_call_count INT, machine_id TEXT)

meta (key TEXT PK, value TEXT)          -- seeded with 'db_version'=0 and 'classifier_state'=ready

proposals (id TEXT PK, created_at TEXT, session_id TEXT, event_ids TEXT,
           proposed_stream_id TEXT, proposed_new_stream TEXT,
           confidence REAL, reasoning TEXT, status TEXT DEFAULT 'pending',
           classifier_generation INT)   -- newest classifier that answered it; NULL predates the column

classified_sessions (session_id TEXT PK, classified_at TEXT,
                     prompt_count INT, rechecked INT)   -- one follow-up re-check per session

machines (machine_id TEXT PK, label TEXT, last_sync_at TEXT, last_event_id TEXT)

pane_session_bindings (machine_id TEXT, pane_id TEXT, session_id TEXT, observed_at TEXT,
                       PK(machine_id, pane_id, observed_at))
```

Timestamps: ISO 8601 TEXT (`2024-01-15T10:30:00.000Z`), always UTC, millisecond precision. Lexicographic order = chronological order.

**`streams.first_event_at` and `streams.last_event_at` are dead columns. Nothing writes them, and nothing may read them to answer a question.** They are a denormalized cache of `MIN(timestamp)`/`MAX(timestamp)` over that stream's `events`, filled by the deleted `--apply` engine and never since: `update_stream_times` — the only stream writer `tt recompute` calls — sets `time_direct_ms`, `time_delegated_ms`, `updated_at` and `needs_recompute` and nothing else, `mint_stream_in` inserts NULL, and `insert_stream` merely persists what the caller passed, which is `None` at every call site in the tree. Measured on the live table: **985 of 1,245 streams have both NULL**, the 260 that do not were all created in 2026-03 and 2026-04, and their newest value is **2026-04-30 — 99 days behind the newest event**.

Reading one is not a stale-data risk, it is a wrong answer. `tt streams list` filtered its seven-day window on `last_event_at` and printed **"No streams with activity in the last 7 days."** on a database where 90 streams had events in that window and `tt report` showed 75h of direct time. Both readers now read `events` — `get_streams_for_display` through `stream_activity_windows`, and `streams_in_range` through its own aggregate — so the columns are unread by any code that concludes anything.

They are **kept in the schema deliberately**. Dropping a column means a table rebuild plus a `SCHEMA_VERSION` bump plus a migration arm, and a version bump hard-fails every machine's `tt` with `SchemaVersionMismatch` until each is redeployed — which has already happened here for real, when devbox sat at schema 10 and could not open the database at all. That is a large blast radius for deleting two columns nothing reads, and the additive-only rule points the same way. `Stream` keeps the two fields for the same reason `row_to_stream` still selects them: the row round-trips faithfully into the daemon's JSON, where `web/src/lib/types.ts` declares them and no component reads them. The guard against a future reader is the doc comment on each field, not their absence.

**A timestamp that cannot be read is an error, never a substitution.** `row_to_stream` used to log a warning and hand back `Utc::now()` for an unparseable `streams.created_at`/`updated_at`, which turned a data defect into wrong data that reads as correct and re-dated the stream on every read. `parse_stream_timestamp` now fails instead, naming the stream, the column, and the offending text.

That is safe because the one unreadable shape that ever reached the table is repaired first. Four streams (`office-admin-2026w14`, `oc-voice-ce`, `startup-credits-2026w14`, `personal-2026w14`) held a `created_at` like `2026-03-04 14:32:13` — SQLite's `CURRENT_TIMESTAMP` shape, a real creation time missing only its timezone, which is why `parse_from_rfc3339` gave up with "premature end of input". All four carry hand-written slug ids and nothing in `tt` can produce that shape (`insert_stream` formats through `format_timestamp`; `tt sync` copies events but never streams), so `normalize_stream_timestamps` reads them as UTC, rewrites them as RFC 3339, and logs each repair at `info`.

Three properties of that repair are deliberate. It is **recorded in `meta`** (`stream_timestamps_normalized`) rather than gated on `SCHEMA_VERSION`: the table's shape is unchanged, so this is not a schema migration, and a version bump would hard-fail every machine's `tt` binary with `SchemaVersionMismatch` until each was redeployed — a large blast radius for repairing four rows. It **does not bump `db_version`**, because a creation time feeds no attribution and no verdict, so signalling the daemon would buy only a spurious recompute. And it **repairs exactly one shape**: a value it cannot read is left alone and warned about, so the read path refuses it. Guessing at a second shape, or deriving a creation time from the wall clock, is the defect being removed.

**`meta.session_scan_cursor`** is the other non-schema key worth naming here: an RFC 3339 instant recording how far session ingest has read. `tt ingest sessions` reads it back minus a safety overlap and uses it as the `since` bound for both transcript stores, so the daemon's ~30s tick re-derives only what changed instead of a 6.7 GB corpus. Three rules travel with it. It needs **no `SCHEMA_VERSION` bump** — `meta` already exists, and bumping would hard-fail every machine's `tt` with `SchemaVersionMismatch` until redeployed. It **must not bump `db_version`**: a cursor changes no event, stream, or assignment, so signalling the daemon would fire its 2s watcher on every ingest tick for nothing. And a value that **cannot be parsed is an error**, not a substitution — reporting it as absent would silently restore the full scan the cursor exists to avoid, and substituting `now()` would skip every session written before this moment. The rule for *when* it may advance lives with its only writer; see root `AGENTS.md`, "A scan that could not read the store is not a scan that found nothing".

### Indexes

`idx_events_timestamp`, `idx_events_type`, `idx_events_stream`, `idx_events_cwd`, `idx_events_session`, `idx_events_git_project`, `idx_events_machine`, `idx_streams_updated`, `idx_streams_slug` (unique), `idx_stream_tags_tag`, `idx_proposals_status`, `idx_agent_sessions_start_time`, `idx_agent_sessions_project_path`, `idx_agent_sessions_parent`, `idx_pane_session_bindings_lookup`

## `db_version` — the daemon's change signal

`meta.db_version` is a monotonic counter the `tt-serve` daemon polls every 2s to detect writes from other processes. The contract — **do not break it**:

- **Attribution-visible mutations** (event insert/assign, stream create/update/time, tag edits, proposal accept/reject, …) call the private `bump_db_version_in_transaction` *inside their own transaction* and *only when rows actually changed* (e.g. `if count > 0`).
- **Bookkeeping writes must NOT bump**: classifier health (`record_classifier_*`) and recheck marks (`mark_rechecked`) are explicitly documented "without changing `db_version`". Bumping on these would make the daemon reprocess on every health tick.
- `get_db_version()` reads it; `bump_db_version()` is the standalone bump.

## Allocation entry point

`allocate_for_period(db, start, end, period_end, config) -> AllocationResult` (free function in `lib.rs`) is **the only permitted time-allocation entry point outside `tt-core`** — it streams the half-open window into `tt_core::allocation::Allocator`, rather than collecting it, and carries the sole `#[expect(clippy::disallowed_methods, …)]`. **`end` is exclusive**: an event at exactly `end` belongs to the next period (internally it queries `start..=end − 1ms`).

## Key Types

- `Database` — wraps `rusqlite::Connection`. `Send` but not `Sync`.
- `StoredEvent` — implements `tt_core::AllocatableEvent`
- `Stream` — work unit with computed time fields + `description`/`color`
- `ActivityWindow` — the `first`..`last` span a stream's events cover; the classifier roster's ordering key
- `Proposal` / `ProposalStatus` / `AcceptProposalOutcome` — a pending classifier suggestion (session/events → proposed existing or new stream, confidence, reasoning, status) and the result of accepting one. `ProposalStatus` also carries `Superseded`: a later classifier verdict answered the same target confidently and applied it, so the queued question is spent. Deliberately not `Rejected`, which is a human verdict `has_rejected_proposal` reads to suppress future answers. `Proposal.classifier_generation` is the newest `tt_llm::CLASSIFIER_GENERATION` that has answered this question — written when the proposal is filed and rewritten when a later pass re-answers it, so it names the classifier the queue currently reflects rather than the one that first asked
- `RankedProposal` — a pending proposal paired with `attention_events`, the count of its `user_message` / `window_focus` / `tmux_pane_focus` events. Counted in events rather than milliseconds because a proposal is answered before its events are allocated, and allocation needs the stream the reviewer has not supplied yet
- `DissolveMode` / `DissolveOutcome` — whether a dissolution commits or rolls back, and its released/retained/retired counts
- `ReleaseMode` / `ReleaseOutcome` — whether a pane-focus release commits or rolls back, and its released/retained/streams-affected counts
- `PaneSessionBindingBackfillOutcome` — historical pane focuses that gained a recent observed session identity, plus human assignments retained untouched
- `MergeMode` / `MergedSource` — whether a merge commits or rolls back, and per source what it moved: `events_moved`, `user_events_moved`, `tags_moved`, `retired`
- `JunkRoutingOutcome` — what one bulk junk-routing step settled: `sessions` routed and `events` moved. Two counts rather than one because the per-session junk path moves both `junked` and `assigned`, and a pass summary folding in only the first would understate the pass by exactly the work the bulk step took over
- `ClassifierHealth` / `ClassifierHealthState` — persisted classifier state (`Ready` / `Unconfigured` / `Failing`) + consecutive-failure backoff
- `Machine` — a known remote machine with sync state
- `DbError` — `Sqlite(rusqlite::Error)` | `SchemaVersionMismatch { found, expected }` | `MergeIntoSelf` / `MergeTargetNotFound` (a merge naming one stream twice, or a target that no longer exists)
- `SourceStatus` — last event timestamp per source, over every machine
- `LocalEventTypeStatus` — last event timestamp per event type, over this machine only

## Method Reference

### Events
| Method | Purpose |
|--------|---------|
| `insert_event` / `insert_events` | Idempotent insert (`INSERT OR IGNORE`); bumps `db_version` when rows land |
| `get_events` | All events, optional time_after/time_before filters |
| `event_time_bounds` | `MIN`/`MAX` of `events.timestamp`, or `None` when empty. What `recompute` used to derive by materializing all 2.7M events. Reads the raw table, so unlike `get_events` it does not drop rows `row_to_event` cannot parse |
| `get_events_in_range` | Collects events between start..end (inclusive). Do not use for allocation: its `Vec` materializes the window |
| `for_each_event_in_range` | Streaming counterpart of `get_events_in_range`: visits each row in ascending timestamp order without collecting. The allocation pass walks forward only, so it never needed the `Vec` |
| `sessions_spanning_multiple_streams` | Sessions whose events point at more than one stream, with those streams. A data-integrity report, ordered for stable output |
| `sessions_with_tool_use_but_no_start` | Sessions with `agent_tool_use` in a window but no `agent_session`/`started` in it — the query form of a pre-pass that used to run over a materialized `Vec` |
| `get_events_by_stream` / `unassigned_event_ids` / `count_events_by_stream` | By stream / the unassigned, **ids only** / how many point at one stream, whoever assigned them. `unassigned_event_ids` deliberately returns ids rather than rows: as `get_events_without_stream` it was the only whole-table scan of unassigned events, and cwd inference read each row's `cwd` straight out of it. See root `AGENTS.md`, "A folder is not a project" |
| `get_last_event_per_source` | Latest timestamp per source name, across **every** machine. Answers "is anything reporting", not "is this machine's watcher alive" — a synced remote's healthy watcher shares the `local.cosmic` source string and makes the type look current |
| `last_local_event_per_type` | Latest timestamp per event type over the rows **this machine** produced, ordered by type. "This machine" is derived from the data: `machines` holds only remotes registered by `tt sync`, so a `machine_id` absent from it (NULL rows included) was produced here — reading `machine.json` would put the answer outside the database and out of reach of an in-memory test. A type this machine never produced is **absent rather than old**, which is what lets its caller tell silence from absence. See root `AGENTS.md`, "A dead input must announce itself" |
| `delete_events_by_machine` | Drop a remote's events |
| `delete_non_user_message_events` | Drop `user_message` rows for sessions reclassified away from `user` |
| `prune_user_message_events` | Retire `user_message` rows the current extractor no longer derives, given a per-session keep-set. Sessions absent from the keep-set are untouched — a caller can only speak for transcripts it holds |

### Streams
| Method | Purpose |
|--------|---------|
| `insert_stream` | Create new stream. Stores the name [normalized](../tt-core/AGENTS.md) (trimmed, internal whitespace collapsed), so no row can be born carrying a name only whitespace tells apart from another's. Normalizing is *all* it does — it takes an id the caller already chose, so it has no way to report a reuse |
| `get_stream` / `get_streams` / `get_stream_by_slug` | Retrieve by ID, all, or slug |
| `stream_exists` | Whether a stream id names a live row, by **id only** (`STREAM_EXISTS_SQL`). The mandatory guard before writing any externally supplied id onto an event — `events.stream_id` is a foreign key, and a stale classifier roster otherwise aborts the whole pass |
| `junk_stream_id` | Resolve the reserved junk stream (slug `JUNK_STREAM_SLUG`), creating it on first use. Junking routes rather than deletes, so `tt streams dissolve junk` reverses a rule that starts eating real work |
| `set_stream_slug` / `set_stream_description` / `rename_stream` | Set stable slug / description / display name. `rename_stream` normalizes like `insert_stream` — the column's invariant has to hold whichever path wrote it, and here a whitespace variant would also defeat the `merge_streams` step the rename exists to set up. Names carry no uniqueness constraint, so two streams may legitimately share one mid-repair |
| `streams_in_range` | Streams whose events span into a time range, ordered by earliest event. The span is aggregated from `events`, **not** from `streams.first_event_at`/`last_event_at` — those columns name this exact quantity and nothing writes them. A stream with no events has no span and is excluded, which is the only shape a missing span can take once it is read from `events`: MIN and MAX are absent together |
| `resolve_stream` | Find by ID, slug, or exact name |
| `find_stream_by_normalized_name` | The stream carrying a name once **both sides are normalized**. The authoritative reuse check, and the reason the classifier's roster may be capped at all: a name the model proposes for a stream it was never shown becomes reuse of that row, so the cap can only ever cost a *semantically* near-duplicate. Reads the table rather than a caller's roster snapshot, which is loaded once per pass and goes stale — two rows named `agent-c: eval-3 traccar environment (eval-3 integration)` were minted eleven minutes apart. Normalizes the stored side too, because rows predating the write-side invariant still carry whitespace and are the rows most in need of being found. Several rows may share a name; the **earliest created** wins, so successive passes converge on one row instead of alternating |
| `stream_activity_windows` / `stream_activity_window` | The period each stream (or one stream) has been active over, as an `ActivityWindow`. The classifier roster's ordering key and `tt streams list`'s recency filter, and a *period* rather than a timestamp because one timestamp cannot express "was already underway when this session ran" — the strongest reuse signal there is. Read from `events`, **not** from `streams.first_event_at`/`last_event_at`: **nothing writes those** (an earlier revision of this line said `tt recompute` did, and that was wrong — `update_stream_times` never touches them), so 985 of the live table's 1,245 streams have them NULL and the other 260 are 99 days stale. A stream with no events is absent rather than present-and-old. An unreadable timestamp warns and yields `None`, dropping the stream to the roster's tail — this key orders a presentation, so failing the read would cost a whole pass its classifications to mis-sort one row |
| `assign_event_to_stream` / `assign_events_to_stream` | Set stream_id + source on named events, unguarded. The caller supplies every id, so these carry no `'user'` protection |
| `assign_events_by_session_id` / `assign_events_by_ids` | Set stream_id + source in bulk, **skipping `assignment_source = 'user'`**. The machine writers' entry point |
| `reassign_session_as_user` | Record a human's verdict over a whole session, overwriting *every* source including an earlier `'user'` — a human must be able to change their own mind. `'user'` is hardcoded, not a parameter, so this cannot be reused as an inference primitive. The only caller is `tt streams assign` |
| `inherit_stream_for_session` | Give a subagent the stream its parent resolved to. Claims events that are unassigned *or* already `inherited`, so a subagent follows a reclassified parent; every other `assignment_source` is a verdict about that session and is never overridden |
| `claim_unassigned_events_for_classified_sessions` | Give an unassigned event the stream of the classified session it belongs to. The half the pane-focus stamp was missing: the classifier claims a session's events *at classification time*, so an event that acquires that `session_id` afterwards was reached by nothing. Reads the stream from the session's **own assigned events** — `classified_sessions` never records which stream it landed on — and **skips a session whose assigned events name more than one stream**, picking no winner and breaking no tie. Claims only `stream_id IS NULL`, strictly narrower than skipping `'user'`. Source hardcoded `session_membership`: not `inferred` (nothing here judges content) and not `inherited` (that is a subagent following its parent, and gets re-pointed). See root `AGENTS.md`, "A stamped session id is worthless until something claims it" |
| `delete_orphaned_streams` | Remove streams with no events |
| `dissolve_stream` | Release a stream's non-`user` events back to unassigned, then retire it. `DissolveMode::DryRun` runs the same statements and rolls back, so a preview reports the counts a real run would produce |
| `release_unattributable_pane_focus` | Release the attribution carried by `tmux_pane_focus` rows with a NULL `session_id` — the one population no writer here can select, since every session-keyed writer filters on `session_id = ?` and the only id-keyed one is fed from a `type = 'window_focus'` query. A stream on such a row came from the deleted cwd propagator, which wrote `'inferred'`, so it cannot be found by `assignment_source`. Selection is hardcoded (`UNATTRIBUTABLE_PANE_FOCUS_SQL` + `RELEASABLE_ATTRIBUTION_SQL`) so this cannot become a bulk-release primitive. Both columns go NULL, `'user'` is never touched, no row is deleted, no stream is retired. `ReleaseMode::DryRun` rolls back. See root `AGENTS.md`, "Deleting the propagator did not undo it" |
| `backfill_pane_session_bindings` | Restore a historical `tmux_pane_focus` row's `session_id` from the newest process-tree-observed identity for its exact machine and pane, only when that observation is within the 30-minute agent timeout. It writes neither `stream_id` nor `assignment_source`; the existing session-membership pass remains the sole stream writer. Human assignments are skipped, no event row is deleted, and `ReleaseMode::DryRun` rolls back. |
| `merge_streams` | Re-point every source's events at one target, move its tags via `INSERT OR IGNORE`, retire the emptied sources. The counterpart to `dissolve_stream`, and the opposite on one point: **`user` assignments move too**, because a merge corrects which row holds the work, not the human's verdict about what it was. All sources in one transaction; no event row is ever deleted; the target is marked `needs_recompute` with `updated_at` left alone. `MergeMode::DryRun` rolls back, so a preview reports a real run's counts. Errors with `MergeIntoSelf` / `MergeTargetNotFound` rather than letting a stale id surface as a foreign-key failure mid-write |
| `update_stream_times` | Set direct/delegated ms, `updated_at` and `needs_recompute = 0`. **Not** `first_event_at`/`last_event_at` — it never has, and `tt_core::StreamTime` carries no such fields to write |
| `mark_streams_for_recompute` / `get_streams_needing_recompute` | Recompute flagging |

### Tags
| Method | Purpose |
|--------|---------|
| `add_tag` / `delete_tag` | Idempotent add / remove |
| `get_tags` / `get_all_tags` / `get_streams_with_tags` | Per-stream / all / joined |

### Proposals & classification bookkeeping
| Method | Purpose |
|--------|---------|
| `insert_proposal` / `get_proposals` / `set_proposal_status` | Create / list (by `ProposalStatus`) / update status |
| `pending_proposals_by_attention` | The review queue, ordered by the attention answering each item would resolve, then `created_at ASC` and the id so equal attention is stable. A **second view** rather than a change to `get_proposals`, whose `created_at ASC` other callers depend on. Measured on the live queue of 477 pending proposals, the 12 resolving the most attention hold 41.8% of all attention in it, so age-ordering spends a reviewer at random. A **re-ordering and never a filter**: zero-attention proposals sort last rather than vanish, and `PROPOSAL_IS_ANSWERABLE_SQL` is deliberately *not* applied — `tt proposals ls` is where a reviewer learns a stranded row exists. Reads only; ranking a queue writes nothing. See root `AGENTS.md`, "A review queue is spent attention" |
| `accept_proposal` | Confirm a proposal's assignments (→ `AcceptProposalOutcome`). A proposal naming a *new* stream **reuses an existing row already carrying that normalized name** rather than minting a second one — this is the other path that mints a stream from a model-authored name, and a proposal sits in the queue for as long as a human takes to review it while the classifier keeps running. Reusing is what accepting means here: the human agreed the work belongs under that name, not that a fresh row must exist. `created_stream` reports which happened, and the proposal's tags arrive either way |
| `get_pending_proposal_for_events` / `has_pending_proposal_for_session` | The duplicate guard, one per proposal scope: a target whose answer is already waiting on a human must not queue a second copy of the same question on every pass. Neither holds its candidate *back* — selection stopped reading proposals when a queue nobody reviewed was found to have frozen 194 sessions out of every later pass. Both apply `PROPOSAL_IS_ANSWERABLE_SQL`: a proposal naming a dissolved stream suppresses nothing, because no reviewer can accept it |
| `supersede_pending_proposals_for_session` / `_for_events` | Retire the proposals waiting on one target, a later verdict having answered it. Status `superseded`, **never `rejected`** — that is a human verdict `has_rejected_proposal` reads, and manufacturing one would silence the classifier on that target for good. Wider than the guard above and on purpose: a stranded proposal must not suppress a fresh answer, but a verdict that answers it leaves nothing to strand. Bookkeeping: never bumps `db_version`, because it changes no assignment of its own and only ever follows one |
| `has_pending_proposal_for_events_at_generation` | The gate that stops a bounded pass paying to re-ask a window run it has already answered. Keyed on the generation rather than on mere existence, so bumping `tt_llm::CLASSIFIER_GENERATION` re-opens every queued question at once; a `NULL` generation matches nothing and is therefore re-asked. Applies `PROPOSAL_IS_ANSWERABLE_SQL` like every other suppression read. See root `AGENTS.md`, "Re-asking is conditional" |
| `stamp_pending_proposals_for_session` / `_for_events` | Record that a generation has now answered the questions waiting on one target, without touching status, stream, confidence or reasoning. The write the gate above reads, and what makes a generation bump cost one pass over the queue rather than an unbounded number. Not a verdict of any kind, and above all not a rejection. Bookkeeping: never bumps `db_version`. No answerability filter, matching the supersede pair — narrowing would leave a row misstating who last looked at it |
| `has_rejected_proposal` / `has_rejected_new_stream_proposal` | Avoid re-proposing rejected work |
| `unclassified_user_sessions` | The user sessions a bounded pass should spend LLM calls on: `session_type = 'user'`, at least one event with no stream, `ORDER BY start_time DESC`, capped at `limit`. Returns each session with its machine. Reads no proposals — see root `AGENTS.md`, "A proposal escalates a question" |
| `route_structurally_junk_sessions` | Settle the user sessions `tt_core::is_structurally_junk` judges worthless, in one transaction, **before** a pass spends its bounded budget selecting candidates. Assigns their events to the junk stream, records each in `classified_sessions`, and lets their subagents inherit the junk stream — the same three writes the per-session path performs, hoisted ahead of selection because junk costs no model call yet was taking one of `SESSIONS_PER_PASS`'s 200 slots. Routes, never deletes, and claims only `stream_id IS NULL`, which is strictly narrower than skipping `'user'`. Its private `structurally_junk_sessions` selection is a bounded pre-filter, not a second rule: every row is re-checked against `is_structurally_junk` itself, so the SQL and the function cannot drift. Bumps `db_version` only when rows moved. See root `AGENTS.md`, "Structurally junk sessions are routed in bulk before selection" |
| `subagent_ids_for_parent` / `orphan_subagent_ids` | Children of one session / subagents still holding unassigned events whose parent was never indexed |
| `get_recheck_candidates` / `mark_rechecked` | One follow-up re-check once a session gains prompts (no `db_version` bump) |

### Classifier health
| Method | Purpose |
|--------|---------|
| `get_classifier_health` | Read persisted state (never bumps `db_version`) |
| `record_classifier_success` / `_failure` / `_unconfigured` / `_ready` | Update health + backoff (never bumps `db_version`) |

### Agent sessions & machines
| Method | Purpose |
|--------|---------|
| `upsert_agent_session` | Insert or update session metadata |
| `agent_sessions_in_range` / `get_agent_session_start_events` | Sessions overlapping a range / their start events |
| `upsert_machine` / `upsert_machine_with_sync_time` / `list_machines` | Register + list remotes |
| `get_machine_last_event_id_by_label` / `get_machine_last_sync_at_by_label` / `get_latest_event_id_for_machine` | Per-remote incremental-sync cursors |
| `get_session_scan_cursor` / `set_session_scan_cursor` | Read / record how far session ingest has scanned. Bookkeeping: never bumps `db_version`. An unparseable stored value is an error, never a substituted time |

## Thread Safety

`Database` is `Send` (movable between threads) but NOT `Sync` (no shared access). The `tt-serve` daemon uses **separate instances per unit of work** — a fresh `Database` opened inside each `spawn_blocking` closure and dropped before any `.await` — rather than an `Arc<Mutex<Database>>` or a pool. Follow that pattern; see root `AGENTS.md` Anti-Patterns.

## Testing

Use `Database::open_in_memory()` for all tests. Helpers: `make_event(id, timestamp, event_type)` returns `StoredEvent` with sensible defaults; `db_with_assigned_stream` and `db_for_merge` build a stream per `assignment_source` so attribution moves can be asserted per source. 140+ unit tests cover field persistence, idempotency, range queries, cascading deletes, the `db_version` bump contract, and schema-version checks.
