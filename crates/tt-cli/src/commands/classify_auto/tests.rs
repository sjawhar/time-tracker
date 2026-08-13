//! Tests for automatic classification: selection, junk routing, inheritance, guards.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::*;
use chrono::{DateTime, Duration, TimeZone};
use serde_json::json;
use tt_core::EventType;
use tt_core::session::{AgentSession, SessionSource, SessionType};
use tt_llm::{
    CLASSIFIER_GENERATION, ClassificationOutput, HttpFailure, LlmError, MockClassifier,
    StreamChoice,
};

fn timestamp(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap() + Duration::minutes(minutes)
}

fn stream(id: &str, description: Option<&str>) -> tt_db::Stream {
    let now = timestamp(0);
    tt_db::Stream {
        id: id.to_string(),
        name: Some(id.to_string()),
        slug: None,
        description: description.map(String::from),
        color: None,
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    }
}

fn event(id: &str, session_id: Option<&str>, event_type: EventType) -> tt_db::StoredEvent {
    tt_db::StoredEvent {
        id: id.to_string(),
        timestamp: timestamp(0),
        event_type,
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
        cwd: Some("/work/project".to_string()),
        session_id: session_id.map(String::from),
        stream_id: None,
        assignment_source: None,
        data: json!({}),
    }
}

/// A session with enough structure that the junk rule sends it to the classifier.
fn session(id: &str, prompts: &[&str]) -> AgentSession {
    AgentSession {
        session_id: id.to_string(),
        source: SessionSource::Claude,
        parent_session_id: None,
        session_type: SessionType::User,
        project_path: "/work/project".to_string(),
        project_name: "project".to_string(),
        start_time: timestamp(0),
        end_time: None,
        message_count: 4,
        summary: None,
        user_prompts: prompts.iter().map(ToString::to_string).collect(),
        starting_prompt: prompts.first().map(ToString::to_string),
        assistant_message_count: 2,
        tool_call_count: 3,
        user_message_timestamps: Vec::new(),
        tool_call_timestamps: Vec::new(),
    }
}

fn insert_session_candidate(db: &tt_db::Database, session_id: &str, prompts: &[&str]) {
    db.upsert_agent_session(&session(session_id, prompts), Some("machine-a"))
        .unwrap();
    db.insert_event(&event("event-a", Some(session_id), EventType::AgentSession))
        .unwrap();
}

/// Stores a candidate session with the given start time and one unassigned event.
fn insert_dated_session(db: &tt_db::Database, session_id: &str, start_time: DateTime<Utc>) {
    let mut dated = session(session_id, &["do the work"]);
    dated.start_time = start_time;
    db.upsert_agent_session(&dated, Some("machine-a")).unwrap();
    db.insert_event(&event(
        &format!("event-{session_id}"),
        Some(session_id),
        EventType::AgentSession,
    ))
    .unwrap();
}

/// Stores a candidate that ran no tool, with the message depth the junk rule reads.
fn insert_tool_free_session(db: &tt_db::Database, session_id: &str, message_count: i32) {
    let mut shallow = session(session_id, &["hello"]);
    shallow.tool_call_count = 0;
    shallow.message_count = message_count;
    db.upsert_agent_session(&shallow, Some("machine-a"))
        .unwrap();
    db.insert_event(&event(
        &format!("event-{session_id}"),
        Some(session_id),
        EventType::AgentSession,
    ))
    .unwrap();
}

/// Stores a structurally-junk candidate with the start time selection orders by.
fn insert_dated_junk_session(db: &tt_db::Database, session_id: &str, start_time: DateTime<Utc>) {
    let mut shallow = session(session_id, &["hello"]);
    shallow.tool_call_count = 0;
    shallow.message_count = 2;
    shallow.start_time = start_time;
    db.upsert_agent_session(&shallow, Some("machine-a"))
        .unwrap();
    db.insert_event(&event(
        &format!("event-{session_id}"),
        Some(session_id),
        EventType::AgentSession,
    ))
    .unwrap();
}

/// Stores a subagent of `parent_session_id` holding one unassigned event.
fn insert_subagent(db: &tt_db::Database, session_id: &str, parent_session_id: &str) {
    let mut subagent = session(session_id, &["do a piece"]);
    subagent.session_type = SessionType::Subagent;
    subagent.parent_session_id = Some(parent_session_id.to_string());
    db.upsert_agent_session(&subagent, Some("machine-a"))
        .unwrap();
    db.insert_event(&event(
        &format!("event-{session_id}"),
        Some(session_id),
        EventType::AgentToolUse,
    ))
    .unwrap();
}

fn scripted(choice: StreamChoice, confidence: f64) -> MockClassifier {
    let classifier = MockClassifier::default();
    classifier
        .scripted
        .lock()
        .unwrap()
        .push_back(Ok(ClassificationOutput {
            choice,
            confidence,
            reasoning: "test reasoning".to_string(),
        }));
    classifier
}

/// A classifier that answers by session id rather than by call order.
///
/// A chunk's calls run at the same time, so the order they reach a shared queue in is
/// not the order the pass selected them in. Keying the answers on the session is what
/// keeps a test about *which* answer a session got deterministic. Call order is only
/// meaningful across chunks, and
/// `a_session_started_today_is_classified_before_one_from_last_month` is the test for
/// that.
fn by_session(answers: Vec<(&str, Result<ClassificationOutput, LlmError>)>) -> MockClassifier {
    let answers: Mutex<HashMap<String, Result<ClassificationOutput, LlmError>>> = Mutex::new(
        answers
            .into_iter()
            .map(|(session_id, answer)| (session_id.to_string(), answer))
            .collect(),
    );
    MockClassifier {
        brain: Some(Box::new(move |input, _fetch| {
            answers
                .lock()
                .unwrap()
                .remove(&input.session_id)
                .unwrap_or_else(|| {
                    Err(LlmError::Api(format!(
                        "no scripted answer for {}",
                        input.session_id
                    )))
                })
        })),
        ..MockClassifier::default()
    }
}

/// A classifier that gives every session the same answer.
///
/// [`scripted`] queues exactly one, which is enough while a pass asks one question at a
/// time. A pass wider than one chunk needs an answer per call and must not depend on the
/// order the calls arrive in.
fn always(choice: StreamChoice, confidence: f64) -> MockClassifier {
    MockClassifier {
        brain: Some(Box::new(move |_input, _fetch| {
            Ok(ClassificationOutput {
                choice: choice.clone(),
                confidence,
                reasoning: "test reasoning".to_string(),
            })
        })),
        ..MockClassifier::default()
    }
}

fn stored_event(db: &tt_db::Database, event_id: &str) -> tt_db::StoredEvent {
    db.get_events(None, None)
        .unwrap()
        .into_iter()
        .find(|event| event.id == event_id)
        .unwrap()
}

fn junk_stream_id(db: &tt_db::Database) -> String {
    db.get_stream_by_slug(tt_db::JUNK_STREAM_SLUG)
        .unwrap()
        .unwrap()
        .id
}

#[test]
fn high_confidence_existing_classification_is_resolved_automatically() {
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.assigned, 1);
    let event = stored_event(&db, "event-a");
    assert_eq!(event.stream_id.as_deref(), Some("stream-a"));
    assert_eq!(event.assignment_source.as_deref(), Some("inferred"));
}

#[test]
fn low_confidence_existing_classification_creates_proposal_without_assignment() {
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.7,
    );

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.proposed, 1);
    assert!(stored_event(&db, "event-a").stream_id.is_none());
    assert_eq!(
        db.get_proposals(Some(tt_db::ProposalStatus::Pending))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn automatic_assignment_preserves_todo_linked_events() {
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let protected = event(
        "event-protected",
        Some("session-a"),
        EventType::AgentToolUse,
    );
    db.insert_event(&protected).unwrap();
    db.assign_event_to_stream("event-protected", "stream-a", "todo_link")
        .unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(
        stored_event(&db, "event-protected")
            .assignment_source
            .as_deref(),
        Some("todo_link")
    );
}

#[test]
fn high_confidence_new_classification_creates_described_stream_and_assigns() {
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = scripted(
        StreamChoice::New {
            name: "Priority dashboard".to_string(),
            description: Some("Dashboard work".to_string()),
        },
        0.9,
    );

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.assigned, 1);
    let streams = db.get_streams().unwrap();
    assert_eq!(streams[0].description.as_deref(), Some("Dashboard work"));
    assert_eq!(
        stored_event(&db, "event-a").assignment_source.as_deref(),
        Some("inferred")
    );
}

#[test]
fn a_named_stream_with_no_description_is_created_for_backfill_to_finish() {
    // Given: the model identified the work and named a stream, but omitted the
    // description. Discarding that verdict is what one live pass did 315 times.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = scripted(
        StreamChoice::New {
            name: "Priority dashboard".to_string(),
            description: None,
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the stream exists and holds the work, with the gap recorded as a gap.
    assert_eq!(outcome.assigned, 1);
    assert_eq!(outcome.errors, 0);
    let streams = db.get_streams().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].name.as_deref(), Some("Priority dashboard"));
    assert!(streams[0].description.is_none());

    // And: `tt streams describe --backfill` reaches it, which is the whole reason
    // the description is left absent instead of filled with a placeholder.
    let describer = MockClassifier::default();
    describer
        .descriptions
        .lock()
        .unwrap()
        .push_back(Ok("Priority dashboard work".to_string()));
    let backfilled = crate::commands::streams::backfill(&db, &describer, true).unwrap();
    assert_eq!(backfilled.len(), 1);
    assert_eq!(
        db.get_streams().unwrap()[0].description.as_deref(),
        Some("Priority dashboard work")
    );
}

#[test]
fn low_confidence_new_classification_creates_proposal_without_stream() {
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = scripted(
        StreamChoice::New {
            name: "Priority dashboard".to_string(),
            description: Some("Dashboard work".to_string()),
        },
        0.7,
    );

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.proposed, 1);
    assert!(db.get_streams().unwrap().is_empty());
    let proposal = db
        .get_proposals(Some(tt_db::ProposalStatus::Pending))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let proposed_new_stream = proposal.proposed_new_stream.unwrap();
    let payload: serde_json::Value = serde_json::from_str(&proposed_new_stream).unwrap();
    assert_eq!(
        payload,
        json!({
            "name": "Priority dashboard",
            "description": "Dashboard work",
            "tags": [],
        })
    );
}

#[test]
fn a_proposal_for_an_undescribed_stream_can_still_be_accepted() {
    // Given: the same partial answer, offered too weakly to apply. `proposals` has no
    // foreign key and its payload is only parsed at accept time, so a shape the
    // accepter rejects would sit in the queue looking answerable and fail on the
    // reviewer.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = scripted(
        StreamChoice::New {
            name: "Priority dashboard".to_string(),
            description: None,
        },
        0.7,
    );
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();
    assert_eq!(outcome.proposed, 1);
    let proposal = db
        .get_proposals(Some(tt_db::ProposalStatus::Pending))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(&proposal.proposed_new_stream.clone().unwrap()).unwrap();
    assert_eq!(
        payload,
        json!({
            "name": "Priority dashboard",
            "description": null,
            "tags": [],
        })
    );

    // When
    db.accept_proposal(&proposal.id).unwrap();

    // Then: the stream is minted with the gap intact, ready for backfill.
    let streams = db.get_streams().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].name.as_deref(), Some("Priority dashboard"));
    assert!(streams[0].description.is_none());
}

#[test]
fn roster_includes_existing_stream_tags() {
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    db.add_tag("stream-a", "backend").unwrap();
    db.add_tag("stream-a", "urgent").unwrap();
    let classifier = MockClassifier::default();
    let config = Config::default();

    let resolver = Resolver::new(&db, &config, &classifier).unwrap();

    assert_eq!(resolver.roster[0].tags, ["backend", "urgent"]);
}

#[test]
fn repeated_high_confidence_new_choice_reuses_the_created_stream() {
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    insert_dated_session(&db, "session-b", timestamp(10));
    let classifier = MockClassifier::default();
    let mut scripted = classifier.scripted.lock().unwrap();
    for confidence in [0.95, 0.9] {
        scripted.push_back(Ok(ClassificationOutput {
            choice: StreamChoice::New {
                name: "Priority dashboard".to_string(),
                description: Some("Dashboard work".to_string()),
            },
            confidence,
            reasoning: "test reasoning".to_string(),
        }));
    }
    drop(scripted);

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.assigned, 2);
    let streams = db.get_streams().unwrap();
    assert_eq!(streams.len(), 1);
    let stream_id = streams[0].id.as_str();
    assert_eq!(
        stored_event(&db, "event-session-a").stream_id.as_deref(),
        Some(stream_id)
    );
    assert_eq!(
        stored_event(&db, "event-session-b").stream_id.as_deref(),
        Some(stream_id)
    );
}

#[test]
fn rejected_existing_choice_is_not_proposed_again() {
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let proposal = tt_db::Proposal {
        id: "proposal-a".to_string(),
        created_at: timestamp(0),
        session_id: Some("session-a".to_string()),
        event_ids: None,
        proposed_stream_id: Some("stream-a".to_string()),
        proposed_new_stream: None,
        confidence: 0.7,
        reasoning: "no".to_string(),
        status: tt_db::ProposalStatus::Rejected,
        classifier_generation: Some(CLASSIFIER_GENERATION),
    };
    db.insert_proposal(&proposal).unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.7,
    );

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.skipped, 1);
    assert!(
        db.get_proposals(Some(tt_db::ProposalStatus::Pending))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn rejected_new_stream_choice_is_not_proposed_again() {
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let proposal = tt_db::Proposal {
        id: "proposal-a".to_string(),
        created_at: timestamp(0),
        session_id: Some("session-a".to_string()),
        event_ids: None,
        proposed_stream_id: None,
        proposed_new_stream: Some("Priority dashboard".to_string()),
        confidence: 0.7,
        reasoning: "no".to_string(),
        status: tt_db::ProposalStatus::Rejected,
        classifier_generation: Some(CLASSIFIER_GENERATION),
    };
    db.insert_proposal(&proposal).unwrap();
    let classifier = scripted(
        StreamChoice::New {
            name: "Priority dashboard".to_string(),
            description: Some("Dashboard work".to_string()),
        },
        0.7,
    );

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.skipped, 1);
    assert!(
        db.get_proposals(Some(tt_db::ProposalStatus::Pending))
            .unwrap()
            .is_empty()
    );
}

/// The generation a proposal filed by an earlier classifier carries.
///
/// A bump is what invalidates a skip, and a `const` cannot be changed at run time, so
/// these tests move the *proposal* instead. The comparison is the same either way.
/// Generations start at 1, so this subtraction is also a compile-time guard against
/// anyone resetting the constant to zero.
const PREVIOUS_GENERATION: u32 = CLASSIFIER_GENERATION - 1;

/// Files one pending proposal against `session_id`, the way a weak verdict would.
///
/// The generation is spelled out at every call site because it is now a material fact:
/// it says which classifier authored the queued answer, and therefore whether asking
/// again could produce a better one.
fn insert_pending_proposal(
    db: &tt_db::Database,
    session_id: &str,
    stream_id: &str,
    classifier_generation: Option<u32>,
) {
    db.insert_proposal(&tt_db::Proposal {
        id: "proposal-a".to_string(),
        created_at: timestamp(0),
        session_id: Some(session_id.to_string()),
        event_ids: None,
        proposed_stream_id: Some(stream_id.to_string()),
        proposed_new_stream: None,
        confidence: 0.5,
        reasoning: "unsure".to_string(),
        status: tt_db::ProposalStatus::Pending,
        classifier_generation,
    })
    .unwrap();
}

/// Files one pending proposal against an exact window-run event set.
fn insert_pending_window_proposal(
    db: &tt_db::Database,
    event_ids: &[&str],
    stream_id: &str,
    classifier_generation: Option<u32>,
) {
    db.insert_proposal(&tt_db::Proposal {
        id: "proposal-a".to_string(),
        created_at: timestamp(0),
        session_id: None,
        event_ids: Some(event_ids.iter().map(ToString::to_string).collect()),
        proposed_stream_id: Some(stream_id.to_string()),
        proposed_new_stream: None,
        confidence: 0.5,
        reasoning: "unsure".to_string(),
        status: tt_db::ProposalStatus::Pending,
        classifier_generation,
    })
    .unwrap();
}

/// Stores one unassigned window-focus event the run builder will group on its own.
fn insert_window_event(db: &tt_db::Database, event_id: &str) {
    let mut window = event(event_id, None, EventType::WindowFocus);
    window.window_app_id = Some("org.example.Editor".to_string());
    window.window_title = Some("resolver.rs".to_string());
    db.insert_event(&window).unwrap();
}

#[test]
fn a_confident_answer_supersedes_the_proposal_it_answers_past() {
    // Given: a session whose only pending proposal was too weak to apply, and a
    // classifier that is now sure. Holding the session back for a human froze all 37
    // August candidates and 157 of 185 July ones; replaying the current classifier over
    // 17 of them placed 13 confidently.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    insert_pending_proposal(&db, "session-a", "stream-a", None);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the assignment lands and the queue loses a question nobody needs to answer.
    assert_eq!(outcome.assigned, 1);
    assert_eq!(outcome.superseded, 1);
    assert_eq!(
        stored_event(&db, "event-a").stream_id.as_deref(),
        Some("stream-a")
    );
    let proposals = db.get_proposals(None).unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].status, tt_db::ProposalStatus::Superseded);

    // And: superseding is not a rejection, so it suppresses no later answer. Writing
    // `rejected` here would falsify a human verdict `has_rejected_proposal` reads.
    assert!(!db.has_rejected_proposal("session-a", "stream-a").unwrap());
}

#[test]
fn a_second_weak_answer_leaves_the_pending_proposal_exactly_as_it_was() {
    // Given: the same session, and a classifier still unsure. Genuinely ambiguous work
    // keeps waiting for a human — and must not accumulate a proposal per pass.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    insert_pending_proposal(&db, "session-a", "stream-a", None);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.7,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: one proposal, untouched, and nothing assigned.
    assert_eq!(outcome.proposed, 0);
    assert_eq!(outcome.superseded, 0);
    assert_eq!(outcome.skipped, 1);
    let pending = db
        .get_proposals(Some(tt_db::ProposalStatus::Pending))
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "proposal-a");
    assert!((pending[0].confidence - 0.5).abs() < f64::EPSILON);
    assert!(stored_event(&db, "event-a").stream_id.is_none());
}

#[test]
fn a_weak_answer_still_files_a_proposal_when_the_pending_one_is_unacceptable() {
    // Given: a session held by a proposal naming a stream `tt streams dissolve` has
    // deleted. Nobody can accept it, so it must suppress nothing — the read side is
    // deliberately narrower than the write side.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    insert_pending_proposal(&db, "session-a", "dissolved-stream", None);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.7,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: a second, answerable proposal joins the queue.
    assert_eq!(outcome.proposed, 1);
    assert_eq!(outcome.skipped, 0);
    assert_eq!(
        db.get_proposals(Some(tt_db::ProposalStatus::Pending))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn a_confident_window_run_answer_supersedes_its_pending_proposal() {
    // Given: a window run already awaiting review. The dedup lookup used to make the
    // pass skip the run outright, which is the same freeze one level down.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_window_event(&db, "window-a");
    insert_pending_window_proposal(&db, &["window-a"], "stream-a", None);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    assert_eq!(outcome.assigned, 1);
    assert_eq!(outcome.superseded, 1);
    assert_eq!(
        stored_event(&db, "window-a").stream_id.as_deref(),
        Some("stream-a")
    );
    assert_eq!(
        db.get_proposals(None).unwrap()[0].status,
        tt_db::ProposalStatus::Superseded
    );
}

#[test]
fn a_second_weak_window_run_answer_adds_no_duplicate_proposal() {
    // Given: the same run, answered weakly again.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_window_event(&db, "window-a");
    insert_pending_window_proposal(&db, &["window-a"], "stream-a", None);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.7,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    assert_eq!(outcome.proposed, 0);
    assert_eq!(outcome.skipped, 1);
    assert_eq!(
        db.get_proposals(Some(tt_db::ProposalStatus::Pending))
            .unwrap()
            .len(),
        1
    );
    assert!(stored_event(&db, "window-a").stream_id.is_none());
}

#[test]
fn a_window_run_this_generation_already_answered_costs_no_model_call() {
    // Given: a run whose queued answer came from this very classifier. Asking again
    // buys a verdict already on file, and the pass has a bounded number of calls to spend on 71,635
    // unassigned focus events spanning 149 days. Measured live, 212 window-run
    // proposals were being re-asked identically every pass.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_window_event(&db, "window-a");
    insert_pending_window_proposal(&db, &["window-a"], "stream-a", Some(CLASSIFIER_GENERATION));
    // A classifier that would place the run confidently if it were ever asked, so the
    // untouched queue below can only mean the call was never made.
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: no model call was spent.
    assert_eq!(classifier.scripted.lock().unwrap().len(), 1);
    assert_eq!(outcome.skipped_answered, 1);
    assert_eq!(outcome.assigned, 0);
    assert_eq!(outcome.errors, 0);

    // And: the run is left exactly where it was, still waiting on the same reviewer.
    assert!(stored_event(&db, "window-a").stream_id.is_none());
    let pending = db
        .get_proposals(Some(tt_db::ProposalStatus::Pending))
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "proposal-a");
}

#[test]
fn a_window_run_answered_by_an_older_classifier_is_re_asked_exactly_once() {
    // Given: a run whose queued answer predates the current classifier. Bumping the
    // generation is how a materially improved classifier says every earlier refusal is
    // worth revisiting — the precedent being 731 proposals authored before the roster
    // was cut to 200 streams, which froze all 37 August candidates.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_window_event(&db, "window-a");
    insert_pending_window_proposal(&db, &["window-a"], "stream-a", Some(PREVIOUS_GENERATION));
    // Two weak answers queued, so a second call would have one waiting to consume.
    let classifier = MockClassifier::default();
    for _ in 0..2 {
        classifier
            .scripted
            .lock()
            .unwrap()
            .push_back(Ok(ClassificationOutput {
                choice: StreamChoice::Existing {
                    stream_id: "stream-a".to_string(),
                },
                confidence: 0.7,
                reasoning: "still unsure".to_string(),
            }));
    }

    // When: the first pass runs.
    let first = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the run was asked, and the answer added no duplicate to the queue.
    assert_eq!(first.skipped_answered, 0);
    assert_eq!(first.skipped, 1);
    assert_eq!(first.proposed, 0);
    assert_eq!(classifier.scripted.lock().unwrap().len(), 1);

    // When: a second pass runs against the same unchanged run.
    let second = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: exactly once. The queued question now carries this generation, so the
    // re-ask is spent rather than repeated every pass forever.
    assert_eq!(second.skipped_answered, 1);
    assert_eq!(classifier.scripted.lock().unwrap().len(), 1);
    let pending = db
        .get_proposals(Some(tt_db::ProposalStatus::Pending))
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "proposal-a");
    assert_eq!(
        pending[0].classifier_generation,
        Some(CLASSIFIER_GENERATION)
    );
    assert!(stored_event(&db, "window-a").stream_id.is_none());
}

#[test]
fn a_window_run_with_no_queued_answer_is_classified_normally() {
    // Given: a run nobody has asked about. The gate must default to asking — this is
    // an attribution path for direct time, and skipping is the exception.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_window_event(&db, "window-a");
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: asked, answered, attributed.
    assert_eq!(outcome.skipped_answered, 0);
    assert_eq!(outcome.assigned, 1);
    assert!(classifier.scripted.lock().unwrap().is_empty());
    assert_eq!(
        stored_event(&db, "window-a").stream_id.as_deref(),
        Some("stream-a")
    );
}

#[test]
fn a_session_whose_answer_is_queued_is_still_re_classified() {
    // Given: a session carrying a proposal from this very generation. The window-run
    // gate must NOT be extended here, and this is the guard for that decision: a run's
    // identity is its exact event set, so a run that gains an event stops matching its
    // proposal and is re-asked on its own. A session has no such property — it keeps
    // its id while its prompts grow, and `get_recheck_candidates` reads only sessions
    // already classified, so a skip here would freeze a lengthening session out until
    // someone bumped a constant.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    insert_pending_proposal(&db, "session-a", "stream-a", Some(CLASSIFIER_GENERATION));
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the call was spent and the session placed.
    assert_eq!(outcome.skipped_answered, 0);
    assert_eq!(outcome.assigned, 1);
    assert!(classifier.scripted.lock().unwrap().is_empty());
}

#[test]
fn a_weak_session_answer_stamps_the_pending_proposal_with_this_generation() {
    // Given: a session whose queued answer predates this classifier, answered weakly
    // again. Nothing reads a session's generation to skip it, but the column has one
    // meaning across both scopes — the newest classifier that has answered this
    // question — and `propose` is one code path, not a branch per scope.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    insert_pending_proposal(&db, "session-a", "stream-a", None);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.7,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: no duplicate, and the row now records who last answered it.
    assert_eq!(outcome.proposed, 0);
    assert_eq!(outcome.skipped, 1);
    let pending = db
        .get_proposals(Some(tt_db::ProposalStatus::Pending))
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].classifier_generation,
        Some(CLASSIFIER_GENERATION)
    );
    // And: stamping is bookkeeping, not a verdict — the queued answer itself is
    // untouched and still pending for a human.
    assert_eq!(pending[0].status, tt_db::ProposalStatus::Pending);
    assert!((pending[0].confidence - 0.5).abs() < f64::EPSILON);
}

#[test]
fn window_focus_run_is_assigned_with_existing_stream_choice() {
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    let mut window = event("window-a", None, EventType::WindowFocus);
    window.window_app_id = Some("org.example.Editor".to_string());
    window.window_title = Some("resolver.rs".to_string());
    db.insert_event(&window).unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.assigned, 1);
    assert_eq!(
        stored_event(&db, "window-a").stream_id.as_deref(),
        Some("stream-a")
    );
}

#[test]
fn recheck_reassigns_only_inferred_events_and_marks_session_complete() {
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    db.insert_stream(&stream("stream-b", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["one", "two"]);
    db.assign_event_to_stream("event-a", "stream-a", "inferred")
        .unwrap();
    db.record_classification("session-a", 1).unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-b".to_string(),
        },
        0.9,
    );

    run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(
        stored_event(&db, "event-a").stream_id.as_deref(),
        Some("stream-b")
    );
    assert!(db.get_recheck_candidates().unwrap().is_empty());
}

#[test]
fn classifier_failure_leaves_events_unclassified_and_records_health() {
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = MockClassifier::default();
    classifier
        .scripted
        .lock()
        .unwrap()
        .push_back(Err(LlmError::Api("offline".to_string())));

    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    assert_eq!(outcome.errors, 1);
    assert!(stored_event(&db, "event-a").stream_id.is_none());
    assert_eq!(db.get_classifier_health().unwrap().consecutive_failures, 1);
}

#[test]
fn a_session_started_today_is_classified_before_one_from_last_month() {
    // Given: one candidate from last month and a chunk's worth from today. A chunk's
    // calls run at the same time, so "before" is a statement about chunks: the oldest
    // candidate can only be reached once every newer one already has been.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-first", None)).unwrap();
    db.insert_stream(&stream("stream-second", None)).unwrap();
    insert_dated_session(&db, "old", timestamp(0));
    for filler in 0..CLASSIFY_CONCURRENCY - 1 {
        insert_dated_session(&db, &format!("filler-{filler}"), timestamp(60 * 24 * 29));
    }
    insert_dated_session(&db, "recent", timestamp(60 * 24 * 30));
    // The first chunk's answers name one stream and everything after it names another.
    let calls = AtomicUsize::new(0);
    let classifier = MockClassifier {
        brain: Some(Box::new(move |_input, _fetch| {
            let stream_id = if calls.fetch_add(1, Ordering::Relaxed) < CLASSIFY_CONCURRENCY {
                "stream-first"
            } else {
                "stream-second"
            };
            Ok(ClassificationOutput {
                choice: StreamChoice::Existing {
                    stream_id: stream_id.to_string(),
                },
                confidence: 0.9,
                reasoning: "test reasoning".to_string(),
            })
        })),
        ..MockClassifier::default()
    };

    // When
    run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the newest session was answered in the first chunk and the month-old one was
    // left to the last. Session ids are hash-like, so the `ORDER BY session_id` this
    // replaced would have answered `old` first.
    assert_eq!(
        stored_event(&db, "event-recent").stream_id.as_deref(),
        Some("stream-first")
    );
    assert_eq!(
        stored_event(&db, "event-old").stream_id.as_deref(),
        Some("stream-second")
    );
}

/// Waits until a whole chunk's worth of calls has arrived, reporting whether it did.
///
/// Bounded, so a pass that runs its calls one at a time fails in seconds instead of
/// hanging the suite forever. Nothing waits on the passing path: the last call to arrive
/// releases the rest.
#[expect(
    clippy::significant_drop_tightening,
    reason = "a condvar wait owns its guard: `wait_timeout` consumes it and hands it back, \
              and the loop condition reads the count through it. Dropping it where the lint \
              points would not compile, let alone wait."
)]
fn every_call_arrived(arrivals: &(Mutex<usize>, Condvar)) -> bool {
    let (arrived, everyone) = arrivals;
    let mut count = arrived.lock().unwrap();
    *count += 1;
    everyone.notify_all();
    while *count < CLASSIFY_CONCURRENCY {
        let (waited, bound) = everyone
            .wait_timeout(count, std::time::Duration::from_secs(5))
            .unwrap();
        count = waited;
        if bound.timed_out() {
            return false;
        }
    }
    true
}

#[test]
fn a_chunks_calls_all_run_at_the_same_time() {
    // Given: a full chunk of candidates and a classifier that answers nobody until every
    // call in the chunk has arrived. Run one at a time, the first call waits for a
    // partner that cannot be dispatched until it returns.
    //
    // This is the only test that fails if the calls go back to being serial. Every other
    // one passes either way, because chunking decides the same thing serially — it just
    // takes the sum of the waits instead of the longest of them.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    for index in 0..CLASSIFY_CONCURRENCY {
        insert_dated_session(
            &db,
            &format!("session-{index}"),
            timestamp(i64::try_from(index).unwrap()),
        );
    }
    let arrivals = Arc::new((Mutex::new(0_usize), Condvar::new()));
    let classifier = MockClassifier {
        brain: Some(Box::new({
            let arrivals = Arc::clone(&arrivals);
            move |_input, _fetch| {
                if !every_call_arrived(&arrivals) {
                    return Err(LlmError::Api("call ran alone".to_string()));
                }
                Ok(ClassificationOutput {
                    choice: StreamChoice::Existing {
                        stream_id: "stream-a".to_string(),
                    },
                    confidence: 0.9,
                    reasoning: "test reasoning".to_string(),
                })
            }
        })),
        ..MockClassifier::default()
    };

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: every call was in flight at once, so every one of them answered.
    assert_eq!(outcome.errors, 0);
    assert_eq!(
        outcome.assigned,
        u64::try_from(CLASSIFY_CONCURRENCY).unwrap()
    );
}

#[test]
fn every_session_is_classified_when_a_pass_spans_more_than_one_chunk() {
    // Given: more candidates than one chunk holds, so the pass runs several — including
    // a last one that is not full, which is where an off-by-one in the chunking would
    // silently drop work.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    let candidates = CLASSIFY_CONCURRENCY * 2 + 3;
    for index in 0..candidates {
        insert_dated_session(
            &db,
            &format!("session-{index}"),
            timestamp(i64::try_from(index).unwrap()),
        );
    }
    let classifier = always(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: every candidate was asked and answered. A chunk that never ran would leave
    // its sessions unassigned, which reads as classification lag rather than as a bug.
    assert_eq!(outcome.assigned, u64::try_from(candidates).unwrap());
    assert_eq!(outcome.errors, 0);
    for index in 0..candidates {
        let event = stored_event(&db, &format!("event-session-{index}"));
        assert_eq!(
            event.stream_id.as_deref(),
            Some("stream-a"),
            "session-{index}"
        );
    }
}

#[test]
fn one_chunk_naming_one_new_stream_many_times_creates_it_once() {
    // Given: a full chunk of candidates, every one of which answers with the same new
    // stream name. Their calls run at the same time, so none of them can see the stream
    // another is about to mint — they all answer from the roster snapshot the chunk began
    // with. This is the duplicate-stream failure that kept concurrency off the table:
    // `stream_named` finds and inserts in two operations rather than one transaction, so
    // two concurrent copies of it would both find nothing and both insert.
    let db = tt_db::Database::open_in_memory().unwrap();
    for index in 0..CLASSIFY_CONCURRENCY {
        insert_dated_session(
            &db,
            &format!("session-{index}"),
            timestamp(i64::try_from(index).unwrap()),
        );
    }
    let classifier = always(
        StreamChoice::New {
            name: "Priority dashboard".to_string(),
            description: Some("Dashboard work".to_string()),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: one row, not eight. Verdicts are applied serially, so `stream_named` still
    // runs one at a time: the first mint is on the roster before the second verdict is
    // resolved, and the `streams` table is the authority behind it either way.
    let streams = db.get_streams().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].name.as_deref(), Some("Priority dashboard"));

    // And: every session landed on it.
    assert_eq!(
        outcome.assigned,
        u64::try_from(CLASSIFY_CONCURRENCY).unwrap()
    );
    let stream_id = streams[0].id.as_str();
    for index in 0..CLASSIFY_CONCURRENCY {
        let event = stored_event(&db, &format!("event-session-{index}"));
        assert_eq!(
            event.stream_id.as_deref(),
            Some(stream_id),
            "session-{index}"
        );
    }
}

#[test]
fn a_panicking_worker_costs_its_own_session_and_no_other() {
    // Given: a chunk in which one call panics. Panicking is deliberate here, so the
    // stderr this test prints is the test working. A scoped thread that panics takes the
    // whole scope down with it unless its handle is joined, which would cost the pass
    // every candidate beside it — the blast radius the vanished-stream crash had.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_dated_session(&db, "first", timestamp(0));
    insert_dated_session(&db, "panicking", timestamp(10));
    insert_dated_session(&db, "third", timestamp(20));
    let classifier = MockClassifier {
        brain: Some(Box::new(|input, _fetch| {
            assert_ne!(input.session_id, "panicking", "deliberate test panic");
            Ok(ClassificationOutput {
                choice: StreamChoice::Existing {
                    stream_id: "stream-a".to_string(),
                },
                confidence: 0.9,
                reasoning: "test reasoning".to_string(),
            })
        })),
        ..MockClassifier::default()
    };

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the panic cost exactly its own session, which rests unassigned like any
    // other failed call rather than being junked or given a stream nobody chose.
    assert_eq!(outcome.assigned, 2);
    assert_eq!(outcome.errors, 1);
    assert!(stored_event(&db, "event-panicking").stream_id.is_none());
    assert_eq!(
        stored_event(&db, "event-third").stream_id.as_deref(),
        Some("stream-a")
    );
    assert_eq!(
        stored_event(&db, "event-first").stream_id.as_deref(),
        Some("stream-a")
    );

    // And: it is recorded as the failure it is, naming what happened. A panic is a
    // defect in this process rather than anything the provider did, so no retry changes
    // it — which is what `api` means.
    assert_eq!(
        outcome.causes,
        ErrorCauses {
            api: 1,
            ..ErrorCauses::default()
        }
    );
    let last_error = db.get_classifier_health().unwrap().last_error.unwrap();
    assert!(last_error.contains("panicked"), "{last_error}");
}

#[test]
fn a_session_with_no_tools_and_one_exchange_is_junked_without_an_llm_call() {
    // Given: a session that did nothing and discussed nothing, and a classifier with
    // no scripted answer — any call it receives would be counted as an error.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_tool_free_session(&db, "session-a", 2);
    let classifier = MockClassifier::default();

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: routed to the reserved junk stream, no call spent, nothing deleted.
    assert_eq!(outcome.junked, 1);
    assert_eq!(outcome.errors, 0);
    let junked = stored_event(&db, "event-session-a");
    assert_eq!(
        junked.stream_id.as_deref(),
        Some(junk_stream_id(&db).as_str())
    );
    assert_eq!(junked.assignment_source.as_deref(), Some("junk"));
}

#[test]
fn structurally_junk_sessions_leave_attention_unassigned() {
    // Given: a structurally junk session carrying agent activity and all three attention types.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_tool_free_session(&db, "session-a", 2);
    for (event_id, event_type) in [
        ("event-user-message", EventType::UserMessage),
        ("event-window-focus", EventType::WindowFocus),
        ("event-pane-focus", EventType::TmuxPaneFocus),
    ] {
        db.insert_event(&event(event_id, Some("session-a"), event_type))
            .unwrap();
    }
    let classifier = scripted(StreamChoice::Undetermined, 0.0);

    // When: automatic classification settles the structurally junk session.
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: only the agent activity is routed to junk; attention remains unassigned.
    assert_eq!(outcome.errors, 0);
    let junked = stored_event(&db, "event-session-a");
    assert_eq!(
        junked.stream_id.as_deref(),
        Some(junk_stream_id(&db).as_str())
    );
    assert_eq!(junked.assignment_source.as_deref(), Some("junk"));
    for event_id in [
        "event-user-message",
        "event-window-focus",
        "event-pane-focus",
    ] {
        let attention = stored_event(&db, event_id);
        assert_eq!(attention.stream_id, None, "{event_id} was routed to junk");
        assert_eq!(
            attention.assignment_source, None,
            "{event_id} kept a source"
        );
    }
}

#[test]
fn a_tool_free_session_with_depth_still_reaches_the_classifier() {
    // Given: no tool calls but six messages — the shape of a contract review or a
    // vendor pricing discussion, which structure cannot judge.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_tool_free_session(&db, "session-a", 6);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    assert_eq!(outcome.junked, 0);
    assert_eq!(outcome.assigned, 1);
    assert_eq!(
        stored_event(&db, "event-session-a").stream_id.as_deref(),
        Some("stream-a")
    );
}

#[test]
fn a_pass_spends_its_bounded_session_budget_on_real_work_rather_than_on_junk() {
    // Given: more structurally-junk sessions than a whole bounded pass, every one of them
    // NEWER than the real work, plus three real sessions.
    //
    // That ordering is the failure reproduced exactly. `unclassified_user_sessions` sorts
    // `start_time DESC` so today's work is reached ahead of a backlog, and junk costs no
    // model call — but it was recognised only *after* selection, so it still took one of
    // the 200 slots. Measured on the live database over one hour: **840 structurally-junk
    // sessions classified against 20 real ones**, pass summaries reading `junked=171..177`
    // of 200. Model-call concurrency could not help, because only ~29 real sessions per
    // pass ever reached the model.
    //
    // Without bulk routing ahead of selection, the newest 200 candidates here are all
    // junk and not one real session is classified in this pass.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    let real = ["real-a", "real-b", "real-c"];
    for (index, session_id) in real.iter().enumerate() {
        let minutes = i64::try_from(index).expect("test index fits in i64");
        insert_dated_session(&db, session_id, timestamp(minutes));
    }
    for index in 0..=SESSIONS_PER_PASS {
        let minutes = i64::try_from(index + 1_000).expect("test index fits in i64");
        insert_dated_junk_session(&db, &format!("junk-{index:04}"), timestamp(minutes));
    }
    // Keyed by session, so an answer meant for real work can never be spent on junk.
    let classifier = by_session(
        real.iter()
            .map(|session_id| {
                (
                    *session_id,
                    Ok(ClassificationOutput {
                        choice: StreamChoice::Existing {
                            stream_id: "stream-a".to_string(),
                        },
                        confidence: 0.9,
                        reasoning: "test reasoning".to_string(),
                    }),
                )
            })
            .collect(),
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: every real session was classified in this one pass, and the junk is routed
    // rather than deleted — `tt streams dissolve junk` still reverses it.
    assert_eq!(outcome.errors, 0);
    for session_id in real {
        assert_eq!(
            stored_event(&db, &format!("event-{session_id}"))
                .stream_id
                .as_deref(),
            Some("stream-a"),
            "{session_id} never reached the classifier: junk ate the bounded budget"
        );
    }
    let junk_stream = junk_stream_id(&db);
    assert_eq!(
        stored_event(&db, "event-junk-0000").stream_id.as_deref(),
        Some(junk_stream.as_str())
    );
    assert_eq!(
        outcome.junked,
        u64::try_from(SESSIONS_PER_PASS + 1).expect("test bound fits in u64")
    );
}

#[test]
fn subagents_inherit_their_parents_stream_without_an_llm_call() {
    // Given: a parent with two subagents, one of which holds an event a human owns.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    db.insert_stream(&stream("stream-held", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    insert_subagent(&db, "sub-a", "session-a");
    insert_subagent(&db, "sub-b", "session-a");
    db.insert_event(&event(
        "event-human",
        Some("sub-a"),
        EventType::AgentToolUse,
    ))
    .unwrap();
    db.assign_event_to_stream("event-human", "stream-held", "user")
        .unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "stream-a".to_string(),
        },
        0.9,
    );

    // When: exactly one answer is queued, for the parent.
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the subagents took the parent's stream on no call of their own.
    assert_eq!(outcome.errors, 0);
    for event_id in ["event-sub-a", "event-sub-b"] {
        let inherited = stored_event(&db, event_id);
        assert_eq!(inherited.stream_id.as_deref(), Some("stream-a"));
        assert_eq!(inherited.assignment_source.as_deref(), Some("inherited"));
    }
    let human = stored_event(&db, "event-human");
    assert_eq!(human.stream_id.as_deref(), Some("stream-held"));
    assert_eq!(human.assignment_source.as_deref(), Some("user"));
}

#[test]
fn subagents_of_a_junked_session_inherit_the_junk_stream() {
    // Given: a structurally junk parent with one subagent.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_tool_free_session(&db, "session-a", 1);
    insert_subagent(&db, "sub-a", "session-a");
    let classifier = MockClassifier::default();

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: a subagent of work that does not exist is not work either.
    assert_eq!(outcome.errors, 0);
    assert_eq!(
        stored_event(&db, "event-sub-a").stream_id.as_deref(),
        Some(junk_stream_id(&db).as_str())
    );
}

#[test]
fn subagents_whose_parent_was_never_indexed_are_junked_without_an_llm_call() {
    // Given: a subagent naming a parent absent from `agent_sessions`, the shape of the
    // bounded 2026-04 ingest defect.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_subagent(&db, "sub-orphan", "never-ingested");
    let classifier = MockClassifier::default();

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    assert_eq!(outcome.junked, 1);
    assert_eq!(outcome.errors, 0);
    let junked = stored_event(&db, "event-sub-orphan");
    assert_eq!(
        junked.stream_id.as_deref(),
        Some(junk_stream_id(&db).as_str())
    );
    assert_eq!(junked.assignment_source.as_deref(), Some("junk"));
}

#[test]
fn a_throwaway_verdict_routes_the_session_to_junk_without_creating_a_stream() {
    // Given: a session structure cannot judge, which the classifier calls trivial.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["Are you there?"]);
    db.insert_event(&event(
        "event-user-message",
        Some("session-a"),
        EventType::UserMessage,
    ))
    .unwrap();
    let classifier = scripted(StreamChoice::Throwaway, 0.9);

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the reserved stream is the only one that exists.
    assert_eq!(outcome.junked, 1);
    let streams = db.get_streams().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].slug.as_deref(), Some(tt_db::JUNK_STREAM_SLUG));
    assert_eq!(
        stored_event(&db, "event-a").assignment_source.as_deref(),
        Some("junk")
    );
    let attention = stored_event(&db, "event-user-message");
    assert_eq!(attention.stream_id, None);
    assert_eq!(attention.assignment_source, None);
}

#[test]
fn a_misnamed_new_stream_is_refused_and_the_session_stays_unassigned() {
    // Given: the classifier proposes a container for work it could not identify.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["ssh devbox"]);
    let classifier = scripted(
        StreamChoice::New {
            name: "other: shell / nav / transitional".to_string(),
            description: Some("Terminal navigation between tasks".to_string()),
        },
        0.95,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: nothing is created and nothing is proposed. Unassigned reads as
    // classification lag; an invented container is a lie that survives.
    assert_eq!(outcome.refused, 1);
    assert_eq!(outcome.assigned, 0);
    assert!(db.get_streams().unwrap().is_empty());
    assert!(stored_event(&db, "event-a").stream_id.is_none());
    assert!(
        db.get_proposals(Some(tt_db::ProposalStatus::Pending))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_misnamed_name_is_refused_even_when_no_description_came_with_it() {
    // Given: the invented container arrives as the partial answer that used to be
    // thrown away wholesale. Keeping such an answer must not smuggle the name past
    // the guard: `is_misnamed_stream` judges the name, and the name alone.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["ssh devbox"]);
    let classifier = scripted(
        StreamChoice::New {
            name: "other: shell / nav / transitional".to_string(),
            description: None,
        },
        0.95,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: refused exactly as a fully-described misnamed one is.
    assert_eq!(outcome.refused, 1);
    assert_eq!(outcome.assigned, 0);
    assert!(db.get_streams().unwrap().is_empty());
    assert!(stored_event(&db, "event-a").stream_id.is_none());
}

#[test]
fn a_low_confidence_misnamed_name_is_refused_rather_than_left_for_review() {
    // Given: the same misnamed shape, offered too weakly to apply.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["ssh devbox"]);
    let classifier = scripted(
        StreamChoice::New {
            name: "misc: stragglers".to_string(),
            description: Some("Leftover sessions".to_string()),
        },
        0.4,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: a proposal is a junk stream one accept away, so the guard runs first.
    assert_eq!(outcome.refused, 1);
    assert_eq!(outcome.proposed, 0);
    assert!(
        db.get_proposals(Some(tt_db::ProposalStatus::Pending))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_new_stream_name_sharing_a_generic_word_is_still_created() {
    // Given: real work whose name contains `navigation`, which a substring rule ate.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["fix the calendar month jump"]);
    let classifier = scripted(
        StreamChoice::New {
            name: "agent-c: calendar navigation debugging".to_string(),
            description: Some("Debugging month navigation in the calendar agent".to_string()),
        },
        0.95,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    assert_eq!(outcome.refused, 0);
    assert_eq!(outcome.assigned, 1);
    let streams = db.get_streams().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(
        streams[0].name.as_deref(),
        Some("agent-c: calendar navigation debugging")
    );
}

#[test]
fn a_verdict_naming_a_vanished_stream_is_refused_and_the_pass_completes() {
    // Given: the classifier names a stream from a roster it read before
    // `tt streams dissolve` deleted the row.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "dissolved-stream".to_string(),
        },
        0.9,
    );

    // When: the pass must reach its end. Writing the id raises a foreign-key error
    // that aborts every candidate still queued behind it.
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: refused and counted, and the session stays unassigned — the honest state
    // for work the classifier could not place.
    assert_eq!(outcome.refused_missing_stream, 1);
    assert_eq!(outcome.assigned, 0);
    assert_eq!(outcome.junked, 0);
    assert_eq!(outcome.proposed, 0);
    assert!(stored_event(&db, "event-a").stream_id.is_none());
}

#[test]
fn a_low_confidence_verdict_naming_a_vanished_stream_is_not_left_as_a_proposal() {
    // Given: the same vanished stream, offered too weakly to apply.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "dissolved-stream".to_string(),
        },
        0.7,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: no proposal. One naming a vanished stream can never be accepted, and a
    // pending proposal exiles its session from every later pass.
    assert_eq!(outcome.refused_missing_stream, 1);
    assert_eq!(outcome.proposed, 0);
    assert!(
        db.get_proposals(Some(tt_db::ProposalStatus::Pending))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn one_vanished_stream_verdict_does_not_cost_the_rest_of_the_pass() {
    // Given: three candidates and three answers, the middle one naming a stream that no
    // longer exists. Candidates arrive newest-first and share one chunk, whose calls run
    // at the same time, so each answer is keyed to the session it belongs to.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    db.insert_stream(&stream("stream-b", None)).unwrap();
    insert_dated_session(&db, "first", timestamp(0));
    insert_dated_session(&db, "second", timestamp(10));
    insert_dated_session(&db, "third", timestamp(20));
    let classifier = by_session(
        ["third", "second", "first"]
            .into_iter()
            .zip(["stream-a", "dissolved-stream", "stream-b"])
            .map(|(session_id, stream_id)| {
                (
                    session_id,
                    Ok(ClassificationOutput {
                        choice: StreamChoice::Existing {
                            stream_id: stream_id.to_string(),
                        },
                        confidence: 0.9,
                        reasoning: "test reasoning".to_string(),
                    }),
                )
            })
            .collect(),
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: one bad id costs exactly its own candidate. The crash it replaced cost
    // every session queued behind it.
    assert_eq!(outcome.refused_missing_stream, 1);
    assert_eq!(outcome.assigned, 2);
    assert_eq!(outcome.errors, 0);
    assert_eq!(
        stored_event(&db, "event-third").stream_id.as_deref(),
        Some("stream-a")
    );
    assert!(stored_event(&db, "event-second").stream_id.is_none());
    assert_eq!(
        stored_event(&db, "event-first").stream_id.as_deref(),
        Some("stream-b")
    );
}

#[test]
fn a_window_run_verdict_naming_a_vanished_stream_is_refused() {
    // Given: the crash surfaced on the window-run path, which writes event ids
    // directly rather than going through a session.
    let db = tt_db::Database::open_in_memory().unwrap();
    let mut window = event("window-a", None, EventType::WindowFocus);
    window.window_app_id = Some("org.example.Editor".to_string());
    window.window_title = Some("resolver.rs".to_string());
    db.insert_event(&window).unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "dissolved-stream".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    assert_eq!(outcome.refused_missing_stream, 1);
    assert_eq!(outcome.assigned, 0);
    assert!(stored_event(&db, "window-a").stream_id.is_none());
}

#[test]
fn a_recheck_verdict_naming_a_vanished_stream_leaves_the_earlier_assignment() {
    // Given: a session already placed by an earlier pass, revisited after gaining a
    // prompt, whose new answer names a stream that has since been dissolved.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    insert_session_candidate(&db, "session-a", &["one", "two"]);
    db.assign_event_to_stream("event-a", "stream-a", "inferred")
        .unwrap();
    db.record_classification("session-a", 1).unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "dissolved-stream".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the refusal changes nothing rather than dropping the placement it had.
    assert_eq!(outcome.refused_missing_stream, 1);
    assert_eq!(
        stored_event(&db, "event-a").stream_id.as_deref(),
        Some("stream-a")
    );
}

#[test]
fn a_verdict_answering_with_a_slug_instead_of_an_id_is_refused() {
    // Given: a live stream, named by its slug rather than its id. Real passes do this
    // — two stored proposals name `agentc-core`, which is a real slug.
    let db = tt_db::Database::open_in_memory().unwrap();
    let mut target = stream("ab33c96c", None);
    target.slug = Some("agentc-core".to_string());
    db.insert_stream(&target).unwrap();
    insert_session_candidate(&db, "session-a", &["review the agent-c PRs"]);
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "agentc-core".to_string(),
        },
        0.9,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: refused, not resolved. `events.stream_id` stores the literal string and
    // its foreign key points at `streams.id`, and proposal acceptance matches on id
    // alone — so resolving the slug here would rewrite the answer rather than refuse
    // it, and diverge from what accepting the same verdict would do.
    assert_eq!(outcome.refused_missing_stream, 1);
    assert_eq!(outcome.assigned, 0);
    assert!(stored_event(&db, "event-a").stream_id.is_none());
}

/// Queues one failure of every kind `LlmError` draws a distinction between.
fn failing(errors: Vec<LlmError>) -> MockClassifier {
    let classifier = MockClassifier::default();
    let mut scripted = classifier.scripted.lock().unwrap();
    for error in errors {
        scripted.push_back(Err(error));
    }
    drop(scripted);
    classifier
}

#[test]
fn classifier_failures_are_counted_by_cause() {
    // Given: five candidates, and one failure of each kind the error type can tell
    // apart. A bare `errors=5` says a share of the pass failed and nothing about why,
    // which is the whole defect: an overloaded provider, a provider that answered
    // nothing at all, a terminal API error, malformed output and a missing key want
    // five different responses.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-api", timestamp(0));
    insert_dated_session(&db, "session-parse", timestamp(10));
    insert_dated_session(&db, "session-key", timestamp(20));
    insert_dated_session(&db, "session-overloaded", timestamp(30));
    insert_dated_session(&db, "session-timeout", timestamp(40));
    let classifier = failing(vec![
        LlmError::Api("HTTP 400: invalid_request_error".to_string()),
        LlmError::Overloaded(HttpFailure::new(529, "overloaded_error".to_string())),
        LlmError::Parse("new streams require both a name and description".to_string()),
        LlmError::MissingApiKey("ANTHROPIC_API_KEY".to_string()),
        LlmError::Timeout("request timed out".to_string()),
    ]);

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: the total still holds, and every failure is attributed to a cause.
    assert_eq!(outcome.errors, 5);
    assert_eq!(
        outcome.causes,
        ErrorCauses {
            api: 1,
            overloaded: 1,
            timeout: 1,
            parse: 1,
            missing_api_key: 1,
        }
    );
}

#[test]
fn the_pass_summary_names_what_the_failures_were() {
    // Given: the outcome of a pass that failed on two different causes.
    let outcome = AutoClassifyOutcome {
        assigned: 35_505,
        junked: 181,
        errors: 3,
        causes: ErrorCauses {
            api: 2,
            overloaded: 0,
            timeout: 0,
            parse: 1,
            missing_api_key: 0,
        },
        ..AutoClassifyOutcome::default()
    };

    // When
    let line = summary_line(&outcome);

    // Then: the breakdown rides on the total, and a cause nobody hit is not named —
    // a bucket at zero is noise, and every non-zero one is a different response.
    assert_eq!(
        line,
        "Auto-classify: assigned=35505, junked=181, proposed=0, superseded=0, refused=0, \
         missing_stream=0, undetermined=0, turns_exhausted=0, skipped_answered=0, skipped=0, \
         errors=3 (api=2, parse=1)"
    );
}

#[test]
fn a_pass_the_provider_never_answered_reads_differently_from_an_overloaded_one() {
    // Given: two passes that failed the same number of times. One met a provider that
    // answered 529 every time; the other met a provider that answered nothing at all.
    // Before requests were bounded the second could not produce a pass summary at all
    // — the pass simply stopped, 3 sessions in 20 minutes with one open socket.
    let overloaded = AutoClassifyOutcome {
        errors: 3,
        causes: ErrorCauses {
            overloaded: 3,
            ..ErrorCauses::default()
        },
        ..AutoClassifyOutcome::default()
    };
    let silent = AutoClassifyOutcome {
        errors: 3,
        causes: ErrorCauses {
            timeout: 3,
            ..ErrorCauses::default()
        },
        ..AutoClassifyOutcome::default()
    };

    // When
    let (capacity, reachability) = (summary_line(&overloaded), summary_line(&silent));

    // Then: distinguishable without opening the log, because they point at different
    // things to check. A 529 proves a model was reached and is answering, so the
    // question is capacity; silence says nothing came back, so the question is
    // connectivity — or a session whose agentic loop cannot finish inside its
    // allowance.
    assert!(capacity.ends_with("errors=3 (overloaded=3)"), "{capacity}");
    assert!(
        reachability.ends_with("errors=3 (timeout=3)"),
        "{reachability}"
    );
}

#[test]
fn a_pass_degraded_by_provider_overload_reads_differently_from_a_broken_one() {
    // Given: two passes that failed the same number of times. One met a provider
    // that stayed overloaded; the other met errors that will recur identically.
    let overloaded = AutoClassifyOutcome {
        errors: 3,
        causes: ErrorCauses {
            overloaded: 3,
            ..ErrorCauses::default()
        },
        ..AutoClassifyOutcome::default()
    };
    let broken = AutoClassifyOutcome {
        errors: 3,
        causes: ErrorCauses {
            api: 3,
            ..ErrorCauses::default()
        },
        ..AutoClassifyOutcome::default()
    };

    // When
    let (degraded, failing) = (summary_line(&overloaded), summary_line(&broken));

    // Then: an operator can tell them apart without opening the log. The first will
    // drain by itself once the provider recovers and its candidates are reached again
    // next pass; the second needs someone to look.
    assert!(degraded.ends_with("errors=3 (overloaded=3)"), "{degraded}");
    assert!(failing.ends_with("errors=3 (api=3)"), "{failing}");
    assert_ne!(degraded, failing);
}

#[test]
fn a_pass_that_failed_nowhere_carries_no_breakdown() {
    // Given: a clean pass. The parenthetical exists to explain failures, so with none
    // to explain it would only add a line to read.
    let outcome = AutoClassifyOutcome {
        assigned: 12,
        ..AutoClassifyOutcome::default()
    };

    // When/Then
    assert_eq!(
        summary_line(&outcome),
        "Auto-classify: assigned=12, junked=0, proposed=0, superseded=0, refused=0, \
         missing_stream=0, undetermined=0, turns_exhausted=0, skipped_answered=0, skipped=0, \
         errors=0"
    );
}

#[test]
fn an_undetermined_verdict_leaves_the_session_unassigned_without_failing_the_pass() {
    // Given: a session the classifier answers about, and its answer is that it cannot
    // identify the work — exactly what the prompt asks for when nothing is
    // identifiable. In production this arrived as a hard parse error, and 53
    // consecutive ones had throttled the daemon's classify loop into silence against a
    // backlog of thousands.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["The following tool was executed"]);
    let classifier = scripted(StreamChoice::Undetermined, 0.1);

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: counted in its own bucket, and nowhere near the error tally.
    assert_eq!(outcome.undetermined, 1);
    assert_eq!(outcome.errors, 0);
    assert_eq!(outcome.causes, ErrorCauses::default());

    // And: the session is left honestly unassigned — not junked, not proposed, and
    // with no container invented for it. `junk_stream_id` creates the reserved stream
    // on demand, so an empty roster proves the junk path was never taken.
    assert_eq!(outcome.junked, 0);
    assert_eq!(outcome.assigned, 0);
    assert_eq!(outcome.proposed, 0);
    assert!(db.get_streams().unwrap().is_empty());
    assert!(stored_event(&db, "event-a").stream_id.is_none());

    // And: nothing drives the failure backoff. This is the assertion the production
    // bug fails: `classifier_consecutive_failures` stood at 53 and
    // `classifier_retry_delay` had grown to its five-minute cap.
    let health = db.get_classifier_health().unwrap();
    assert_eq!(health.consecutive_failures, 0);
    assert!(health.last_failure_at.is_none());
    assert!(health.last_error.is_none());
    assert!(health.last_success_at.is_some());
}

#[test]
fn a_mixed_pass_completes_and_tallies_each_verdict_in_its_own_bucket() {
    // Given: three candidates and the three answers a pass can now meet — one the model
    // placed, one it declined, and one whose output never parsed. They share one chunk,
    // whose calls run at the same time, so each answer is keyed to its session.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-1", Some("Existing work")))
        .unwrap();
    insert_dated_session(&db, "determined", timestamp(20));
    insert_dated_session(&db, "declined", timestamp(10));
    insert_dated_session(&db, "malformed", timestamp(0));
    let classifier = by_session(vec![
        (
            "determined",
            Ok(ClassificationOutput {
                choice: StreamChoice::Existing {
                    stream_id: "stream-1".to_string(),
                },
                confidence: 0.95,
                reasoning: "names the work".to_string(),
            }),
        ),
        (
            "declined",
            Ok(ClassificationOutput {
                choice: StreamChoice::Undetermined,
                confidence: 0.1,
                reasoning: "cannot identify the work".to_string(),
            }),
        ),
        (
            "malformed",
            Err(LlmError::Parse("not a verdict".to_string())),
        ),
    ]);

    // When: the pass must reach its end. A declined answer must not stop it, and must not
    // stop the candidates beside it from being classified.
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: three buckets, three counts, no double-counting between them.
    assert_eq!(outcome.assigned, 1);
    assert_eq!(outcome.undetermined, 1);
    assert_eq!(outcome.errors, 1);
    assert_eq!(
        outcome.causes,
        ErrorCauses {
            parse: 1,
            ..ErrorCauses::default()
        }
    );

    // And: the determinable session was classified, and neither of the other two
    // acquired a stream.
    assert_eq!(
        stored_event(&db, "event-determined").stream_id.as_deref(),
        Some("stream-1")
    );
    assert!(stored_event(&db, "event-declined").stream_id.is_none());
    assert!(stored_event(&db, "event-malformed").stream_id.is_none());

    // And: the one genuine failure is the only thing recorded as one.
    let last_error = db.get_classifier_health().unwrap().last_error.unwrap();
    assert!(last_error.contains("unparseable"), "{last_error}");
}

#[test]
fn a_declined_answer_and_a_throwaway_are_not_the_same_verdict() {
    // Given: two identical sessions, answered by the two verdicts that both leave the
    // classifier naming no stream. Throwaway says no attributable work exists;
    // undetermined says work may exist and was not identified. Conflating them files
    // real work as nothing, which is why they must diverge here.
    let junked_db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&junked_db, "session-a", &["Are you there?"]);
    let declined_db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&declined_db, "session-a", &["Are you there?"]);

    // When
    let junked = run_auto(
        &junked_db,
        &Config::default(),
        &scripted(StreamChoice::Throwaway, 0.9),
    )
    .unwrap();
    let declined = run_auto(
        &declined_db,
        &Config::default(),
        &scripted(StreamChoice::Undetermined, 0.9),
    )
    .unwrap();

    // Then: throwaway still routes to the reserved junk stream, unchanged.
    assert_eq!(junked.junked, 1);
    assert_eq!(junked.undetermined, 0);
    assert_eq!(
        stored_event(&junked_db, "event-a")
            .assignment_source
            .as_deref(),
        Some("junk")
    );

    // And: the declined answer reaches no stream at all, at the same confidence.
    // Confidence is not what tells them apart — the verdict is.
    assert_eq!(declined.undetermined, 1);
    assert_eq!(declined.junked, 0);
    assert!(stored_event(&declined_db, "event-a").stream_id.is_none());
}

#[test]
fn the_pass_summary_reports_declined_answers_apart_from_failures() {
    // Given: a pass that met both. They are counted separately because they ask
    // different things of whoever reads the line: declined answers are the classifier
    // working as instructed on input it cannot place, while errors are calls that
    // broke. Folded together, a prompt working correctly reads as a defect.
    let outcome = AutoClassifyOutcome {
        assigned: 40,
        undetermined: 7,
        errors: 2,
        causes: ErrorCauses {
            parse: 2,
            ..ErrorCauses::default()
        },
        ..AutoClassifyOutcome::default()
    };

    // When/Then
    assert_eq!(
        summary_line(&outcome),
        "Auto-classify: assigned=40, junked=0, proposed=0, superseded=0, refused=0, \
         missing_stream=0, undetermined=7, turns_exhausted=0, skipped_answered=0, skipped=0, \
         errors=2 (parse=2)"
    );
}

#[test]
fn a_spent_turn_budget_leaves_the_session_unassigned_without_failing_the_pass() {
    // Given: a session whose classification burned every model call it was allowed
    // without ever delivering a verdict. In production this arrived as
    // `model call failed: PromptError: MaxTurnsError: reached max turns`, so each one
    // cost a session *and* incremented `classifier_consecutive_failures`, whose
    // exponential backoff had throttled the drain from 1.67 to 0.17
    // classifications a minute.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["The following tool was executed"]);
    let classifier = scripted(StreamChoice::TurnsExhausted, 0.0);

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: counted in its own bucket, and nowhere near the error tally.
    assert_eq!(outcome.turns_exhausted, 1);
    assert_eq!(outcome.errors, 0);
    assert_eq!(outcome.causes, ErrorCauses::default());

    // And: left honestly unassigned — not junked, not proposed, no container invented.
    // `junk_stream_id` creates the reserved stream on demand, so an empty roster proves
    // the junk path was never taken.
    assert_eq!(outcome.junked, 0);
    assert_eq!(outcome.assigned, 0);
    assert_eq!(outcome.proposed, 0);
    assert!(db.get_streams().unwrap().is_empty());
    assert!(stored_event(&db, "event-a").stream_id.is_none());

    // And: nothing drives the failure backoff, which is the whole defect.
    let health = db.get_classifier_health().unwrap();
    assert_eq!(health.consecutive_failures, 0);
    assert!(health.last_failure_at.is_none());
    assert!(health.last_error.is_none());
    assert!(health.last_success_at.is_some());
}

#[test]
fn a_spent_turn_budget_is_tallied_apart_from_a_model_that_answered_nothing() {
    // Given: two identical sessions and the two answers that both leave the classifier
    // naming no stream. One is the model declining, as the prompt instructs; the other
    // is the model never converging before its model-call ceiling. They rest in the
    // same place, but only the second says a budget was spent chasing an answer — which
    // is the signal that says the fetch loop, not the input, is the problem.
    let declined_db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&declined_db, "session-a", &["Are you there?"]);
    let exhausted_db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&exhausted_db, "session-a", &["Are you there?"]);

    // When
    let declined = run_auto(
        &declined_db,
        &Config::default(),
        &scripted(StreamChoice::Undetermined, 0.1),
    )
    .unwrap();
    let exhausted = run_auto(
        &exhausted_db,
        &Config::default(),
        &scripted(StreamChoice::TurnsExhausted, 0.0),
    )
    .unwrap();

    // Then: one counter each, never both.
    assert_eq!(declined.undetermined, 1);
    assert_eq!(declined.turns_exhausted, 0);
    assert_eq!(exhausted.turns_exhausted, 1);
    assert_eq!(exhausted.undetermined, 0);

    // And: both sessions end up in the same honest place.
    assert!(stored_event(&declined_db, "event-a").stream_id.is_none());
    assert!(stored_event(&exhausted_db, "event-a").stream_id.is_none());
}

#[test]
fn the_pass_summary_reports_spent_turn_budgets_apart_from_declined_answers() {
    // Given: a pass that met both, plus real failures. Three counts that must stay
    // three: a declined answer needs nothing, a spent turn budget says the agentic
    // loop is over-fetching, and an error says a call broke.
    let outcome = AutoClassifyOutcome {
        assigned: 40,
        undetermined: 7,
        turns_exhausted: 4,
        errors: 2,
        causes: ErrorCauses {
            parse: 2,
            ..ErrorCauses::default()
        },
        ..AutoClassifyOutcome::default()
    };

    // When/Then: reported outside `errors`, because no call failed.
    assert_eq!(
        summary_line(&outcome),
        "Auto-classify: assigned=40, junked=0, proposed=0, superseded=0, refused=0, \
         missing_stream=0, undetermined=7, turns_exhausted=4, skipped_answered=0, skipped=0, \
         errors=2 (parse=2)"
    );
}

#[test]
fn the_pass_summary_reports_superseded_proposals() {
    // Given: a pass that answered past questions already sitting in the review queue.
    // Its own field because it is the only count that says the queue shrank without a
    // human touching it — a reviewer seeing fewer proposals than yesterday needs to
    // know whether they were answered or lost.
    let outcome = AutoClassifyOutcome {
        assigned: 40,
        proposed: 3,
        superseded: 5,
        ..AutoClassifyOutcome::default()
    };

    // When/Then: reported beside `proposed`, which is the counter it drains.
    assert_eq!(
        summary_line(&outcome),
        "Auto-classify: assigned=40, junked=0, proposed=3, superseded=5, refused=0, \
         missing_stream=0, undetermined=0, turns_exhausted=0, skipped_answered=0, skipped=0, \
         errors=0"
    );
}

#[test]
fn the_pass_summary_reports_questions_this_generation_already_answered() {
    // Given: a pass that spent most of its window-run budget on runs it had already
    // answered. Its own field because a pass that skips everything otherwise reads as
    // an idle one — the counters that move are all zero — and the operator needs to see
    // the budget being conserved rather than wonder why nothing happened.
    let outcome = AutoClassifyOutcome {
        assigned: 40,
        skipped_answered: 97,
        skipped: 3,
        ..AutoClassifyOutcome::default()
    };

    // When/Then: reported beside `skipped`, the counter it splits away from. A skip
    // here costs no model call, while `skipped` counts answers that cost one and then
    // added nothing to the queue.
    assert_eq!(
        summary_line(&outcome),
        "Auto-classify: assigned=40, junked=0, proposed=0, superseded=0, refused=0, \
         missing_stream=0, undetermined=0, turns_exhausted=0, skipped_answered=97, skipped=3, \
         errors=0"
    );
}

#[test]
fn a_new_stream_name_differing_only_in_whitespace_reuses_the_existing_stream() {
    // Given: a stream already named, and a verdict that re-emits that name with a leading
    // space. This is the live defect: the table holds
    // `" agent-c: eval-3 prometheus test-stage (round 2)"` beside its unspaced twin,
    // minted three minutes apart, because the reuse check was a plain string comparison.
    let db = tt_db::Database::open_in_memory().unwrap();
    let mut held = stream("held", Some("Standing up eval-3 environments"));
    held.name = Some("agent-c: eval-3 integration".to_string());
    db.insert_stream(&held).unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let classifier = scripted(
        StreamChoice::New {
            name: "  agent-c: eval-3 integration ".to_string(),
            description: Some("Standing up eval-3 environments".to_string()),
        },
        0.95,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: one stream, and the events landed on the one that already existed.
    assert_eq!(outcome.assigned, 1);
    assert_eq!(db.get_streams().unwrap().len(), 1);
    assert_eq!(
        stored_event(&db, "event-session-a").stream_id.as_deref(),
        Some("held")
    );
}

#[test]
fn a_new_stream_name_matching_a_stream_absent_from_the_roster_still_reuses_it() {
    // Given: a stream the roster the model saw did not contain. The roster is now capped,
    // so this is the ordinary case rather than an edge one — and it is what makes capping
    // safe: the model proposes a name it could not see, and the answer is reuse rather
    // than a second row. Checked against the database precisely because the roster
    // snapshot cannot answer it.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let classifier = scripted(
        StreamChoice::New {
            name: "agent-c: eval-3 integration".to_string(),
            description: None,
        },
        0.95,
    );
    let config = Config::default();
    // Built before the stream exists, so the resolver's snapshot never holds it.
    let mut resolver = Resolver::new(&db, &config, &classifier).unwrap();
    let mut planted = stream("planted-later", None);
    planted.name = Some("agent-c: eval-3 integration".to_string());
    db.insert_stream(&planted).unwrap();
    assert!(
        !resolver
            .roster
            .iter()
            .any(|listed| listed.id == "planted-later"),
        "the snapshot must not already hold the planted stream"
    );

    // When
    let input = tt_llm::ClassificationInput {
        has_session: true,
        session_id: "session-a".to_string(),
        machine: None,
        cwd: None,
        starting_prompt: None,
        user_prompts: vec!["do the work".to_string()],
        window_titles: Vec::new(),
        started_at: Some(timestamp(0)),
    };
    let output = resolver.classify(&input).expect("the mock answers once");
    let landed_on = resolver
        .resolve(
            output,
            AssignmentTarget::Session {
                session_id: "session-a",
                prompt_count: 1,
            },
            input.started_at,
        )
        .unwrap();

    // Then
    assert_eq!(landed_on.as_deref(), Some("planted-later"));
    assert_eq!(db.get_streams().unwrap().len(), 1);
}

#[test]
fn a_new_stream_name_naming_an_existing_initiative_reuses_it_rather_than_minting() {
    // Given: a stream already covering an initiative, and a verdict naming that same work
    // in slightly different words. This is the ordinary case now that the roster shows 200
    // of 2,288 streams: the model names work it was never shown, does not reproduce the
    // wording, and the exact lookup cannot collapse the result precisely because it is not
    // exact. Every such miss minted a row -- 1,638 streams in August against 143 in May.
    let db = tt_db::Database::open_in_memory().unwrap();
    let mut held = stream("held", None);
    held.name = Some("dojo: smart home ideation + planning".to_string());
    db.insert_stream(&held).unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let classifier = scripted(
        StreamChoice::New {
            name: "dojo: smart home automation ideation + planning".to_string(),
            description: None,
        },
        0.95,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: one stream, and the events landed on the one that already existed.
    assert_eq!(outcome.assigned, 1);
    assert_eq!(db.get_streams().unwrap().len(), 1);
    assert_eq!(
        stored_event(&db, "event-session-a").stream_id.as_deref(),
        Some("held")
    );
}

#[test]
fn an_exactly_named_stream_wins_over_a_merely_near_one() {
    // Given: two streams the proposed name could land on -- one carrying it exactly, one
    // only near it, and the near one created first so a scan would reach it first. Exact
    // is the one answer that is not a judgement, so it has to win. Both are planted after
    // the resolver is built, so its roster snapshot cannot answer either lookup and the
    // ordering being tested is the database's.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let classifier = scripted(
        StreamChoice::New {
            name: "dojo: smart home automation ideation + planning".to_string(),
            description: None,
        },
        0.95,
    );
    let config = Config::default();
    let mut resolver = Resolver::new(&db, &config, &classifier).unwrap();
    for (id, created, name) in [
        ("near", timestamp(0), "dojo: smart home ideation + planning"),
        (
            "exact",
            timestamp(10),
            "dojo: smart home automation ideation + planning",
        ),
    ] {
        let mut planted = stream(id, None);
        planted.name = Some(name.to_string());
        planted.created_at = created;
        db.insert_stream(&planted).unwrap();
    }

    // When
    let input = tt_llm::ClassificationInput {
        has_session: true,
        session_id: "session-a".to_string(),
        machine: None,
        cwd: None,
        starting_prompt: None,
        user_prompts: vec!["do the work".to_string()],
        window_titles: Vec::new(),
        started_at: Some(timestamp(0)),
    };
    let output = resolver.classify(&input).expect("the mock answers once");
    let landed_on = resolver
        .resolve(
            output,
            AssignmentTarget::Session {
                session_id: "session-a",
                prompt_count: 1,
            },
            input.started_at,
        )
        .unwrap();

    // Then
    assert_eq!(landed_on.as_deref(), Some("exact"));
    assert_eq!(db.get_streams().unwrap().len(), 2);
}

#[test]
fn a_stream_minted_mid_pass_carries_the_period_it_was_minted_for() {
    // Given: a pass that mints a new stream for a session.
    //
    // Pushed onto the roster with no period, the stream is invisible for the rest of
    // that pass: `prompt::build`'s `proximity` sorts every period-less stream behind
    // every stream that has one, and `ROSTER_LIMIT` then cuts the tail -- on the live
    // table that is 200 of ~1,245. So the model cannot see the stream it just created,
    // and names a near-miss for the same work, which `find_stream_by_normalized_name`
    // cannot collapse precisely because it is not an exact match. Three streams for one
    // initiative were minted that way inside seven hours.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let classifier = scripted(
        StreamChoice::New {
            name: "dojo: smart home ideation".to_string(),
            description: None,
        },
        0.95,
    );
    let config = Config::default();
    let mut resolver = Resolver::new(&db, &config, &classifier).unwrap();

    // When: the session is classified and the stream is created.
    let input = tt_llm::ClassificationInput {
        has_session: true,
        session_id: "session-a".to_string(),
        machine: None,
        cwd: None,
        starting_prompt: None,
        user_prompts: vec!["do the work".to_string()],
        window_titles: Vec::new(),
        started_at: Some(timestamp(0)),
    };
    let output = resolver.classify(&input).expect("the mock answers once");
    let stream_id = resolver
        .resolve(
            output,
            AssignmentTarget::Session {
                session_id: "session-a",
                prompt_count: 1,
            },
            input.started_at,
        )
        .unwrap()
        .expect("a confident verdict assigns");

    // Then: the roster entry carries the period of the work it was minted for, so the
    // next session in the same era ranks it closest rather than losing it off the tail.
    // This invents nothing -- that session is the stream's first activity, so it is what
    // `stream_activity_windows` reports for it on the next pass, one pass earlier.
    let listed = resolver
        .roster
        .iter()
        .find(|listed| listed.id == stream_id)
        .expect("a minted stream must join the roster");
    assert_eq!(
        (listed.first_active, listed.last_active),
        (Some(timestamp(0)), Some(timestamp(0))),
        "a period-less roster entry is cut by ROSTER_LIMIT and mints near-duplicates"
    );
}

#[test]
fn a_created_stream_is_stored_under_its_normalized_name() {
    // Given: a verdict whose name carries whitespace no human meant. Storing it verbatim
    // is what made the duplicate findable by nothing.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let classifier = scripted(
        StreamChoice::New {
            name: " agent-c:  eval-3 integration ".to_string(),
            description: None,
        },
        0.95,
    );

    // When
    run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    let streams = db.get_streams().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(
        streams[0].name.as_deref(),
        Some("agent-c: eval-3 integration")
    );
}

#[test]
fn a_proposed_new_stream_carries_the_normalized_name() {
    // Given: a low-confidence verdict, so the name reaches the proposal queue instead of
    // a stream. `tt proposals accept` reuses by normalized name, so a payload holding the
    // raw form would make the queued row disagree with what accepting it would do.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let classifier = scripted(
        StreamChoice::New {
            name: " agent-c:  eval-3 integration ".to_string(),
            description: None,
        },
        0.5,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    assert_eq!(outcome.proposed, 1);
    let proposals = db
        .get_proposals(Some(tt_db::ProposalStatus::Pending))
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(proposals[0].proposed_new_stream.as_deref().unwrap()).unwrap();
    assert_eq!(payload["name"], "agent-c: eval-3 integration");
}

#[test]
fn a_misnamed_name_is_still_refused_after_normalization() {
    // Given: a name that only whitespace kept from matching the guard's shape. Normalizing
    // before the guard must tighten it, never loosen it.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let classifier = scripted(
        StreamChoice::New {
            name: " misc:   stragglers ".to_string(),
            description: None,
        },
        0.95,
    );

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then
    assert_eq!(outcome.refused, 1);
    assert_eq!(outcome.assigned, 0);
    assert!(db.get_streams().unwrap().is_empty());
}

#[test]
fn the_roster_orders_streams_by_their_newest_event() {
    // Given: three streams whose creation order is the reverse of their activity order.
    // The roster's ordering key is live event activity, not `streams.last_event_at` — only
    // `tt recompute` writes that column, and 758 of the live table's 1,018 streams have it
    // NULL, which would leave three quarters of the roster in an undifferentiated tail.
    let db = tt_db::Database::open_in_memory().unwrap();
    for id in ["first", "second", "third"] {
        db.insert_stream(&stream(id, None)).unwrap();
    }
    for (event_id, minutes, stream_id) in [
        ("e-first", 5, "first"),
        ("e-second", 90, "second"),
        ("e-third", 40, "third"),
    ] {
        let mut activity = event(event_id, None, EventType::AgentToolUse);
        activity.timestamp = timestamp(minutes);
        activity.stream_id = Some(stream_id.to_string());
        db.insert_event(&activity).unwrap();
    }
    let classifier = MockClassifier::default();
    let config = Config::default();

    // When
    let resolver = Resolver::new(&db, &config, &classifier).unwrap();

    // Then: `last_active` carries the newest event, which is what `prompt::build` sorts on.
    let activity = |id: &str| {
        resolver
            .roster
            .iter()
            .find(|listed| listed.id == id)
            .and_then(|listed| listed.last_active)
    };
    assert_eq!(activity("second"), Some(timestamp(90)));
    assert_eq!(activity("third"), Some(timestamp(40)));
    assert_eq!(activity("first"), Some(timestamp(5)));
}

#[test]
fn a_stream_with_no_events_reports_no_activity() {
    // Given: a stream nothing has ever touched. 171 of the live table's streams are in
    // that state, and `prompt::build` sorts them to the tail — which only works if the
    // roster says "never active" rather than inventing a time.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("untouched", None)).unwrap();
    let classifier = MockClassifier::default();
    let config = Config::default();

    // When
    let resolver = Resolver::new(&db, &config, &classifier).unwrap();

    // Then
    assert_eq!(resolver.roster[0].last_active, None);
}

#[test]
fn one_success_clears_the_failure_streak_without_waiting_for_the_pass_to_end() {
    // Given a classifier that fails once and then answers.
    //
    // Regression: success used to be recorded once, after all four phases of
    // `run_auto` returned, while failure was recorded per call. A pass is bounded at
    // 200 sessions and runs for hours, so the counter read as "failures since the last
    // completed pass" rather than consecutive failures. The live daemon showed the
    // shape exactly: `classifier_consecutive_failures` at 3 and a
    // `classifier_last_success_at` a day old, while it was classifying successfully
    // right then -- arming the exponential `classifier_retry_delay` against a provider
    // that was answering. Observing this needs the resolver directly, because through
    // `run_auto` the end-of-pass write hides it.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = MockClassifier::default();
    {
        let mut scripted = classifier.scripted.lock().unwrap();
        scripted.push_back(Err(LlmError::Api("offline".to_string())));
        scripted.push_back(Ok(ClassificationOutput {
            choice: StreamChoice::New {
                name: "time-tracker: classifier health".to_string(),
                description: None,
            },
            confidence: 0.95,
            reasoning: "test reasoning".to_string(),
        }));
    }
    let config = Config::default();
    let mut resolver = Resolver::new(&db, &config, &classifier).unwrap();
    let input = tt_llm::ClassificationInput {
        has_session: true,
        session_id: "session-a".to_string(),
        machine: None,
        cwd: None,
        starting_prompt: None,
        user_prompts: vec!["do the work".to_string()],
        window_titles: Vec::new(),
        started_at: Some(timestamp(0)),
    };

    // When the first call fails, the streak is armed.
    assert!(resolver.classify(&input).is_none());
    assert_eq!(db.get_classifier_health().unwrap().consecutive_failures, 1);

    // Then the very next success clears it, mid-pass.
    assert!(resolver.classify(&input).is_some());
    let health = db.get_classifier_health().unwrap();
    assert_eq!(
        health.consecutive_failures, 0,
        "a success must clear the streak when it happens, not when the pass ends"
    );
    assert!(
        health.last_success_at.is_some(),
        "the success must also be timestamped as it happens"
    );
}

/// Stores one unassigned window-focus event that groups into a run of its own.
///
/// A distinct `window_app_id` is what starts a new run, so `index` buys both a
/// separate run and a distinct position in time.
fn insert_distinct_window_run(db: &tt_db::Database, index: usize) {
    let offset = i64::try_from(index).expect("test index fits in i64");
    let mut window = event(&format!("window-{index}"), None, EventType::WindowFocus);
    window.timestamp = timestamp(offset);
    window.window_app_id = Some(format!("org.example.App{index}"));
    window.window_title = Some(format!("file-{index}.rs"));
    db.insert_event(&window).unwrap();
}

/// Queues `count` identical high-confidence answers naming `stream_id`.
fn scripted_repeatedly(stream_id: &str, count: usize) -> MockClassifier {
    let classifier = MockClassifier::default();
    let mut queue = classifier.scripted.lock().unwrap();
    for _ in 0..count {
        queue.push_back(Ok(ClassificationOutput {
            choice: StreamChoice::Existing {
                stream_id: stream_id.to_string(),
            },
            confidence: 0.9,
            reasoning: "test reasoning".to_string(),
        }));
    }
    drop(queue);
    classifier
}

#[test]
fn window_focus_is_classified_even_when_the_session_backlog_fills_a_whole_pass() {
    // Given: a session backlog one larger than a whole bounded pass, and a single
    // unassigned window run. Focus events carry no session — 0 of 29,730 have one — so
    // this phase is the only path that ever attributes them, and it used to run *after*
    // the session phase. At the observed ~1 classification a minute, a full
    // `SESSIONS_PER_PASS` is ~3.3 hours a focus event waits before anything looks at it.
    // Measured on the live database: `assignment_source = 'inferred'` on focus events
    // stopped at 2026-08-05T02:16, 2026-08-06 attributed 0 of 1,882, and over the last
    // six hours it ran 0 of 383.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    for index in 0..=SESSIONS_PER_PASS {
        let minutes = i64::try_from(index).expect("test index fits in i64");
        insert_dated_session(&db, &format!("session-{index:04}"), timestamp(minutes));
    }
    insert_window_event(&db, "window-a");
    // Enough answers for the whole session phase plus one, so the session backlog cannot
    // exhaust the budget. What this pins is the guarantee that survives ordering: the two
    // phases hold INDEPENDENT bounds, so a full session backlog cannot starve focus
    // attribution of calls.
    //
    // It deliberately no longer pins the phase *order*. That was inverted for a while so
    // focus ran first, and the arithmetic went against it: a session classification
    // attributes every event carrying its id (~376 each, 1,339,657 across 3,559 sessions)
    // while the window phase places ~11 focus events a day, because 0 of 403 pending
    // proposals reach the 0.80 threshold. Running focus first spent whole passes on the
    // phase that attributes almost nothing — measured, `classified_sessions` unmoved at
    // 2,003 across 25 minutes while the classifier reported success throughout.
    let classifier = MockClassifier::default();
    {
        let mut queue = classifier.scripted.lock().unwrap();
        for _ in 0..=SESSIONS_PER_PASS {
            queue.push_back(Ok(ClassificationOutput {
                choice: StreamChoice::Existing {
                    stream_id: "stream-a".to_string(),
                },
                confidence: 0.9,
                reasoning: "test reasoning".to_string(),
            }));
        }
    }

    // When
    run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: direct time was attributed even with a full session backlog ahead of it.
    assert_eq!(
        stored_event(&db, "window-a").stream_id.as_deref(),
        Some("stream-a"),
        "focus attribution starved behind the session backlog"
    );
}

#[test]
fn a_pass_classifies_window_focus_and_sessions_together() {
    // Given: one window run and one session, and one answer each. The two phases hold
    // independent bounds, so neither can spend the other's budget. The scripted order
    // pins the phase order: sessions run first, because a session classification
    // attributes every event carrying its id (~376 each) while the window phase places
    // ~11 focus events a day.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-window", None)).unwrap();
    db.insert_stream(&stream("stream-session", None)).unwrap();
    insert_window_event(&db, "window-a");
    insert_session_candidate(&db, "session-a", &["implement classification"]);
    let classifier = MockClassifier::default();
    {
        let mut queue = classifier.scripted.lock().unwrap();
        for stream_id in ["stream-session", "stream-window"] {
            queue.push_back(Ok(ClassificationOutput {
                choice: StreamChoice::Existing {
                    stream_id: stream_id.to_string(),
                },
                confidence: 0.9,
                reasoning: "test reasoning".to_string(),
            }));
        }
    }

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: both landed, and the session consumed the first call.
    assert_eq!(outcome.assigned, 2);
    assert_eq!(outcome.errors, 0);
    assert_eq!(
        stored_event(&db, "window-a").stream_id.as_deref(),
        Some("stream-window")
    );
    assert_eq!(
        stored_event(&db, "event-a").stream_id.as_deref(),
        Some("stream-session")
    );
}

#[test]
fn the_window_run_phase_stops_at_its_per_pass_bound() {
    // Given: more unassigned window runs than one pass may spend calls on, and an
    // answer waiting for every one of them, so nothing but the bound can stop the
    // phase. Unbounded, it emitted one call per run in a single burst — the exact
    // shape `SESSIONS_PER_PASS` exists to prevent for the other phase.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("stream-a", None)).unwrap();
    let runs = WINDOW_RUNS_PER_PASS + 5;
    for index in 0..runs {
        insert_distinct_window_run(&db, index);
    }
    let classifier = scripted_repeatedly("stream-a", runs);

    // When
    let outcome = run_auto(&db, &Config::default(), &classifier).unwrap();

    // Then: one event per run, capped at the bound, and nothing failed — the runs past
    // the bound were never asked rather than asked and refused.
    assert_eq!(
        outcome.assigned,
        u64::try_from(WINDOW_RUNS_PER_PASS).unwrap()
    );
    assert_eq!(outcome.errors, 0);

    // And: it spent them on the newest runs. `build_unassigned_window_runs` yields
    // oldest-first, so a bound applied to that order would freeze today's focus behind
    // the 29,730-event backlog — the same starvation one level down.
    assert!(
        stored_event(&db, &format!("window-{}", runs - 1))
            .stream_id
            .is_some(),
        "the newest run must be inside the bound"
    );
    assert!(
        stored_event(&db, "window-0").stream_id.is_none(),
        "the oldest run must be the one left for the next pass"
    );
}

#[test]
fn a_stream_id_the_model_spaced_out_still_resolves() {
    // Given: a verdict naming a real stream, with whitespace the model inserted.
    //
    // Observed live, both of these named an existing stream and were refused:
    //   " eb2754ad-e9a3-469b-bdb3-1768bc8860e8"   (leading space)
    //   " e638ad6d -fd e6 -4f 85 -b 9d 0-7 e9 b0 2b 98 9a8 "  (spaces throughout)
    // Each refusal spent a model call and left the work unassigned, for a difference no
    // human could have meant — the same rule `normalize_stream_name` applies to names.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let mut planted = stream("eb2754ad-e9a3-469b-bdb3-1768bc8860e8", None);
    planted.name = Some("dojo: smart home ideation".to_string());
    db.insert_stream(&planted).unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: " eb2754ad-e9a3 -469b -bdb3-1768bc8860e8 ".to_string(),
        },
        0.95,
    );
    let config = Config::default();
    let mut resolver = Resolver::new(&db, &config, &classifier).unwrap();

    // When: the session is classified.
    let input = tt_llm::ClassificationInput {
        has_session: true,
        session_id: "session-a".to_string(),
        machine: None,
        cwd: None,
        starting_prompt: None,
        user_prompts: vec!["do the work".to_string()],
        window_titles: Vec::new(),
        started_at: Some(timestamp(0)),
    };
    let output = resolver.classify(&input).expect("the mock answers once");
    let landed = resolver
        .resolve(
            output,
            AssignmentTarget::Session {
                session_id: "session-a",
                prompt_count: 1,
            },
            input.started_at,
        )
        .unwrap();

    // Then: it lands on the real stream rather than being refused.
    assert_eq!(
        landed.as_deref(),
        Some("eb2754ad-e9a3-469b-bdb3-1768bc8860e8"),
        "whitespace in a uuid must not cost a correct verdict"
    );
    assert_eq!(
        resolver.outcome.refused_missing_stream, 0,
        "a recoverable id must not count as a missing stream"
    );
}

#[test]
fn a_stream_id_that_is_merely_short_is_still_refused() {
    // Given: a verdict naming an 8-character prefix of a real stream id. The report
    // renders short ids, so the model does emit them ("ff383f53" was observed live).
    //
    // This stays REFUSED. Stripping whitespace erases a difference no human could have
    // meant; accepting a prefix would be resolving an ambiguous reference, which is
    // guessing. Two ids sharing a prefix would bind work to the wrong stream silently,
    // and refusing leaves it unassigned where it reads as classification lag.
    let db = tt_db::Database::open_in_memory().unwrap();
    insert_dated_session(&db, "session-a", timestamp(0));
    let mut planted = stream("ff383f53-1111-2222-3333-444455556666", None);
    planted.name = Some("devbox: infra".to_string());
    db.insert_stream(&planted).unwrap();
    let classifier = scripted(
        StreamChoice::Existing {
            stream_id: "ff383f53".to_string(),
        },
        0.95,
    );
    let config = Config::default();
    let mut resolver = Resolver::new(&db, &config, &classifier).unwrap();

    // When: the session is classified.
    let input = tt_llm::ClassificationInput {
        has_session: true,
        session_id: "session-a".to_string(),
        machine: None,
        cwd: None,
        starting_prompt: None,
        user_prompts: vec!["do the work".to_string()],
        window_titles: Vec::new(),
        started_at: Some(timestamp(0)),
    };
    let output = resolver.classify(&input).expect("the mock answers once");
    let landed = resolver
        .resolve(
            output,
            AssignmentTarget::Session {
                session_id: "session-a",
                prompt_count: 1,
            },
            input.started_at,
        )
        .unwrap();

    // Then: nothing is assigned and it is counted as a missing stream.
    assert!(
        landed.is_none(),
        "a prefix must not be resolved to a stream"
    );
    assert_eq!(resolver.outcome.refused_missing_stream, 1);
}
