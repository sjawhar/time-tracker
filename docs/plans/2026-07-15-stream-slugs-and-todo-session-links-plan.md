# Stream Slugs + Todo↔Session Links Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/plans/2026-07-15-stream-slugs-and-todo-session-links-design.md`

**Goal:** Give streams short unique slugs (identity separate from LLM-generated display names) and let agent sessions be linked to todos via `tt todo link`, so classification can use links as evidence, auto-assign repeat sessions, and backfill todo streams.

**Architecture:** Additive schema migration (v9→v10) adds a nullable unique `slug` to streams. Todo-store references (todo.stream, streams.md) move from names to slugs. `Todo` gains a `sessions` array in its hidden JSON metadata; `tt todo link` resolves the current session from agent env vars. Classify reads links at show time (evidence + match-only auto-assign) and backfills todo streams at apply time.

**Tech Stack:** Rust workspace (tt-core / tt-db / tt-cli), rusqlite, serde, clap, insta snapshots.

## Global Constraints

- Slug format: `[a-z0-9]+(-[a-z0-9]+)*`, max 32 chars, validated on every write.
- Streams are NEVER auto-created from assignment references or todo links — only `classify --apply` `streams` definitions create streams.
- Migration adds the column only; it does NOT invent slugs for existing streams.
- Library crates use `thiserror`; tt-cli uses `anyhow` with `.context()`.
- Lint suppressions use `#[expect(clippy::..., reason = "...")]`, never bare `#[allow]`.
- `cargo clippy --all-targets` must pass with zero warnings; `cargo fmt --check` clean.
- **Commits: this repo uses jj with a squash workflow — do NOT commit per task.** All work accumulates in `@`; a single `jj describe` happens at the end (see Final Verification). Ignore any per-step commit instructions from skills.
- Snapshot changes: run `cargo insta review` (or `cargo insta accept` after manually verifying the diff is expected).

## File Structure

```
crates/tt-db/src/lib.rs                      # Stream.slug, schema v10, migration, accessors
crates/tt-core/src/slug.rs                   # NEW: slug format validation
crates/tt-core/src/lib.rs                    # pub mod slug
crates/tt-core/src/todos/model.rs            # Todo.sessions, TodoMetadata.sessions
crates/tt-core/src/todos/parse.rs            # parse sessions from metadata
crates/tt-core/src/todos/render.rs           # render sessions into metadata
crates/tt-cli/src/cli.rs                     # StreamsAction::Slug, TodoAction::{Link,Unlink}
crates/tt-cli/src/main.rs                    # dispatch new subcommands; thread config into classify
crates/tt-cli/src/todo_dispatch.rs           # dispatch Link/Unlink
crates/tt-cli/src/commands/streams.rs        # slug column in list output
crates/tt-cli/src/commands/streams/slug.rs   # NEW: tt streams slug
crates/tt-cli/src/commands/streams/link.rs   # slug-first resolution, writes slug
crates/tt-cli/src/commands/todo/drift.rs     # slug-keyed drift join + name fallback
crates/tt-cli/src/commands/todo/link.rs      # NEW: tt todo link / unlink + session resolution
crates/tt-cli/src/commands/todo/mutate.rs    # Todo construction gains sessions: Vec::new()
crates/tt-cli/src/commands/classify.rs       # StreamDef.slug, no auto-create, linked_todo, auto-assign, backfill
crates/tt-cli/tests/e2e_flow.rs              # end-to-end link → classify → assign/backfill
.opencode/skills/todo/SKILL.md               # instruct agents to link
.opencode/skills/infer-streams/SKILL.md      # slugs in StreamDefs, linked_todo evidence
~/.dotfiles (outside repo)                   # OpenCode plugin injecting OPENCODE_SESSION_ID
```

---

### Task 1: tt-db — `Stream.slug`, schema v10, migration

**Files:**
- Modify: `crates/tt-db/src/lib.rs`

**Interfaces:**
- Produces: `Stream { ..., slug: Option<String> }`; `STREAM_COLUMNS: &str` const; schema v10 with `streams.slug` + unique index. All existing `Stream` constructors in tt-db tests and tt-cli will need `slug: None` added (compiler-driven).

- [ ] **Step 1: Write the failing tests** (in `#[cfg(test)] mod tests` at the bottom of `crates/tt-db/src/lib.rs`, following existing test style):

```rust
#[test]
fn fresh_db_streams_have_nullable_slug() {
    let db = Database::open_in_memory().unwrap();
    insert_test_stream(&db, "s1", "project-x"); // existing helper or inline insert_stream
    let stream = db.get_stream("s1").unwrap().unwrap();
    assert_eq!(stream.slug, None);
}

#[test]
fn migration_from_v9_adds_slug_column() {
    // Create a v9-shaped DB on disk, then reopen through Database::open.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tt.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_info (version INTEGER NOT NULL);
             INSERT INTO schema_info (version) VALUES (9);
             CREATE TABLE streams (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                name TEXT,
                time_direct_ms INTEGER DEFAULT 0,
                time_delegated_ms INTEGER DEFAULT 0,
                first_event_at TEXT,
                last_event_at TEXT,
                needs_recompute INTEGER DEFAULT 0
             );
             INSERT INTO streams (id, created_at, updated_at, name)
             VALUES ('old1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'legacy stream');",
        )
        .unwrap();
    }
    let db = Database::open(&path).unwrap();
    let stream = db.get_stream("old1").unwrap().unwrap();
    assert_eq!(stream.name.as_deref(), Some("legacy stream"));
    assert_eq!(stream.slug, None);
}

#[test]
fn slug_unique_index_rejects_duplicates() {
    let db = Database::open_in_memory().unwrap();
    insert_test_stream(&db, "s1", "a");
    insert_test_stream(&db, "s2", "b");
    db.set_stream_slug("s1", "shared").unwrap();
    let err = db.set_stream_slug("s2", "shared").unwrap_err();
    assert!(matches!(err, DbError::SlugTaken { .. }));
}
```

(The third test compiles only after Task 2 adds `set_stream_slug`; write it now, mark it `#[ignore = "implemented in set_stream_slug task"]`, and un-ignore it in Task 2.)

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-db fresh_db_streams_have_nullable_slug migration_from_v9`
Expected: compile error — `Stream` has no field `slug`.

- [ ] **Step 3: Implement**

1. Add to `Stream` struct (after `name`):

```rust
    /// Short unique identifier (kebab-case), used by todo-store references.
    pub slug: Option<String>,
```

2. Add a column-list const next to `EVENT_COLUMNS` (~line 42), with `slug` LAST so existing row index positions stay stable:

```rust
const STREAM_COLUMNS: &str = "id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute, slug";
```

3. Replace the hardcoded column list in every stream SELECT with `{STREAM_COLUMNS}` via `format!` (the codebase's `prepare(&format!(...))` pattern): `get_stream`, `get_streams`, `get_streams_needing_recompute`, `resolve_stream`.

4. In `row_to_stream` (~line 1258), read `slug: row.get(9)?` (index 9 = last).

5. In `insert_stream`, add the column and param:

```rust
"INSERT INTO streams (id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute, slug)
 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
```

with `stream.slug` appended to `params![...]`.

6. In `init()`'s `execute_batch` CREATE TABLE for streams, add `slug TEXT,` after `name TEXT,` and add alongside the other indexes:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_streams_slug ON streams(slug);
```

(SQLite unique indexes permit multiple NULLs — slug-less streams coexist fine.)

7. Bump `SCHEMA_VERSION` to `10` and restructure the migration match so v8 and v9 both migrate forward:

```rust
match existing_version {
    Some(v) if v == SCHEMA_VERSION => {}
    Some(v @ (8 | 9)) => {
        let tx = self.conn.unchecked_transaction()?;
        if v == 8 {
            tx.execute("ALTER TABLE events ADD COLUMN window_app_id TEXT", [])?;
            tx.execute("ALTER TABLE events ADD COLUMN window_title TEXT", [])?;
        }
        tx.execute("ALTER TABLE streams ADD COLUMN slug TEXT", [])?;
        tx.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_streams_slug ON streams(slug)",
            [],
        )?;
        tx.execute(
            "UPDATE schema_info SET version = ?1",
            params![SCHEMA_VERSION],
        )?;
        tx.commit()?;
    }
    Some(v) => { /* existing mismatch error */ }
    None => {}
}
```

8. Fix every `Stream { ... }` literal the compiler flags (tt-db tests, `classify.rs` `run_apply`, `streams::create`) by adding `slug: None` for now — Task 6 sets it properly in classify.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-db && cargo build`
Expected: PASS (ignored test skipped); whole workspace compiles.

---

### Task 2: tt-db — slug accessors

**Files:**
- Modify: `crates/tt-db/src/lib.rs`

**Interfaces:**
- Produces:
  - `DbError::SlugTaken { slug: String }`
  - `Database::get_stream_by_slug(&self, slug: &str) -> Result<Option<Stream>, DbError>`
  - `Database::set_stream_slug(&self, stream_id: &str, slug: &str) -> Result<(), DbError>`
  - `Database::resolve_stream` now resolves id → slug → name.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn get_stream_by_slug_roundtrip() {
    let db = Database::open_in_memory().unwrap();
    insert_test_stream(&db, "s1", "project-x");
    db.set_stream_slug("s1", "proj-x").unwrap();
    let stream = db.get_stream_by_slug("proj-x").unwrap().unwrap();
    assert_eq!(stream.id, "s1");
    assert_eq!(stream.slug.as_deref(), Some("proj-x"));
    assert!(db.get_stream_by_slug("nope").unwrap().is_none());
}

#[test]
fn set_stream_slug_overwrites_existing() {
    let db = Database::open_in_memory().unwrap();
    insert_test_stream(&db, "s1", "project-x");
    db.set_stream_slug("s1", "old").unwrap();
    db.set_stream_slug("s1", "new").unwrap();
    assert!(db.get_stream_by_slug("old").unwrap().is_none());
    assert_eq!(db.get_stream_by_slug("new").unwrap().unwrap().id, "s1");
}

#[test]
fn resolve_stream_prefers_id_then_slug_then_name() {
    let db = Database::open_in_memory().unwrap();
    insert_test_stream(&db, "s1", "long display name");
    db.set_stream_slug("s1", "short").unwrap();
    assert_eq!(db.resolve_stream("s1").unwrap().unwrap().id, "s1");
    assert_eq!(db.resolve_stream("short").unwrap().unwrap().id, "s1");
    assert_eq!(db.resolve_stream("long display name").unwrap().unwrap().id, "s1");
}
```

Also remove the `#[ignore]` from `slug_unique_index_rejects_duplicates` (Task 1).

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-db slug`
Expected: compile error — no method `set_stream_slug`.

- [ ] **Step 3: Implement**

Add `DbError` variant:

```rust
    /// Slug already assigned to another stream.
    #[error("slug '{slug}' is already in use by another stream")]
    SlugTaken { slug: String },
```

Add methods in the Stream Methods section:

```rust
    /// Retrieves a stream by slug.
    pub fn get_stream_by_slug(&self, slug: &str) -> Result<Option<Stream>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STREAM_COLUMNS} FROM streams WHERE slug = ?1"
        ))?;
        let mut rows = stmt.query(params![slug])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_stream(row)?)),
            None => Ok(None),
        }
    }

    /// Sets (or replaces) a stream's slug.
    ///
    /// Returns `SlugTaken` if another stream already uses the slug.
    pub fn set_stream_slug(&self, stream_id: &str, slug: &str) -> Result<(), DbError> {
        let result = self.conn.execute(
            "UPDATE streams SET slug = ?1, updated_at = ?2 WHERE id = ?3",
            params![slug, format_timestamp(Utc::now()), stream_id],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(DbError::SlugTaken { slug: slug.to_string() })
            }
            Err(e) => Err(e.into()),
        }
    }
```

Extend `resolve_stream`: after the ID check and before the name lookup, insert:

```rust
        // Then try by slug
        if let Some(stream) = self.get_stream_by_slug(query)? {
            return Ok(Some(stream));
        }
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-db`
Expected: PASS, including the un-ignored uniqueness test.

---

### Task 3: tt-core — slug validation

**Files:**
- Create: `crates/tt-core/src/slug.rs`
- Modify: `crates/tt-core/src/lib.rs` (add `pub mod slug;`)

**Interfaces:**
- Produces: `tt_core::slug::validate_slug(slug: &str) -> Result<(), SlugError>` and `SlugError` (thiserror). Used by Tasks 4 and 6.

- [ ] **Step 1: Write the failing tests** (bottom of the new `slug.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_slugs() {
        for slug in ["a", "watcher-rewrite", "eval3-moto", "a1-b2-c3"] {
            assert!(validate_slug(slug).is_ok(), "{slug} should be valid");
        }
    }

    #[test]
    fn rejects_invalid_slugs() {
        for slug in ["", "-leading", "trailing-", "double--dash", "UPPER", "has space", "has_underscore", "ünïcode"] {
            assert!(validate_slug(slug).is_err(), "{slug} should be invalid");
        }
    }

    #[test]
    fn rejects_slugs_over_32_chars() {
        let long = "a".repeat(33);
        assert!(matches!(validate_slug(&long), Err(SlugError::TooLong { .. })));
        assert!(validate_slug(&"a".repeat(32)).is_ok());
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-core slug`
Expected: compile error — module doesn't exist.

- [ ] **Step 3: Implement** `crates/tt-core/src/slug.rs`:

```rust
//! Stream slug format validation.
//!
//! Slugs are short, stable, kebab-case identifiers: `[a-z0-9]+(-[a-z0-9]+)*`, max 32 chars.

use thiserror::Error;

/// Maximum slug length in bytes (slugs are ASCII, so bytes == chars).
pub const MAX_SLUG_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SlugError {
    #[error("slug '{slug}' is invalid: expected lowercase kebab-case ([a-z0-9]+(-[a-z0-9]+)*)")]
    InvalidFormat { slug: String },
    #[error("slug '{slug}' is too long: {len} chars (max {MAX_SLUG_LEN})")]
    TooLong { slug: String, len: usize },
}

/// Validates a stream slug: `[a-z0-9]+(-[a-z0-9]+)*`, max 32 chars.
pub fn validate_slug(slug: &str) -> Result<(), SlugError> {
    if slug.len() > MAX_SLUG_LEN {
        return Err(SlugError::TooLong { slug: slug.to_string(), len: slug.len() });
    }
    let valid = !slug.is_empty()
        && slug.split('-').all(|segment| {
            !segment.is_empty()
                && segment.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(SlugError::InvalidFormat { slug: slug.to_string() })
    }
}
```

Add `pub mod slug;` to `crates/tt-core/src/lib.rs`.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-core slug`
Expected: PASS.

---

### Task 4: tt-cli — `tt streams slug` subcommand

**Files:**
- Create: `crates/tt-cli/src/commands/streams/slug.rs`
- Modify: `crates/tt-cli/src/commands/streams.rs` (declare + re-export module, matching how `link` is wired)
- Modify: `crates/tt-cli/src/cli.rs` (StreamsAction)
- Modify: `crates/tt-cli/src/main.rs` (dispatch)

**Interfaces:**
- Consumes: `db.resolve_stream`, `db.set_stream_slug` (Task 2), `tt_core::slug::validate_slug` (Task 3).
- Produces: `streams::set_slug(db: &Database, stream_ref: &str, slug: &str) -> Result<()>`.

- [ ] **Step 1: Write the failing tests** (in `crates/tt-cli/src/commands/streams/slug.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tt_db::Database;

    fn db_with_stream(id: &str, name: &str) -> Database {
        let db = Database::open_in_memory().unwrap();
        let stream = tt_db::Stream {
            id: id.to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            name: Some(name.to_string()),
            slug: None,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        };
        db.insert_stream(&stream).unwrap();
        db
    }

    #[test]
    fn sets_slug_by_stream_id() {
        let db = db_with_stream("s1", "agent-c: eval-3 moto");
        set_slug(&db, "s1", "eval3-moto").unwrap();
        assert_eq!(db.get_stream_by_slug("eval3-moto").unwrap().unwrap().id, "s1");
    }

    #[test]
    fn rejects_invalid_slug_format() {
        let db = db_with_stream("s1", "x");
        let err = set_slug(&db, "s1", "Bad Slug").unwrap_err();
        assert!(err.to_string().contains("invalid"));
    }

    #[test]
    fn rejects_unknown_stream() {
        let db = Database::open_in_memory().unwrap();
        let err = set_slug(&db, "nope", "fine-slug").unwrap_err();
        assert!(err.to_string().contains("no stream"));
    }

    #[test]
    fn rejects_taken_slug() {
        let db = db_with_stream("s1", "x");
        let stream2 = tt_db::Stream {
            id: "s2".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            name: Some("y".to_string()),
            slug: None,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        };
        db.insert_stream(&stream2).unwrap();
        set_slug(&db, "s1", "taken").unwrap();
        let err = set_slug(&db, "s2", "taken").unwrap_err();
        assert!(err.to_string().contains("already in use"));
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-cli streams::slug`
Expected: compile error — `set_slug` not defined.

- [ ] **Step 3: Implement** `crates/tt-cli/src/commands/streams/slug.rs`:

```rust
use anyhow::{Context, Result, bail};
use tt_core::slug::validate_slug;
use tt_db::Database;

/// Sets a stream's slug. `stream_ref` may be an ID, an existing slug, or an exact name.
pub fn set_slug(db: &Database, stream_ref: &str, slug: &str) -> Result<()> {
    validate_slug(slug)?;
    let Some(stream) = db.resolve_stream(stream_ref).context("failed to resolve stream")? else {
        bail!("no stream matching '{stream_ref}' (tried id, slug, exact name)");
    };
    db.set_stream_slug(&stream.id, slug)
        .with_context(|| format!("failed to set slug on stream {}", stream.id))?;
    let name = stream.name.as_deref().unwrap_or("(unnamed)");
    println!("Set slug '{slug}' on stream {name} ({})", &stream.id[..8.min(stream.id.len())]);
    Ok(())
}
```

Wire the module the same way `link` is wired in `crates/tt-cli/src/commands/streams.rs` (add `mod slug;` / `pub use slug::set_slug;` next to the existing link declarations).

Add to `StreamsAction` in `cli.rs`:

```rust
    /// Set a stream's slug (short stable identifier used by todo references).
    Slug {
        /// Stream reference: ID, existing slug, or exact display name.
        stream: String,

        /// New slug: lowercase kebab-case, max 32 chars.
        slug: String,
    },
```

Add to the `Commands::Streams` match in `main.rs`:

```rust
                StreamsAction::Slug { stream, slug } => streams::set_slug(&db, stream, slug)?,
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-cli streams::slug && cargo build`
Expected: PASS.

---

### Task 5: tt-cli — slug column in `tt streams` list

**Files:**
- Modify: `crates/tt-cli/src/commands/streams.rs` (`StreamEntry`, `get_streams_for_display`, `format_streams`)
- Snapshots: `crates/tt-cli/src/commands/streams/snapshots/`

**Interfaces:**
- Produces: `StreamEntry { ..., slug: Option<String> }` (also serialized in `tt streams --json`).

- [ ] **Step 1: Add `slug` to the display model**

In `StreamEntry` add `pub slug: Option<String>,`. In `get_streams_for_display`'s map, add `slug: stream.slug,` (move it out before `stream.id` is moved — order fields accordingly).

In `format_streams`, add a `Slug` column between `ID` and `Name` (width 16, truncating like `name_display` does with chars-not-bytes; `-` when absent), updating the header, divider, and row `writeln!`s consistently.

- [ ] **Step 2: Run tests, review snapshots**

Run: `cargo test -p tt-cli streams`
Expected: snapshot failures for streams list output.

Run: `cargo insta review` (accept the new column layout after eyeballing alignment).

Run: `cargo test -p tt-cli`
Expected: PASS.

---

### Task 6: classify — `StreamDef.slug` required; assignments never auto-create streams

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`

**Interfaces:**
- Consumes: `validate_slug`, `get_stream_by_slug`.
- Produces: `StreamDef { name: String, slug: String, tags: Vec<String> }`; `run_apply` resolves assignment `stream` refs slug-first-then-unique-name against existing+defined streams and errors on unknown refs. Assignment structs (`SessionAssignment` etc.) are unchanged — their `stream` field now holds a slug (or legacy name).

- [ ] **Step 1: Write/adjust the failing tests**

Update existing `run_apply` tests (~line 1027) — every `StreamDef` literal gains a `slug`. Add new tests following the existing apply-test setup pattern in that module:

```rust
#[test]
fn apply_rejects_unknown_assignment_stream() {
    let db = tt_db::Database::open_in_memory().unwrap();
    // insert one event with session_id "sess-1" using the module's existing event fixture pattern
    let input = ClassifyApplyInput {
        streams: vec![],
        assign_by_session: vec![SessionAssignment {
            session_id: "sess-1".to_string(),
            stream: "never-defined".to_string(),
        }],
        assign_by_pattern: vec![],
        assign_by_event_ids: vec![],
        assign_by_time: vec![],
    };
    let err = apply_input(&db, input).unwrap_err(); // see Step 3: extract testable core
    assert!(err.to_string().contains("unknown stream"));
}

#[test]
fn apply_creates_stream_with_slug_and_resolves_assignment_by_slug() {
    let db = tt_db::Database::open_in_memory().unwrap();
    // insert one event with session_id "sess-1"
    let input = ClassifyApplyInput {
        streams: vec![StreamDef {
            name: "agent-c: eval-3 moto".to_string(),
            slug: "eval3-moto".to_string(),
            tags: vec![],
        }],
        assign_by_session: vec![SessionAssignment {
            session_id: "sess-1".to_string(),
            stream: "eval3-moto".to_string(),
        }],
        assign_by_pattern: vec![],
        assign_by_event_ids: vec![],
        assign_by_time: vec![],
    };
    apply_input(&db, input).unwrap();
    let stream = db.get_stream_by_slug("eval3-moto").unwrap().unwrap();
    assert_eq!(stream.name.as_deref(), Some("agent-c: eval-3 moto"));
}

#[test]
fn apply_rejects_invalid_slug() {
    let db = tt_db::Database::open_in_memory().unwrap();
    let input = ClassifyApplyInput {
        streams: vec![StreamDef {
            name: "x".to_string(),
            slug: "Not A Slug".to_string(),
            tags: vec![],
        }],
        ..empty_apply_input() // add this tiny test helper if not present
    };
    assert!(apply_input(&db, input).is_err());
}

#[test]
fn apply_reapply_same_slug_and_name_is_idempotent() {
    let db = tt_db::Database::open_in_memory().unwrap();
    let def = || StreamDef {
        name: "x".to_string(),
        slug: "x-slug".to_string(),
        tags: vec![],
    };
    apply_input(&db, ClassifyApplyInput { streams: vec![def()], ..empty_apply_input() }).unwrap();
    apply_input(&db, ClassifyApplyInput { streams: vec![def()], ..empty_apply_input() }).unwrap();
    assert_eq!(db.get_streams().unwrap().len(), 1);
}

#[test]
fn apply_rejects_slug_collision_with_different_name() {
    let db = tt_db::Database::open_in_memory().unwrap();
    apply_input(&db, ClassifyApplyInput {
        streams: vec![StreamDef { name: "x".to_string(), slug: "shared".to_string(), tags: vec![] }],
        ..empty_apply_input()
    }).unwrap();
    let err = apply_input(&db, ClassifyApplyInput {
        streams: vec![StreamDef { name: "different".to_string(), slug: "shared".to_string(), tags: vec![] }],
        ..empty_apply_input()
    }).unwrap_err();
    assert!(err.to_string().contains("shared"));
}
```

Note: `ClassifyApplyInput` derives only `Deserialize`; if `..empty_apply_input()` spread requires it, write a `fn empty_apply_input() -> ClassifyApplyInput` helper constructing all fields explicitly instead of using struct-update syntax on a non-Default type.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-cli classify`
Expected: compile errors (missing `slug` field, missing `apply_input`).

- [ ] **Step 3: Implement**

1. `StreamDef` gains `pub slug: String` (no `#[serde(default)]` — required).
2. Split `run_apply` so the JSON-reading shell stays, and the logic becomes a testable `fn apply_input(db: &tt_db::Database, input: ClassifyApplyInput) -> Result<ApplyOutcome>` (define `ApplyOutcome { session_stream_slugs: HashMap<String, String> }` — maps applied `session_id` → assigned stream slug; Task 12's backfill consumes it, so build it in Phase 2 from successful `assign_by_session` applications where the stream has a slug). `run_apply` calls `apply_input` and keeps printing.
3. Phase 1 rework:

```rust
    // Resolve existing streams by slug and by name
    let mut ref_to_id: HashMap<String, String> = HashMap::new();
    let existing = db.get_streams().context("failed to query streams")?;
    for s in &existing {
        if let Some(slug) = &s.slug {
            ref_to_id.insert(slug.clone(), s.id.clone());
        }
        if let Some(name) = &s.name {
            ref_to_id.entry(name.clone()).or_insert_with(|| s.id.clone());
        }
    }

    // Create streams ONLY from definitions
    for def in &input.streams {
        validate_slug(&def.slug)?;
        if let Some(existing) = db.get_stream_by_slug(&def.slug)? {
            if existing.name.as_deref() != Some(def.name.as_str()) {
                bail!(
                    "slug '{}' already belongs to stream '{}'; refusing to reuse it for '{}'",
                    def.slug,
                    existing.name.as_deref().unwrap_or("(unnamed)"),
                    def.name
                );
            }
            // idempotent re-apply
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            let stream = tt_db::Stream {
                id: id.clone(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                name: Some(def.name.clone()),
                slug: Some(def.slug.clone()),
                time_direct_ms: 0,
                time_delegated_ms: 0,
                first_event_at: None,
                last_event_at: None,
                needs_recompute: true,
            };
            db.insert_stream(&stream)
                .with_context(|| format!("failed to create stream: {}", def.name))?;
            println!("Created stream: {} [{}] ({})", def.name, def.slug, &id[..8]);
        }
        let stream = db.get_stream_by_slug(&def.slug)?.expect("just ensured");
        ref_to_id.insert(def.slug.clone(), stream.id.clone());
        ref_to_id.insert(def.name.clone(), stream.id.clone());
    }
```

4. Delete the `all_stream_names` chain + auto-create loop. Every assignment phase looks up `ref_to_id.get(&assignment.stream)` and errors `unknown stream: '{ref}' — define it in "streams" or use an existing slug` when absent. (Name keys keep old LLM outputs working; slug keys are the forward path.)
5. In `run_show`'s output: wherever stream names are surfaced for classified data (the `proposed_stream` / stream listings), include slug — concretely, `SessionSummary.proposed_stream` stays, and the top-level `ClassifyOutput` gains `streams: Vec<StreamRef>` where `StreamRef { id: String, slug: Option<String>, name: Option<String> }` listing streams referenced in the output period, so the LLM always sees id+slug+name together.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-cli classify && cargo insta review`
Expected: PASS after accepting classify snapshot changes.

---

### Task 7: `tt streams link` — slug-first resolution, writes slugs

**Files:**
- Modify: `crates/tt-cli/src/commands/streams/link.rs`
- Modify: `crates/tt-cli/src/commands/streams/tests.rs` (existing link tests)

**Interfaces:**
- Consumes: `db.get_stream_by_slug`, `db.get_streams`.
- Produces: streams.md `StreamPriorityLink.stream` values are slugs for all new links.

- [ ] **Step 1: Write the failing tests.** `link.rs` has no test module today; add `#[cfg(test)] mod tests` at the bottom of `crates/tt-cli/src/commands/streams/link.rs`. `Config` is directly constructible (`Config { database_path, todo_store_path }`, both pub):

```rust
#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tt_db::{Database, Stream};

    use super::*;

    fn fixture() -> (tempfile::TempDir, Database, Config) {
        let temp = tempfile::TempDir::new().unwrap();
        let store = temp.path().join("todo-store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("priorities.md"),
            "- [ ] IPI <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n",
        )
        .unwrap();
        let config = Config {
            database_path: temp.path().join("tt.db"),
            todo_store_path: store,
        };
        let db = Database::open_in_memory().unwrap();
        (temp, db, config)
    }

    fn insert_stream(db: &Database, id: &str, name: &str, slug: Option<&str>) {
        let now = Utc::now();
        db.insert_stream(&Stream {
            id: id.to_string(),
            name: Some(name.to_string()),
            slug: slug.map(String::from),
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        })
        .unwrap();
    }

    fn streams_md(config: &Config) -> String {
        std::fs::read_to_string(config.todo_store_path.join("streams.md")).unwrap()
    }

    #[test]
    fn link_resolves_slug_and_writes_slug() {
        let (_temp, db, config) = fixture();
        insert_stream(&db, "s1", "Project X: the big one", Some("proj-x"));
        link(&db, &config, &LinkOptions { stream: "proj-x".to_string(), priority: "ipi".to_string() }).unwrap();
        assert!(streams_md(&config).contains("- proj-x <!-- tt-stream:{\"priority\":\"ipi\"} -->"));
    }

    #[test]
    fn link_resolves_unique_name_but_writes_slug() {
        let (_temp, db, config) = fixture();
        insert_stream(&db, "s1", "Project X", Some("proj-x"));
        link(&db, &config, &LinkOptions { stream: "Project X".to_string(), priority: "ipi".to_string() }).unwrap();
        let content = streams_md(&config);
        assert!(content.contains("- proj-x "));
        assert!(!content.contains("- Project X "));
    }

    #[test]
    fn link_rejects_stream_without_slug() {
        let (_temp, db, config) = fixture();
        insert_stream(&db, "s1", "Project X", None);
        let err = link(&db, &config, &LinkOptions { stream: "Project X".to_string(), priority: "ipi".to_string() }).unwrap_err();
        assert!(err.to_string().contains("tt streams slug"));
    }
}
```

Note: `link()` calls `validate_priority_exists`, hence the priorities.md fixture. If `load_mutating` requires the store dir to exist, the fixture already creates it.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-cli streams`
Expected: new tests FAIL (link currently writes names and accepts slug-less streams).

- [ ] **Step 3: Implement**

Replace `resolve_exact_stream_name` with a resolver returning the slug to write:

```rust
/// Resolves a stream reference (slug or exact unique name) to the slug that
/// should be written into streams.md.
fn resolve_stream_slug(db: &Database, stream_ref: &str) -> Result<String> {
    if let Some(stream) = db.get_stream_by_slug(stream_ref).context("failed to load stream")? {
        return Ok(stream.slug.expect("fetched by slug"));
    }
    let matches: Vec<_> = db
        .get_streams()
        .context("failed to load streams")?
        .into_iter()
        .filter(|stream| stream.name.as_deref() == Some(stream_ref))
        .collect();
    match matches.as_slice() {
        [] => bail!("no stream with slug or name '{stream_ref}'"),
        [stream] => stream.slug.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "stream '{stream_ref}' has no slug; assign one first: tt streams slug '{stream_ref}' <slug>"
            )
        }),
        _ => bail!("multiple streams named '{stream_ref}'; refer to it by slug instead"),
    }
}
```

`link()` uses the returned slug for both `validate_stream_has_no_link` and the written `StreamPriorityLink.stream`. Update `validate_stream_has_no_link` to compare against the slug.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-cli streams`
Expected: PASS.

---

### Task 8: drift — slug-keyed join with deprecation fallback for name references

**Files:**
- Modify: `crates/tt-cli/src/commands/todo/drift.rs`
- Snapshots: `crates/tt-cli/src/commands/todo/snapshots/` (drift output, if snapshotted)

**Interfaces:**
- Consumes: `Stream.slug`. tt-core `compute_drift` is UNCHANGED — resolution to a common key happens in tt-cli before calling it.
- Produces: drift matches streams.md references by slug (key = slug when present, else name), with unique-name fallback + warning.

- [ ] **Step 1: Write the failing tests** (in drift.rs's existing test module, which already builds DB + report fixtures):

```rust
#[test]
fn drift_matches_links_by_slug() {
    // Follows the existing duplicate_named_db_streams_warn_and_keep_combined_time fixture style.
    let db = Database::open_in_memory().unwrap();
    let created_at = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
    db.insert_stream(&Stream {
        id: "stream-a".to_string(),
        name: Some("agent-c: eval-3 moto".to_string()),
        slug: Some("eval3-moto".to_string()),
        created_at,
        updated_at: created_at,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })
    .unwrap();
    let report_streams = vec![report::ReportStreamTime {
        id: "stream-a".to_string(),
        name: Some("agent-c: eval-3 moto".to_string()),
        time_direct_ms: 60_000,
        time_delegated_ms: 0,
    }];

    let stream_times = stream_times_with_idle_named_streams(&db, &report_streams).unwrap();
    let links = vec![StreamPriorityLink {
        stream: "eval3-moto".to_string(),
        priority: "ipi".to_string(),
    }];
    let (resolved_links, warnings) = resolve_stream_links(&db, links).unwrap(); // new fn, Step 3
    let drift = compute_drift(
        &[Priority {
            slug: "ipi".to_string(),
            value: 9,
            status: PriorityStatus::Active,
            description: None,
        }],
        &resolved_links,
        &stream_times,
    )
    .unwrap();

    assert_eq!(drift.priorities[0].direct_ms, 60_000);
    assert!(warnings.is_empty());
}

#[test]
fn drift_name_reference_falls_back_with_deprecation_warning() {
    // Same fixture as above, but the link references the display NAME.
    let db = Database::open_in_memory().unwrap();
    let created_at = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
    db.insert_stream(&Stream {
        id: "stream-a".to_string(),
        name: Some("agent-c: eval-3 moto".to_string()),
        slug: Some("eval3-moto".to_string()),
        created_at,
        updated_at: created_at,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })
    .unwrap();
    let report_streams = vec![report::ReportStreamTime {
        id: "stream-a".to_string(),
        name: Some("agent-c: eval-3 moto".to_string()),
        time_direct_ms: 60_000,
        time_delegated_ms: 0,
    }];

    let stream_times = stream_times_with_idle_named_streams(&db, &report_streams).unwrap();
    let links = vec![StreamPriorityLink {
        stream: "agent-c: eval-3 moto".to_string(),
        priority: "ipi".to_string(),
    }];
    let (resolved_links, warnings) = resolve_stream_links(&db, links).unwrap();
    assert_eq!(resolved_links[0].stream, "eval3-moto");
    assert_eq!(
        warnings,
        vec!["streams.md reference 'agent-c: eval-3 moto' matches by name; update to slug 'eval3-moto'".to_string()]
    );

    let drift = compute_drift(
        &[Priority {
            slug: "ipi".to_string(),
            value: 9,
            status: PriorityStatus::Active,
            description: None,
        }],
        &resolved_links,
        &stream_times,
    )
    .unwrap();
    assert_eq!(drift.priorities[0].direct_ms, 60_000);
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-cli drift`
Expected: second test FAILS (no warning today), first may fail (slug keys unknown).

- [ ] **Step 3: Implement**

In `stream_times_with_idle_named_streams`, key each stream by slug when present:

```rust
    let key = |stream: &tt_db::Stream| -> Option<String> {
        stream.slug.clone().or_else(|| stream.name.clone())
    };
```

Use that key as `StreamTimeInput.stream_name` for DB streams, and map `report_streams` entries through a `stream_id → key` lookup built from `db.get_streams()` (falling back to the report entry's own name/id as today when the stream row is gone).

Before calling `compute_drift`, resolve links through a new named function (the tests above call it directly):

```rust
/// Resolves streams.md link references to canonical stream keys (slug when the
/// stream has one, else name). Name references to slugged streams are rewritten
/// to the slug with a deprecation warning. Unknown refs pass through untouched
/// so compute_drift keeps erroring on them as today.
fn resolve_stream_links(
    db: &Database,
    links: Vec<StreamPriorityLink>,
) -> Result<(Vec<StreamPriorityLink>, Vec<String>)> {
    let streams = db.get_streams().context("failed to get streams for link resolution")?;
    let known_keys: std::collections::HashSet<String> = streams
        .iter()
        .filter_map(|s| s.slug.clone().or_else(|| s.name.clone()))
        .collect();
    let mut resolved_links = Vec::with_capacity(links.len());
    let mut warnings = Vec::new();
    for link in links {
        if known_keys.contains(&link.stream) {
            resolved_links.push(link);
            continue;
        }
        let by_name: Vec<_> = streams
            .iter()
            .filter(|s| s.name.as_deref() == Some(link.stream.as_str()))
            .collect();
        if let [stream] = by_name.as_slice() {
            if let Some(slug) = &stream.slug {
                warnings.push(format!(
                    "streams.md reference '{}' matches by name; update to slug '{slug}'",
                    link.stream
                ));
                resolved_links.push(StreamPriorityLink {
                    stream: slug.clone(),
                    priority: link.priority,
                });
                continue;
            }
        }
        resolved_links.push(link);
    }
    Ok((resolved_links, warnings))
}
```

Pass `resolved_links` to `compute_drift`; surface `warnings` through the existing warning-printing path.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-cli drift && cargo insta review`
Expected: PASS.

---

### Task 9: tt-core — `Todo.sessions`

**Files:**
- Modify: `crates/tt-core/src/todos/model.rs` (`Todo`, `TodoMetadata`)
- Modify: `crates/tt-core/src/todos/parse.rs` (`todo_from_metadata`)
- Modify: `crates/tt-core/src/todos/render.rs` (TodoMetadata construction, ~line 71)
- Modify: `crates/tt-cli/src/commands/todo/mutate.rs` + any other `Todo { ... }` literals (compiler-driven; includes `raw.rs` and test fixtures)

**Interfaces:**
- Produces: `Todo { ..., sessions: Vec<String> }` — serialized as `"sessions":[...]` in the hidden JSON, omitted when empty; absent field parses as empty (backward compatible despite `deny_unknown_fields`, which rejects unknown fields, not missing ones).

- [ ] **Step 1: Write the failing tests** (in `crates/tt-core/src/todos/tests.rs`, matching its round-trip test style):

```rust
#[test]
fn todo_sessions_roundtrip() {
    let line = "- [ ] Fix watcher <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"ses_abc\",\"0199-uuid\"]} -->\n";
    let (file, diagnostics) = parse_todos(line);
    assert!(diagnostics.is_empty());
    let TodoFileItem::Todo(todo) = &file.items[0].item else { panic!("expected todo") };
    assert_eq!(todo.sessions, vec!["ses_abc", "0199-uuid"]);
    // Re-render preserves sessions
    assert!(file.to_string().contains("\"sessions\":[\"ses_abc\",\"0199-uuid\"]"));
}

#[test]
fn todo_without_sessions_parses_and_renders_without_field() {
    let line = "- [ ] Old todo <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    let (file, diagnostics) = parse_todos(line);
    assert!(diagnostics.is_empty());
    let TodoFileItem::Todo(todo) = &file.items[0].item else { panic!("expected todo") };
    assert!(todo.sessions.is_empty());
    assert!(!file.to_string().contains("sessions"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-core todos`
Expected: compile error — no field `sessions`.

- [ ] **Step 3: Implement**

`Todo` gains (after `block`):

```rust
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
```

`TodoMetadata` gains the same field/attributes. `todo_from_metadata` maps `sessions: metadata.sessions`. The `TodoMetadata` construction in `render.rs` adds `sessions: todo.sessions.clone()` (match the field-by-field style there). Fix all `Todo { ... }` literals the compiler flags with `sessions: Vec::new()`.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-core && cargo build`
Expected: PASS; workspace compiles.

---

### Task 10: `tt todo link` / `tt todo unlink` + `--stream` slug validation

**Files:**
- Create: `crates/tt-cli/src/commands/todo/link.rs`
- Modify: `crates/tt-cli/src/commands/todo/mod.rs` or `todo.rs` module root (wire `pub use link::{run_link, run_unlink};` following how `mutate` fns are exported)
- Modify: `crates/tt-cli/src/cli.rs` (TodoAction)
- Modify: `crates/tt-cli/src/todo_dispatch.rs`
- Modify: `crates/tt-cli/src/commands/todo/mutate.rs` (`run_add` slug validation, Steps 5-6)
- Modify: `crates/tt-cli/src/main.rs` (`Commands::Todo` arm opens DB for `Add` with `--stream`)

**Interfaces:**
- Consumes: `load_mutating`, `write_todos`, `unique_todo_line_index` pattern from `mutate.rs` (make that helper `pub(super)` if private).
- Produces:
  - `todo::run_link(config: &Config, id: &str, session: Option<String>) -> Result<()>`
  - `todo::run_unlink(config: &Config, id: &str, session: Option<String>) -> Result<()>`
  - `resolve_session_id(explicit: Option<String>, claude_env: Option<String>, opencode_env: Option<String>) -> Result<String>` (pure, tested directly)
  - `todo::run_add(config, db: Option<&tt_db::Database>, options)` — signature change; validates `--stream` slug exists in DB (spec: error, not create)

- [ ] **Step 1: Write the failing tests** (in `link.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_prefers_explicit_then_claude_then_opencode() {
        assert_eq!(
            resolve_session_id(Some("x".into()), Some("c".into()), Some("o".into())).unwrap(),
            "x"
        );
        assert_eq!(resolve_session_id(None, Some("c".into()), Some("o".into())).unwrap(), "c");
        assert_eq!(resolve_session_id(None, None, Some("o".into())).unwrap(), "o");
        let err = resolve_session_id(None, None, None).unwrap_err();
        assert!(err.to_string().contains("no agent session detected"));
    }

    #[test]
    fn resolution_ignores_empty_env_values() {
        assert_eq!(
            resolve_session_id(None, Some(String::new()), Some("o".into())).unwrap(),
            "o"
        );
    }
}
```

Plus file-level tests for link/unlink using the same tempdir + `Config` fixture pattern the other todo command tests use:

```rust
#[test]
fn link_appends_session_id_idempotently() {
    // seed todos.md with one todo td_1 (no sessions)
    // run_link(config, "td_1", Some("ses_abc")) twice
    // reload store: todo.sessions == ["ses_abc"]
}

#[test]
fn unlink_removes_session_and_errors_when_absent() {
    // seed todo with sessions ["ses_abc"]
    // run_unlink(config, "td_1", Some("ses_abc")) -> ok, sessions empty
    // run_unlink again -> error contains "not linked"
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-cli todo::link`
Expected: compile error.

- [ ] **Step 3: Implement** `crates/tt-cli/src/commands/todo/link.rs`:

```rust
use anyhow::{Result, bail};
use tt_core::todos::TodoFileItem;

use super::mutate::unique_todo_line_index;
use crate::Config;
use crate::todo_store::{load_mutating, write_todos};

/// Resolves the agent session to link, in priority order:
/// explicit flag > Claude Code env > OpenCode env.
fn resolve_session_id(
    explicit: Option<String>,
    claude_env: Option<String>,
    opencode_env: Option<String>,
) -> Result<String> {
    let non_empty = |value: Option<String>| value.filter(|v| !v.is_empty());
    non_empty(explicit)
        .or_else(|| non_empty(claude_env))
        .or_else(|| non_empty(opencode_env))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no agent session detected (CLAUDE_CODE_SESSION_ID / OPENCODE_SESSION_ID unset); pass --session <id>"
            )
        })
}

fn session_from_env(explicit: Option<String>) -> Result<String> {
    resolve_session_id(
        explicit,
        std::env::var("CLAUDE_CODE_SESSION_ID").ok(),
        std::env::var("OPENCODE_SESSION_ID").ok(),
    )
}

pub fn run_link(config: &Config, id: &str, session: Option<String>) -> Result<()> {
    let session_id = session_from_env(session)?;
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    if todo.sessions.iter().any(|s| s == &session_id) {
        println!("Already linked: {session_id} → {id} \"{}\"", todo.text);
        return Ok(());
    }
    todo.sessions.push(session_id.clone());
    let text = todo.text.clone();
    write_todos(config, &loaded.store.todos)?;
    println!("Linked {session_id} → {id} \"{text}\"");
    Ok(())
}

pub fn run_unlink(config: &Config, id: &str, session: Option<String>) -> Result<()> {
    let session_id = session_from_env(session)?;
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    let before = todo.sessions.len();
    todo.sessions.retain(|s| s != &session_id);
    if todo.sessions.len() == before {
        bail!("session {session_id} is not linked to todo '{id}'");
    }
    let text = todo.text.clone();
    write_todos(config, &loaded.store.todos)?;
    println!("Unlinked {session_id} from {id} \"{text}\"");
    Ok(())
}
```

Add to `TodoAction` in `cli.rs`:

```rust
    /// Link the current agent session to a todo.
    ///
    /// Session is auto-detected from CLAUDE_CODE_SESSION_ID or OPENCODE_SESSION_ID.
    Link {
        id: String,

        /// Explicit session ID (overrides env detection).
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },

    /// Remove an agent session link from a todo.
    Unlink {
        id: String,

        /// Explicit session ID (overrides env detection).
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
```

Add to `run_todo_action` in `todo_dispatch.rs`:

```rust
        TodoAction::Link { id, session } => todo::run_link(config, id, session.clone()),
        TodoAction::Unlink { id, session } => todo::run_unlink(config, id, session.clone()),
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-cli todo && cargo build`
Expected: PASS.

- [ ] **Step 5: `tt todo add --stream <slug>` validates against the DB — write the failing test** (in `mutate.rs`'s test module or `link.rs` if mutate has none; same tempdir `Config` fixture as Task 7):

```rust
#[test]
fn add_with_stream_requires_existing_slug() {
    // db: in-memory, one stream with slug "proj-x"
    // run_add with stream: Some("proj-x".into()) and Some(&db) -> ok
    // run_add with stream: Some("typo-slug".into()) and Some(&db) -> err containing "no stream with slug"
    // run_add with stream: None and None db -> ok (no db needed)
}
```

(Write it as real code once the signature lands: `run_add(config, db: Option<&Database>, options)`.)

- [ ] **Step 6: Implement the validation**

1. `run_add` gains a `db: Option<&tt_db::Database>` parameter. At the top:

```rust
    if let Some(slug) = options.stream.as_deref() {
        let db = db.context("--stream requires the database")?;
        if db.get_stream_by_slug(slug)
            .context("failed to look up stream slug")?
            .is_none()
        {
            bail!("no stream with slug '{slug}'; create it via classification or set one with: tt streams slug <stream> {slug}");
        }
    }
```

2. In `main.rs`, the `Commands::Todo` arm currently opens the DB only for `Drift`; extend the condition:

```rust
            if matches!(action, TodoAction::Drift { .. })
                || matches!(action, TodoAction::Add { stream: Some(_), .. })
            {
                let (db, config) = open_database(cli.config.as_deref())?;
                run_todo_action(Some(&db), &config, action)?;
            } else {
                let config = load_config(cli.config.as_deref())?;
                run_todo_action(None, &config, action)?;
            }
```

3. In `todo_dispatch.rs`, pass `db` through to `run_add`.

- [ ] **Step 7: Run tests to verify pass**

Run: `cargo test -p tt-cli && cargo build`
Expected: PASS.

---

### Task 11: classify show — `linked_todo` evidence + match-only auto-assign

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`
- Modify: `crates/tt-cli/src/main.rs` (pass `&config` to `run_show`)
- Snapshots: classify output snapshots

**Interfaces:**
- Consumes: `load_read_only(config)` from `todo_store`, `db.get_stream_by_slug`, `db.assign_events_by_session_id`, `Todo.sessions`.
- Produces:
  - `run_show(db, config, ...)` — new `config: &Config` parameter after `db`.
  - `SessionSummary` gains `#[serde(skip_serializing_if = "Option::is_none")] linked_todo: Option<LinkedTodo>` with `struct LinkedTodo { id: String, text: String, stream_slug: Option<String> }`.
  - Auto-assign writes use `assignment_source = "todo_link"`.

- [ ] **Step 1: Write the failing tests**

Extract the link-processing into a testable unit and test it directly:

```rust
/// Maps session_id -> the todo that links it (first wins; a session linked to
/// multiple todos keeps the first todo in file order and emits a note line).
fn build_session_todo_index(loaded: &LoadedTodoStore) -> HashMap<String, LinkedTodo> { ... }

/// For every linked session whose todo has a stream slug matching an existing
/// DB stream, assign the session's events ("todo_link" source).
/// Returns human-readable notes for unmatched slugs.
fn auto_assign_linked_sessions(
    db: &tt_db::Database,
    index: &HashMap<String, LinkedTodo>,
) -> Result<Vec<String>> { ... }
```

```rust
#[test]
fn auto_assign_matches_existing_slug_only() {
    let db = tt_db::Database::open_in_memory().unwrap();
    // stream with slug "proj-x"; two events with session_id "sess-1"; one event "sess-2"
    let mut index = HashMap::new();
    index.insert("sess-1".to_string(), LinkedTodo {
        id: "td_1".to_string(),
        text: "do the thing".to_string(),
        stream_slug: Some("proj-x".to_string()),
    });
    index.insert("sess-2".to_string(), LinkedTodo {
        id: "td_2".to_string(),
        text: "other".to_string(),
        stream_slug: Some("no-such-stream".to_string()),
    });
    let notes = auto_assign_linked_sessions(&db, &index).unwrap();
    // sess-1 events now have stream_id of proj-x stream + assignment_source "todo_link"
    // sess-2 untouched; notes mention "no-such-stream"
    assert_eq!(notes.len(), 1);
}

#[test]
fn session_todo_index_reads_sessions_from_store() {
    let loaded = parse_store_contents(
        "",
        "- [ ] Fix it <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[],\"stream\":\"proj-x\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"ses_abc\"]} -->\n",
        "",
    );
    let index = build_session_todo_index(&loaded);
    assert_eq!(index["ses_abc"].stream_slug.as_deref(), Some("proj-x"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tt-cli classify`
Expected: compile errors.

- [ ] **Step 3: Implement**

1. Define `LinkedTodo` (Serialize, Clone, Debug) and the two functions above. `build_session_todo_index` walks `loaded.store.todos.items`, skipping done todos (`todo.done`), inserting each session ID → LinkedTodo (first todo wins on duplicates).
2. `auto_assign_linked_sessions`: for each `(session_id, linked)` with `Some(slug) = &linked.stream_slug`: `db.get_stream_by_slug(slug)?` — on `Some(stream)`, `db.assign_events_by_session_id(session_id, &stream.id, "todo_link")?`; on `None`, push note `format!("todo {} references slug '{slug}' with no matching stream; session {session_id} left unclassified", linked.id)`.
3. `run_show` gains `config: &Config` (second parameter). At the top: `let loaded = crate::todo_store::load_read_only(config)?;` then build index, run auto-assign (BEFORE querying events, so assignments are reflected in this very output), print notes to stderr via `eprintln!` (keeps `--json` stdout clean).
4. When building each `SessionSummary`, set `linked_todo: session_todo_index.get(&session.session_id).cloned()`.
5. Update `main.rs` call site (line ~225) to pass `&config` (the classify arm already has config in scope via `open_database`).

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-cli classify && cargo insta review`
Expected: PASS after accepting snapshot additions.

---

### Task 12: classify apply — backfill todo streams

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs` (`run_apply` shell)
- Modify: `crates/tt-cli/src/main.rs` (pass `&config` to `run_apply`)

**Interfaces:**
- Consumes: `ApplyOutcome.session_stream_slugs` (Task 6), `load_mutating`, `write_todos`.
- Produces: `run_apply(db, config, input_path)`; `fn backfill_todo_streams(config: &Config, session_stream_slugs: &HashMap<String, String>) -> Result<Vec<String>>` returning report lines.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn backfill_sets_stream_on_streamless_linked_todos_only() {
    // tempdir Config; todos.md with:
    //   td_1: sessions ["sess-1"], stream null      -> gets backfilled
    //   td_2: sessions ["sess-1"], stream "existing" -> untouched
    //   td_3: sessions ["sess-9"], stream null      -> untouched (session not in map)
    let mut map = HashMap::new();
    map.insert("sess-1".to_string(), "proj-x".to_string());
    let lines = backfill_todo_streams(&config, &map).unwrap();
    // reload store: td_1.stream == Some("proj-x"); td_2.stream == Some("existing"); td_3.stream == None
    assert_eq!(lines, vec!["Backfilled stream 'proj-x' → td_1"]);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p tt-cli backfill`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
/// After apply, write stream slugs onto streamless todos linked to newly
/// assigned sessions. Skips the todo store entirely when the map is empty.
fn backfill_todo_streams(
    config: &Config,
    session_stream_slugs: &HashMap<String, String>,
) -> Result<Vec<String>> {
    if session_stream_slugs.is_empty() {
        return Ok(Vec::new());
    }
    let mut loaded = crate::todo_store::load_mutating(config)?;
    let mut lines = Vec::new();
    for file_line in &mut loaded.store.todos.items {
        let TodoFileItem::Todo(todo) = &mut file_line.item else { continue };
        if todo.stream.is_some() || todo.done {
            continue;
        }
        let Some(slug) = todo
            .sessions
            .iter()
            .find_map(|session| session_stream_slugs.get(session))
        else {
            continue;
        };
        todo.stream = Some(slug.clone());
        lines.push(format!("Backfilled stream '{slug}' → {}", todo.id));
    }
    if !lines.is_empty() {
        crate::todo_store::write_todos(config, &loaded.store.todos)?;
    }
    Ok(lines)
}
```

In Task 6's `apply_input`, populate `ApplyOutcome.session_stream_slugs` in Phase 2: for each successful `assign_by_session`, look up the assigned stream's slug (`ref_to_id` → `db.get_stream(id)` → `stream.slug`); insert only when the slug exists. `run_apply` calls `backfill_todo_streams` after `apply_input` and prints each returned line. Update the `main.rs` call site to pass `&config`.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p tt-cli`
Expected: PASS.

---

### Task 13: end-to-end test

**Files:**
- Modify: `crates/tt-cli/tests/e2e_flow.rs`

**Interfaces:**
- Consumes: everything above via the actual binary. Uses `TT_DATABASE_PATH` and `TT_TODO_STORE_PATH` env overrides (both already supported by Figment config) plus `OPENCODE_SESSION_ID`.

- [ ] **Step 1: Write the test** (following the file's existing `Command`-spawning helpers):

```text
Flow (all via the tt binary in a tempdir sandbox):
 1. tt todo add "Prototype watcher rewrite"           → capture td id from todos.md
 2. tt todo link <td_id>  with env OPENCODE_SESSION_ID=ses_e2e  → exit 0, output contains "Linked ses_e2e"
 3. tt ingest / seed an event with session_id ses_e2e (reuse the file's existing event-seeding helper)
 4. echo '{"streams":[{"name":"Watcher rewrite work","slug":"watcher-rewrite","tags":[]}],
           "assign_by_session":[{"session_id":"ses_e2e","stream":"watcher-rewrite"}]}' | tt classify --apply -
    → output contains "Backfilled stream 'watcher-rewrite' → <td_id>"
 5. Assert todos.md now contains "stream":"watcher-rewrite" on the todo line
 6. tt todo link <td_id> with NO session env and no --session → exit non-zero,
    stderr contains "no agent session detected"
```

Write it as a real `#[test] fn e2e_todo_session_link_flow()` with explicit `Command` invocations and assertions; use `env_remove("CLAUDE_CODE_SESSION_ID")` on every command to isolate from the developer's own agent environment (these tests may themselves run inside an agent session).

- [ ] **Step 2: Run it**

Run: `cargo test -p tt-cli --test e2e_flow e2e_todo_session_link_flow`
Expected: PASS. If any step fails, fix the underlying task code (not the test).

---

### Task 14: skill + docs updates

**Files:**
- Modify: `.opencode/skills/todo/SKILL.md`
- Modify: `.opencode/skills/infer-streams/SKILL.md`
- Modify: `AGENTS.md` (commands list) and `crates/tt-db/AGENTS.md` if it documents the streams schema

- [ ] **Step 1: `.opencode/skills/todo/SKILL.md`** — add a section instructing agents:

```markdown
## Linking your session

When you start working on a todo, link your session to it (zero-arg session
detection from CLAUDE_CODE_SESSION_ID / OPENCODE_SESSION_ID):

    tt todo link <todo-id>

This feeds classification: your session inherits the todo's stream, or the
todo acquires a stream when the session gets classified.
```

- [ ] **Step 2: `.opencode/skills/infer-streams/SKILL.md`** — read the file, then update its `tt classify --apply` JSON documentation:
  - Every entry in `"streams"` now REQUIRES a `"slug"` (lowercase kebab-case, ≤32 chars, stable identity — pick short memorable slugs like `watcher-rewrite`).
  - `assign_by_*` `"stream"` values should reference slugs.
  - New evidence field: sessions may carry `"linked_todo": {"id", "text", "stream_slug"}` — treat sessions sharing a `linked_todo.id` as the same stream, and use the todo text when naming/slugging new streams.

- [ ] **Step 3: Update the stale AGENTS.md line** — root `AGENTS.md` says "No migrations: Schema version mismatch = hard error"; the code (and now this feature) does additive forward migrations. Correct it to: "Additive migrations only: supported older schema versions migrate forward in `init()`; unsupported versions fail fast." Add `tt streams slug` and `tt todo link` to the commands list.

- [ ] **Step 4: Verify**

Run: `grep -rn "slug" .opencode/skills/ AGENTS.md | head -20`
Expected: updated docs mention slugs coherently.

---

### Task 15: OpenCode plugin — `OPENCODE_SESSION_ID` (outside this repo)

**Files:**
- Create: `~/.dotfiles/.config/opencode/plugin/session-env.js` (adjust to wherever dotfiles keeps opencode plugins; check `~/.dotfiles` layout and the user's opencode config for the plugin directory convention before writing)

**Interfaces:**
- Produces: every Bash tool invocation inside OpenCode carries `OPENCODE_SESSION_ID=<current session>`.

- [ ] **Step 1: Write the plugin.** OpenCode plugins export an async factory; the `tool.execute.before` hook receives `(input, output)` where `input.sessionID` identifies the session and `output.args` holds the bash command. Prepending an `export` preserves per-session correctness even though the server process is shared:

```javascript
export const SessionEnvPlugin = async () => ({
  "tool.execute.before": async (input, output) => {
    if (input.tool !== "bash" || !input.sessionID) return;
    const id = input.sessionID.replace(/[^A-Za-z0-9_-]/g, "");
    output.args.command = `export OPENCODE_SESSION_ID='${id}'; ${output.args.command}`;
  },
});
```

Verify the exact hook signature against the installed OpenCode version's plugin docs (`sjawhar/opencode` fork) before finalizing — if the fork exposes a cleaner per-tool env mechanism, prefer that.

- [ ] **Step 2: Verify end-to-end.** In a fresh OpenCode session run `echo $OPENCODE_SESSION_ID`.
Expected: prints the current session ID (a `ses_...` value). Then `tt todo link <some-todo-id>` with no flags links that session.

---

## Final Verification

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets` — zero warnings
- [ ] `cargo test` — full workspace
- [ ] `cargo deny check`
- [ ] Manual QA (real usage, not just tests):
  - `tt streams slug <ref> <slug>` on the real DB against one live stream; `tt streams` shows the slug.
  - `tt todo link <id> --session ses_manual` then inspect `~/.local/share/time-tracker/todos.md`.
  - `OPENCODE_SESSION_ID=ses_x tt todo link <id>` (env detection path).
  - `tt todo drift` still runs clean against the real store (name-fallback warnings at most).
- [ ] Single commit: `jj describe -m "feat: stream slugs + todo-session links"` (work stays in `@` per squash workflow; push only when asked).

## Rollout (after merge/deploy)

1. Deploy the new binary everywhere first (`scripts/deploy-remote.sh`) — old binaries reject todo lines containing `sessions` and DBs migrated to v10.
2. One-time manual slugging: assign slugs to the ~16 streams referenced in `~/.local/share/time-tracker/streams.md` (and any other streams worth referencing) via `tt streams slug` — human/agent-chosen names, dispatched as a batch of subagents if desired. NOT automated derivation.
3. Install the OpenCode plugin in dotfiles; confirm `echo $OPENCODE_SESSION_ID` in a fresh session.
