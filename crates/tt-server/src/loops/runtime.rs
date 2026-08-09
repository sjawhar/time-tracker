use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;

use super::classifier::{ClassifyInputs, classify_loop};
use super::operations::{compute_status, ingest_once, poll_db_version, read_db_version, sync_once};
use super::{DbVersionWatcher, SyncBackoff};
use crate::ServerEvent;
use tt_cli::logging;

pub struct LoopRuntimeConfig {
    pub database_path: PathBuf,
    pub config: tt_cli::Config,
    pub classifier: Option<Arc<dyn tt_llm::Classifier>>,
    pub events: broadcast::Sender<ServerEvent>,
}

pub struct LoopRuntime {
    database_path: PathBuf,
    config: tt_cli::Config,
    classifier: Option<Arc<dyn tt_llm::Classifier>>,
    events: broadcast::Sender<ServerEvent>,
}

impl LoopRuntime {
    pub fn new(config: LoopRuntimeConfig) -> Self {
        Self {
            database_path: config.database_path,
            config: config.config,
            classifier: config.classifier,
            events: config.events,
        }
    }

    pub async fn run(self, shutdown: watch::Receiver<bool>) {
        let (classify_trigger, classify_rx) = watch::channel(0_u64);
        let mut workers = JoinSet::new();
        workers.spawn(ingest_loop(
            self.database_path.clone(),
            classify_trigger.clone(),
            shutdown.clone(),
        ));
        workers.spawn(sync_loop(
            self.database_path.clone(),
            classify_trigger,
            shutdown.clone(),
        ));
        if let Some(classifier) = self.classifier {
            workers.spawn(classify_loop(
                ClassifyInputs::new(self.database_path.clone(), self.config.clone(), classifier),
                classify_rx,
                shutdown.clone(),
            ));
        }
        workers.spawn(db_version_loop(
            self.database_path,
            self.config,
            self.events,
            shutdown,
        ));

        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                tracing::error!(?error, "daemon loop stopped unexpectedly");
            }
        }
    }
}

async fn ingest_loop(
    database_path: PathBuf,
    classify_trigger: watch::Sender<u64>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = interval.tick() => {
                let Some(result) = complete_or_shutdown(&mut shutdown, ingest_once(database_path.clone())).await else {
                    return;
                };
                match result {
                    Ok(imported) if imported > 0 => arm_classification(&classify_trigger),
                    Ok(_) => {},
                    Err(error) => tracing::warn!(error = %logging::chain(&error), "session ingest failed"),
                }
            }
        }
    }
}

async fn sync_loop(
    database_path: PathBuf,
    classify_trigger: watch::Sender<u64>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut backoff = SyncBackoff::default();
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = interval.tick() => {
                let Some(result) = complete_or_shutdown(
                    &mut shutdown,
                    sync_once(database_path.clone(), &mut backoff),
                )
                .await else {
                    return;
                };
                match result {
                    Ok(imported) if imported > 0 => arm_classification(&classify_trigger),
                    Ok(_) => {},
                    Err(error) => tracing::warn!(error = %logging::chain(&error), "remote sync failed"),
                }
            }
        }
    }
}

pub(super) async fn db_version_loop(
    database_path: PathBuf,
    config: tt_cli::Config,
    events: broadcast::Sender<ServerEvent>,
    mut shutdown: watch::Receiver<bool>,
) {
    let initial_version = match read_db_version(database_path.clone()).await {
        Ok(version) => version,
        Err(error) => {
            tracing::warn!(error = %logging::chain(&error), "initial database version read failed");
            0
        }
    };
    let mut watcher = DbVersionWatcher::new(initial_version);
    let mut previous_dangling_links: Vec<String> = vec![];
    let mut previous_stale_sources: Vec<tt_cli::drift::StaleEventSource> = vec![];
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = interval.tick() => {
                let Some((next_watcher, changed)) = complete_or_shutdown(
                    &mut shutdown,
                    poll_db_version(watcher, database_path.clone(), events.clone()),
                )
                .await else {
                    return;
                };
                watcher = next_watcher;
                match changed {
                    Ok(true) => match compute_status(database_path.clone(), config.clone()).await {
                        Ok(verdict) => {
                            if tt_cli::drift::should_announce_dangling_links(&previous_dangling_links, &verdict.dangling_stream_links) {
                                for stream in &verdict.dangling_stream_links {
                                    tracing::warn!(
                                        stream = stream.as_str(),
                                        "streams.md links a stream that no longer exists; skipping it in the verdict"
                                    );
                                }
                            }
                            if tt_cli::drift::should_announce_stale_sources(&previous_stale_sources, &verdict.stale_event_sources) {
                                for source in &verdict.stale_event_sources {
                                    tracing::warn!(
                                        event_type = %source.event_type,
                                        emitter = source.emitter,
                                        last_seen = %source.last_seen,
                                        "a local event source has stopped reporting; direct time is missing an input"
                                    );
                                }
                            }
                            previous_stale_sources = verdict.stale_event_sources;
                            previous_dangling_links = verdict.dangling_stream_links;
                            match events.send(ServerEvent::StatusChanged) {
                                Ok(_) | Err(_) => {},
                            }
                        },
                        Err(error) => tracing::warn!(error = %logging::chain(&error), "status recomputation failed"),
                    },
                    Ok(false) => {},
                    Err(error) => tracing::warn!(error = %logging::chain(&error), "database version watcher failed"),
                }
            },
        }
    }
}

pub(super) async fn complete_or_shutdown<T>(
    shutdown: &mut watch::Receiver<bool>,
    task: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        _ = shutdown.changed() => None,
        result = task => Some(result),
    }
}

fn arm_classification(trigger: &watch::Sender<u64>) {
    trigger.send_modify(|version| *version = version.saturating_add(1));
}
