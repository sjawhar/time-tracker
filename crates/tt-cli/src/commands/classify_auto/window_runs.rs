//! Window-focus activity grouped into runs the classifier can reason about.
//!
//! `window_focus` events carry no cwd and no session, so they arrive as a stream of
//! individual focus changes rather than as work. Grouping consecutive events for one
//! app on one machine into a run gives the classifier a unit with a duration and a few
//! titles — enough to judge, and small enough to put in a prompt.
//!
//! Only *unassigned* runs are built. Focus the terminal and artifact attribution passes
//! already resolved is not re-litigated here.

use std::cmp::Reverse;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

/// A stretch of focus on one app, on one machine.
#[derive(Debug, Serialize)]
pub struct WindowRun {
    pub start: String,
    pub end: String,
    pub duration_minutes: i64,
    pub app_id: String,
    pub event_ids: Vec<String>,
    pub titles: Vec<String>,
    pub machine_id: Option<String>,
    pub stream_id: Option<String>,
}

/// Builds runs from the unassigned window-focus events in a period.
pub fn build_unassigned_window_runs(db: &tt_db::Database) -> Result<Vec<WindowRun>> {
    // Selected in SQL, never filtered in memory. Loading the range and filtering
    // afterwards meant reading 2.7M rows to reach ~10k window-focus events, which
    // held 1.3 GB of RSS at 236% CPU for the whole of every pass.
    let unassigned = db
        .unattributed_terminal_focus_events()
        .context("load unassigned window events for automatic classification")?;
    let refs: Vec<_> = unassigned.iter().collect();
    Ok(synthesize_window_runs(&refs))
}

/// Reorders runs newest first, so a bounded pass spends its calls on today's attention.
///
/// Runs are built oldest-first and grouped per machine, which is the right order to
/// *group* in and the wrong one to *spend a budget* in: capping an oldest-first list
/// puts every pass at the far end of the backlog, so the focus that arrived this morning
/// is reached only once the 29,730-event tail is gone. That is the same starvation the
/// bound exists to end, moved one level down, and it is why
/// `SESSIONS_PER_PASS` is paired with `ORDER BY start_time DESC` on the other phase.
///
/// A start that cannot be read sorts to the tail rather than failing the pass — the same
/// degradation the roster's activity window takes, for the same reason: this key orders a
/// selection, so refusing to read one row would cost the whole pass its classifications.
/// `None < Some(_)`, so reversing the key is what puts it last. It parses without warning
/// on purpose: this sees every run in the backlog while the pass reaches only the bound's
/// worth, so warning here would file thousands of lines about runs nobody looked at — the
/// run that *is* reached warns as it is classified.
pub fn newest_first(mut runs: Vec<WindowRun>) -> Vec<WindowRun> {
    runs.sort_by_key(|run| Reverse(started_at(run)));
    runs
}

/// A run's start as an instant, or `None` when it cannot be read.
fn started_at(run: &WindowRun) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&run.start)
        .ok()
        .map(|start| start.with_timezone(&Utc))
}

fn synthesize_window_runs(events: &[&tt_db::StoredEvent]) -> Vec<WindowRun> {
    let mut sorted: Vec<_> = events
        .iter()
        .copied()
        .filter(|event| event.event_type == tt_core::EventType::WindowFocus)
        .collect();
    if sorted.is_empty() {
        return Vec::new();
    }

    sorted.sort_by(|a, b| {
        let machine_cmp = a.machine_id.cmp(&b.machine_id);
        if machine_cmp == std::cmp::Ordering::Equal {
            a.timestamp.cmp(&b.timestamp)
        } else {
            machine_cmp
        }
    });

    let gap_threshold = Duration::minutes(30);
    let mut runs = Vec::new();
    let first = sorted[0];
    let mut current = WindowRunBuilder::new(first);

    for event in &sorted[1..] {
        let same_machine = event.machine_id == current.machine_id;
        let same_app = event.window_app_id.as_deref().unwrap_or("(unknown)") == current.app_id;
        let within_gap = event.timestamp - current.end < gap_threshold;

        if same_machine && same_app && within_gap {
            current.push(event);
        } else {
            runs.push(current.finish());
            current = WindowRunBuilder::new(event);
        }
    }

    runs.push(current.finish());
    runs
}

struct WindowRunBuilder {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    app_id: String,
    event_ids: Vec<String>,
    titles: Vec<String>,
    machine_id: Option<String>,
    stream_id: Option<String>,
}

impl WindowRunBuilder {
    fn new(event: &tt_db::StoredEvent) -> Self {
        let mut builder = Self {
            start: event.timestamp,
            end: event.timestamp,
            app_id: event
                .window_app_id
                .clone()
                .unwrap_or_else(|| "(unknown)".to_string()),
            event_ids: Vec::new(),
            titles: Vec::new(),
            machine_id: event.machine_id.clone(),
            stream_id: event.stream_id.clone(),
        };
        builder.push(event);
        builder
    }

    fn push(&mut self, event: &tt_db::StoredEvent) {
        const MAX_TITLES: usize = 5;

        self.end = event.timestamp;
        self.event_ids.push(event.id.clone());
        if self.stream_id.is_none() {
            self.stream_id.clone_from(&event.stream_id);
        }
        if let Some(title) = &event.window_title {
            let is_consecutive_duplicate = self.titles.last() == Some(title);
            if !is_consecutive_duplicate && self.titles.len() < MAX_TITLES {
                self.titles.push(title.clone());
            }
        }
    }

    fn finish(self) -> WindowRun {
        WindowRun {
            start: self.start.to_rfc3339(),
            end: self.end.to_rfc3339(),
            duration_minutes: (self.end - self.start).num_minutes(),
            app_id: self.app_id,
            event_ids: self.event_ids,
            titles: self.titles,
            machine_id: self.machine_id,
            stream_id: self.stream_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn ts(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + Duration::minutes(minutes)
    }

    fn window_event(
        id: &str,
        timestamp: DateTime<Utc>,
        app_id: &str,
        title: &str,
        machine_id: Option<&str>,
    ) -> tt_db::StoredEvent {
        tt_db::StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type: tt_core::EventType::WindowFocus,
            source: "watcher".to_string(),
            machine_id: machine_id.map(String::from),
            schema_version: 1,
            cwd: None,
            git_project: None,
            git_workspace: None,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            status: None,
            idle_duration_ms: None,
            action: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: serde_json::Value::Null,
            window_app_id: Some(app_id.to_string()),
            window_title: Some(title.to_string()),
        }
    }

    #[test]
    fn groups_by_app_gap_and_machine() {
        // Given: focus events spanning an app change, a long gap, and a machine change.
        let events = [
            window_event("a", ts(0), "brave", "PR #1", Some("m1")),
            window_event("b", ts(1), "brave", "PR #2", Some("m1")),
            window_event("c", ts(2), "slack", "Threads", Some("m1")),
            window_event("d", ts(90), "slack", "Threads", Some("m1")),
            window_event("e", ts(3), "brave", "PR #1", Some("m2")),
        ];
        let refs: Vec<_> = events.iter().collect();

        // When: they are grouped.
        let runs = synthesize_window_runs(&refs);

        // Then: each boundary starts a new run.
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0].event_ids, ["a", "b"]);
        assert_eq!(runs[0].titles, ["PR #1", "PR #2"]);
        assert_eq!(runs[1].event_ids, ["c"]);
        assert_eq!(runs[2].event_ids, ["d"]);
        assert_eq!(runs[3].machine_id.as_deref(), Some("m2"));
    }

    #[test]
    fn builds_only_unassigned_runs() {
        // Given: one assigned and one unassigned window event.
        let db = tt_db::Database::open_in_memory().unwrap();
        let stream = tt_db::Stream {
            id: "s1".to_string(),
            created_at: ts(0),
            updated_at: ts(0),
            name: Some("taken".to_string()),
            slug: None,
            description: None,
            color: None,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        };
        db.insert_stream(&stream).unwrap();
        db.insert_event(&window_event("taken", ts(0), "brave", "PR #1", None))
            .unwrap();
        db.insert_event(&window_event("free", ts(1), "brave", "PR #2", None))
            .unwrap();
        db.assign_event_to_stream("taken", "s1", "artifact_reference")
            .unwrap();

        // When: runs are built for the period.
        let runs = build_unassigned_window_runs(&db).unwrap();

        // Then: attribution the passes already resolved is not re-offered.
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].event_ids, ["free"]);
    }

    #[test]
    fn returns_nothing_when_no_window_events_are_unassigned() {
        let db = tt_db::Database::open_in_memory().unwrap();
        assert!(build_unassigned_window_runs(&db).unwrap().is_empty());
    }

    /// A run carrying nothing but the start the ordering reads.
    fn run_starting(start: &str) -> WindowRun {
        WindowRun {
            start: start.to_string(),
            end: start.to_string(),
            duration_minutes: 0,
            app_id: "brave".to_string(),
            event_ids: vec![start.to_string()],
            titles: Vec::new(),
            machine_id: None,
            stream_id: None,
        }
    }

    #[test]
    fn orders_runs_newest_first_across_machines() {
        // Given: runs as the builder yields them — grouped per machine, oldest first
        // within each. A pass that caps this order spends every call on the far end of
        // the backlog and never reaches today's focus.
        let runs = vec![
            run_starting(&ts(0).to_rfc3339()),
            run_starting(&ts(120).to_rfc3339()),
            run_starting(&ts(60).to_rfc3339()),
        ];

        // When
        let ordered = newest_first(runs);

        // Then
        let starts: Vec<_> = ordered.iter().map(|run| run.start.clone()).collect();
        assert_eq!(
            starts,
            [
                ts(120).to_rfc3339(),
                ts(60).to_rfc3339(),
                ts(0).to_rfc3339()
            ]
        );
    }

    #[test]
    fn a_run_whose_start_cannot_be_read_sorts_to_the_tail() {
        // Given: one unreadable start among readable ones. The ordering key must degrade
        // rather than fail — refusing to read one row would cost a whole pass its
        // classifications — and the unreadable run belongs last, where the bound reaches
        // it only once everything datable is done.
        let runs = vec![
            run_starting("not a timestamp"),
            run_starting(&ts(0).to_rfc3339()),
            run_starting(&ts(60).to_rfc3339()),
        ];

        // When
        let ordered = newest_first(runs);

        // Then
        assert_eq!(ordered[0].start, ts(60).to_rfc3339());
        assert_eq!(ordered[1].start, ts(0).to_rfc3339());
        assert_eq!(ordered[2].start, "not a timestamp");
    }
}
