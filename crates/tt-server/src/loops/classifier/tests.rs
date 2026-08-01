//! What decides whether one bounded pass is followed by another.
//!
//! Every test drives the real [`classify_loop`] against a file-backed database and a
//! scripted classifier, because the property under test is a scheduling decision the
//! loop makes from what the database says afterwards — a unit test of the predicate
//! alone would not catch a loop that never consults it.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;
use tokio::sync::{broadcast, watch};
use tt_core::EventType;
use tt_core::session::{AgentSession, SessionSource, SessionType};
use tt_db::{Database, StoredEvent};
use tt_llm::{ClassificationOutput, Classifier, LlmError, MockClassifier, StreamChoice};

use super::{
    ClassifyInputs, DRAIN_PROBE_SESSIONS, classifier_retry_delay, classify_loop, should_drain,
};
use crate::ServerEvent;
use crate::loops::operations::pending_candidates;
use crate::loops::runtime::db_version_loop;

/// A stream the fixture really holds, so a verdict naming it assigns.
const LIVE_STREAM: &str = "stream-live";
/// A stream id no row carries, so a verdict naming it is refused and leaves its
/// candidate unassigned — a pass that advances nothing without failing.
const DISSOLVED_STREAM: &str = "stream-dissolved";

#[tokio::test]
async fn a_pass_that_leaves_candidates_it_advanced_past_schedules_another_without_a_new_import()
-> Result<()> {
    // Given: three unclassified sessions, and a classifier that places the newest and
    // refuses the rest, so one pass cannot clear the backlog.
    let database_file = tempfile::NamedTempFile::new()?;
    seed_unclassified_sessions(database_file.path(), 3)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let classifier = counting_classifier(&calls, |session_id| {
        Ok(verdict(if session_id == "session-0" {
            LIVE_STREAM
        } else {
            DISSOLVED_STREAM
        }))
    });

    // When: a single trigger arrives and nothing imports afterwards.
    let (trigger, shutdown, task) = spawn_loop(database_file.path(), classifier);
    trigger.send_modify(|version| *version += 1);
    let drained = wait_until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) > 3).await;
    shutdown.send(true)?;
    task.await?;

    // Then: a second pass ran on the leftovers without a second trigger.
    assert!(
        drained,
        "expected a second pass; classifier calls stopped at {}",
        calls.load(Ordering::SeqCst)
    );
    Ok(())
}

#[tokio::test]
async fn a_backlog_present_at_startup_is_classified_without_any_import() -> Result<()> {
    // Given: two unclassified sessions already in the database when the daemon starts,
    // and a classifier that places both.
    let database_file = tempfile::NamedTempFile::new()?;
    seed_unclassified_sessions(database_file.path(), 2)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let classifier = counting_classifier(&calls, |_| Ok(verdict(LIVE_STREAM)));

    // When: the loop starts and NO trigger is ever sent — no ingest, no sync, no import.
    let (_trigger, shutdown, task) = spawn_loop(database_file.path(), classifier);
    let ran = wait_until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) > 0).await;
    shutdown.send(true)?;
    task.await?;

    // Then: the backlog was still classified. Arming only on `imported > 0` left a
    // restarted daemon idle against a 3,800-session backlog — measured at 0 sessions
    // in 7 minutes before the startup arm existed.
    assert!(
        ran,
        "expected the startup arm to classify the existing backlog with no import"
    );
    Ok(())
}

#[tokio::test]
async fn a_pass_that_leaves_no_candidates_does_not_schedule_another() -> Result<()> {
    // Given: two unclassified sessions and a classifier that places both.
    let database_file = tempfile::NamedTempFile::new()?;
    seed_unclassified_sessions(database_file.path(), 2)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let classifier = counting_classifier(&calls, |_| Ok(verdict(LIVE_STREAM)));

    // When: one trigger arrives and the loop is left alone well past its debounce.
    let (trigger, shutdown, task) = spawn_loop(database_file.path(), classifier);
    trigger.send_modify(|version| *version += 1);
    let ran = wait_until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) >= 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after_idling = calls.load(Ordering::SeqCst);
    shutdown.send(true)?;
    task.await?;

    // Then: the pass ran once and the emptied backlog scheduled nothing further.
    assert!(ran, "expected the first pass to classify both sessions");
    assert_eq!(after_idling, 2);
    Ok(())
}

#[tokio::test]
async fn a_pass_that_advances_no_candidate_does_not_schedule_another() -> Result<()> {
    // Given: two unclassified sessions and a classifier whose every verdict names a
    // stream that no longer exists, so both candidates survive the pass untouched.
    let database_file = tempfile::NamedTempFile::new()?;
    seed_unclassified_sessions(database_file.path(), 2)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let classifier = counting_classifier(&calls, |_| Ok(verdict(DISSOLVED_STREAM)));

    // When: one trigger arrives and the loop is left alone well past its debounce.
    let (trigger, shutdown, task) = spawn_loop(database_file.path(), classifier);
    trigger.send_modify(|version| *version += 1);
    let ran = wait_until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) >= 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after_idling = calls.load(Ordering::SeqCst);
    shutdown.send(true)?;
    task.await?;

    // Then: work remained, but a pass that moved nothing does not pay to repeat itself.
    assert!(ran, "expected the first pass to reach both sessions");
    assert_eq!(after_idling, 2);
    Ok(())
}

#[tokio::test]
async fn a_failing_classifier_waits_out_its_backoff_even_though_the_pass_advanced() -> Result<()> {
    // Given: two sessions every call fails on, plus one that structure alone routes to
    // junk — so the pass advances a candidate without a single call succeeding, and a
    // drain that ignored the failure would find progress to act on.
    let database_file = tempfile::NamedTempFile::new()?;
    seed_unclassified_sessions(database_file.path(), 2)?;
    seed_junk_session(database_file.path())?;
    let calls = Arc::new(AtomicUsize::new(0));
    let classifier = counting_classifier(&calls, |_| {
        Err(LlmError::Api("provider unavailable".to_owned()))
    });

    // When: one trigger arrives and the loop is left alone well past its debounce.
    let (trigger, shutdown, task) = spawn_loop(database_file.path(), classifier);
    trigger.send_modify(|version| *version += 1);
    let ran = wait_until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) >= 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after_idling = calls.load(Ordering::SeqCst);
    let health = Database::open(database_file.path())?.get_classifier_health()?;
    shutdown.send(true)?;
    task.await?;

    // Then: the accrued failures govern the next attempt, which is seconds away rather
    // than a debounce away, so no second pass has been paid for.
    assert!(ran, "expected the first pass to attempt both sessions");
    assert_eq!(after_idling, 2);
    assert_eq!(health.consecutive_failures, 2);
    Ok(())
}

#[tokio::test]
async fn a_classifier_that_spends_its_turn_budget_does_not_wait_out_a_backoff() -> Result<()> {
    // Given: two sessions whose classifications each burn every model call they are
    // allowed without ever delivering a verdict. Reaching `tt-cli` as an error, each one
    // incremented `classifier_consecutive_failures` and armed `classifier_retry_delay`
    // — exponential and capped at five minutes — which is what cut the live drain from
    // 1.67 to 0.17 classifications a minute.
    let database_file = tempfile::NamedTempFile::new()?;
    seed_unclassified_sessions(database_file.path(), 2)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let classifier = counting_classifier(&calls, |_| Ok(exhausted_verdict()));

    // When: one trigger arrives and the pass runs to its end.
    let (trigger, shutdown, task) = spawn_loop(database_file.path(), classifier);
    trigger.send_modify(|version| *version += 1);
    let ran = wait_until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) >= 2).await;
    // The pass records success at its end, so this is the positive signal that it
    // finished — and it is also the assertion, because a pass that counted these as
    // failures would never record one.
    let finished = wait_until(Duration::from_secs(5), || {
        classifier_health(database_file.path())
            .is_some_and(|health| health.last_success_at.is_some())
    })
    .await;
    let health = Database::open(database_file.path())?.get_classifier_health()?;
    shutdown.send(true)?;
    task.await?;

    // Then: nothing accrued against the classifier, so the next trigger is served when
    // it arrives rather than after a backoff. Nothing failed, because the provider
    // answered every call — the loop simply never converged on a verdict.
    assert!(ran, "expected the first pass to attempt both sessions");
    assert!(finished, "the pass must record success, not failure");
    assert_eq!(health.consecutive_failures, 0);
    assert!(health.last_error.is_none());
    assert!(health.last_success_at.is_some());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_draining_classifier_does_not_stop_the_database_version_loop() -> Result<()> {
    // Given: a backlog the classifier clears one session per pass, slowly enough that
    // it is still draining when the two-second database-version tick comes round.
    let database_file = tempfile::NamedTempFile::new()?;
    seed_unclassified_sessions(database_file.path(), 20)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let placed = Arc::new(AtomicUsize::new(0));
    let classifier = drip_classifier(&calls, &placed);
    let (events, mut subscriber) = broadcast::channel(16);
    let (trigger, shutdown, classify_task) = spawn_loop(database_file.path(), classifier);
    let version_task = tokio::spawn(db_version_loop(
        database_file.path().to_path_buf(),
        test_config(database_file.path()),
        events,
        shutdown.subscribe(),
    ));

    // When: another process appends an event while the drain is under way.
    trigger.send_modify(|version| *version += 1);
    let started = wait_until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) > 0).await;
    insert_external_event(database_file.path())?;
    let before_broadcast = calls.load(Ordering::SeqCst);
    let broadcast = tokio::time::timeout(Duration::from_secs(8), subscriber.recv()).await??;
    let after_broadcast = calls.load(Ordering::SeqCst);
    let passes = placed.load(Ordering::SeqCst);
    shutdown.send(true)?;
    classify_task.await?;
    version_task.await?;

    // Then: the sibling loop reported the append part-way through a multi-pass drain,
    // and classification carried on across it.
    assert!(started, "expected the drain to begin");
    assert!(matches!(broadcast, ServerEvent::EventsAppended { .. }));
    assert!(
        passes > 1,
        "expected the broadcast to land mid-drain, but only {passes} pass(es) had run"
    );
    assert!(
        after_broadcast > before_broadcast,
        "expected classification to keep working across the broadcast, \
         but calls stalled at {before_broadcast}"
    );
    Ok(())
}

type LoopHandles = (
    watch::Sender<u64>,
    watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
);

/// Starts a classification loop whose debounce is short enough to observe.
fn spawn_loop(database_path: &Path, classifier: Arc<dyn Classifier>) -> LoopHandles {
    let (trigger, trigger_rx) = watch::channel(0_u64);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut inputs = ClassifyInputs::new(
        database_path.to_path_buf(),
        test_config(database_path),
        classifier,
    );
    inputs.debounce = Duration::from_millis(10);
    (
        trigger,
        shutdown,
        tokio::spawn(classify_loop(inputs, trigger_rx, shutdown_rx)),
    )
}

fn test_config(database_path: &Path) -> tt_cli::Config {
    tt_cli::Config {
        database_path: database_path.to_path_buf(),
        todo_store_path: database_path.with_extension("todos"),
        ..tt_cli::Config::default()
    }
}

/// A classifier that counts its calls and answers however the test decides.
fn counting_classifier(
    calls: &Arc<AtomicUsize>,
    answer: impl Fn(&str) -> Result<ClassificationOutput, LlmError> + Send + Sync + 'static,
) -> Arc<dyn Classifier> {
    let calls = Arc::clone(calls);
    Arc::new(MockClassifier {
        brain: Some(Box::new(move |input, _session| {
            calls.fetch_add(1, Ordering::SeqCst);
            answer(&input.session_id)
        })),
        ..MockClassifier::default()
    })
}

/// A classifier that places exactly one session per pass, taking real time per call.
///
/// Sessions are seeded newest-first, so every pass leads with `session-{placed}`: this
/// places that one, refuses the rest of the pass, and the next pass leads with the next
/// id. That is the shape of a backlog no single bounded pass can clear.
fn drip_classifier(calls: &Arc<AtomicUsize>, placed: &Arc<AtomicUsize>) -> Arc<dyn Classifier> {
    let placed = Arc::clone(placed);
    let just_placed = Arc::new(AtomicBool::new(false));
    counting_classifier(calls, move |session_id| {
        std::thread::sleep(Duration::from_millis(15));
        let leads_the_pass = session_id == format!("session-{}", placed.load(Ordering::SeqCst));
        // A pass that just placed one moves on to the id that will lead the *next*
        // pass; without this the whole backlog cascades into a single pass.
        if leads_the_pass && !just_placed.swap(true, Ordering::SeqCst) {
            placed.fetch_add(1, Ordering::SeqCst);
            return Ok(verdict(LIVE_STREAM));
        }
        just_placed.store(false, Ordering::SeqCst);
        Ok(verdict(DISSOLVED_STREAM))
    })
}

fn verdict(stream_id: &str) -> ClassificationOutput {
    ClassificationOutput {
        choice: StreamChoice::Existing {
            stream_id: stream_id.to_owned(),
        },
        confidence: 0.99,
        reasoning: "fixture".to_owned(),
    }
}

/// The answer a classification gives when it spent every model call without reaching one.
fn exhausted_verdict() -> ClassificationOutput {
    ClassificationOutput {
        choice: StreamChoice::TurnsExhausted,
        confidence: 0.0,
        reasoning: "fixture".to_owned(),
    }
}

/// The persisted classifier health, or `None` while the database cannot be read.
fn classifier_health(database_path: &Path) -> Option<tt_db::ClassifierHealth> {
    Database::open(database_path)
        .and_then(|db| db.get_classifier_health())
        .ok()
}

/// Seeds one live stream plus `count` user sessions holding unassigned events.
fn seed_unclassified_sessions(database_path: &Path, count: usize) -> Result<()> {
    let db = Database::open(database_path)?;
    let now = Utc::now();
    db.insert_stream(&tt_db::Stream {
        id: LIVE_STREAM.to_owned(),
        name: Some("time-tracker daemon".to_owned()),
        slug: Some("tt-daemon".to_owned()),
        description: Some("the daemon's own work".to_owned()),
        color: None,
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })?;
    for index in 0..count {
        let session_id = format!("session-{index}");
        let started = now - ChronoDuration::minutes(i64::try_from(index)?);
        db.upsert_agent_session(
            &AgentSession {
                session_id: session_id.clone(),
                source: SessionSource::Claude,
                parent_session_id: None,
                session_type: SessionType::User,
                project_path: "/home/tester/project".to_owned(),
                project_name: "project".to_owned(),
                start_time: started,
                end_time: None,
                message_count: 8,
                summary: None,
                user_prompts: vec![format!("do the {index} thing")],
                starting_prompt: Some(format!("do the {index} thing")),
                assistant_message_count: 4,
                tool_call_count: 6,
                user_message_timestamps: Vec::new(),
                tool_call_timestamps: Vec::new(),
            },
            Some("machine-1"),
        )?;
        db.insert_event(&unassigned_event(&session_id, started))?;
    }
    Ok(())
}

/// Seeds a session structure alone routes to junk: no tool call, no exchange to judge.
fn seed_junk_session(database_path: &Path) -> Result<()> {
    let db = Database::open(database_path)?;
    let started = Utc::now() - ChronoDuration::hours(1);
    db.upsert_agent_session(
        &AgentSession {
            session_id: "session-junk".to_owned(),
            source: SessionSource::Claude,
            parent_session_id: None,
            session_type: SessionType::User,
            project_path: "/home/tester/project".to_owned(),
            project_name: "project".to_owned(),
            start_time: started,
            end_time: None,
            message_count: 2,
            summary: None,
            user_prompts: Vec::new(),
            starting_prompt: None,
            assistant_message_count: 1,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        },
        Some("machine-1"),
    )?;
    db.insert_event(&unassigned_event("session-junk", started))?;
    Ok(())
}

fn insert_external_event(database_path: &Path) -> Result<()> {
    let db = Database::open(database_path)?;
    db.insert_event(&unassigned_event("external", Utc::now()))?;
    Ok(())
}

fn unassigned_event(session_id: &str, timestamp: chrono::DateTime<Utc>) -> StoredEvent {
    StoredEvent {
        id: format!("event-{session_id}"),
        timestamp,
        event_type: EventType::UserMessage,
        source: "test".to_owned(),
        machine_id: Some("machine-1".to_owned()),
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
        cwd: Some("/home/tester/project".to_owned()),
        session_id: Some(session_id.to_owned()),
        stream_id: None,
        assignment_source: None,
        data: json!({}),
    }
}

/// Polls `condition` until it holds or `limit` elapses.
async fn wait_until(limit: Duration, condition: impl Fn() -> bool) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < limit {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    condition()
}

#[test]
fn classifier_retry_delay_grows_from_persisted_failures_and_resets_on_success() -> Result<()> {
    // Given: classifier health persisted in a file-backed database.
    let database_file = tempfile::NamedTempFile::new()?;
    let db = Database::open(database_file.path())?;

    // When: failures accumulate, then a successful call resets the health counter.
    db.record_classifier_failure(Utc::now(), "temporary outage")?;
    let first_delay = classifier_retry_delay(&db.get_classifier_health()?);
    db.record_classifier_failure(Utc::now(), "temporary outage")?;
    let second_delay = classifier_retry_delay(&db.get_classifier_health()?);
    for _ in 0..8 {
        db.record_classifier_failure(Utc::now(), "temporary outage")?;
    }
    let capped_delay = classifier_retry_delay(&db.get_classifier_health()?);
    db.record_classifier_success(Utc::now())?;
    let reset_delay = classifier_retry_delay(&db.get_classifier_health()?);

    // Then: retries slow down exponentially and resume immediately after success.
    assert_eq!(first_delay, Duration::from_secs(5));
    assert_eq!(second_delay, Duration::from_secs(10));
    assert_eq!(capped_delay, Duration::from_secs(300));
    assert_eq!(reset_delay, Duration::ZERO);
    Ok(())
}

#[test]
fn classifier_retry_delay_uses_hard_backoff_for_auth_failures() -> Result<()> {
    // Given: a persisted authentication failure from the provider.
    let database_file = tempfile::NamedTempFile::new()?;
    let db = Database::open(database_file.path())?;
    db.record_classifier_failure(Utc::now(), "model call failed: 401 Unauthorized")?;

    // When: the classifier scheduler reads health before its next pass.
    let retry_delay = classifier_retry_delay(&db.get_classifier_health()?);

    // Then: it does not retry on the normal fast path.
    assert_eq!(retry_delay, Duration::from_secs(3_600));
    Ok(())
}

#[test]
fn a_pass_that_emptied_the_backlog_schedules_nothing_although_it_advanced() {
    // Given: a pass that placed every candidate it was handed.
    let before = HashSet::from(["session-0".to_owned(), "session-1".to_owned()]);
    let after = HashSet::new();

    // When: the loop asks whether to run again.
    let drains = should_drain(&before, &after);

    // Then: it does not. A pass with no candidates is not free — it still walks the
    // whole event table looking for unassigned window-focus runs.
    assert!(!drains);
}

#[test]
fn a_pass_that_advanced_nothing_schedules_no_repeat_of_itself() {
    // Given: a pass that refused every candidate, so all of them survive it.
    let before = HashSet::from(["session-0".to_owned(), "session-1".to_owned()]);
    let after = before.clone();

    // When: the loop asks whether to run again.
    let drains = should_drain(&before, &after);

    // Then: it does not. Work remains, but repeating it would buy the same refusals.
    assert!(!drains);
}

#[tokio::test]
async fn a_saturated_backlog_shows_progress_a_remaining_count_would_hide() -> Result<()> {
    // Given: more candidates than one bounded pass can hold, which is the daemon's
    // ordinary state — the live database carries some fourteen thousand of them.
    let database_file = tempfile::NamedTempFile::new()?;
    seed_unclassified_sessions(database_file.path(), DRAIN_PROBE_SESSIONS + 5)?;
    let before =
        pending_candidates(database_file.path().to_path_buf(), DRAIN_PROBE_SESSIONS).await?;

    // When: a pass places one of the candidates it was handed, and the probe refills
    // from the backlog behind it.
    Database::open(database_file.path())?.assign_events_by_session_id(
        "session-0",
        LIVE_STREAM,
        "inferred",
    )?;
    let after =
        pending_candidates(database_file.path().to_path_buf(), DRAIN_PROBE_SESSIONS).await?;

    // Then: both probes are full, so a remaining-work count reads as unchanged and
    // would stall the drain forever. The set difference sees the candidate that moved.
    assert_eq!(before.len(), after.len());
    assert!(!after.contains("session-0"));
    assert!(should_drain(&before, &after));
    Ok(())
}
