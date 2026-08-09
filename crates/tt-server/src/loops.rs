use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::broadcast;

use crate::ServerEvent;

mod classifier;
mod operations;
mod runtime;

pub use runtime::{LoopRuntime, LoopRuntimeConfig};

const SYNC_BACKOFF_TICKS: u8 = 5;

#[derive(Debug)]
pub struct ClassifyDebounce {
    delay: Duration,
    deadline: Option<Instant>,
}

impl ClassifyDebounce {
    pub const fn new(delay: Duration) -> Self {
        Self {
            delay,
            deadline: None,
        }
    }

    pub fn arm(&mut self, now: Instant) {
        self.deadline = Some(now + self.delay);
    }

    pub fn arm_after(&mut self, now: Instant, delay: Duration) {
        self.deadline = Some(now + delay);
    }

    pub fn take_if_due(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(deadline) if now >= deadline => {
                self.deadline = None;
                true
            }
            Some(_) | None => false,
        }
    }
}

#[derive(Debug, Default)]
pub struct SyncBackoff {
    remaining_skips: HashMap<String, u8>,
}

impl SyncBackoff {
    pub fn record_failure(&mut self, remote: &str) {
        self.remaining_skips
            .insert(remote.to_owned(), SYNC_BACKOFF_TICKS);
    }

    pub fn record_success(&mut self, remote: &str) {
        self.remaining_skips.remove(remote);
    }

    pub fn should_sync(&mut self, remote: &str) -> bool {
        let Some(remaining) = self.remaining_skips.get_mut(remote) else {
            return true;
        };
        if *remaining > 1 {
            *remaining -= 1;
            return false;
        }
        self.remaining_skips.remove(remote);
        false
    }
}

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("database error")]
    Database(#[from] tt_db::DbError),
    #[error("database version regressed from {previous} to {current}")]
    VersionRegressed { previous: i64, current: i64 },
}

#[derive(Debug, Clone)]
pub struct DbVersionWatcher {
    observed_version: i64,
}

impl DbVersionWatcher {
    pub const fn new(observed_version: i64) -> Self {
        Self { observed_version }
    }

    pub fn poll(
        &mut self,
        database_path: &Path,
        events: &broadcast::Sender<ServerEvent>,
    ) -> Result<bool, WatcherError> {
        let db = tt_db::Database::open(database_path)?;
        let current_version = db.get_db_version()?;
        drop(db);
        if current_version == self.observed_version {
            return Ok(false);
        }
        let count = current_version.checked_sub(self.observed_version).ok_or(
            WatcherError::VersionRegressed {
                previous: self.observed_version,
                current: current_version,
            },
        )?;
        let count = u64::try_from(count).map_err(|_| WatcherError::VersionRegressed {
            previous: self.observed_version,
            current: current_version,
        })?;
        self.observed_version = current_version;
        match events.send(ServerEvent::EventsAppended { count }) {
            Ok(_) | Err(_) => {}
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use anyhow::Result;
    use chrono::Utc;
    use serde_json::json;
    use tokio::sync::broadcast;
    use tt_core::EventType;
    use tt_db::{Database, StoredEvent};

    use super::{ClassifyDebounce, DbVersionWatcher, SyncBackoff};
    use crate::ServerEvent;

    #[derive(Default)]
    struct MockClassifier {
        run_auto_calls: usize,
    }

    impl MockClassifier {
        fn run_auto(&mut self) {
            self.run_auto_calls += 1;
        }
    }

    #[test]
    fn classify_debounce_runs_auto_once_when_two_rapid_db_version_bumps_arrive() {
        // Given
        let start = Instant::now();
        let mut debounce = ClassifyDebounce::new(Duration::from_secs(5));
        let mut classifier = MockClassifier::default();

        // When
        debounce.arm(start);
        debounce.arm(start + Duration::from_secs(2));
        let before_deadline = debounce.take_if_due(start + Duration::from_secs(6));
        let at_deadline = debounce.take_if_due(start + Duration::from_secs(7));
        if at_deadline {
            classifier.run_auto();
        }

        // Then
        assert!(!before_deadline);
        assert!(at_deadline);
        assert_eq!(classifier.run_auto_calls, 1);
    }

    #[test]
    fn sync_backoff_skips_a_failing_remote_for_five_ticks() {
        // Given
        let mut backoff = SyncBackoff::default();
        backoff.record_failure("offline");

        // When
        let skipped: Vec<_> = (0..5).map(|_| backoff.should_sync("offline")).collect();
        let resumed = backoff.should_sync("offline");

        // Then
        assert_eq!(skipped, vec![false; 5]);
        assert!(resumed);
    }

    #[test]
    fn db_version_watcher_broadcasts_external_temp_file_writes() -> Result<()> {
        // Given
        let database_file = tempfile::NamedTempFile::new()?;
        let path = database_file.path();
        let db = Database::open(path)?;
        let mut watcher = DbVersionWatcher::new(db.get_db_version()?);
        drop(db);
        let (sender, mut receiver) = broadcast::channel(1);

        // When
        insert_external_event(path)?;
        let changed = watcher.poll(path, &sender)?;

        // Then
        assert!(changed);
        assert_eq!(
            receiver.try_recv()?,
            ServerEvent::EventsAppended { count: 1 }
        );
        Ok(())
    }

    fn insert_external_event(path: &Path) -> Result<()> {
        let db = Database::open(path)?;
        let event = StoredEvent {
            id: "external-event".to_owned(),
            timestamp: Utc::now(),
            event_type: EventType::TmuxPaneFocus,
            source: "test".to_owned(),
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
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: json!({}),
        };
        db.insert_event(&event)?;
        Ok(())
    }
}
