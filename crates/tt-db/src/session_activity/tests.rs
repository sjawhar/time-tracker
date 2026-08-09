//! Tests for activity-scoped agent-session listing.
//!
//! The invariant throughout: a window lists the sessions that *did something* in
//! it, and reports how long they were active inside it. A session's nominal span
//! never enters into either answer.

use chrono::{DateTime, TimeZone, Utc};
use tt_core::session::{AgentSession, SessionSource, SessionType};

use crate::{Database, StoredEvent};

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .expect("test timestamp is valid")
}

fn session(
    session_id: &str,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
) -> AgentSession {
    AgentSession {
        session_id: session_id.to_string(),
        source: SessionSource::OpenCode,
        parent_session_id: None,
        session_type: SessionType::User,
        project_path: "/home/sami/time-tracker".to_string(),
        project_name: "time-tracker".to_string(),
        start_time,
        end_time,
        message_count: 3,
        summary: None,
        user_prompts: Vec::new(),
        starting_prompt: Some("do the thing".to_string()),
        assistant_message_count: 1,
        tool_call_count: 1,
        user_message_timestamps: Vec::new(),
        tool_call_timestamps: Vec::new(),
    }
}

fn tool_use(id: &str, timestamp: DateTime<Utc>, session_id: &str) -> StoredEvent {
    StoredEvent {
        id: id.to_string(),
        timestamp,
        event_type: tt_core::EventType::AgentToolUse,
        source: "remote.agent".to_string(),
        machine_id: None,
        schema_version: 1,
        pane_id: None,
        tmux_session: None,
        window_index: None,
        git_project: None,
        git_workspace: None,
        status: None,
        idle_duration_ms: None,
        window_app_id: None,
        window_title: None,
        action: None,
        cwd: None,
        session_id: Some(session_id.to_string()),
        stream_id: None,
        assignment_source: None,
        data: serde_json::json!({}),
    }
}

/// A session whose harness never wrote an `ended` marker, active only on day one.
fn unterminated_session_active_on_day_one() -> Database {
    let db = Database::open_in_memory().expect("in-memory database opens");
    db.upsert_agent_session(&session("ses-open", at(2026, 4, 11, 9, 0), None), None)
        .expect("session upserts");
    db.insert_events(&[
        tool_use("open-1", at(2026, 4, 11, 9, 1), "ses-open"),
        tool_use("open-2", at(2026, 4, 11, 9, 41), "ses-open"),
    ])
    .expect("events insert");
    db
}

#[test]
fn unterminated_session_is_absent_from_later_windows() {
    // Given: a session with no `end_time` that last did anything in April.
    let db = unterminated_session_active_on_day_one();
    let july_start = at(2026, 7, 20, 0, 0);
    let july_end = at(2026, 7, 21, 0, 0);

    // When: a July window is listed both ways.
    let by_span = db
        .agent_sessions_in_range(july_start, july_end)
        .expect("span listing succeeds");
    let by_activity = db
        .agent_sessions_active_in_range(july_start, july_end)
        .expect("activity listing succeeds");

    // Then: reading the missing `end_time` as "still running" put an April
    // session in a July report; scoping by activity leaves the window empty.
    assert_eq!(
        by_span.len(),
        1,
        "the nominal span still overlaps every later window"
    );
    assert!(
        by_activity.is_empty(),
        "a session that did nothing in July must not be listed for July: {by_activity:?}"
    );
}

#[test]
fn unterminated_session_is_present_in_the_window_it_worked_in() {
    // Given: the same session, and the window it was actually active in.
    let db = unterminated_session_active_on_day_one();

    // When: that window is listed.
    let listed = db
        .agent_sessions_active_in_range(at(2026, 4, 11, 0, 0), at(2026, 4, 12, 0, 0))
        .expect("activity listing succeeds");

    // Then: it is reported, spanning its two events rather than the whole day.
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session.session_id, "ses-open");
    assert_eq!(listed[0].active_ms(), 40 * 60_000);
}

#[test]
fn session_spanning_the_window_without_activity_is_absent() {
    // Given: a terminated session whose nominal span brackets a quiet day, with
    // activity on the days either side but none within it.
    let db = Database::open_in_memory().expect("in-memory database opens");
    db.upsert_agent_session(
        &session(
            "ses-long",
            at(2026, 7, 15, 8, 0),
            Some(at(2026, 7, 25, 8, 0)),
        ),
        None,
    )
    .expect("session upserts");
    db.insert_events(&[
        tool_use("long-before", at(2026, 7, 15, 8, 5), "ses-long"),
        tool_use("long-after", at(2026, 7, 25, 7, 55), "ses-long"),
    ])
    .expect("events insert");

    // When: the quiet day in the middle is listed.
    let listed = db
        .agent_sessions_active_in_range(at(2026, 7, 20, 0, 0), at(2026, 7, 21, 0, 0))
        .expect("activity listing succeeds");

    // Then: it is absent. Spanning a window is not contributing to it — this is
    // the population a span cap cannot fix, because its activity is spread too.
    assert!(listed.is_empty(), "{listed:?}");
}

#[test]
fn active_ms_never_reports_the_window_length_for_a_single_event() {
    // Given: a long-lived session that touched the window exactly once.
    let db = Database::open_in_memory().expect("in-memory database opens");
    db.upsert_agent_session(
        &session(
            "ses-touch",
            at(2026, 7, 15, 8, 0),
            Some(at(2026, 7, 25, 8, 0)),
        ),
        None,
    )
    .expect("session upserts");
    db.insert_events(&[tool_use("touch", at(2026, 7, 20, 12, 0), "ses-touch")])
        .expect("events insert");

    // When: the day is listed.
    let listed = db
        .agent_sessions_active_in_range(at(2026, 7, 20, 0, 0), at(2026, 7, 21, 0, 0))
        .expect("activity listing succeeds");

    // Then: it reports the moment it was active, not the 24 hours it spanned.
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].active_ms(), 0);
}

#[test]
fn activity_window_is_half_open_at_its_end() {
    // Given: one session whose only event sits exactly on a window boundary.
    let db = Database::open_in_memory().expect("in-memory database opens");
    let boundary = at(2026, 7, 21, 0, 0);
    db.upsert_agent_session(
        &session("ses-boundary", at(2026, 7, 20, 23, 0), Some(boundary)),
        None,
    )
    .expect("session upserts");
    db.insert_events(&[tool_use("boundary", boundary, "ses-boundary")])
        .expect("events insert");

    // When: the windows either side of the boundary are listed.
    let earlier = db
        .agent_sessions_active_in_range(at(2026, 7, 20, 0, 0), boundary)
        .expect("activity listing succeeds");
    let later = db
        .agent_sessions_active_in_range(boundary, at(2026, 7, 22, 0, 0))
        .expect("activity listing succeeds");

    // Then: the boundary event belongs to the later window, matching
    // `allocate_for_period`'s exclusive `end`.
    assert!(earlier.is_empty(), "{earlier:?}");
    assert_eq!(later.len(), 1);
}

#[test]
fn sessions_are_ordered_by_start_time() {
    // Given: two sessions active in one window, inserted newest first.
    let db = Database::open_in_memory().expect("in-memory database opens");
    for (id, start) in [
        ("ses-late", at(2026, 7, 20, 15, 0)),
        ("ses-early", at(2026, 7, 20, 9, 0)),
    ] {
        db.upsert_agent_session(
            &session(id, start, Some(start + chrono::Duration::hours(1))),
            None,
        )
        .expect("session upserts");
        db.insert_events(&[tool_use(&format!("{id}-event"), start, id)])
            .expect("events insert");
    }

    // When: the window is listed.
    let listed = db
        .agent_sessions_active_in_range(at(2026, 7, 20, 0, 0), at(2026, 7, 21, 0, 0))
        .expect("activity listing succeeds");

    // Then: the order follows `start_time`, not insertion.
    let ids: Vec<&str> = listed
        .iter()
        .map(|entry| entry.session.session_id.as_str())
        .collect();
    assert_eq!(ids, vec!["ses-early", "ses-late"]);
}

#[test]
fn user_messages_count_as_activity() {
    // Given: a session that only ever received a user message — no tool ran.
    let db = Database::open_in_memory().expect("in-memory database opens");
    db.upsert_agent_session(
        &session(
            "ses-talk",
            at(2026, 7, 20, 10, 0),
            Some(at(2026, 7, 20, 10, 5)),
        ),
        None,
    )
    .expect("session upserts");
    let mut message = tool_use("talk", at(2026, 7, 20, 10, 1), "ses-talk");
    message.event_type = tt_core::EventType::UserMessage;
    db.insert_events(&[message]).expect("events insert");

    // When: the day is listed.
    let listed = db
        .agent_sessions_active_in_range(at(2026, 7, 20, 0, 0), at(2026, 7, 21, 0, 0))
        .expect("activity listing succeeds");

    // Then: attention spent without a tool call is still activity.
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session.session_id, "ses-talk");
}
