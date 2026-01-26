# UX: Terminal UI Dashboard

_To be designed after user stories are established._

## Purpose

_When would users use the TUI vs CLI?_

## Layout

_Screen layout and information hierarchy._

## Interactions

_How do users interact with the TUI?_

---

## Preliminary Ideas

> **Note**: The following are preliminary ideas from early brainstorming. They should be validated against user stories and refined before implementation.

### Dashboard Mockup

```
┌─ tt dashboard ──────────────────────────────────────────────────────┐
│                                                                     │
│  TODAY: 6h 23m tracked                          Human: 4h 12m      │
│  ───────────────────────────────────────────    Agent: 2h 11m      │
│                                                                     │
│  ACTIVE CONTEXTS                                                    │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │ ● acme-webapp / Fix auth bug (#1234)          2h 15m  [A]   │   │
│  │   └─ claude-session-abc (running 12m)         ████░░░░      │   │
│  │ ○ personal / claude-code PR review            1h 45m        │   │
│  │ ○ acme-webapp / Dashboard feature (#1235)     45m           │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  AGENT SESSIONS                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │ claude-code  tmux:dev-server:0  acme-webapp    12m  ●       │   │
│  │ claude-code  tmux:dev-server:2  personal       idle         │   │
│  │ codex        tmux:local:1       experiments    3m   ●       │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  [c]ontexts  [a]gents  [r]eport  [s]ettings  [q]uit               │
└─────────────────────────────────────────────────────────────────────┘
```

### Key Bindings (Sketch)

- `c` - Focus contexts list
- `a` - Focus agents list
- `r` - Open report view
- `s` - Settings
- `q` - Quit
- `Enter` - Drill into selected item
- `?` - Help

### TUI is Post-MVP

The TUI dashboard is not needed for MVP. Basic reporting via CLI is sufficient initially.
