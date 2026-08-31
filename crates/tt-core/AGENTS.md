# tt-core — Domain Logic

Core algorithms and types for time tracking. Pure computation + session parsing (file I/O for Claude and omp, SQLite for OpenCode).

## Modules

| Module | Lines | Role |
|--------|-------|------|
| `allocation.rs` | ~2300 | Time allocation algorithm (direct + delegated) |
| `session.rs` | ~1130 | Claude Code session scanning/parsing |
| `opencode.rs` | ~1720 | OpenCode session scanning. Reads `session` from monolithic `opencode.db`; reads `message`/`part` from per-session shard at `sessions/<id>.db` when present, else from monolithic. |
| `omp.rs` | ~730 | omp (oh-my-pi) session scanning/parsing |
| `injection.rs` | ~250 | `is_injected` — denylist telling harness-injected text apart from human input |
| `classification.rs` | ~500 | `is_structurally_junk` + `is_misnamed_stream` + `normalize_stream_name` — what the classifier is asked, what it may answer, and when two answers are one answer |
| `attribution.rs` | ~550 | `is_terminal_focus` + `resolve_terminal_focus` (temporal, terminals only); `artifact_in_title` + `artifact_refs_in_text` + `resolve_artifact_focus` (content binding, browsers) — attributing `window_focus` events, which carry no `cwd` |
| `project.rs` | ~110 | Git remote → project name extraction |

## Allocation Algorithm (`allocation.rs`)

Computes direct (human focus) and delegated (agent) time per stream.

### Flow

1. Build **focus timeline** from `tmux_pane_focus`, `afk_change`, `tmux_scroll`, `window_focus`, `browser_tab` events
2. Build **agent activity timeline** from `agent_session` + `agent_tool_use` events
3. Walk intervals: attribute time based on focus state and agent state

> **Capture status (important):** `tmux_scroll` is now emitted by `tt ingest scroll`, wired to the `pane-mode-changed` tmux hook in `config/tmux-hook.conf` (fires on copy-mode *entry*, e.g. mouse-wheel up — NOT on every wheel tick, so long copy-mode reading past the `attention_window` still relies on the cap). `window_focus`/`afk_change` come from the COSMIC `tt-watcher` daemon. `browser_tab` remains an **unimplemented input** — no emission path, 0 such events in the DB — so browser focus falls back to the window's own stream (→ UNASSIGNED until classified). Heads-down terminal work between focus/scroll events still leans on the `attention_window` cap.

### Key Types

- `AllocatableEvent` — trait that `StoredEvent` (tt-db) implements. Methods: `timestamp()`, `event_type()`, `stream_id()`, `session_id()`, `action()`, `data()`
- `AllocationConfig` — `attention_window_ms` (default 300s / 5min; tests use 60s), `agent_timeout_ms` (default 30min)
- `StreamTime` — result per stream: `time_direct_ms` + `time_delegated_ms`
- `FocusState` — enum: `Focused { stream_id, focus_start }` | `Unfocused`
- `AgentSession` — tracks per-session: `first_tool_use_at`, `last_tool_use_at`, `ended`

### Rules

- Focus gaps > `attention_window_ms` are capped (no inflated time from sparse events)
- AFK with `idle_duration_ms` retroactively subtracts idle time (capped at attention_window)
- Agent sessions without tool_use events get zero delegated time
- Agent timeout: no tool_use for `agent_timeout_ms` → session ends at last tool_use
- `user_message` events establish focus on their stream (like `tmux_pane_focus`) — sending a message to an agent counts as direct work
- Focus hierarchy (`resolve_focus_stream`): terminal app → tmux stream; browser app → browser-tab stream, falling back to the window's own stream when there's no `browser_tab` info; other GUI app → the window's stream
- `window_focus` establishes focus for non-terminal/non-browser GUI apps (Slack, doc/PDF readers): it closes the prior interval against the *old* window state first, then opens the new focus. A GUI/browser window with **no resolvable stream still accrues direct time to the UNASSIGNED bucket** (same as unassigned tmux focus) — active GUI time is never dropped to zero; it waits in UNASSIGNED until classify attributes it.

### Streams are semantic — there is NO deterministic surface→stream mapping

A **stream** is a coherent unit of *work*, identified by human/LLM judgment. It is **not** derivable from any surface signal. Each of these is NOT a stream:

- a **working directory** is not a stream (one repo/dir hosts many streams; one stream spans many dirs)
- a **window title** is not a stream
- a **browser tab / URL** is not a stream
- an **app name** is not a stream
- **"unfocused"** is not a stream

**Do NOT add deterministic rules that map cwd / window title / browser tab / app name → stream.** That approach is *fundamentally unsound* and is a known dead end: the same surface belongs to different streams over time, and a single stream spans many surfaces. There is no rule that recovers the mapping — only semantic judgment does.

Classification is therefore done by human/LLM judgment via `tt classify` — `--apply` to commit human/LLM assignments, or `--auto` (also driven continuously by the `tt-serve` daemon) to let the configured LLM classify — never by a fixed surface rule. Surface signals may at most be **weak temporal hints** for that classifier — never an attribution rule. Events with no resolvable stream stay **UNASSIGNED** until semantically classified; they must not be silently dropped, nor back-filled by surface heuristics.

#### Selecting what the classifier is shown is presentation, not attribution

The rule above bans a surface→stream *assignment*. It does not ban deciding which streams the classifier gets to look at, or in what order — that is a rendering decision, and the model still chooses. The distinction is load-bearing in both directions: `tt-llm`'s `prompt` module orders its roster by how close each stream's activity falls to the session being classified and caps it at `ROSTER_LIMIT`, and that is legitimate. A rule that *concluded* the stream from the same signal would not be.

The cap is not an optimisation; without it the roster is a feedback loop. Every stream created makes the next reuse target harder to find, so the model creates a neighbour, which enlarges the roster again. Measured on the live table it had reached 1,018 streams and 329 KB per prompt while the classifier minted ~101 streams an hour — roughly one per session.

#### A stream is an initiative, not a task instance

Granularity is the axis `is_misnamed_stream` deliberately does not judge. `agent-c: eval-3 saleor environment (eval-3 integration)` names real work by a real subject, so the guard passes it — and so does the same sentence with `traccar`, `bookstack`, `prometheus`, `benchling` and `mayan LIMS` in place of `saleor`, all created within one hour. That is one initiative rendered as six rows, and a report split that finely answers nobody's question.

There is no mechanical signature for it, exactly as there is none for a bucket too coarse to name the work. It is stated in the prompt instead: reuse is the default, creation the exception, and a stream is one initiative spanning many sessions over days or weeks. Two mechanisms back that up, both outside this crate:

- `tt_core::normalize_stream_name` plus `tt_db::find_stream_by_normalized_name` make an exact or whitespace-only duplicate impossible to create. This is what makes capping the roster safe — a name the model proposes for a stream it was never shown becomes reuse of that row, so a cap can only ever cost a *semantically* near-duplicate.
- Collapsing streams that are already too granular is an operator judgement, not a rule: `tt streams rename` then `tt streams merge`. Nothing infers it.


### Testing

`TestEvent` struct with builder methods: `tmux_focus()`, `afk_change()`, `agent_session()`, `agent_tool_use()`, `user_message()`, `window_focus()`, `browser_tab()`. 34 test cases cover edge cases (gaps, capping, concurrent agents, AFK retroactive, user message focus).

## Session Scanning (`session.rs` + `opencode.rs` + `omp.rs`)

Parses Claude Code (`~/.claude/projects/`) JSONL session files, OpenCode (`~/.local/share/opencode/opencode.db`) SQLite database, and omp (`~/.omp/agent/sessions/`) JSONL session files. The user's OpenCode fork shards messages/parts into per-session SQLite files at `~/.local/share/opencode/sessions/<id>.db`; `build_agent_session` opens the shard when present and falls back to the monolithic connection when not (schema is identical between the two). Corrupt or non-SQLite shards trigger a logged warning and the same fallback. omp's own line shapes — a session having no fixed "text"/"toolCall" line types, subagent nesting, title resolution across format versions — are documented in `omp.rs`'s module doc comment rather than duplicated here.

**An empty session is an expected condition, not a parse failure.** An agent session aborted before its first message leaves nothing to parse, and both scanners used to warn about it on every pass — `tt-serve` ingests every ~30s, so two local shards and 31 on devbox filled the daemon log forever. `SessionError::EmptySession` names that state, and both `scan_claude_sessions` and `scan_opencode_sessions` log it at `debug` while every other variant still warns.

The two sources detect it differently, and in both cases the discriminator is structural rather than a size heuristic:

- **`OpenCode`**: `open_session_shard` returns `SessionShard::Ready` / `Empty` / `Absent`. It answers both questions in the one `sqlite_master` probe it already ran, by asking whether the `message` table exists: a non-`SQLite` file errors and becomes `Absent` (warn + monolith fallback, unchanged), while a valid but never-written shard reports no table and becomes `Empty`. **File size cannot make this call** — locally the two shards lacking `message` are 0 and 4096 bytes, but a *second* 4096-byte shard is a perfectly good database. An `Empty` shard is skipped rather than falling back to the monolith, which holds no rows for it either and would index a phantom zero-message session.
- **Claude**: `parse_session_file` distinguishes three states, because "non-empty" is not the same question as "defective". No content at all is `EmptySession`. Content whose lines all parsed but yielded no message is `NoMessageRecords` — a session that only fired a `SessionStart` hook leaves exactly that, and so does any line `might_be_relevant` filters before the deserializer sees it. Only a *message-shaped* line that fails to deserialize sets the parse-failure flag and reports `NoMessages`, which still warns. The first two are logged at `debug`: they recur on every ~30s ingest, and one hook-only file warning forever is noise, not a signal.

### Key Types

- `AgentSession` — parsed session: `session_id`, `source`, `parent_session_id`, `session_type`, `project_path`, `start_time`, `end_time`, `message_count`, `user_prompts`, etc.
- `SessionSource` — enum: `Claude` | `OpenCode` | `Omp`
- `SessionType` — enum: `User` | `Agent` | `Subagent` | `Continuation`. In `session.rs` inferred from session_id format; in `opencode.rs` and `omp.rs`, `Subagent` is set when `parent_id` is present. omp sets `Continuation` only when `parentSession` names a valid parent UUID distinct from the transcript's own session ID.

### Parsing Rules

- `user_prompts`: max 5, each truncated to `MAX_PROMPT_LENGTH` bytes (currently 2000), UTF-8 boundary safe
- `user_message_timestamps`: max 1000
- `message_count`, `assistant_message_count`, `tool_call_count` are `i32` and saturate at `i32::MAX`
- Parent session ID extracted from directory structure (Claude, omp) or session metadata (OpenCode)
- Empty/whitespace-only user prompts are skipped
- Harness-injected messages (`injection::is_injected`) are skipped entirely: no prompt, no timestamp, no contribution to the user-message count. The session's start/end times still advance across them — an injection proves the session was alive, just unattended. See root `AGENTS.md`, "Injected text is not attention".

## Project Identification (`project.rs`)

`extract_project_name()`: workspace path → project name. Strips known workspace prefixes, falls back to last path component.

## Injection Detection (`injection.rs`)

`is_injected(message) -> bool` and its `filter_map` companion `human_message`. `INJECTION_MARKERS` is public because classification consumes the same list — a proposal built from injected text describes the harness, not the work.

A message is injected when it *opens* with a known marker, after stripping leading whitespace and Markdown horizontal rules (the harness separates injected payloads with `---`). Rationale for denylist-and-leading-token, and how to extend the list, live in root `AGENTS.md`.

## Focus Attribution (`attribution.rs`)

`window_focus` events carry `window_app_id` and `window_title` but **no `cwd`** and no session id, so nothing else in the pipeline reaches them and every hour of GUI attention fell through to whatever container a classifier had invented. This module resolves the one case that needs no content understanding: a terminal attached to a remote host is showing whatever that host was doing, and the host's own events are already classified.

- `is_terminal_focus(app_id, title)` — true when **either** the `app_id` is a known terminal (`TERMINAL_APP_IDS`) **or** the title opens with a remote-shell command (`REMOTE_SHELL_PREFIXES`: `tmux `, `mosh `, `ssh `). Either alone suffices because the app set grows when the terminal is swapped and the title set grows when a new remote tool is used; requiring both would silently drop attention. An unrecognised terminal returns `false`, leaving the event unassigned — a visible, safe failure.
- `resolve_terminal_focus(focus_at, sorted, window_ms)` — strict plurality of the streams active within `window_ms` of `focus_at`, located by binary search over a caller-supplied sorted slice. **A tie returns `None`**: a coin flip between two streams is an invented answer.
- `TERMINAL_CORRELATION_WINDOW_MS` = 60_000. Measured, not chosen: over 1,581 terminal-focus events in 2026-07-15..21, ±15s resolves 92.0%, ±30s 97.5%, ±60s 99.4%, ±120s 99.8%, ±600s 100%. The curve is flat past 60s, and a wider window lets a focus event be adopted by work the user had already left.

This is a **temporal correlation against already-classified work**, not a surface→stream rule of the kind forbidden above: the app ID and title decide only *whether the window is a terminal*, never *which stream it is*. The stream comes from the remote host's own classified events.

Callers: `tt_db::{unattributed_terminal_focus_events, remote_activity_for_correlation}` supply the rows; `tt ingest sessions` runs the pass from `attribute_unassigned_events`, first of the two passes it applies to unassigned events, and writes with `assignment_source = 'terminal_focus'`.

### Non-terminal focus: bind the artifact, never the moment

A browser is **not** a view of the machine's current activity the way a terminal is, so the terminal pass's temporal correlation does not transfer to it. That was measured rather than assumed. Using the content-bound artifacts below as ground truth, repo-scoped temporal plurality on a PR page agreed **53.7%** of the time (36 agree / 31 disagree / 44 unresolved at ±60s; 57.5% at ±300s) — a coin flip, and its errors are plausible neighbours (`time-tracker: tooling` for `time-tracker: tt fixes + todo CLI`), which is precisely "adopting whatever was nearby". Unfiltered temporal plurality was worse. **Do not add a temporal pass for browser or application focus.**

What does hold is that some titles name a *durable work artifact*, and the work an artifact belongs to is recorded by the classified session that did it:

- `artifact_in_title(title)` → `Option<ArtifactRef>`. A specific GitHub pull request or issue only. A repository-wide listing (`Pull requests · sjawhar/time-tracker`), an account-wide one (`Work · Pull requests`), and `New Tab - Brave` all return `None` — a repository hosts many streams, so a title reaching no further than a repository has identified no work. `agent-c` alone spans **224 streams**, which is why no repo→stream shortcut is permissible.
- `artifact_refs_in_text(text, project)` → the artifacts one piece of classified work refers to. Two forms, both **identifiers**: a GitHub URL (`github.com/<owner>/<repo>/(pull|issues)/<n>`), which is self-scoping, and a bare `#<n>`, scoped by the repository the work was done in and ignored entirely when that is unknown. `parse_hash_number` requires a word boundary after the digits, so `#ff0000` and `#47abc` are not references.
- `resolve_artifact_focus(target, mentions)` — strict plurality over the mentions of *that one artifact*. Mentions of any other artifact are not candidates at all, which is what stops this degenerating into temporal adoption. A tie returns `None`, same discipline as the terminal pass.
- `ArtifactRef::is_same_artifact_as` — repo and number must agree; a `None` owner is *compatible*, not wildcard-equal. Forks make that load-bearing: `METR/hawk` and `trajectory-labs-pbc/hawk` share a repo name and number space but are different work, so two **known** differing owners never bind.

`MAX_ARTIFACT_NUMBER_DIGITS` = 6. The largest number in the corpus is `#13986`; the sixth digit is slack.

This is a **content binding against work records**, not a surface→stream rule: the title supplies an artifact identity, and the stream comes from the session that demonstrably worked on that artifact.

#### tmux panes: the session is read from the pane's process tree

A `tmux_pane_focus` event carries no window title (0 of 102,600), no `window_app_id` and no session id — only `pane_id`, `tmux_session` and `cwd`. So the window-run classifier has nothing to read, `cwd` is banned, and for the whole history of this event type nothing attributed one, while they made up **102,746 of the 118,561 unassigned focus events (77%)**. What a pane does have is a process tree, and a pane hosting an interactive agent holds that agent's session id in it.

`tt ingest pane-focus` therefore resolves the focused pane's process tree to a session id and stores it on `events.session_id`. The mechanism lives in `tt-cli`'s `commands/ingest/pane_session.rs`; the tmux hook passes `--pane-pid #{q:pane_pid}`.

**It is an identity, not an inference, and that is why it is permitted where plurality is not.** The pane being focused *is* running that session. Nothing here derives a stream: it writes `session_id` and never `stream_id` or `assignment_source`, and the attribution comes entirely from the already-trusted session→stream path — `Database::assign_events_by_session_id` when the session is classified *after* the stamp, and `Database::claim_unassigned_events_for_classified_sessions` when it was classified *before* it, which is the common case and the half that had to be built separately. A pane focus event therefore inherits exactly the judgement the classifier applied to the session, through no new engine and no new surface rule. The **stamping** is still not a pass in `attribute_unassigned_events`: it happens at capture, where the process tree still exists. What *claims* the stamp is a pass, and it is the third one; see root `AGENTS.md`, "A stamped session id is worthless until something claims it".

**The coverage figure first recorded here was contaminated and is withdrawn.** It reported a session id in **101 of 101 observations (100%)** for a pane hosting an interactive agent, and explained it by the harness process being long-lived in that pane. Both halves are wrong. Sampling ran every 5 seconds *while the agent was continuously executing commands — the sampling script itself among them* — and the session id lives only on processes a tool call spawns, so the measurement manufactured the condition it was measuring. Diagnosed afterwards on the same machine, the harness's long-lived process is **not** a descendant of the pane's shell at all (the pane tree is `-bash` → `uv run --script`), and between commands exactly two processes on the whole machine carried a session id: a transient command shell, and a detached `tt-serve` that had merely inherited the variable when it was launched from inside a session. **Live steady-state coverage is 0 of 17 pane-focus events stamped over 90 minutes**, and the reason is structural: a `tmux_pane_focus` fires when the user *switches* panes, which is precisely when a tool call is not in flight in that pane. The stamp therefore lands only on the coincidence of a focus event with an executing command, which is rare rather than typical. Plain shell panes scoring 0% remains correct — they have no session to attribute to — but that was never the interesting half.

**The decisive limitation is that it is forward-only.** The pairing exists in live process memory and nowhere else, so it stamps focus events as they are captured and can do nothing whatsoever for the **102,746 tmux panes already unassigned** — those processes are long gone. It stops the backlog growing; it does not reduce it, and nothing short of human labelling will.

**The read is bounded and touches exactly two variables.** An agent process's environment holds credentials, so `session_id_in_environ` matches `OPENCODE_SESSION_ID` and `CLAUDE_CODE_SESSION_ID` **by entry name** and returns only that one value; every other byte read is dropped, never logged and never stored. By name and not by substring, because a real environment contains `MY_OPENCODE_SESSION_ID` and notes whose *value* spells one of these names. The walk is breadth-first from the pane's own process, capped at 64 environments and 8 generations — observed trees are 2–8 processes at depth ≤ 5, so both bounds cap pathology rather than real work, and one walk costs 0.62 ms against the three `jj` subprocesses the same hook already spawns.

**Children must be read per thread.** `/proc/<pid>/task/<tid>/children` lists the children of *that thread*, and the harnesses fork from worker threads: across every live pane on this machine, reading only the main thread's file found **0** session ids where reading all of them found the agent. The main-thread shortcut resolves nothing at all.

**Every failure is silent and total.** No pane pid (an install that has not re-sourced `config/tmux-hook.conf`, where tmux is asked directly instead), non-Linux, no `/proc`, permission denied, a process exiting mid-walk, a pid too large to parse — each records the focus event exactly as it was recorded before this existed. A focus event is never lost to this lookup, which is also why `--pane-pid` is taken as text rather than a number: clap must not be able to reject a value and cost the event.

#### Rejected alternatives (measured, do not retry)

**Extending the terminal pass to `tmux_pane_focus` was measured and refuted — 10.0% accurate on the population it targets. Do not retry it.** `unattributed_terminal_focus_events` reads `type = 'window_focus'` only, while the unassigned focus backlog is **55,519 `tmux_pane_focus` against 16,372 `window_focus`** — so the one mechanism that places focus for free never looks at 77% of the work, and a tmux pane is a terminal by construction. That makes extending it the obvious move, and it is still an open question. What blocks answering it is the absence of ground truth. **A `tmux_pane_focus` event carries no window title (0 of 102,600), no `window_app_id`, and no `session_id`** — only `pane_id`, `tmux_session` and `cwd`. So the window-run phase has literally no evidence for one, and indeed **0 tmux panes were placed by any source in the last 24 hours**. The only assignments they carry are **47,081 rows of `assignment_source = 'inferred'` spanning March to August**, and the sole field they expose that could have produced those is `cwd` — i.e. they are the deleted cwd propagator's output, the engine this repo removed for mapping one directory onto as many as 73 streams. **An earlier revision of this entry cited a measured 25.8% agreement as grounds for rejecting the extension. That figure is withdrawn.** It compared ±60s temporal plurality against those cwd-propagated labels, so it measured agreement with a known-bad oracle rather than with truth, and a disagreement is exactly as consistent with the correlation being right. Do not cite it. Answering this properly needs human-labelled ground truth, of which the database currently holds **none** for this event type: no `user`, no `todo_link`, no `terminal_focus` row exists on a tmux pane. **That question is now answered, and the answer is no.** Two independent measurements, both using ground truth rather than the cwd-propagated labels the withdrawn figure relied on.

The first came from the process-tree identity above, which is what finally created ground truth for this event type: 91 `tmux_pane_focus` events carry a stamped `session_id` resolving to a single known stream. Against those, requiring the nearest assigned attention event on *each side* within 5 minutes to name the **same** stream — unanimity, not plurality, refusing on disagreement — predicted 17 of 91 (18.7%) and was right on **16 of 17 (94.1%)**. That looked like a pass, and it was a small-sample artifact.

The second is a hold-out over the whole assigned-attention timeline (30 days, 30,287 events, 6,000 sampled): hide one event's stream, predict it from its neighbours, and **skip neighbours from the same `session_id`**, without which the test is circular because a session's own events surround each other. 1,933 predictions: **60.3% overall, and 10.0% on `tmux_pane_focus` itself — 1 correct of 10.** By type: `window_focus` 75.6% (n=1,028), `user_message` 43.2% (n=895), tmux panes 10.0%. The population the extension exists to serve is the one it gets wrong nine times in ten, which is worse than the 53.7% that already disqualified the browser temporal pass.

Two structural facts explain it and are worth keeping. **71.7% of unassigned panes have *different* streams on either side** within 5 minutes, so the rule refuses most cases — and where it does bracket, agreement of the two neighbours turns out to carry almost no signal about the pane between them. A pane switch is *how* the user moves between initiatives, so the moment either side of it is the least reliable guide to it. Do not retry this by widening the window, by relaxing unanimity to plurality, or on the strength of a small stamped sample: n=17 said 94% and n=1,933 said 10%. Note the scope this question now has: the process-tree identity above attributes pane focus **as it is captured**, so what is left unanswered here is the already-unassigned backlog, whose processes are gone.

**The option space for a tmux pane is small, and it is fully enumerated.** Four signals exist on one of these events. *Window title*: there is none — 0 of 102,600, so the classifier has nothing to read. *`cwd`*: banned outright, a folder is not a project, and it is the field the deleted propagator used to produce the 47,081 labels above. *`pane_id` / `tmux_session`*: an identity join would be exact rather than inferred, but there is nothing in the database to join to — `pane_id` is present on 102,605 of 102,605 `tmux_pane_focus` rows and on **0** of 2,270,791 `agent_tool_use`, **0** of 207,381 `agent_session` and **0** of 112,719 `user_message` rows, because agent events are derived from harness transcripts that carry no tmux context while focus events come from the tmux hook. Measured on the live table: 1,903 distinct pane ids on unassigned focus, **0** on classified activity. *Timestamp*: temporal correlation, the one remaining candidate, and the one that cannot be validated without the labels named above. So a mechanism here must either create the missing counterpart or wait for ground truth. Those are the only two moves; everything else in this list has been checked and closed. **Creating the counterpart is what the process-tree identity above does** — the pane's live processes hold the session id the transcripts never carried — and it does so for future events only, which is why the temporal question below stays open for the backlog.

*(Method note worth keeping: `datetime()` yields `YYYY-MM-DD HH:MM:SS` while events store `YYYY-MM-DDTHH:MM:SS.mmmZ`, so a string `BETWEEN` over those columns matches almost nothing — the first run of that measurement reported 2 of 400 events as having any concurrent activity. Compare with `julianday`.)*

Two prose-matching schemes were built and thrown away. Both are the forbidden surface→stream rule wearing a disguise, and both produced confident nonsense:

- **Title subject words vs. session text** (token containment, single-stream winner): bound `API Keys | Claude Platform` → `agent-c: rl-eval postgres`, `AWS Support Console` → `legion: opencode-api shared serve`, `Amazon.com Shopping Cart` → `agent-c: calendar reskins`. A single accidental co-occurrence wins "unanimously".
- **Exact PR subject phrase vs. session text**: coverage collapses on the length threshold (min 20 chars → 4 artifacts, min 30 → 1). The knob decides the answer, not the data, so it is a coverage tuning dial and was dropped.

A third, **workspace identity**, is sound in principle and simply has no data: Legion names workspaces after the issue they serve (`trajectory-labs-pbc-agent-c-7985`, `sjawhar-legion-188`, `pr-9974`), which is an exact identifier — but the numbered-workspace era (issues ≤ ~11804) does not overlap the window-focus capture window (2026-06-21..07-29, PRs 12945–13986), so it binds **0** artifacts. Implementing it would ship an unmeasured mechanism; revisit only when the two ranges overlap.

A fourth, **handing the same temporal signal to the model as evidence instead of applying it as a rule**, was built, measured and **removed**. Nothing in this crate implements it any more, and that absence is deliberate rather than an oversight. A `streams_active_near` tallied the classified activity within ±120s of a window run and returned **every** stream with its share, naming no winner — no plurality, no tie-break, no `Option<String>` a caller could write into `events.stream_id`, and a tie *kept* where `resolve_terminal_focus` discards it. That return type did keep it on the legitimate side of the line this file draws, so the reason it is gone is not constitutional. It simply did not work.

Its window was **±120s**, measured over the 400 newest unassigned focus events on the live corpus rather than chosen: coverage 82% / 84% / **89%** / 90% / 90% at ±30 / 60 / 120 / 240 / 480s, against a busiest-stream share of 0.594 / 0.563 / **0.543** / 0.471 / 0.391. Coverage climbs steeply to 120s and then stops, so the last 5 points of coverage cost 2 points of concentration while going wider buys 1 for 7. Focus events cluster inside active sittings, which is why work within a quarter of an hour is almost always work within two minutes — the sizing was already at its knee, so widening it is not the untried idea it looks like.

**It raised confidence and placed nothing.** A 45-run, 90-call A/B against the production model and roster lifted mean confidence from 0.243 to 0.343 (+0.110 on the 41 runs with any evidence, 30 of 41 up) and moved **0** additional runs across the 0.80 threshold. Worse, the gain was largest where the title names no work: +0.115 on the population this file says cannot be attributed, +0.058 on the population it says can, and **+0.000** on `terminal`. A Google Meet **room code** went 0.35 → 0.75 and named an existing stream; an auth flow went 0.15 → 0.65. That is the 53.7% "adopt whatever was nearby" failure re-appearing as confidence inflation, and **only the 0.80 threshold stopped it becoming a wrong assignment — the strongest evidence on file for never lowering that threshold and never special-casing it per scope.** Full numbers and the do-not-retry list are in the root `AGENTS.md`, "Giving a window run temporal evidence".

#### Coverage is small on purpose

Of 3,234 unassigned non-terminal focus events (2026-06-21..07-29): 669 are browser chrome or blank (`New Tab - Brave` alone is 548), 362 name a Google doc/sheet/drive page by prose, 131 are auth or consent flows, 9 are Slack, 352 are GitHub PR/issue pages, 1,711 are other. Only the PR/issue class is attributable, and within it only artifacts a classified session actually referenced: **20 of 523 artifacts bind** (111 focus events, of which 12 were still unassigned at measurement). The binding rate is capped by what is persisted — `agent_sessions` stores at most 5 prompts of 2,000 bytes, so most PR URLs an agent emitted are simply not in the DB.

That is the intended shape. Everything unattributable stays unassigned, where it reads as classification lag; **`Threads - Trajectory Labs - Slack` names the workspace, not the initiative, and tt cannot read thread content, so Slack focus is genuinely unattributable from a window title.** Do not widen any of these rules for coverage.

## Classification Predicates (`classification.rs`)

Three pure helpers, no DB and no I/O, bounding `tt classify --auto` on both sides: what is worth an LLM call, and what the LLM is allowed to answer.

- `is_structurally_junk(tool_call_count, message_count)` — `tool_call_count == 0 && message_count <= 2`. Nothing was done and nothing was discussed, so no call can find work. **The depth bound is load-bearing**: among tool-free sessions with three or more messages roughly half are real work (a work-order review, a vendor pricing evaluation, a discussion of eval technique) and half are `"Hello"`; that population goes to the LLM.
- `is_misnamed_stream(name) -> Option<MisnamedReason>` — rejects a name describing an `ActivityType`, a `DateRange`, or a `CatchAll` before a stream is minted.
- `normalize_stream_name(name) -> String` — trims and collapses internal whitespace, and nothing else. The form that decides whether two names are the same name, applied before the guard reads a name and before any writer stores one. **Case is preserved deliberately**: the live table holds 13 streams under a `DPI:` prefix and 7 under `dpi:`, and deciding those are one initiative is `tt streams merge`, not a side effect of writing a name.

### Shape, never substring

A name fails only when **every** token in it is generic, so one subject token rescues the name: `agent-c: calendar navigation debugging` passes despite `navigation`, and `misc: webcam troubleshooting` passes despite `misc`. Substring rules do the opposite — a `%nav%` rule written during remediation matched that first name, which holds real work. A leading single-token namespace (`misc:`, `ops:`) is stripped first, because the prefix groups streams and the body is what names the work; a bare number is neither generic nor a subject.

The two generic vocabularies decide the reason: any posture or surface word (`shell`, `nav`, `terminal`, `context-switching`, `transient`, `devbox`) makes it `ActivityType`, otherwise it is `CatchAll` (`misc`, `stragglers`, `trivial`). Both lists grow from measured stream names, the discipline `INJECTION_MARKERS` follows.

Dates are checked first and are the one shape that outvotes a real subject: `workorder-5: IPI envs + wo-005 (Jun14-20)` is rejected even though its subject is genuine, because the suffix buckets work into a period and the next period mints a fresh catch-all. **A month followed by a day is the whole test** — a lone day buckets exactly as a week does, because a stream is a unit of work and not a date, so `infra: devbox-mx recovery (Jun6)` is rejected on the same grounds and the next incident cannot mint its own dated twin. Every range spelling opens with a month and a day anyway, so requiring the span as well is precisely what let `(Jun6)` through. That covers `Jun6` / `Jun14-20` / `Jul 5 - 11` / `Jun14-Jul20`, with the month token required to end at a non-letter so `marathon14-20` is not a date. Purely numeric ISO notation is the one asymmetry: it carries no month word for that test to see, so a name needs **two** ISO dates to read as a period — one alone names a dated artifact (`hawk: 2026-06-14 incident postmortem` passes). The reason is still spelled `DateRange`; it has covered a lone date since. All of this is safe **because this predicate only gates stream creation** — a rejection leaves the candidate unassigned, which reads as classification lag. Feeding the same pattern to a bulk dissolution is what would release the ~25k events such dated streams already hold; that is a different operation with a different blast radius.

The design's fourth rejected shape — a bucket too coarse to name the work, such as `other: dev-env (dotfiles/settings/jj/oma)` — has **no mechanical signature**: it names concrete subjects and is wrong only in scope. It stays a human judgement and is not a `MisnamedReason`.
