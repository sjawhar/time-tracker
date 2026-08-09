use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::broadcast;

use super::{DbVersionWatcher, SyncBackoff};
use crate::ServerEvent;

pub(super) struct ClassifyAttempt {
    pub(super) health: tt_db::ClassifierHealth,
    pub(super) had_failure: bool,
}

/// Runs one ingest tick.
///
/// Incremental: this fires every ~30s, and re-deriving the whole corpus each time is
/// what pinned the daemon at ~200% CPU. A forced full pass is `tt ingest sessions
/// --full`, run by hand.
pub async fn ingest_once(database_path: PathBuf) -> Result<usize> {
    tokio::task::spawn_blocking(move || {
        let db = tt_db::Database::open(&database_path)?;
        Ok(tt_cli::commands::ingest::index_sessions_quiet(
            &db,
            tt_cli::commands::ingest::ScanMode::Incremental,
        )?
        .imported_events())
    })
    .await
    .context("ingest task panicked")?
}

pub async fn sync_once(database_path: PathBuf, backoff: &mut SyncBackoff) -> Result<usize> {
    let remotes = tokio::task::spawn_blocking({
        let database_path = database_path.clone();
        move || {
            let db = tt_db::Database::open(&database_path)?;
            Ok::<Vec<String>, anyhow::Error>(
                db.list_machines()?
                    .into_iter()
                    .map(|machine| machine.label)
                    .collect(),
            )
        }
    })
    .await
    .context("machine-list task panicked")??;
    let remotes: Vec<_> = remotes
        .into_iter()
        .filter(|remote| backoff.should_sync(remote))
        .collect();
    if remotes.is_empty() {
        return Ok(0);
    }
    let report = tokio::task::spawn_blocking(move || {
        let db = tt_db::Database::open(&database_path)?;
        tt_cli::commands::sync::sync_all(
            &db,
            &remotes,
            &tt_cli::commands::sync::SyncMode::Incremental,
        )
    })
    .await
    .context("sync task panicked")??;
    for machine in &report.machines {
        match machine {
            tt_cli::commands::sync::SyncMachineReport::Imported { remote, .. } => {
                backoff.record_success(remote);
            }
            tt_cli::commands::sync::SyncMachineReport::Failed { remote, error } => {
                backoff.record_failure(remote);
                tracing::warn!(remote, %error, "remote sync failed; backing off for five ticks");
            }
        }
    }
    Ok(report.imported_events())
}

pub(super) async fn classify_once(
    database_path: PathBuf,
    config: tt_cli::Config,
    classifier: Arc<dyn tt_llm::Classifier>,
) -> Result<ClassifyAttempt> {
    tokio::task::spawn_blocking(move || -> Result<ClassifyAttempt> {
        let db = tt_db::Database::open(&database_path)?;
        let had_failure =
            match tt_cli::commands::classify_auto::run_auto(&db, &config, &*classifier) {
                Ok(outcome) => outcome.errors > 0,
                Err(error) => {
                    db.record_classifier_failure(Utc::now(), &error.to_string())
                        .context("record classifier failure")?;
                    true
                }
            };
        Ok(ClassifyAttempt {
            health: db.get_classifier_health()?,
            had_failure,
        })
    })
    .await
    .context("classification task panicked")?
}

/// Reads the session ids a bounded classification pass would be handed next.
///
/// This is the query `tt classify --auto` selects its candidates with, so a set taken
/// before a pass and one taken after say between them whether the pass advanced past
/// anything it looked at. Ids only: the decision is a set comparison, and the prompts
/// each row carries are what would make keeping the rows expensive.
pub(super) async fn pending_candidates(
    database_path: PathBuf,
    limit: usize,
) -> Result<HashSet<String>> {
    tokio::task::spawn_blocking(move || -> Result<HashSet<String>> {
        let db = tt_db::Database::open(&database_path)?;
        Ok(db
            .unclassified_user_sessions(limit)?
            .into_iter()
            .map(|(session, _)| session.session_id)
            .collect())
    })
    .await
    .context("classification backlog probe panicked")?
}

pub(super) async fn read_classifier_health(
    database_path: PathBuf,
) -> Result<tt_db::ClassifierHealth> {
    tokio::task::spawn_blocking(move || {
        let db = tt_db::Database::open(&database_path)?;
        Ok(db.get_classifier_health()?)
    })
    .await
    .context("classifier-health task panicked")?
}

pub async fn read_db_version(database_path: PathBuf) -> Result<i64> {
    tokio::task::spawn_blocking(move || {
        let db = tt_db::Database::open(&database_path)?;
        Ok(db.get_db_version()?)
    })
    .await
    .context("database-version task panicked")?
}

pub async fn poll_db_version(
    mut watcher: DbVersionWatcher,
    database_path: PathBuf,
    events: broadcast::Sender<ServerEvent>,
) -> (DbVersionWatcher, Result<bool>) {
    let fallback = watcher.clone();
    match tokio::task::spawn_blocking(move || {
        let result = watcher
            .poll(&database_path, &events)
            .map_err(anyhow::Error::from);
        (watcher, result)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => (
            fallback,
            Err(anyhow::Error::new(error).context("database watcher task panicked")),
        ),
    }
}

pub async fn compute_status(
    database_path: PathBuf,
    config: tt_cli::Config,
) -> Result<tt_cli::drift::Verdict> {
    tokio::task::spawn_blocking(move || {
        let db = tt_db::Database::open(&database_path)?;
        tt_cli::drift::compute_verdict(&db, &config, Utc::now())
    })
    .await
    .context("status task panicked")?
}
