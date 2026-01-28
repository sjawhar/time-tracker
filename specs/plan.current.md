# Plan: Direct/Delegated Time Calculation

## Spec
- Source: `specs/architecture/overview.md` (Attention Allocation Algorithm + constants)
- Supporting: `specs/design/data-model.md` (event types + AFK notes)

## Approach
- Implement an interval-based allocator in `tt-db` that replays ordered events and computes per-stream `time_direct_ms` and `time_delegated_ms`.
- Track state:
  - `afk_state` (bool), `last_attention_at` (DateTime)
  - `window_focus` (app + stream_id), `tmux_focus` (stream_id), `browser_focus` (stream_id)
  - `agent_sessions`: per stream `{active, last_activity_at}`
- Attention signals refresh `last_attention_at`: `user_message`, `tmux_scroll`, `tmux_pane_focus`, `window_focus`, `browser_tab`.
- Focus hierarchy for direct time:
  - If window focus is terminal → use `tmux_pane_focus` stream.
  - Else if window focus is browser → use `browser_tab` stream.
  - Else → use `window_focus` stream.
  - If no stream resolved → attribute to `Unattributed` (stream_id = null).
- Delegated time:
  - Accrues to any stream with an active agent session.
  - If no explicit end, keep active while `now - last_activity_at <= AGENT_ACTIVITY_WINDOW`.
  - `agent_tool_use` updates `last_activity_at` (and `agent_session` start should set it so sessions count initially).
- AFK correction:
  - For `afk_change` with `status=idle` and `idle_duration_ms`, inject a synthetic AFK start at `timestamp - idle_duration_ms` so preceding intervals are reclassified.

## Files to Change
- `crates/tt-db/Cargo.toml` (add `serde_json` dependency)
- `crates/tt-db/src/lib.rs`:
  - New event parsing helpers (parse JSON payloads for `afk_change`, `window_focus`, `browser_tab`, `agent_session`).
  - New allocator function (pure) that returns `HashMap<Option<String>, TimeTotals>` for direct/delegated.
  - New `Database` method to recompute and persist `streams.time_direct_ms` / `streams.time_delegated_ms` (update rows, zero missing streams).

## Tests (tt-db)
- Direct time respects attention window + AFK:
  - Focus event at T0, next event at T0+5m, with attention window 120s → only 120s direct time.
  - AFK idle event with `idle_duration_ms` retroactively zeros direct time for that interval.
- Focus hierarchy attribution:
  - `window_focus` app="Terminal" + `tmux_pane_focus` stream A → direct time to A.
  - `window_focus` app="Chrome" + `browser_tab` stream B → direct time to B.
- Delegated overlap:
  - Two streams with active `agent_session` → both accrue delegated time over the same interval.
  - `agent_tool_use` extends delegated time within `AGENT_ACTIVITY_WINDOW`.
- Unattributed:
  - Unknown focus (no stream_id) yields `Unattributed` direct time without panicking.

## Open Questions
- None. Assumptions: heuristic terminal/browser detection based on `window_focus.data.app` strings (Terminal/iTerm/Alacritty and Chrome/Firefox/Safari/Edge/Brave).

## Checklist
- [x] Spec exists and is complete
- [x] Files to change identified
- [x] Test cases outlined
- [x] No open questions remain
