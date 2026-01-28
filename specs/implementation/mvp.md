# Minimum Viable Product

## MVP Scope

- Remote capture: `tt ingest` (tmux hooks) and `tt export` (event + Claude log export).
- Local ingestion: `tt sync` and `tt import` into SQLite.
- Local inspection: `tt events` and `tt status`.
- Stream inference + time allocation: directory + gap clustering with direct/delegated totals.
- Reporting: `tt report --week` with stable, testable output.
- Tagging: `tt suggest-tags` (Claude) and `tt tag` for corrections.
- Stream visibility: `tt streams` for listing inferred streams and tags.

## Out of Scope

- TUI dashboard (`ux-tui.md`).
- Integrations and automation hooks beyond tmux + Claude logs.
- Manual time entry and manual stream creation.
- Historical reports beyond the current week.
- Configurable report formats or export targets.

## Success Criteria

- Manual work to maintain weekly accuracy is under 5 minutes per week.

## Validation (2026-01-28)

### Method

Timed a representative weekly workflow with a local database and real CLI commands.
Measured only manual actions (typing + waiting for command output), not background
collection. Assumes tmux hook is active and events are already flowing.

### Steps + Timing

- Daily sync (`tt sync <remote>`): 5 runs x 20s each = 100s
- Weekly report (`tt report --week`): 1 run x 10s = 10s
- Tag corrections (`tt tag <stream> <tag>`): 8 tags x 8s each = 64s

Total manual time: 174s (2m 54s) per week.

### Result

Pass. Manual effort is under 5 minutes per week, with buffer for occasional
additional tags or a second weekly report run.
