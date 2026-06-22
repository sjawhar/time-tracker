pub mod backend;
pub mod cosmic;

use std::{path::Path, time::Duration as StdDuration};

use anyhow::{Context, Result};
use backend::{ActiveWindow, IdleState, Snapshot, WindowBackend};
use chrono::{DateTime, Duration, Utc};
use cosmic::CosmicBackend;
use serde_json::Value;
use tt_core::EventType;
use tt_db::StoredEvent;

const SOURCE: &str = "local.cosmic";
const DEFAULT_DEBOUNCE_MS: i64 = 500;
const DEFAULT_POLL_MS: u64 = 1_000;

pub struct EmitState {
    last_window: Option<ActiveWindow>,
    is_idle: bool,
    idle_start: Option<DateTime<Utc>>,
    pending: Option<(ActiveWindow, DateTime<Utc>)>,
    machine_id: String,
    debounce: Duration,
}

impl EmitState {
    pub const fn new(machine_id: String) -> Self {
        Self::new_with_debounce(machine_id, Duration::milliseconds(DEFAULT_DEBOUNCE_MS))
    }

    pub const fn new_with_debounce(machine_id: String, debounce: Duration) -> Self {
        Self {
            last_window: None,
            is_idle: false,
            idle_start: None,
            pending: None,
            machine_id,
            debounce,
        }
    }

    pub fn observe(&mut self, snap: &Snapshot, now: DateTime<Utc>) -> Vec<StoredEvent> {
        let mut out = Vec::new();

        match snap.idle {
            IdleState::Idle { since_ms } if !self.is_idle => {
                let idle_start = now - Duration::milliseconds(since_ms);
                self.idle_start = Some(idle_start);
                self.is_idle = true;
                self.pending = None;
                out.push(self.afk(idle_start, "idle", None));
                return out;
            }
            IdleState::Active if self.is_idle => {
                let idle_duration_ms = self
                    .idle_start
                    .map(|start| now.signed_duration_since(start).num_milliseconds());
                self.idle_start = None;
                self.is_idle = false;
                self.pending = None;
                out.push(self.afk(now, "active", idle_duration_ms));

                if let Some(window) = &snap.active {
                    out.push(self.window(now, window));
                }
                self.last_window.clone_from(&snap.active);
                return out;
            }
            _ => {}
        }

        if self.is_idle {
            return out;
        }

        self.observe_window(snap.active.as_ref(), now, &mut out);
        out
    }

    fn observe_window(
        &mut self,
        active: Option<&ActiveWindow>,
        now: DateTime<Utc>,
        out: &mut Vec<StoredEvent>,
    ) {
        let Some(window) = active else {
            self.last_window = None;
            self.pending = None;
            return;
        };

        if self.last_window.as_ref() == Some(window) {
            self.pending = None;
            return;
        }

        if self.last_window.is_none() || self.debounce <= Duration::zero() {
            out.push(self.window(now, window));
            self.last_window = Some(window.clone());
            self.pending = None;
            return;
        }

        match &self.pending {
            Some((pending_window, since)) if pending_window == window => {
                if now.signed_duration_since(*since) >= self.debounce {
                    out.push(self.window(now, window));
                    self.last_window = Some(window.clone());
                    self.pending = None;
                }
            }
            _ => {
                self.pending = Some((window.clone(), now));
            }
        }
    }

    fn window(&self, ts: DateTime<Utc>, window: &ActiveWindow) -> StoredEvent {
        let ts_ms = timestamp_ms_z(ts);
        let title_hash = short_hash(&window.title);
        let id = format!(
            "{}:{SOURCE}:window_focus:{ts_ms}:{}:{title_hash}",
            self.machine_id, window.app_id
        );

        StoredEvent {
            id,
            timestamp: ts,
            event_type: EventType::WindowFocus,
            source: SOURCE.to_string(),
            machine_id: Some(self.machine_id.clone()),
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: Some(window.app_id.clone()),
            window_title: Some(window.title.clone()),
            action: None,
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: Value::Null,
        }
    }

    fn afk(&self, ts: DateTime<Utc>, status: &str, idle_duration_ms: Option<i64>) -> StoredEvent {
        let ts_ms = timestamp_ms_z(ts);
        let id = format!("{}:{SOURCE}:afk_change:{ts_ms}:{status}", self.machine_id);

        StoredEvent {
            id,
            timestamp: ts,
            event_type: EventType::AfkChange,
            source: SOURCE.to_string(),
            machine_id: Some(self.machine_id.clone()),
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: Some(status.to_string()),
            idle_duration_ms,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: Value::Null,
        }
    }
}

pub fn run_once<B: WindowBackend>(
    db: &tt_db::Database,
    backend: &mut B,
    state: &mut EmitState,
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let snapshot = backend.poll()?;
    let events = state.observe(&snapshot, now);
    Ok(db.insert_events(&events)?)
}

pub fn run(
    config_path: Option<&Path>,
    idle_timeout: Option<u64>,
    poll_ms: Option<u64>,
    no_write: bool,
    once: bool,
) -> Result<()> {
    let config = tt_cli::Config::load_from(config_path).context("failed to load configuration")?;
    if let Some(parent) = config.database_path.parent() {
        std::fs::create_dir_all(parent).context("failed to create database directory")?;
    }
    let db = tt_db::Database::open(&config.database_path).context("failed to open database")?;
    let identity = tt_cli::machine::require_machine_identity()?;
    let mut backend = CosmicBackend::new(idle_timeout).context(
        "failed to initialize COSMIC watcher backend; tt-watcher requires a COSMIC Wayland session",
    )?;
    let mut state = EmitState::new(identity.machine_id);
    let poll_interval = StdDuration::from_millis(poll_ms.unwrap_or(DEFAULT_POLL_MS));

    loop {
        std::thread::sleep(poll_interval);
        let emitted = if no_write {
            poll_and_print(&mut backend, &mut state)?
        } else {
            run_once(&db, &mut backend, &mut state, Utc::now())?
        };
        tracing::debug!(emitted, no_write, "watch iteration complete");

        if once {
            return Ok(());
        }
    }
}

fn poll_and_print<B: WindowBackend>(backend: &mut B, state: &mut EmitState) -> Result<usize> {
    let snapshot = backend.poll()?;
    let events = state.observe(&snapshot, Utc::now());
    for event in &events {
        println!(
            "{}",
            serde_json::to_string(event).context("failed to serialize watch event")?
        );
    }
    Ok(events.len())
}

fn timestamp_ms_z(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn short_hash(value: &str) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = value.as_bytes().iter().fold(OFFSET_BASIS, |hash, byte| {
        let xored = hash ^ u64::from(*byte);
        xored.wrapping_mul(PRIME)
    });

    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend::{ActiveWindow, FakeBackend, IdleState, Snapshot};
    use chrono::{DateTime, Duration, TimeZone, Utc};
    use tt_core::EventType;
    use tt_db::Database;

    fn aw(app: &str, title: &str) -> ActiveWindow {
        ActiveWindow {
            app_id: app.into(),
            title: title.into(),
        }
    }

    fn snap(active: Option<ActiveWindow>, idle: IdleState) -> Snapshot {
        Snapshot { active, idle }
    }

    fn t(ms: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 14, 10, 0, 0)
            .single()
            .expect("valid test timestamp")
            + Duration::milliseconds(ms)
    }

    #[test]
    fn emits_window_focus_on_startup_and_change_after_debounce() {
        let mut state = EmitState::new("machine-1".to_string());

        let startup = state.observe(&snap(Some(aw("firefox", "Docs")), IdleState::Active), t(0));
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].event_type, EventType::WindowFocus);
        assert_eq!(startup[0].window_app_id.as_deref(), Some("firefox"));
        assert_eq!(startup[0].window_title.as_deref(), Some("Docs"));

        let unchanged = state.observe(
            &snap(Some(aw("firefox", "Docs")), IdleState::Active),
            t(1_000),
        );
        assert!(unchanged.is_empty());

        let pending = state.observe(
            &snap(Some(aw("firefox", "Other")), IdleState::Active),
            t(2_000),
        );
        assert!(pending.is_empty());

        let changed = state.observe(
            &snap(Some(aw("firefox", "Other")), IdleState::Active),
            t(2_500),
        );
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].event_type, EventType::WindowFocus);
        assert_eq!(changed[0].window_title.as_deref(), Some("Other"));
    }

    #[test]
    fn debounce_delays_churned_titles_and_drops_too_fast_changes() {
        let mut state = EmitState::new("machine-1".to_string());
        state.observe(&snap(Some(aw("editor", "A")), IdleState::Active), t(0));

        assert!(
            state
                .observe(&snap(Some(aw("editor", "B")), IdleState::Active), t(100))
                .is_empty()
        );
        assert!(
            state
                .observe(&snap(Some(aw("editor", "C")), IdleState::Active), t(300))
                .is_empty()
        );
        assert!(
            state
                .observe(&snap(Some(aw("editor", "A")), IdleState::Active), t(450))
                .is_empty()
        );
        assert!(
            state
                .observe(&snap(Some(aw("editor", "A")), IdleState::Active), t(1_000))
                .is_empty()
        );
    }

    #[test]
    fn idle_is_backdated_and_resume_reemits_window_with_duration() {
        let mut state = EmitState::new("machine-1".to_string());
        state.observe(&snap(Some(aw("slack", "general")), IdleState::Active), t(0));

        let idle = state.observe(
            &snap(
                Some(aw("slack", "general")),
                IdleState::Idle { since_ms: 185_000 },
            ),
            t(185_000),
        );

        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].event_type, EventType::AfkChange);
        assert_eq!(idle[0].status.as_deref(), Some("idle"));
        assert_eq!(idle[0].timestamp, t(0));
        assert_eq!(idle[0].idle_duration_ms, None);

        let resume = state.observe(
            &snap(Some(aw("slack", "general")), IdleState::Active),
            t(600_000),
        );
        assert_eq!(resume.len(), 2);

        let active = resume
            .iter()
            .find(|event| event.event_type == EventType::AfkChange)
            .expect("active AFK event");
        assert_eq!(active.status.as_deref(), Some("active"));
        assert_eq!(active.timestamp, t(600_000));
        assert_eq!(active.idle_duration_ms, Some(600_000));

        let focus = resume
            .iter()
            .find(|event| event.event_type == EventType::WindowFocus)
            .expect("resume focus event");
        assert_eq!(focus.window_app_id.as_deref(), Some("slack"));
        assert_eq!(focus.window_title.as_deref(), Some("general"));
        assert_eq!(focus.timestamp, t(600_000));
    }

    #[test]
    fn ids_are_deterministic_and_same_timestamp_title_changes_do_not_collide() {
        let mut state = EmitState::new_with_debounce("machine-1".to_string(), Duration::zero());
        let first = state.observe(
            &snap(Some(aw("browser", "Title A")), IdleState::Active),
            t(0),
        );
        let second = state.observe(
            &snap(Some(aw("browser", "Title B")), IdleState::Active),
            t(0),
        );

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].id, second[0].id);
        assert!(
            first[0].id.starts_with(
                "machine-1:local.cosmic:window_focus:2026-06-14T10:00:00.000Z:browser:"
            )
        );

        let mut repeat = EmitState::new_with_debounce("machine-1".to_string(), Duration::zero());
        let repeated = repeat.observe(
            &snap(Some(aw("browser", "Title A")), IdleState::Active),
            t(0),
        );
        assert_eq!(first[0].id, repeated[0].id);

        let idle = state.observe(
            &snap(
                Some(aw("browser", "Title B")),
                IdleState::Idle { since_ms: 1_000 },
            ),
            t(2_000),
        );
        assert_eq!(
            idle[0].id,
            "machine-1:local.cosmic:afk_change:2026-06-14T10:00:01.000Z:idle"
        );
    }

    #[test]
    fn run_once_persists_window_and_afk_events() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let mut backend = FakeBackend::new(vec![
            snap(Some(aw("firefox", "Docs")), IdleState::Active),
            snap(
                Some(aw("firefox", "Docs")),
                IdleState::Idle { since_ms: 185_000 },
            ),
            snap(Some(aw("firefox", "Docs")), IdleState::Active),
        ]);
        let mut state = EmitState::new("machine-1".to_string());

        assert_eq!(
            run_once(&db, &mut backend, &mut state, t(0)).expect("startup insert"),
            1
        );
        assert_eq!(
            run_once(&db, &mut backend, &mut state, t(185_000)).expect("idle insert"),
            1
        );
        assert_eq!(
            run_once(&db, &mut backend, &mut state, t(600_000)).expect("resume insert"),
            2
        );

        let events = db.get_events(None, None).expect("persisted events");
        assert_eq!(events.len(), 4);
        assert!(events.iter().any(|event| {
            event.event_type == EventType::WindowFocus
                && event.window_app_id.as_deref() == Some("firefox")
                && event.window_title.as_deref() == Some("Docs")
        }));
        assert!(events.iter().any(|event| {
            event.event_type == EventType::AfkChange && event.status.as_deref() == Some("idle")
        }));
        assert!(events.iter().any(|event| {
            event.event_type == EventType::AfkChange
                && event.status.as_deref() == Some("active")
                && event.idle_duration_ms == Some(600_000)
        }));
    }

    #[test]
    fn run_once_returns_zero_when_duplicate_event_is_ignored() {
        let db = Database::open_in_memory().expect("open in-memory database");
        let snapshot = snap(Some(aw("firefox", "Docs")), IdleState::Active);

        let mut first_backend = FakeBackend::new(vec![snapshot.clone()]);
        let mut first_state = EmitState::new("machine-1".to_string());
        assert_eq!(
            run_once(&db, &mut first_backend, &mut first_state, t(0)).expect("first insert"),
            1
        );

        let mut duplicate_backend = FakeBackend::new(vec![snapshot]);
        let mut duplicate_state = EmitState::new("machine-1".to_string());
        assert_eq!(
            run_once(&db, &mut duplicate_backend, &mut duplicate_state, t(0))
                .expect("duplicate insert ignored"),
            0
        );

        let events = db.get_events(None, None).expect("persisted events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].window_app_id.as_deref(), Some("firefox"));
    }
}
