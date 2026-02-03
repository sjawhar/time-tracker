# Migrate Codex Features to Default Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add missing event types and focus hierarchy from codex to default's time allocation algorithm.

**Architecture:** Default already has a complete allocation algorithm in `tt-core/src/allocation.rs`. We add support for `window_focus` and `browser_tab` event types, then implement focus hierarchy logic that routes direct time based on which application has focus (terminal apps → tmux stream, browser apps → browser stream).

**Tech Stack:** Rust, chrono, serde_json (all already in use)

---

## Summary of Missing Features

| Feature | Codex | Default | Priority |
|---------|-------|---------|----------|
| `window_focus` event type | ✅ | ❌ | High |
| `browser_tab` event type | ✅ | ❌ | High |
| Focus hierarchy (app → stream routing) | ✅ | ❌ | High |
| Daily breakdowns in weekly report | ✅ | ❌ | Low (defer) |
| AFK `idle_duration_ms` retroactive | ✅ | ❌ | Medium |

**Deferred:** Daily breakdowns require significant report refactoring. Can be added later.

---

### Task 1: Add `window_focus` Event Handling

**Files:**
- Modify: `crates/tt-core/src/allocation.rs:82-93` (add `WindowFocusState`)
- Modify: `crates/tt-core/src/allocation.rs:223-248` (add match arm)

**Step 1: Write the failing test**

Add to `crates/tt-core/src/allocation.rs` in the `mod tests` section:

```rust
#[test]
fn test_window_focus_sets_active_window() {
    let events = vec![
        TestEvent::window_focus(ts(0), "Terminal", Some("A")),
        TestEvent::tmux_focus(ts(0), "A"),
        TestEvent::tmux_focus(ts(10), "A"), // Activity to close interval
    ];

    let config = AllocationConfig::default();
    let result = allocate_time(&events, &config, Some(ts(10)));

    let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
    // Window focus + tmux focus on same stream = 10 minutes
    assert_eq!(stream_a.time_direct_ms, 10 * 60 * 1000);
}
```

Also add the `TestEvent::window_focus` helper:

```rust
fn window_focus(ts: DateTime<Utc>, app: &str, stream_id: Option<&str>) -> Self {
    Self {
        timestamp: ts,
        event_type: "window_focus".to_string(),
        stream_id: stream_id.map(String::from),
        session_id: None,
        data: json!({"app": app, "title": "test window"}),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p tt-core test_window_focus_sets_active_window`
Expected: FAIL - unrecognized event type, no effect on time

**Step 3: Add window focus state tracking**

In `allocation.rs`, after the `FocusState` enum (~line 93), add:

```rust
/// Current window focus state.
#[derive(Debug, Clone, Default)]
struct WindowFocusState {
    /// Currently focused application name (lowercase).
    app: Option<String>,
    /// Stream associated with window focus event.
    stream_id: Option<String>,
}
```

**Step 4: Add window focus state to allocation function**

In `allocate_time()`, after `let mut focus_state = FocusState::Unfocused;` (~line 144), add:

```rust
let mut window_focus_state = WindowFocusState::default();
```

**Step 5: Add match arm for window_focus event**

In the match block (~line 223), add before the `_ => {}` arm:

```rust
"window_focus" => {
    let app = data
        .get("app")
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_lowercase());
    window_focus_state.app = app;
    window_focus_state.stream_id = event.stream_id().map(String::from);
}
```

**Step 6: Run test to verify it passes**

Run: `cargo test -p tt-core test_window_focus_sets_active_window`
Expected: PASS

**Step 7: Commit**

```bash
jj describe -m "feat(allocation): add window_focus event handling"
```

---

### Task 2: Add `browser_tab` Event Handling

**Files:**
- Modify: `crates/tt-core/src/allocation.rs` (add state and match arm)

**Step 1: Write the failing test**

```rust
#[test]
fn test_browser_tab_tracks_stream() {
    let events = vec![
        TestEvent::browser_tab(ts(0), "B"),
        TestEvent::browser_tab(ts(10), "B"), // Activity to close interval
    ];

    let config = AllocationConfig::default();
    let result = allocate_time(&events, &config, Some(ts(10)));

    // Browser tab alone doesn't grant direct time without window focus
    // This test verifies the event is parsed without error
    assert!(result.stream_times.is_empty() ||
            get_stream_time(&result, "B").map(|s| s.time_direct_ms).unwrap_or(0) == 0);
}
```

Add the `TestEvent::browser_tab` helper:

```rust
fn browser_tab(ts: DateTime<Utc>, stream_id: &str) -> Self {
    Self {
        timestamp: ts,
        event_type: "browser_tab".to_string(),
        stream_id: Some(stream_id.to_string()),
        session_id: None,
        data: json!({"url": "https://example.com", "title": "Test Page"}),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p tt-core test_browser_tab_tracks_stream`
Expected: FAIL or unexpected behavior

**Step 3: Add browser focus state**

After `WindowFocusState`, add:

```rust
/// Current browser tab focus state.
#[derive(Debug, Clone, Default)]
struct BrowserFocusState {
    /// Stream associated with the currently focused browser tab.
    stream_id: Option<String>,
}
```

**Step 4: Add browser state to allocation function**

After `window_focus_state`, add:

```rust
let mut browser_focus_state = BrowserFocusState::default();
```

**Step 5: Add match arm for browser_tab event**

```rust
"browser_tab" => {
    browser_focus_state.stream_id = event.stream_id().map(String::from);
}
```

**Step 6: Run test to verify it passes**

Run: `cargo test -p tt-core test_browser_tab_tracks_stream`
Expected: PASS

**Step 7: Commit**

```bash
jj describe -m "feat(allocation): add browser_tab event handling"
```

---

### Task 3: Implement Focus Hierarchy

**Files:**
- Modify: `crates/tt-core/src/allocation.rs`

**Step 1: Write the failing test for terminal hierarchy**

```rust
#[test]
fn test_focus_hierarchy_terminal_uses_tmux_stream() {
    let events = vec![
        TestEvent::window_focus(ts(0), "Terminal", None), // Window focus, no stream
        TestEvent::tmux_focus(ts(0), "A"),                 // Tmux focus on A
        TestEvent::tmux_scroll(ts(5), "A"),                // Activity
    ];

    let config = AllocationConfig::default();
    let result = allocate_time(&events, &config, Some(ts(6)));

    let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
    // Terminal window focus + tmux focus = time goes to tmux stream A
    assert_eq!(stream_a.time_direct_ms, 6 * 60 * 1000);
}

#[test]
fn test_focus_hierarchy_browser_uses_browser_stream() {
    let events = vec![
        TestEvent::window_focus(ts(0), "Chrome", None),
        TestEvent::browser_tab(ts(0), "B"),
        TestEvent::browser_tab(ts(5), "B"), // Activity
    ];

    let config = AllocationConfig::default();
    let result = allocate_time(&events, &config, Some(ts(6)));

    let stream_b = get_stream_time(&result, "B").expect("Stream B should exist");
    // Browser window focus + browser tab = time goes to browser stream B
    assert_eq!(stream_b.time_direct_ms, 6 * 60 * 1000);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p tt-core test_focus_hierarchy`
Expected: FAIL - focus hierarchy not implemented

**Step 3: Add app detection helpers**

After the `calculate_total_tracked` function, add:

```rust
/// Returns true if the app name indicates a terminal application.
fn is_terminal_app(app: &str) -> bool {
    let app_lower = app.to_ascii_lowercase();
    app_lower.contains("terminal")
        || app_lower.contains("iterm")
        || app_lower.contains("alacritty")
        || app_lower.contains("wezterm")
        || app_lower.contains("kitty")
        || app_lower.contains("konsole")
        || app_lower.contains("gnome-terminal")
}

/// Returns true if the app name indicates a browser application.
fn is_browser_app(app: &str) -> bool {
    let app_lower = app.to_ascii_lowercase();
    app_lower.contains("chrome")
        || app_lower.contains("firefox")
        || app_lower.contains("safari")
        || app_lower.contains("edge")
        || app_lower.contains("brave")
        || app_lower.contains("arc")
}
```

**Step 4: Add resolve_focus_stream helper**

```rust
/// Resolves which stream should receive direct time based on focus hierarchy.
///
/// Hierarchy:
/// - If window is a terminal app → use tmux focus stream
/// - If window is a browser app → use browser tab stream
/// - Otherwise → use window focus stream
fn resolve_focus_stream(
    window_state: &WindowFocusState,
    tmux_stream_id: Option<&str>,
    browser_stream_id: Option<&str>,
) -> Option<String> {
    match &window_state.app {
        Some(app) if is_terminal_app(app) => tmux_stream_id.map(String::from),
        Some(app) if is_browser_app(app) => browser_stream_id.map(String::from),
        Some(_) => window_state.stream_id.clone(),
        None => tmux_stream_id.map(String::from), // Fallback to tmux if no window info
    }
}
```

**Step 5: Refactor FocusState to use hierarchy**

This requires changing how focus is tracked. Replace the simple `FocusState::Focused` with a more nuanced approach:

In the `tmux_pane_focus` handler, store the tmux stream separately:

```rust
"tmux_pane_focus" => {
    if let Some(stream_id) = event.stream_id() {
        // Close previous focus interval using resolved stream
        if let FocusState::Focused { focus_start, .. } = &focus_state {
            let resolved = resolve_focus_stream(
                &window_focus_state,
                tmux_focus_stream_id.as_deref(),
                browser_focus_state.stream_id.as_deref(),
            );
            if let Some(resolved_stream) = resolved {
                add_direct(
                    &resolved_stream,
                    *focus_start,
                    event_time,
                    &mut activity_intervals,
                    &mut stream_times,
                );
            }
        }

        tmux_focus_stream_id = Some(stream_id.to_string());
        focus_state = FocusState::Focused {
            stream_id: stream_id.to_string(),
            focus_start: event_time,
        };
    }
}
```

**Step 6: Add tmux_focus_stream_id tracking**

After `let mut browser_focus_state`, add:

```rust
let mut tmux_focus_stream_id: Option<String> = None;
```

**Step 7: Update all focus closing logic**

Update other handlers (afk_change, finalization) to use `resolve_focus_stream`.

**Step 8: Run tests to verify they pass**

Run: `cargo test -p tt-core test_focus_hierarchy`
Expected: PASS

**Step 9: Run all allocation tests**

Run: `cargo test -p tt-core allocation`
Expected: All PASS

**Step 10: Commit**

```bash
jj describe -m "feat(allocation): implement focus hierarchy for window/terminal/browser"
```

---

### Task 4: Add AFK `idle_duration_ms` Retroactive Support

**Files:**
- Modify: `crates/tt-core/src/allocation.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn test_afk_idle_duration_retroactive() {
    // AFK event at 5 min reports user was idle for 3 minutes (since 2 min)
    let events = vec![
        TestEvent::tmux_focus(ts(0), "A"),
        TestEvent::afk_with_duration(ts(5), "idle", 180_000), // idle_duration_ms = 3 min
    ];

    let config = AllocationConfig::default();
    let result = allocate_time(&events, &config, Some(ts(5)));

    let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
    // Direct time: only 0-2 min (idle started at 2 min retroactively)
    assert_eq!(stream_a.time_direct_ms, 2 * 60 * 1000);
}
```

Add the `TestEvent::afk_with_duration` helper:

```rust
fn afk_with_duration(ts: DateTime<Utc>, status: &str, idle_duration_ms: i64) -> Self {
    Self {
        timestamp: ts,
        event_type: "afk_change".to_string(),
        stream_id: None,
        session_id: None,
        data: json!({"status": status, "idle_duration_ms": idle_duration_ms}),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p tt-core test_afk_idle_duration_retroactive`
Expected: FAIL - 5 minutes attributed instead of 2

**Step 3: Update afk_change handler**

```rust
"afk_change" => {
    let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status == "idle" {
        // Check for retroactive idle duration
        let idle_start = if let Some(duration_ms) = data.get("idle_duration_ms").and_then(|v| v.as_i64()) {
            if duration_ms > 0 {
                event_time - Duration::milliseconds(duration_ms)
            } else {
                event_time
            }
        } else {
            event_time
        };

        // Close focus at idle_start, not event_time
        if let FocusState::Focused {
            stream_id,
            focus_start,
        } = &focus_state
        {
            let end_time = idle_start.max(*focus_start); // Don't go before focus started
            if end_time > *focus_start {
                add_direct(
                    stream_id,
                    *focus_start,
                    end_time,
                    &mut activity_intervals,
                    &mut stream_times,
                );
            }
        }
        focus_state = FocusState::Unfocused;
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p tt-core test_afk_idle_duration_retroactive`
Expected: PASS

**Step 5: Run all tests**

Run: `cargo test -p tt-core`
Expected: All PASS

**Step 6: Commit**

```bash
jj describe -m "feat(allocation): support AFK idle_duration_ms for retroactive idle detection"
```

---

### Task 5: Run Full Test Suite and Lint

**Step 1: Run all tests**

Run: `cargo test`
Expected: All PASS

**Step 2: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings

**Step 3: Run format check**

Run: `cargo fmt --check`
Expected: No changes needed

**Step 4: Final commit if any cleanup**

```bash
jj describe -m "chore: cleanup after codex feature migration"
```

---

## Deferred Work

### Daily Breakdowns in Weekly Report (Low Priority)

Codex's report shows per-day totals within a weekly report:

```
DAILY TOTALS
Mon 01-26  2h 30m  (D 1h 30m / A 1h 00m)
Tue 01-27  3h 15m  (D 2h 00m / A 1h 15m)
...
```

This requires:
1. Iterating through each day in the period
2. Filtering events per day
3. Computing allocations per day
4. Formatting the table

Can be added as a separate task later.
