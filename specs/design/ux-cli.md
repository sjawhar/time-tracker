# UX: Command-Line Interface

## Problem Statement

The CLI documentation must reflect the **actual** commands available in the current prototype so users can run the remote collection flow and local queries without guessing.

## Design Principles

- **Event-first**: Commands append or query immutable events; no manual timers.
- **Fast hooks**: Remote ingestion must be quick and safe to call on every focus change.
- **Local analysis**: Reporting and inspection happen on the local machine.
- **Minimal surface**: Only the commands needed for the current workflow are documented.

## Command Structure

```
tt [--verbose] [--config <path>] <command>
```

Global flags:
- `-v`, `--verbose` — extra logging
- `-c`, `--config <path>` — load config from a specific file

## Command Reference

Remote-only commands (run on the dev server):

- `tt ingest pane-focus --pane <id> --cwd <path> --session <name> [--window-index <idx>]`
  - Append a tmux pane focus event to the remote JSONL buffer.
- `tt export`
  - Write buffered events and Claude session events as JSONL to stdout (used by sync).

Local-only commands (run on the laptop):

- `tt sync <remote>`
  - Pull events over SSH by running `tt export` remotely, then import into SQLite.
- `tt import [--source <source>]`
  - Read JSONL from stdin and insert into SQLite (default source applies when missing).
- `tt events`
  - Output all local events as JSONL (for debugging).
- `tt status`
  - Show last event time per source and the database path.

## Examples

tmux focus hook (remote):

```bash
tt ingest pane-focus --pane %3 --cwd /home/sami/project --session dev --window-index 1
```

Sync from remote host (local):

```bash
tt sync devbox
```

Manual export/import (local):

```bash
ssh devbox tt export | tt import --source devbox
```

Inspect local events:

```bash
tt events
```

## MVP Roadmap (Not Implemented Yet)

- `tt streams` — list/manage inferred streams
- `tt tag <stream> <tag>` — correction tags
- `tt report --week` — weekly summary report

## Acceptance Criteria

- `specs/design/ux-cli.md` documents only commands that exist today.
- Examples are copy/pasteable and match real flags and subcommands.
- Remote-only vs local-only usage is explicit.
- Placeholder text from the previous draft is removed.
