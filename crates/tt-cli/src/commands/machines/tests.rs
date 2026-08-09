use chrono::{DateTime, Duration, TimeZone, Utc};
use insta::assert_snapshot;
use serde_json::json;
use tt_db::{Database, StoredEvent};

use super::{MachineStatus, dark_machine_warnings, format_machines, is_dark, load_statuses};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap()
}

fn status(label: &str, last_event_at: Option<DateTime<Utc>>) -> MachineStatus {
    MachineStatus {
        machine_id: format!("{label}-uuid"),
        label: label.to_owned(),
        last_sync_at: Some("2026-08-01T11:00:00.000Z".to_owned()),
        last_event_at,
    }
}

fn make_event(id: &str, timestamp: DateTime<Utc>, machine_id: &str) -> StoredEvent {
    StoredEvent {
        id: id.to_owned(),
        timestamp,
        event_type: tt_core::EventType::TmuxPaneFocus,
        source: "remote.tmux".to_owned(),
        machine_id: Some(machine_id.to_owned()),
        schema_version: 1,
        pane_id: Some("%1".to_owned()),
        tmux_session: None,
        window_index: None,
        git_project: None,
        git_workspace: None,
        status: None,
        idle_duration_ms: None,
        window_app_id: None,
        window_title: None,
        action: None,
        cwd: Some("/project".to_owned()),
        session_id: None,
        stream_id: None,
        assignment_source: None,
        data: json!({}),
    }
}

// ========== Staleness predicate ==========

#[test]
fn machine_silent_for_six_days_is_not_dark() {
    // Given / When / Then: one day inside the 7-day threshold
    assert!(!is_dark(Some(now() - Duration::days(6)), now()));
}

#[test]
fn machine_silent_for_eight_days_is_dark() {
    // Given / When / Then: one day past the 7-day threshold
    assert!(is_dark(Some(now() - Duration::days(8)), now()));
}

#[test]
fn machine_silent_for_exactly_seven_days_is_dark() {
    // Given / When / Then: the threshold itself counts as dark
    assert!(is_dark(Some(now() - Duration::days(7)), now()));
}

#[test]
fn machine_with_zero_events_is_dark() {
    // Given / When / Then: a remote that never reported is the worst case
    assert!(is_dark(None, now()));
}

#[test]
fn machine_with_recent_events_is_not_dark() {
    // Given / When / Then: an event from an hour ago keeps the machine live
    assert!(!is_dark(Some(now() - Duration::hours(1)), now()));
}

// ========== Warning lines ==========

#[test]
fn dark_machine_warnings_name_the_machine_and_its_silence() {
    // Given: one live machine, one long-dark machine, one that never reported
    let statuses = [
        status("devbox", Some(now() - Duration::days(3))),
        status("ghost-wispr", Some(now() - Duration::days(50))),
        status("never-seen", None),
    ];

    // When
    let warnings = dark_machine_warnings(&statuses, now());

    // Then: only the dark ones warn, each naming itself and its silence
    assert_eq!(
        warnings,
        [
            "⚠ ghost-wispr has sent no events for 50d — check that tt is still running there."
                .to_owned(),
            "⚠ never-seen has never sent any events — check that tt is still running there."
                .to_owned(),
        ]
    );
}

#[test]
fn dark_machine_warnings_are_empty_when_every_remote_reports() {
    // Given
    let statuses = [status("devbox", Some(now() - Duration::days(1)))];

    // When / Then
    assert!(dark_machine_warnings(&statuses, now()).is_empty());
}

// ========== Table rendering ==========

#[test]
fn machines_empty() {
    let output = format_machines(&[], now()).unwrap();
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        assert_snapshot!(output);
    });
}

#[test]
fn machines_with_entries() {
    let statuses = [
        status("devbox", Some(now() - Duration::days(3))),
        status("ghost-wispr", Some(now() - Duration::days(50))),
        status("never-seen", None),
    ];
    let output = format_machines(&statuses, now()).unwrap();
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        assert_snapshot!(output);
    });
}

// ========== Database loading ==========

#[test]
fn load_statuses_reports_each_machines_newest_event() {
    // Given: two registered machines, only one of which has ever sent events
    let db = Database::open_in_memory().unwrap();
    db.upsert_machine("live-uuid", "devbox", None).unwrap();
    db.upsert_machine("dark-uuid", "ghost-wispr", None).unwrap();
    db.insert_event(&make_event("e1", now() - Duration::days(9), "live-uuid"))
        .unwrap();
    db.insert_event(&make_event("e2", now() - Duration::hours(2), "live-uuid"))
        .unwrap();

    // When
    let statuses = load_statuses(&db).unwrap();

    // Then: labels sort alphabetically, and each carries its own newest event
    let by_label: Vec<_> = statuses
        .iter()
        .map(|status| (status.label.as_str(), status.last_event_at))
        .collect();
    assert_eq!(
        by_label,
        [
            ("devbox", Some(now() - Duration::hours(2))),
            ("ghost-wispr", None),
        ]
    );
    assert!(!is_dark(statuses[0].last_event_at, now()));
    assert!(is_dark(statuses[1].last_event_at, now()));
}
