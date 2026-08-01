//! When the next classification pass runs.
//!
//! Two questions, one answer each. After a failed pass, how long to wait before
//! spending money again — [`classifier_retry_delay`]. After a pass that worked,
//! whether to run again at all rather than wait for new events to arrive —
//! [`should_drain`].
//!
//! The second question is why draining exists. Classification used to be armed only by
//! an import, and a pass is bounded, so a backlog larger than one pass was structurally
//! unreachable: the daemon kept pace with new work and never caught up on old, which
//! left "where did my direct time go" unanswered for everything older than a day.
//!
//! The whole risk is the opposite failure — re-running a pass that cannot advance,
//! paying for the same refusals every few seconds. [`should_drain`] is what rules it out.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tt_db::ClassifierHealth;

use super::ClassifyDebounce;
use super::operations::{classify_once, pending_candidates, read_classifier_health};
use super::runtime::complete_or_shutdown;
use tt_cli::logging;

const CLASSIFIER_FAILURE_BACKOFF_BASE: Duration = Duration::from_secs(5);
// Cap transient retries at five minutes to protect provider quotas while recovering promptly.
const CLASSIFIER_FAILURE_BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);
// Credentials cannot self-heal in a running process, so 401/403 waits an hour before retrying.
const CLASSIFIER_AUTH_FAILURE_BACKOFF: Duration = Duration::from_secs(60 * 60);

/// How long the loop settles before running a pass, and waits between drained ones.
///
/// Between passes it is a yield window rather than a throttle: a pass costs minutes of
/// model calls, so five seconds adds nothing measurable to a drain, and it guarantees
/// the loop returns to its `select!` — where shutdown and fresh triggers are read —
/// no matter how the progress check behaves.
const CLASSIFY_DEBOUNCE: Duration = Duration::from_secs(5);

/// Candidates the drain probe reads on each side of a pass.
///
/// Meant to be at least `classify_auto`'s per-pass **session** bound, so the probe spans
/// every candidate the pass could have advanced past. Overshooting is harmless: the extra
/// rows are candidates no pass touched, and they simply never leave the set. Falling
/// short is merely conservative — progress the probe cannot see reads as none, and the
/// loop waits for the next trigger instead of draining.
///
/// Sessions only, and the probe deliberately does not grow a window-run half. A pass now
/// bounds its window-focus phase too, so runs can outlive one and this probe cannot see
/// them — which errs in the safe direction, because an unseen leftover makes the loop
/// wait for the next trigger rather than spin. Probing them would mean loading every
/// unassigned `window_focus` row twice per pass to answer a question ingest already
/// answers: the tmux hook's focus events arrive through `import_local_events`, so focus
/// inflow arms classification on its own every ~30s.
const DRAIN_PROBE_SESSIONS: usize = 200;

/// What one classification pass runs against.
pub(super) struct ClassifyInputs {
    database_path: PathBuf,
    config: tt_cli::Config,
    classifier: Arc<dyn tt_llm::Classifier>,
    debounce: Duration,
}

impl ClassifyInputs {
    pub(super) fn new(
        database_path: PathBuf,
        config: tt_cli::Config,
        classifier: Arc<dyn tt_llm::Classifier>,
    ) -> Self {
        Self {
            database_path,
            config,
            classifier,
            debounce: CLASSIFY_DEBOUNCE,
        }
    }
}

/// Runs automatic classification: at startup, on a trigger, and again while a drain
/// is advancing.
pub(super) async fn classify_loop(
    inputs: ClassifyInputs,
    mut classify_trigger: watch::Receiver<u64>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut debounce = ClassifyDebounce::new(inputs.debounce);

    // Arm once at startup. Every other arm comes from an import (`imported > 0`) or
    // from a drain that is still advancing, so a daemon restarted onto an existing
    // backlog and receiving no new events would otherwise never classify at all —
    // measured: 0 sessions in 7 minutes against a 3,800-session backlog. A pass with
    // nothing to do stops on its own, so the cost of being wrong here is one probe.
    schedule_classification(&mut debounce, &inputs).await;

    loop {
        let Some(deadline) = debounce.deadline else {
            tokio::select! {
                biased;
                _ = shutdown.changed() => return,
                result = classify_trigger.changed() => match result {
                    Ok(()) => schedule_classification(&mut debounce, &inputs).await,
                    Err(_) => return,
                },
            }
            continue;
        };
        tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            result = classify_trigger.changed() => match result {
                Ok(()) => schedule_classification(&mut debounce, &inputs).await,
                Err(_) => return,
            },
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                if debounce.take_if_due(Instant::now())
                    && run_pass(&inputs, &mut debounce, &mut shutdown).await.is_none()
                {
                    return;
                }
            },
        }
    }
}

/// Arms the next pass, waiting out any failure backoff the classifier has accrued.
async fn schedule_classification(debounce: &mut ClassifyDebounce, inputs: &ClassifyInputs) {
    let retry_delay = match read_classifier_health(inputs.database_path.clone()).await {
        Ok(health) => classifier_retry_delay(&health),
        Err(error) => {
            tracing::warn!(error = %logging::chain(&error), "classifier health read failed; using normal debounce");
            Duration::ZERO
        }
    };
    debounce.arm_after(Instant::now(), retry_delay.max(inputs.debounce));
}

/// Runs one bounded pass and decides what follows it.
///
/// `None` means shutdown arrived mid-pass and the loop should stop.
async fn run_pass(
    inputs: &ClassifyInputs,
    debounce: &mut ClassifyDebounce,
    shutdown: &mut watch::Receiver<bool>,
) -> Option<()> {
    let before = complete_or_shutdown(
        shutdown,
        pending_candidates(inputs.database_path.clone(), DRAIN_PROBE_SESSIONS),
    )
    .await?
    .unwrap_or_else(|error| {
        tracing::warn!(error = %logging::chain(&error), "classification backlog probe failed; this pass will not drain");
        HashSet::new()
    });
    let result = complete_or_shutdown(
        shutdown,
        classify_once(
            inputs.database_path.clone(),
            inputs.config.clone(),
            Arc::clone(&inputs.classifier),
        ),
    )
    .await?;
    match result {
        Ok(attempt) if attempt.had_failure => {
            let delay = classifier_retry_delay(&attempt.health);
            tracing::warn!(
                consecutive_failures = attempt.health.consecutive_failures,
                retry_seconds = delay.as_secs(),
                "automatic classification failed; retry scheduled"
            );
            debounce.arm_after(Instant::now(), delay);
        }
        Ok(_) => drain_if_advanced(inputs, debounce, &before).await,
        Err(error) => {
            tracing::warn!(error = %logging::chain(&error), "automatic classification task failed");
        }
    }
    Some(())
}

/// Schedules another pass when this one advanced and the backlog outlived it.
async fn drain_if_advanced(
    inputs: &ClassifyInputs,
    debounce: &mut ClassifyDebounce,
    before: &HashSet<String>,
) {
    let probe = pending_candidates(inputs.database_path.clone(), DRAIN_PROBE_SESSIONS).await;
    let after = match probe {
        Ok(after) => after,
        Err(error) => {
            tracing::warn!(
                error = %logging::chain(&error),
                "classification backlog probe failed; waiting for the next trigger"
            );
            return;
        }
    };
    if !should_drain(before, &after) {
        return;
    }
    tracing::info!(
        remaining = after.len(),
        "classification backlog outlived a bounded pass; scheduling another"
    );
    debounce.arm_after(Instant::now(), inputs.debounce);
}

/// Whether a finished pass should be followed by another without a new import.
///
/// `before` is the candidate set the pass was handed, `after` the one the next pass
/// would be handed. Both halves are load-bearing:
///
/// - **Something is left.** `after` is literally the next pass's input, so an empty one
///   means there is nothing to run.
/// - **Something moved.** At least one candidate this pass looked at is gone, which
///   happens only when the classifier placed it, junked it, or queued it for review.
///   Re-arming on remaining work alone would repeat a pass whose every verdict is
///   refused: those candidates survive every pass, so "work remains" stays true forever
///   while nothing changes, and each repetition costs another bounded pass of calls.
///
/// The pass's own tallies cannot answer the second half. `assigned` counts events, not
/// sessions, and includes window-focus runs the watcher keeps supplying, so a pass could
/// report progress on every run while every session it touched was refused.
///
/// It is a set difference rather than a count for the same reason. Both probes are
/// capped at [`DRAIN_PROBE_SESSIONS`], and the live backlog is some fourteen thousand
/// candidates deep, so a pass that places fifty is immediately backfilled to a full
/// two hundred: measured on a copy of the real database, the count before and after is
/// 200 either way, and a count would stall the drain on its first pass.
fn should_drain(before: &HashSet<String>, after: &HashSet<String>) -> bool {
    !after.is_empty() && before.iter().any(|session_id| !after.contains(session_id))
}

pub(super) fn classifier_retry_delay(health: &ClassifierHealth) -> Duration {
    if health.consecutive_failures == 0 {
        return Duration::ZERO;
    }
    if health.last_error.as_deref().is_some_and(is_auth_failure) {
        return CLASSIFIER_AUTH_FAILURE_BACKOFF;
    }

    let exponent = health.consecutive_failures.saturating_sub(1).min(6);
    let seconds = CLASSIFIER_FAILURE_BACKOFF_BASE
        .as_secs()
        .saturating_mul(1_u64 << exponent)
        .min(CLASSIFIER_FAILURE_BACKOFF_CAP.as_secs());
    Duration::from_secs(seconds)
}

fn is_auth_failure(error: &str) -> bool {
    if error.contains("401") || error.contains("403") {
        return true;
    }
    let normalized = error.to_ascii_lowercase();
    normalized.contains("unauthorized") || normalized.contains("forbidden")
}

#[cfg(test)]
mod tests;
