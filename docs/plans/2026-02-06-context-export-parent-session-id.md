# Fix: context export drops parent_session_id from agent sessions

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `parent_session_id` and `session_type` fields to `AgentExport` so `tt context --agents` output includes parent-child session relationships.

**Architecture:** The data already flows correctly through parsing (session.rs) and storage (tt-db). The only gap is the export mapping in `context.rs` — `AgentExport` is missing two fields and `export_agents` doesn't copy them from `AgentSession`.

**Tech Stack:** Rust, serde, chrono

---

### Task 1: Add fields to `AgentExport` struct

**Files:**
- Modify: `crates/tt-cli/src/commands/context.rs:64-79`

**Step 1: Add `parent_session_id` and `session_type` to the struct**

Add two fields after `session_id` in `AgentExport`:

```rust
/// Agent session information for context export.
#[derive(Debug, Serialize)]
pub struct AgentExport {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub session_type: String,
    pub project_path: String,
    pub project_name: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_prompt: Option<String>,
    pub user_prompts: Vec<String>,
    pub user_prompt_count: i32,
    pub assistant_message_count: i32,
    pub tool_call_count: i32,
}
```

Notes:
- `parent_session_id` is `Option<String>` with `skip_serializing_if` (only subagents have parents)
- `session_type` is `String` (serialized from the enum's `as_str()`) — not `Option` since every session has a type

**Step 2: Map the new fields in `export_agents`**

In the `export_agents` function (line 160), add the two fields to the mapping:

```rust
fn export_agents(
    db: &Database,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> anyhow::Result<Vec<AgentExport>> {
    Ok(db
        .agent_sessions_in_range(start, end)?
        .into_iter()
        .map(|s| AgentExport {
            user_prompt_count: i32::try_from(s.user_prompts.len()).unwrap_or(i32::MAX),
            session_id: s.session_id,
            parent_session_id: s.parent_session_id,
            session_type: s.session_type.as_str().to_string(),
            project_path: s.project_path,
            project_name: s.project_name,
            start_time: s.start_time,
            end_time: s.end_time,
            summary: s.summary,
            starting_prompt: s.starting_prompt,
            user_prompts: s.user_prompts,
            assistant_message_count: s.assistant_message_count,
            tool_call_count: s.tool_call_count,
        })
        .collect())
}
```

**Step 3: Run `cargo clippy --all-targets` and `cargo fmt --check`**

Run: `cargo clippy --all-targets && cargo fmt --check`
Expected: No warnings, no format issues

### Task 2: Update existing test and add coverage for new fields

**Files:**
- Modify: `crates/tt-cli/src/commands/context.rs` (test module, line 480+)

**Step 1: Update `test_agent_export_serialization` to cover new fields**

The existing test at line 480 constructs an `AgentExport` without the new fields — it will fail to compile. Update it:

```rust
#[test]
fn test_agent_export_serialization() {
    let agent = AgentExport {
        session_id: "session-123".to_string(),
        parent_session_id: None,
        session_type: "user".to_string(),
        project_path: "/home/user/project".to_string(),
        project_name: "project".to_string(),
        start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        end_time: Some(
            chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
        summary: Some("Implemented feature X".to_string()),
        starting_prompt: Some("Implement feature X".to_string()),
        user_prompts: vec!["Implement feature X".to_string(), "Add tests".to_string()],
        user_prompt_count: 2,
        assistant_message_count: 3,
        tool_call_count: 10,
    };

    let json = serde_json::to_string(&agent).unwrap();
    assert!(json.contains("\"session_id\""));
    assert!(json.contains("\"session_type\""));
    assert!(json.contains("\"user\""));
    assert!(json.contains("\"project_path\""));
    assert!(json.contains("\"user_prompts\""));
    assert!(json.contains("\"tool_call_count\""));
    // parent_session_id should be skipped when None
    assert!(!json.contains("\"parent_session_id\""));
}
```

**Step 2: Add test for subagent with parent_session_id**

```rust
#[test]
fn test_agent_export_with_parent_session_id() {
    let agent = AgentExport {
        session_id: "agent-a913a65".to_string(),
        parent_session_id: Some("parent-session-123".to_string()),
        session_type: "subagent".to_string(),
        project_path: "/home/user/project".to_string(),
        project_name: "project".to_string(),
        start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        end_time: Some(
            chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
        summary: None,
        starting_prompt: Some("Run tests".to_string()),
        user_prompts: vec!["Run tests".to_string()],
        user_prompt_count: 1,
        assistant_message_count: 2,
        tool_call_count: 5,
    };

    let json = serde_json::to_string(&agent).unwrap();
    assert!(json.contains("\"parent_session_id\""));
    assert!(json.contains("\"parent-session-123\""));
    assert!(json.contains("\"session_type\""));
    assert!(json.contains("\"subagent\""));
}
```

**Step 3: Add integration test for export_agents mapping**

Update the existing `test_export_agents_returns_sessions_in_range` (line 897) to also assert new fields, and add a test with a subagent that has a parent:

```rust
#[test]
fn test_export_agents_includes_parent_session_id() {
    let db = tt_db::Database::open_in_memory().unwrap();

    // Insert a subagent session with parent_session_id
    let session = tt_core::session::AgentSession {
        session_id: "agent-a913a65".to_string(),
        source: tt_core::session::SessionSource::default(),
        parent_session_id: Some("parent-session-123".to_string()),
        session_type: tt_core::session::SessionType::Subagent,
        project_path: "/home/user/my-project".to_string(),
        project_name: "my-project".to_string(),
        start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        end_time: Some(
            chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
        message_count: 3,
        summary: None,
        user_prompts: vec!["Run tests".to_string()],
        starting_prompt: Some("Run tests".to_string()),
        assistant_message_count: 2,
        tool_call_count: 5,
        user_message_timestamps: Vec::new(),
    };
    db.upsert_agent_session(&session).unwrap();

    let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T13:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let exports = export_agents(&db, start, end).unwrap();

    assert_eq!(exports.len(), 1);
    let agent = &exports[0];
    assert_eq!(
        agent.parent_session_id.as_deref(),
        Some("parent-session-123")
    );
    assert_eq!(agent.session_type, "subagent");
}
```

**Step 4: Run tests**

Run: `cargo test -p tt-cli`
Expected: All tests pass

**Step 5: Run full checks**

Run: `cargo clippy --all-targets && cargo fmt --check && cargo test`
Expected: All green
