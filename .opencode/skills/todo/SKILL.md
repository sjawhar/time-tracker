---
name: todo
description: Use when the user asks what should I do, what's next, todos, priorities, priority alignment, task ranking, deferred work, or am I drifting.
---

# Todo

Use `tt todo` and `tt priority` as the shared source of truth for priorities, todos, and priority/time alignment. The files live in the daily-standups store; `tt` owns deterministic reads, writes, ordering checks, and drift math.

## Operating Rules

1. **`tt todo next` is THE answer.** For “what should I do?”, “what’s next?”, or similar, run `tt todo next` and return only the tight ranked list it prints. Do not make up extra items, strategy digressions, or negative filler.
2. **Faithful edits only.** When the user asks to add, complete, defer, or rank work, use the commands below with the user’s wording. Keep interpretation in chat, not in stored item text.
3. **The list stays clean.** Store only concrete priorities, todos, stream links, dates, pins, and quick flags. Put context, caveats, rationale, and analysis in the conversation.
4. **Partner mode is read-only.** When asked to think, choose, prioritize, or diagnose drift, read `tt todo check` and `tt todo drift`, then reason in chat. Mutate the store only after an explicit edit request.
5. **Order is shared; value is the user’s.** You may reposition todos with `tt todo rank`. Never add, finish, or change priority values unless the user explicitly asks for that priority operation.
6. **Standup integration.** During standup work, resurface `when:` items whose local day has arrived and archive plan-vs-actual notes under `w<NN>/<DATE>.md` in the daily-standups store.

## Command Vocabulary

```bash
tt todo next [--top N --quick --json --by-priority --later]
tt todo ls
tt todo add "<text>" [--priority <slug>...] [--stream <name>] [--due <date>] [--when <date>] [--quick] [--pin]
tt todo done <id>
tt todo defer <id> <date>
tt todo rank <id> --top|--above <id>|--below <id>
tt todo normalize-ids
tt todo check [--json]
tt todo drift [--week|--last-week|--day|--last-day] [--json]

tt priority add "<title>" --value N [--slug <slug>]
tt priority value <slug> N
tt priority ls
tt priority done <slug>

tt streams link <stream-name> <priority-slug>
```

## Model

- Priorities have a user-set scalar importance `value`.
- Todos link to priorities directly with `--priority` or indirectly through a stream linked by `tt streams link`.
- `tt todo drift` compares priority importance against tracked time in two lenses: direct-only and direct+delegated. Unlinked stream time appears as unattributed work.
- Dates are system-local calendar days. `when:` hides a todo until that day; `due:` appears in the Due section and overrides `when:`.

## Conflict Files

Every todo, priority, and stream-link command preflights for Syncthing `*.sync-conflict-*` files. If a command reports one, surface the path and stop; never hide, resolve, or work around a conflict in chat.

## Common Mistakes

| Mistake | Correct behavior |
| --- | --- |
| User asks “what should I do?” and you draft your own plan | Run `tt todo next`; return the concise ranked result |
| User asks for prioritization help | Read `tt todo check` and `tt todo drift`; discuss in chat |
| Adding rationale into a todo title | Store only the requested action text |
| Treating priority values as agent-owned | Ask or wait for an explicit priority command |
| Ignoring arrived deferred work at standup | Resurface current `when:` items and archive plan-vs-actual |
