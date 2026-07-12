# Stream Slugs + Todo↔Session Links

## Problem

1. **Streams have no stable, human-usable identifier.** A stream has an opaque ULID and a
   long LLM-generated display name (`"agent-c: eval-3 opentofu-localstack"`). Every
   todo-store reference (streams.md priority links, `tt todo drift`) joins on the full name
   string. Nothing enforces name uniqueness, so drift has to warn about duplicate names, and
   a rename silently breaks references. Nobody can type these names by hand.

2. **Agent sessions can't be associated with todos.** Sessions get stream assignments only
   through classification, where the LLM has to re-derive "what was this session about" from
   prompts and file paths. A direct session→todo link would make classification far more
   accurate, and would let todos acquire streams automatically over time.

These are fixed together: session→todo links need a stream reference that is stable and
typeable, which is what slugs provide.

## Part 1: Stream Slugs

Streams gain a `slug` — short, unique, stable, kebab-case (e.g. `watcher-rewrite`,
`eval3-moto`). This mirrors how priorities already work in the todo store
(`"priority":["ipi"]`). The name remains as a long display description. Slug is identity;
name is description.

### Schema (tt-db)

- `streams` gains a nullable `slug TEXT` column with a `CREATE UNIQUE INDEX` (SQLite
  can't add UNIQUE constraints via `ALTER TABLE`; a unique index permits multiple NULLs).
- `SCHEMA_VERSION` 9 → 10, following the existing additive-migration mechanism. The
  migration only adds the column — it does **not** invent slugs.
- Existing streams start slug-less. Slugs are written manually (agent- or human-chosen,
  never mechanically derived) via a new `tt streams slug <stream-ref> <slug>` setter.
  The backlog is a one-time rollout task: only streams that need referencing get slugs
  (~16 referenced in streams.md today, out of 662 total — the rest are dead history).
  Slug-less streams simply can't be referenced from the todo store until given one.
- Slug format validated on every write: `[a-z0-9]+(-[a-z0-9]+)*`, max 32 chars.
- New accessor: `get_stream_by_slug(&str) -> Option<Stream>`.

### Stream creation (classify)

- `ClassifyApplyInput::StreamDef` gains a required `slug` field. The classifying LLM
  proposes the slug along with the name. `--apply` rejects a slug that collides with an
  existing stream's slug unless the name also matches (idempotent re-apply).
- All classify JSON output that shows streams includes the slug.

### Todo-store references switch to slugs

- `todo.stream` and streams.md `StreamPriorityLink.stream` hold slugs going forward.
- `tt streams link <ref> <priority>` resolves `<ref>` as slug first, then as exact unique
  name (convenience). It always **writes** the slug.
- `tt todo drift` joins on slug. For backward compatibility with existing streams.md lines,
  a reference that matches no slug falls back to unique-name matching and emits a
  deprecation diagnostic ("reference 'X' matches by name; update to slug 'Y'").
  Duplicate-name warnings disappear for slug references (structurally impossible).
- `tt streams` display gains a slug column.
- `tt todo add --stream <slug>` validates the slug exists in the DB; error (not create) if
  it doesn't.

## Part 2: Todo↔Session Links

### Data model

`Todo` gains `sessions: Vec<String>` (agent session IDs), stored in the hidden JSON
metadata, `#[serde(default)]`, omitted when empty:

```
- [ ] Prototype watcher rewrite <!-- tt-todo:{"id":"td_abc123",...,"sessions":["ses_9f2k..."]} -->
```

No source field: `agent_sessions` is keyed by `session_id` alone, and the ID format
identifies the source (`ses_` = OpenCode, UUID = Claude).

Deployment note: `TodoMetadata` uses `deny_unknown_fields`, so older `tt` binaries reject
todo lines containing `sessions`. Deploy the new binary to all machines before linking.

### CLI

```
tt todo link <todo-id> [--session <id>]
tt todo unlink <todo-id> [--session <id>]
```

Session ID resolution order:

1. `--session <id>` (explicit override)
2. `$CLAUDE_CODE_SESSION_ID` (set by Claude Code ≥ 2.1.132 in Bash tool subprocesses)
3. `$OPENCODE_SESSION_ID` (requires the plugin below)
4. Error: "no agent session detected; pass --session"

`link` is an idempotent append (no-op if already linked). It does **not** validate the
session exists in `agent_sessions` — in-progress sessions haven't been scanned yet. It
prints what it did: `Linked ses_9f2k... → td_abc123 "Prototype watcher rewrite"`.
`unlink` removes the resolved session ID; errors if not linked.

### Classify integration

Links take effect at classify time, not link time (events arriving after the link are
covered automatically):

- **Evidence (primary path)**: every linked session in `tt classify` output gains
  `linked_todo: {id, text, stream_slug?}`. Sessions linking to the same todo are strong
  grouping evidence for the LLM.
- **Auto-assign, match-only**: if the linked todo has a `stream` slug that matches an
  existing DB stream, the session's events are assigned to that stream and the session
  leaves the unclassified pile. No stream is ever created by this path — only `--apply`
  StreamDefs create streams. A slug with no DB match leaves the session unclassified with a
  note in output.
- **Backfill**: in `tt classify --apply`, after session assignments run, any streamless
  todo linked to a newly-assigned session gets its `stream` field set to that stream's
  slug. Reported in output: `Backfilled stream 'watcher-rewrite' → td_abc123`. This is how
  todos acquire streams — from classification, never from hand-typed names.

Net effect: the first session on a todo gets classified by the LLM (with the todo as
evidence), which backfills the todo's stream; every later session linked to that todo is
auto-assigned with no LLM involvement.

### Agent integration

- **OpenCode plugin** (prerequisite, lives in dotfiles — not a core patch): a tiny
  plugin that injects `OPENCODE_SESSION_ID` into bash tool subprocess env per call
  (the `tool.execute.before` hook knows the session ID), mirroring Claude Code's
  `CLAUDE_CODE_SESSION_ID`.
- **`.opencode/skills/todo` skill**: add instruction — when starting work on a todo, run
  `tt todo link <todo-id>`.
- **`.opencode/skills/infer-streams` skill**: update for slugs (StreamDef requires slug)
  and `linked_todo` evidence.

## Testing

- Parse/serialize round-trip for `sessions` metadata; old lines without the field.
- `tt streams slug` setter: format validation, uniqueness rejection, re-slug of a
  stream that already has one.
- Session resolution chain (env vars set/unset, precedence, error case).
- Classify: auto-assign by slug, no-match note, backfill write, `linked_todo` in JSON.
- Drift/link slug matching + name-fallback deprecation diagnostic.
- Snapshot updates (`tt streams` slug column, classify output).
- E2E: link via env var → classify --apply → assert assignment + backfill.

## Out of Scope

- Syncthing setup between this machine and devbox (ops task, tracked separately).
- Any automatic linking without the agent running `tt todo link` (no reliable signal
  exists for "this session is working on todo X" other than the agent saying so).
