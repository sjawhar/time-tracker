# Implementation Plan

Linear task list for Time Tracker MVP.

## Completed Specs (unlocked Prototype)

- [x] `design/data-model.md` — Event schema, deterministic IDs, UTC timestamps
- [x] `research/technical-foundations.md` — tmux hooks, agent logs documented
- [x] `implementation/tech-stack.md` — Rust everywhere
- [x] `architecture/components.md` — Simplified architecture (no daemon)
- [x] `architecture/decisions/001-event-transport.md` — Pull-based sync via SSH

## Prototype Implementation (Remote)

- [x] Set up Rust workspace
- [x] Create `tt ingest` command
- [x] Configure tmux hook in `~/.tmux.conf` (calls `tt ingest` on pane-focus-in)
- [x] Create `tt export` command (reads `events.jsonl` + parses Claude logs, outputs combined stream)
- [ ] Create Claude log manifest for incremental parsing

## Prototype Implementation (Local)

- [ ] Implement SQLite event store (schema from `data-model.md`)
- [ ] Implement `tt import` command (reads events from stdin, inserts to SQLite)
- [ ] Implement `tt sync <remote>` command (SSH + `tt export` + `tt import`)
- [ ] Implement `tt events` command (query local SQLite)
- [ ] Implement `tt status` command (show last event time per source)

## Prototype Validation

- [ ] Test end-to-end: tmux focus → `events.jsonl` → sync → local SQLite
- [ ] Deploy and start collecting real data

## Specs (unlocks MVP)

- [ ] Finalize CLI commands — Update `ux-cli.md` with actual commands
- [ ] Finalize report format — Update `ux-reports.md` with MVP report output
- [ ] Define attention allocation algorithm — Complete "Critical TODO" in `architecture/overview.md`
- [ ] ADR: Remote analysis architecture — How local learns about remote pane context
- [ ] ADR: ActivityWatch integration — Document decision and rationale

## MVP Implementation

- [ ] Implement stream inference (directory + temporal clustering)
- [ ] Implement direct/delegated time calculation
- [ ] Implement `tt report --week` command
- [ ] Implement LLM tag suggestion (calls Claude API)
- [ ] Implement `tt tag <stream> <tag>` for corrections
- [ ] Implement `tt streams` to list/manage streams
- [ ] Validate against success criteria (<5 min/week manual work)

## Deferred (Post-MVP)

- [ ] `ux-tui.md` — TUI dashboard design
- [ ] `architecture/integrations.md` — Rules engine, webhooks, API server
- [ ] `implementation/phases.md` — Long-term roadmap
