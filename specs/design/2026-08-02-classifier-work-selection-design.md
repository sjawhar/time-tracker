# Classifier Work Selection

How `tt classify --auto` decides *what* to send to an LLM, *what to discard without asking*, and *what the LLM sees* when it is asked.

Companion to `2026-07-24-attribution-engine.md`, which covers the classification mechanism itself (proposals, confidence thresholds, stream assignment). This document covers the input side.

## The problem

Attribution quality is bounded by two things: whether the classifier gets to a session at all, and whether it sees enough of that session to judge it. Both were failing.

Session selection is `SELECT DISTINCT session_id ... ORDER BY session_id`. Session IDs (`ses_13e858295ffeLmBKFh6j2yiz1Y`) are hash-like, so this is arbitrary order — the reporting window has no priority over sessions from five months ago. At a measured ten LLM calls per minute, a full pass over the 38,990 unclassified sessions takes about 65 hours, and today's work is not reached preferentially at any point during it.

The result is that recent days are the *least* attributed days: 22% on 2026-07-27 and 35% on 2026-07-28, against 97–99% for days a week older. Reports for the day you actually care about are computed from a third of the data.

## Principles

**Junk it before asking.** If a session can be confidently judged unattributable by structure alone, discard it without an LLM call. Confidence is the bar — a rule that is merely usually right belongs in the LLM's hands, not in a `WHERE` clause.

**Recency is the priority signal.** The reporting window is what gets read. Backfill is opportunistic.

**Denylist injections; never allowlist by shape.** Automated text injected into a session is a small, closed set that this system controls. Human prompt prefixes are an open set that grows with every new skill, command, and mode. A shape-based rule (`<[a-zA-Z-_]+>`) inverts that and silently discards real intent.

**Discard is recoverable.** Junk is routed to a reserved stream, not deleted, until the rules have been audited against real data.

## Work selection

Only `session_type = 'user'` sessions are sent to the LLM. Subagents inherit their parent's stream assignment.

This is sound because a subagent exists only to serve its parent's task; it has no independent work stream. It is also nearly free: of 35,238 subagent sessions, 34,876 (99%) resolve their `parent_session_id` to an indexed session.

Candidate sessions are ordered `start_time DESC` and each pass is bounded, so a pass always advances the reporting window before touching backfill.

Effect on the current backlog:

| Stage | Sessions requiring an LLM call |
|---|---|
| All unclassified sessions | 38,990 |
| `session_type = 'user'` only | 13,310 |
| Minus confident structural junk (10,065) | **3,245** |

Parent inheritance alone attributes 416,284 unassigned events with no LLM calls at all.

### The April 2026 orphans

362 subagent sessions name a `parent_session_id` that resolves to nothing. They are not a category the design accommodates — they are a data defect with a hard boundary. Every one is `opencode` on `devbox`, and every one falls between 2026-04-09 and 2026-04-14; no other month contains a single orphan. The 126 distinct missing parents appear nowhere else either, including the events table, so the parent sessions were never ingested rather than merely unlinked.

They are junked, because a subagent whose parent left no trace has no context to classify against. Recurrence would mean sessions are being lost at ingest, which is an ingest bug to fix at the source — not a classification policy to extend.

## Confident structural junk

Two rules run before any LLM call. Both route to the reserved junk stream.

**1. No tool calls and at most one exchange** (`tool_call_count = 0 AND message_count <= 2`). Nothing was done and nothing was discussed; there is no work to attribute. This covers 10,065 of the 13,310 unclassified user sessions.

The depth bound is load-bearing. `tool_call_count = 0` *alone* is not confident: among user sessions with zero tool calls and three or more messages, roughly half are real work — a Work Order review, a vendor pricing evaluation, a discussion of eval technique — and half are `"Hello"`, `"login"`, `"Are you there?"`. That population (90 sessions) goes to the LLM.

**2. Nothing survives injection filtering.** After the denylist below is applied per message, a session with no remaining user text never had human intent expressed in it. This is a degenerate case, not a general policy: a session is never discarded for *containing* injected text, only for containing nothing else. It removes almost nothing from the current backlog — rule 1 and the subagent filter already catch nearly every such session — and earns its place as a guard that grows more useful as injection sources multiply.

Anything else goes to the LLM.

## Injection denylist

Injected text is stripped from every user message; whatever remains is what the classifier sees. Filtering must therefore run over the whole message array rather than the opening prompt, because injections are overwhelmingly mid-session — `[SYSTEM DIRECTIVE: OH-MY-OPENCODE — BOULDER CONTINUATION]` appears in 5,285 sessions' message arrays against 11 opening prompts.

Stripped:

| Marker | Sessions affected |
|---|---|
| `<system-reminder>` | 5,958 |
| `[SYSTEM DIRECTIVE: OH-MY-OPENCODE — BOULDER CONTINUATION]` | 5,285 |
| `[NOTIFICATION from …]` | 186 |
| `<local-command-caveat>` | 56 |
| Compaction continuation banner | 1 |

Never stripped — these carry real intent and a shape-based rule would destroy them:

`<skill-instruction>` (83), `<command-instruction>` (40), `<command-message>` (14), `<command_instructions>` (1), `<Work_Context>` (14), `<review_type>` (5), `<ultrawork-mode>` (3), `<hyperplan-mode>` (1), `<bash-input>` (2), `<teammate-message …>` (3), `[analyze-mode]` (1,708), `[CONTEXT]` (1,402), `[search-mode]` (1,130), `[TASK]`, `[GOAL]`.

Adding a marker to the denylist is a deliberate act with a measured count behind it.

## The junk stream

A single stream with a stable reserved slug. Junked sessions are assigned to it, not deleted, so precision can be audited and a bad rule reversed by reassignment.

Four overlapping streams currently serve this role by accident — `other: Misc (trivial sessions)`, `misc: stale test sessions`, `agent-c: remote misc`, `taiga: Misc agent-c sessions`. They consolidate into the reserved slug.

Junk time is excluded from stream reporting but remains visible, so a filter that starts eating real work is detectable rather than silent.

## Classifier verdict

The classifier may return `throwaway` as a first-class verdict alongside a stream assignment, and it is taken at face value. This is the intended path for sessions that are structurally ambiguous but obviously trivial on inspection — the `"Hello"` and `"Are you there?"` half of the 90.

New-stream creation stays enabled. The classifier proposing a stream that does not exist yet is correct behavior; the guard against junk streams is the input filter, not a restriction on the output.

## Agentic classification

The classifier is given tools to pull more of a session on demand rather than receiving a fixed pre-chunked payload. Deciding in advance how much of a session is enough is the thing that produced bad classifications on ambiguous input.

The default payload is the injection-filtered user messages. Those are the backbone: 3,148 of the 3,245 sessions reaching the LLM (97%) carry usable prompt text.

Two tools carry the rest, and the set is short because the schema is the limit, not the imagination:

- **The session's summary, timing and counts.** The summary is the highest-value fetch precisely because the payload has no field for it. It is also why summaries are a supplement rather than a foundation: quality varies by source. Claude sessions carry none (0 of 77 in the target population). OpenCode user sessions always carry one and they are terse but serviceable — `COSMIC DisplayLink rotation bug fix (fork #1)`, `Trajectory Labs red teaming interviews`. Fetching it on demand is what lets a classifier lean on it when it exists and ignore it when it does not.
- **The stored user messages at full length.** The payload truncates each to 500 characters; the extractors store up to 2,000 bytes.

Three candidates do not survive contact with the schema and are not offered:

- **Tool-call names and touched file paths.** Unavailable. `tool_call_timestamps` is an in-memory field on `AgentSession` that is never persisted, `agent_sessions` keeps only a `tool_call_count`, and `events` has no payload column — an `agent_tool_use` row records that a call happened, never which tool or which file. Serving these would mean persisting them at ingest first.
- **The session's distinct working directories.** Available but empty: 51,989 sessions have exactly one and 21 have more, and that one is already in the payload.
- **Neighbouring sessions in time.** Only 19 of 400 sampled thin-prompt sessions had a *classified* neighbour within half an hour, because the backlog drains in bulk. Offering unclassified neighbours instead would bleed one session's subject onto another.

The tools carry exactly the cases a fixed payload handles worst: thin prompts, and prompts that are uninformative regardless of length. 601 unclassified sessions share the byte-identical prompt `The following tool was executed by the user` while carrying 566 distinct summaries — one payload, 566 different pieces of work.

Fetching is bounded at four calls per classification. Exceeding the bound is a clean stop that tells the model to answer from what it has, never an error: a verdict reached under a spent budget is still a verdict, and a model that cannot reach one answers with low confidence, which leaves the session unassigned where it registers as classification lag. Every fetched message runs through the same injection denylist as every other path, and a session whose payload already names the work spends nothing at all.

## What a stream may be

Every event belongs to a work initiative. **There is no transitional time.** The user works in the terminal continuously, so a terminal action is never "navigation between" work — it is part of whatever work it serves. A classifier that cannot identify the work must leave the event unassigned, where it registers as classification lag. It must never invent a container for it.

Four stream shapes are therefore rejected, and all four exist in current data:

- **Activity types.** `other: shell / nav / transitional`, `Devbox terminal context-switching + transient browser nav`. These describe a posture, not work.
- **Date ranges.** `ops: devbox terminal nav (Jul5-11)`, `misc (Jun14-20)`. A stream is a unit of work, not a week. 51 streams currently carry a date range, each one a catch-all minted because the previous week's was not reused.
- **Catch-alls.** `misc: stragglers`, `other: Misc (trivial sessions)`. Leftovers are a symptom of failed classification; naming the symptom hides it.
- **Buckets too coarse to name the work.** `other: dev-env (dotfiles/settings/jj/oma)` holds 23h 45m of genuine work on several distinct initiatives. Coarseness is not a category.

Reuse is preferred over creation, but creating a stream for a genuinely new initiative is correct and stays enabled. The guard against junk streams is the input filter and these shape rules, not a restriction on the classifier's output.

## Attribution signals

Classification currently keys on `cwd`. That is the wrong signal for most of the user's own attention, and it is the direct cause of the misnamed streams above.

`window_focus` events — the laptop's real WM focus changes, and the closest thing to a ground-truth attention signal — **carry no `cwd` at all**. They carry `window_app_id` and `window_title`, populated on 35,343 of 35,347 events. With no `cwd` to key on, every hour of GUI attention fell through to whatever container an agent had invented; on 2026-07-20 that was 687 events spanning 01:49 to 21:58, filed as "shell navigation." They were in fact 432 terminal focuses, 252 browser focuses, and 3 password-manager focuses.

Two rules follow.

**Terminal focus resolves by temporal correlation.** When the focused window is a terminal attached to a remote host (`com.mitchellh.ghostty` with a title like `tmux attach -t dev` or `mosh devbox`), the work is whatever that host was doing at the time. This is not a heuristic reaching for signal that may not exist: on 2026-07-20, 434 of 434 terminal-focus events had concurrent remote activity carrying a `cwd` within two minutes. During those hours the remote was working on `xmodel-eval` (4,575 events), `dpi` (1,458), and `workorder-5` (497) — so the hour filed as shell navigation belonged to the eval that already dominated the day.

**Browser and application focus resolve by title.** `Work · Pull requests` is review on a known repository; `Threads - Trajectory Labs - Slack` is discussion of whichever initiative the thread concerns. The title is the content signal, and it is present.

## Acceptance criteria

1. A session started today is classified before any session from last month.
2. Subagent sessions consume no LLM calls; they resolve to their parent's stream.
3. A session with no tool calls and at most one exchange is junked without an LLM call.
4. Stripping injected text from a session leaves its real messages intact; the session is discarded only when nothing remains.
5. `<skill-instruction>`, `[analyze-mode]`, `[CONTEXT]`, and `[search-mode]` prompts reach the classifier intact.
6. Junked sessions are reassignable; no session content is destroyed.
7. The classifier can request more of a session than it was initially given.
8. Classification does not depend on session summaries being present or meaningful.
9. No stream is created whose name describes an activity, a date range, a leftover bucket, or is too coarse to name the work.
10. `window_focus` events are attributed using `window_app_id` and `window_title`; a terminal focus resolves to the work its host was doing.
11. An event whose work cannot be identified is left unassigned rather than placed in an invented container.
