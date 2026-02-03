# Stream Inference Fixes - Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Address design issues in stream inference before merging PR #4.

**Architecture:** Breaking changes to event format and database schema. No migrations—just change the format.

**Key Changes:**
1. Flatten event data structure (remove unnecessary nesting)
2. Disambiguate "session" naming (tmux vs Claude Code)
3. Directory-based inference prefers jj_project for stream naming
4. LLM inference includes richer context (user prompts, events, previous streams)
5. Event-to-stream assignment is optional for LLM inference

**Token Budget Analysis (from actual session data):**
- Median user prompt: ~135 tokens (541 chars)
- P99 user prompt: ~2,781 tokens (11,126 chars)
- P90 session total: ~1,250 tokens
- At 100k budget: ~41 average sessions fit comfortably

**Recommended limits:**
- `MAX_USER_PROMPTS = 5` per session
- `MAX_PROMPT_LENGTH = 2000` chars (~500 tokens) per prompt
- Total budget: ~100k tokens allows full 24-hour window coverage

---

## Task 1: Flatten Event Data Structure

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs`
- Modify: `crates/tt-db/src/lib.rs` (if schema changes needed)

**Current (unnecessarily nested):**
```rust
pub struct IngestEvent {
    pub data: PaneFocusData,  // Why nested?
}

pub struct PaneFocusData {
    pub pane_id: String,
    pub session_name: String,
    pub jj_project: Option<String>,
    pub jj_workspace: Option<String>,
}
```

**Target (flat):**
```rust
pub struct IngestEvent {
    pub id: String,
    pub timestamp: String,
    pub source: String,
    pub event_type: String,
    pub cwd: Option<String>,
    // Flattened from PaneFocusData:
    pub pane_id: String,
    pub tmux_session: String,  // Renamed for clarity
    pub window_index: Option<u32>,
    pub jj_project: Option<String>,
    pub jj_workspace: Option<String>,
}
```

**Steps:**
1. Update `IngestEvent` struct to flatten fields
2. Rename `session_name` → `tmux_session`
3. Update JSON serialization (this is a breaking change to events.jsonl format)
4. Update any code that reads the nested structure
5. Run tests, update snapshots

---

## Task 2: Disambiguate "Session" Naming

**Files:**
- Modify: `crates/tt-core/src/session.rs`
- Modify: `crates/tt-db/src/lib.rs`
- Modify: `crates/tt-llm/src/lib.rs`

**Rename throughout codebase:**
| Current | New |
|---------|-----|
| `session_name` (tmux) | `tmux_session` |
| `SessionEntry` | `ClaudeSession` |
| `session_id` (Claude) | `claude_session_id` |
| `sessions` table | `claude_sessions` table |
| `scan_sessions_directory` | `scan_claude_sessions` |

**Steps:**
1. Rename `SessionEntry` → `ClaudeSession`
2. Rename database table `sessions` → `claude_sessions`
3. Update all references
4. Run tests

---

## Task 3: Directory-Based Inference Prefers jj_project

**Files:**
- Modify: `crates/tt-core/src/inference.rs`

**Current:** Stream name = directory basename
**Target:** Stream name = `jj_project` if present, else directory basename

**Steps:**

1. Update `InferableEvent` trait to include jj_project:
```rust
pub trait InferableEvent {
    // ... existing methods ...
    fn jj_project(&self) -> Option<&str>;
}
```

2. Update `generate_stream_name` to prefer jj_project:
```rust
fn generate_stream_name(events: &[&E], cwd_key: &str, name_counts: &mut HashMap<String, u32>) -> String {
    // Try jj_project from first event
    let base_name = events.first()
        .and_then(|e| e.jj_project())
        .map(String::from)
        .unwrap_or_else(|| {
            // Fallback to directory basename
            if cwd_key.is_empty() {
                "Uncategorized".to_string()
            } else {
                Path::new(cwd_key)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            }
        });

    // Handle name collisions...
}
```

3. Implement `jj_project()` on `StoredEvent`
4. Update tests

---

## Task 4: Enrich LLM Prompt with User Prompts

**Files:**
- Modify: `crates/tt-core/src/session.rs`
- Modify: `crates/tt-llm/src/lib.rs`

**Current:** Prompt only includes session summaries
**Target:** Include user prompts (truncated), events, and previous streams

### Step 1: Extract user prompts during session parsing

Update `ClaudeSession` struct:
```rust
pub struct ClaudeSession {
    pub claude_session_id: String,
    pub parent_session_id: Option<String>,
    pub project_path: String,
    pub project_name: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub message_count: i32,
    pub summary: Option<String>,
    // NEW: First N user prompts (truncated)
    pub user_prompts: Vec<String>,
}
```

Update `parse_session_file` to extract user prompts:
```rust
const MAX_USER_PROMPTS: usize = 5;
const MAX_PROMPT_LENGTH: usize = 2000;  // ~500 tokens, covers P90

// In parsing loop:
if msg_type == "user" {
    if let Some(content) = header.message_content {
        if user_prompts.len() < MAX_USER_PROMPTS {
            let truncated = if content.len() > MAX_PROMPT_LENGTH {
                format!("{}...", &content[..MAX_PROMPT_LENGTH])
            } else {
                content
            };
            user_prompts.push(truncated);
        }
    }
}
```

### Step 2: Update database schema

Add `user_prompts` column to `claude_sessions` table (JSON array of strings).

### Step 3: Enrich LLM prompt

Update `build_stream_inference_prompt`:
```rust
pub fn build_stream_inference_prompt(
    sessions: &[ClaudeSession],
    events: &[StoredEvent],        // NEW: include events
    previous_streams: &[InferredStream],
) -> String {
    let mut prompt = String::new();

    // 1. Previous streams context (sliding window carryover)
    if !previous_streams.is_empty() {
        prompt.push_str("## Previously Identified Streams\n\n");
        for stream in previous_streams {
            prompt.push_str(&format!(
                "- \"{}\" (id: {}, {} events)\n",
                stream.name, stream.id, stream.session_ids.len()
            ));
        }
        prompt.push('\n');
    }

    // 2. Events summary (grouped by project/cwd)
    prompt.push_str("## Activity Events\n\n");
    let mut by_project: HashMap<&str, Vec<&StoredEvent>> = HashMap::new();
    for event in events {
        let key = event.jj_project().or(event.cwd()).unwrap_or("unknown");
        by_project.entry(key).or_default().push(event);
    }
    for (project, proj_events) in &by_project {
        prompt.push_str(&format!(
            "- {}: {} events ({} to {})\n",
            project,
            proj_events.len(),
            proj_events.first().map(|e| e.timestamp.to_string()).unwrap_or_default(),
            proj_events.last().map(|e| e.timestamp.to_string()).unwrap_or_default(),
        ));
    }
    prompt.push('\n');

    // 3. Claude sessions with user prompts
    prompt.push_str("## Claude Code Sessions\n\n");
    for session in sessions {
        prompt.push_str(&format!(
            "Session: {}\n  Project: {}\n  Time: {} to {}\n",
            session.claude_session_id,
            session.project_name,
            session.start_time,
            session.end_time.map(|t| t.to_string()).unwrap_or("ongoing".to_string()),
        ));

        if let Some(summary) = &session.summary {
            prompt.push_str(&format!("  Summary: {}\n", summary));
        }

        if !session.user_prompts.is_empty() {
            prompt.push_str("  User prompts:\n");
            for (i, prompt_text) in session.user_prompts.iter().enumerate() {
                prompt.push_str(&format!("    {}. {}\n", i + 1, prompt_text));
            }
        }
        prompt.push('\n');
    }

    // 4. Instructions
    prompt.push_str(r#"
Group the above activity into meaningful work streams. Consider:
- Sessions in the same project likely belong together
- User prompts reveal the actual work being done
- Events show when and where work happened

Respond with JSON:
{"streams": [{"id": "uuid", "name": "descriptive name", "session_ids": ["..."], "confidence": 0.0-1.0}]}
"#);

    prompt
}
```

---

## Task 5: Make Event Assignment Optional

**Files:**
- Modify: `crates/tt-cli/src/commands/infer.rs`
- Modify: `crates/tt-cli/src/cli.rs`

**Add CLI flag:**
```rust
Infer {
    #[arg(long)]
    llm: bool,
    #[arg(long)]
    assign_events: bool,  // NEW: whether to assign events to LLM-inferred streams
    // ...
}
```

**Implement optional assignment:**
```rust
pub async fn run_llm(
    db: &Database,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    dry_run: bool,
    assign_events: bool,  // NEW
) -> Result<()> {
    // ... existing stream creation ...

    if assign_events {
        // Match events to streams based on:
        // 1. Event timestamp within session time range
        // 2. Event cwd matches session project_path
        // 3. Event jj_project matches session project_name

        for response in &inferred {
            let stream_sessions: Vec<_> = sessions.iter()
                .filter(|s| response.session_ids.contains(&s.claude_session_id))
                .collect();

            // Find events that match these sessions
            let matching_events: Vec<_> = events.iter()
                .filter(|e| {
                    stream_sessions.iter().any(|s| {
                        // Event timestamp within session time range
                        e.timestamp >= s.start_time
                            && e.timestamp <= s.end_time.unwrap_or(Utc::now())
                            && (e.cwd() == Some(&s.project_path)
                                || e.jj_project() == Some(&s.project_name))
                    })
                })
                .collect();

            let assignments: Vec<_> = matching_events.iter()
                .map(|e| (e.id.clone(), stream_id.as_str().to_string()))
                .collect();

            db.assign_events_to_stream(&assignments, "llm")?;
        }
    }
}
```

---

## Task 6: Implement Previous Streams Context (Sliding Window)

**Files:**
- Modify: `crates/tt-cli/src/commands/infer.rs`
- Modify: `crates/tt-db/src/lib.rs`

**Add database method to get recent streams:**
```rust
pub fn streams_in_range(
    &self,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<Stream>, DbError> {
    // Query streams where first_event_at or last_event_at overlaps with range
}
```

**Update run_llm to fetch previous streams:**
```rust
// Get previous streams for context (4-hour overlap window)
let overlap_start = start - Duration::hours(4);
let previous_streams = db.streams_in_range(overlap_start, start)?;

// Convert to InferredStream format for prompt
let previous: Vec<InferredStream> = previous_streams.iter()
    .map(|s| InferredStream {
        id: StreamId::new(&s.id).unwrap(),
        name: s.name.clone().unwrap_or_default(),
        session_ids: vec![],  // Not needed for context
        confidence: Confidence::MAX,
        first_event_at: s.first_event_at.unwrap_or(Utc::now()),
        last_event_at: s.last_event_at.unwrap_or(Utc::now()),
    })
    .collect();
```

---

## Summary

| Task | Complexity |
|------|------------|
| 1. Flatten event data | Medium |
| 2. Disambiguate "session" | Low |
| 3. Prefer jj_project naming | Low |
| 4. Enrich LLM prompt | High |
| 5. Optional event assignment | Medium |
| 6. Previous streams context | Medium |

**Execution order:** 1 → 2 → 3 → 4 → 5 → 6 (dependencies flow forward)
