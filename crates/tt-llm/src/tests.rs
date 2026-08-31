//! What the agentic classifier is supposed to do, proved without a network call.
//!
//! Every test here drives [`MockClassifier`] with a *brain* — a small deterministic
//! decision procedure that reads what it can see, decides whether that is enough, and
//! fetches only if it is not. The brain is the same in every test; what changes is the
//! payload it starts from and whether fetching is wired up. That is what makes these
//! claims about fetching rather than about a canned answer.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    ClassificationInput, ClassificationOutput, Classifier, ContextLookupOutcome,
    ContextLookupRequest, ContextProvider, ContextProviderError, ContextProviderTools,
    FetchOutcome, FetchRequest, LlmError, MAX_CONTEXT_LOOKUP_CALLS, MAX_FETCH_CALLS, MessagePage,
    MockBrain, MockClassifier, SessionDetail, SessionDetailError, SessionOverview, SessionTools,
    StreamChoice,
};

/// A session the classifier can look into, counting every read.
struct FakeSession {
    summary: Option<String>,
    messages: Vec<String>,
    reads: AtomicUsize,
}

impl FakeSession {
    fn new(summary: Option<&str>, messages: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            summary: summary.map(str::to_owned),
            messages: messages.iter().map(|text| (*text).to_owned()).collect(),
            reads: AtomicUsize::new(0),
        })
    }
}

impl SessionDetail for FakeSession {
    fn overview(&self, _session_id: &str) -> Result<SessionOverview, SessionDetailError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(SessionOverview {
            summary: self.summary.clone(),
            ..SessionOverview::default()
        })
    }

    fn messages(
        &self,
        _session_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<MessagePage, SessionDetailError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(MessagePage {
            messages: self
                .messages
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect(),
            offset,
            total: self.messages.len(),
        })
    }
}

/// A context provider whose reads are observable.
#[derive(Default)]
struct FakeContextProvider {
    lookups: AtomicUsize,
}

impl ContextProvider for FakeContextProvider {
    fn lookup(&self, _query: &str) -> Result<String, ContextProviderError> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        Ok("example-initiative is the sample migration initiative".to_owned())
    }
}

fn is_injected(message: &str) -> bool {
    message.trim_start().starts_with("<system-reminder>")
}

/// The one stream any of these tests can name, and the only evidence for it.
const SUBJECT: &str = "displaylink";
const STREAM: &str = "cosmic-displaylink";

/// Confidence a brain reports when it never found the subject.
///
/// It rides alongside [`StreamChoice::Undetermined`] rather than deciding it: the
/// verdict is what leaves the session unassigned, and confidence only says how sure the
/// model was about whatever it did name.
const CANNOT_DETERMINE: f64 = 0.1;

/// Decides from what it can see, and fetches while it cannot decide.
///
/// It asks for one more thing at a time and re-reads everything it has seen, so it
/// terminates the moment the subject appears. It deliberately tries more times than
/// the budget allows, so a test can tell the budget apart from the loop bound.
fn keyword_brain() -> MockBrain {
    Box::new(|input, session, _context| {
        let mut seen = visible_text(input);
        let mut next_offset = 0;
        for attempt in 0..MAX_FETCH_CALLS + 3 {
            if seen.to_lowercase().contains(SUBJECT) {
                return Ok(verdict(STREAM, 0.9));
            }
            let request = if attempt == 0 {
                FetchRequest::Overview {
                    session_id: input.session_id.clone(),
                }
            } else {
                let offset = next_offset;
                next_offset += 3;
                FetchRequest::Messages {
                    session_id: input.session_id.clone(),
                    offset,
                    limit: 3,
                }
            };
            match session.dispatch(&request) {
                FetchOutcome::Fetched(text) => seen.push_str(&text),
                // Both are clean stops: decide on what is already in hand.
                FetchOutcome::BudgetExhausted | FetchOutcome::Unavailable(_) => break,
            }
        }
        if seen.to_lowercase().contains(SUBJECT) {
            return Ok(verdict(STREAM, 0.9));
        }
        Ok(verdict("", CANNOT_DETERMINE))
    })
}

/// Decides from context lookup only when the real classifier would offer that tool.
fn context_lookup_brain() -> MockBrain {
    Box::new(|_input, _session, context_lookup| {
        let Some(context_lookup) = context_lookup else {
            return Ok(verdict("", CANNOT_DETERMINE));
        };
        match context_lookup.dispatch(&ContextLookupRequest {
            query: "example-initiative".to_owned(),
        }) {
            ContextLookupOutcome::Fetched(text) if text.contains("example-initiative") => {
                Ok(verdict("example-initiative", 0.9))
            }
            ContextLookupOutcome::Fetched(_)
            | ContextLookupOutcome::BudgetExhausted
            | ContextLookupOutcome::Unavailable(_) => Ok(verdict("", CANNOT_DETERMINE)),
        }
    })
}

fn visible_text(input: &ClassificationInput) -> String {
    let mut text = input.starting_prompt.clone().unwrap_or_default();
    for prompt in &input.user_prompts {
        text.push('\n');
        text.push_str(prompt);
    }
    text
}

fn verdict(stream: &str, confidence: f64) -> ClassificationOutput {
    ClassificationOutput {
        choice: if stream.is_empty() {
            // Not `Throwaway`: this brain looked and did not recognise the work, which
            // is nothing like judging that no work is there. Junking it would file real
            // work as nothing.
            StreamChoice::Undetermined
        } else {
            StreamChoice::Existing {
                stream_id: stream.to_owned(),
            }
        },
        confidence,
        reasoning: "keyword brain".to_owned(),
    }
}

fn thinking_classifier(detail: Option<Arc<dyn SessionDetail>>) -> MockClassifier {
    MockClassifier {
        brain: Some(keyword_brain()),
        tools: Some(detail.map_or_else(SessionTools::unavailable, |detail| {
            SessionTools::new(detail, is_injected)
        })),
        ..MockClassifier::default()
    }
}

fn input(starting_prompt: &str) -> ClassificationInput {
    ClassificationInput {
        has_session: true,
        session_id: "ses-1".to_owned(),
        machine: Some("devbox".to_owned()),
        cwd: Some("/home/sami/Code/dotfiles".to_owned()),
        starting_prompt: Some(starting_prompt.to_owned()),
        user_prompts: Vec::new(),
        window_titles: Vec::new(),
        started_at: None,
    }
}

#[test]
fn context_lookup_budget_stops_cleanly_after_its_own_limit() {
    // Given: a provider that records every lookup it is allowed to answer.
    let provider = Arc::new(FakeContextProvider::default());
    let session =
        ContextProviderTools::new(Arc::clone(&provider) as Arc<dyn ContextProvider>).begin();
    let request = ContextLookupRequest {
        query: "example-initiative".to_owned(),
    };

    // When: the classifier spends the entire lookup allowance, then asks once more.
    let served: Vec<_> = (0..MAX_CONTEXT_LOOKUP_CALLS)
        .map(|_| session.dispatch(&request))
        .collect();
    let over_budget = session.dispatch(&request);

    // Then: the final call is a clean tool response, not a classification error, and
    // the provider is not asked past the limit.
    assert!(
        served
            .iter()
            .all(|outcome| matches!(outcome, ContextLookupOutcome::Fetched(_))),
        "calls inside the context-lookup budget must be served: {served:?}"
    );
    assert_eq!(over_budget, ContextLookupOutcome::BudgetExhausted);
    assert_eq!(
        provider.lookups.load(Ordering::SeqCst),
        MAX_CONTEXT_LOOKUP_CALLS
    );
    assert_eq!(session.calls_used(), MAX_CONTEXT_LOOKUP_CALLS);
}

#[test]
fn mock_context_lookup_is_offered_only_when_a_provider_is_wired() {
    // Given: a sessionless scope — external knowledge is still meaningful here — and a
    // provider whose result identifies the initiative.
    let provider = Arc::new(FakeContextProvider::default());
    let configured = MockClassifier {
        brain: Some(context_lookup_brain()),
        context_provider: Some(ContextProviderTools::new(
            Arc::clone(&provider) as Arc<dyn ContextProvider>
        )),
        ..MockClassifier::default()
    };
    let no_provider = MockClassifier {
        brain: Some(context_lookup_brain()),
        ..MockClassifier::default()
    };
    let window_run = ClassificationInput {
        has_session: false,
        ..input("unfamiliar project codename")
    };

    // When
    let configured_output = configured.classify(&window_run, &[], None).unwrap();
    let unconfigured_output = no_provider.classify(&window_run, &[], None).unwrap();

    // Then: the mock follows `RigClassifier`'s provider gate. It never invents a fake
    // unavailable lookup session, and it still offers real context on a sessionless run.
    assert_eq!(
        configured_output.choice,
        StreamChoice::Existing {
            stream_id: "example-initiative".to_owned(),
        }
    );
    assert_eq!(provider.lookups.load(Ordering::SeqCst), 1);
    assert_eq!(unconfigured_output.choice, StreamChoice::Undetermined);
}

/// Test 1, control arm: the payload alone genuinely cannot answer this.
#[test]
fn a_thin_prompt_defeats_a_classifier_that_cannot_fetch() {
    // Given: a session whose whole prompt is uninformative, and no way to look further.
    let classifier = thinking_classifier(None);

    // When
    let output = classifier
        .classify(&input("list a file"), &[], None)
        .unwrap();

    // Then: it declines rather than guessing, and the session stays unassigned.
    assert_eq!(output.choice, StreamChoice::Undetermined);
    assert!(output.confidence <= CANNOT_DETERMINE);
    // And: it did try to look further — what defeats it is the payload, not the brain.
    assert_eq!(classifier.fetches().len(), 1);
}

/// A window run has no session, so it must not be offered the session-fetch tools.
#[test]
fn a_scope_with_no_session_is_never_offered_the_session_fetch_tools() {
    // Given: a provider that would happily answer, and a scope that has no session to
    // answer about — a window run, which passes a synthetic `window:<event_id>`.
    let detail = FakeSession::new(Some("COSMIC DisplayLink rotation bug fix"), &[]);
    let classifier = thinking_classifier(Some(Arc::clone(&detail) as Arc<dyn SessionDetail>));
    let run = ClassificationInput {
        has_session: false,
        session_id: "window:evt-1".to_owned(),
        starting_prompt: None,
        window_titles: vec!["PR #14835 - Brave".to_owned()],
        ..input("list a file")
    };

    // When
    let output = classifier.classify(&run, &[], None).unwrap();

    // Then: the provider is never consulted. `fetches()` records attempts rather than
    // successes, so what matters is that nothing reached a session that cannot exist:
    // offering the tools had the model call them and be told `session window:<id> is not
    // indexed`, which reads as a broken system rather than as a scope that never had a
    // session. Live, that reached 161 of 518 pending proposals, every one window-scoped,
    // averaging 0.491 confidence against 0.597 for the rest of the queue.
    assert_eq!(
        detail.reads.load(Ordering::SeqCst),
        0,
        "a sessionless scope reached the provider"
    );
    // And: it still answers from the titles it does have, rather than erroring.
    assert_eq!(output.choice, StreamChoice::Undetermined);
}

/// Test 1: the same brain, the same thin payload, now able to fetch.
#[test]
fn a_thin_prompt_is_resolved_by_fetching_what_the_payload_omits() {
    // Given: the identical uninformative prompt, with the session's summary reachable.
    // The summary is the only place the subject appears — `ClassificationInput` has no
    // field for it, so this verdict is unreachable from the default payload.
    let detail = FakeSession::new(Some("COSMIC DisplayLink rotation bug fix"), &[]);
    let classifier = thinking_classifier(Some(Arc::clone(&detail) as Arc<dyn SessionDetail>));

    // When
    let output = classifier
        .classify(&input("list a file"), &[], None)
        .unwrap();

    // Then: it reaches the verdict the control arm could not.
    assert_eq!(
        output.choice,
        StreamChoice::Existing {
            stream_id: STREAM.to_owned()
        }
    );
    assert!(output.confidence > CANNOT_DETERMINE);
    // And: it got there by fetching, not by knowing.
    assert_eq!(
        classifier.fetches(),
        vec![FetchRequest::Overview {
            session_id: "ses-1".to_owned()
        }]
    );
    assert_eq!(detail.reads.load(Ordering::SeqCst), 1);
}

/// Test 4: the tools are for the hard cases, not a mandatory round trip.
#[test]
fn a_rich_prompt_reaches_a_verdict_without_fetching_anything() {
    // Given: a prompt that already names the work, and a provider standing by.
    let detail = FakeSession::new(Some("COSMIC DisplayLink rotation bug fix"), &[]);
    let classifier = thinking_classifier(Some(Arc::clone(&detail) as Arc<dyn SessionDetail>));

    // When
    let output = classifier
        .classify(
            &input("Fix the DisplayLink rotation bug on the dock"),
            &[],
            None,
        )
        .unwrap();

    // Then: the verdict is reached from the payload alone.
    assert_eq!(
        output.choice,
        StreamChoice::Existing {
            stream_id: STREAM.to_owned()
        }
    );
    // And: no call was spent, so a rich session costs exactly what it costs today.
    assert!(classifier.fetches().is_empty());
    assert_eq!(detail.reads.load(Ordering::SeqCst), 0);
}

/// Test 3: the budget bounds the loop, and stopping at it still yields a verdict.
#[test]
fn an_unsatisfiable_session_stops_at_the_budget_and_still_answers() {
    // Given: a session that never mentions the subject, so the brain keeps asking —
    // more times than the budget allows.
    let detail = FakeSession::new(
        Some("routine maintenance"),
        &["tidy up", "and again", "once more", "still nothing", "nope"],
    );
    let classifier = thinking_classifier(Some(Arc::clone(&detail) as Arc<dyn SessionDetail>));

    // When
    let result = classifier.classify(&input("list a file"), &[], None);

    // Then: exceeding the budget is a clean stop, not an error.
    let output = result.expect("a spent budget must still yield a verdict");
    // And: it declines, which is what leaves the session unassigned.
    assert_eq!(output.choice, StreamChoice::Undetermined);
    assert!(output.confidence <= CANNOT_DETERMINE);
    // And: the budget is what stopped it — the brain tried MAX_FETCH_CALLS + 3 times.
    assert_eq!(classifier.fetches().len(), MAX_FETCH_CALLS);
    assert_eq!(detail.reads.load(Ordering::SeqCst), MAX_FETCH_CALLS);
}

/// Test 2 in miniature; the load-bearing version runs against the real denylist and a
/// real database in `tt-cli`.
#[test]
fn injected_text_in_a_fetched_page_never_reaches_the_brain() {
    // Given: a session whose later messages name the subject, behind an injection.
    let detail = FakeSession::new(
        None,
        &[
            "<system-reminder>displaylink was mentioned by the harness</system-reminder>",
            "carry on",
        ],
    );
    let tools = SessionTools::new(Arc::clone(&detail) as Arc<dyn SessionDetail>, is_injected);
    let session = tools.begin("ses-1");

    // When
    let rendered = session
        .dispatch(&FetchRequest::Messages {
            session_id: "ses-1".to_owned(),
            offset: 0,
            limit: 3,
        })
        .rendered();

    // Then: the subject the injection mentioned is not evidence, so it never appears.
    assert!(!rendered.to_lowercase().contains(SUBJECT), "{rendered}");
    assert!(rendered.contains("carry on"), "{rendered}");
}

#[test]
fn mock_classifier_returns_the_next_scripted_classification() {
    // Given
    let expected = ClassificationOutput {
        choice: StreamChoice::Existing {
            stream_id: "stream-1".to_owned(),
        },
        confidence: 0.9,
        reasoning: "matched project evidence".to_owned(),
    };
    let classifier = MockClassifier {
        scripted: Mutex::new(VecDeque::from([Ok(expected.clone())])),
        ..MockClassifier::default()
    };

    // When
    let result = classifier.classify(&input("Classify my work"), &[], None);

    // Then
    assert_eq!(result.unwrap(), expected);
}

#[test]
fn mock_classifier_returns_the_next_scripted_description() {
    // Given
    let classifier = MockClassifier {
        descriptions: Mutex::new(VecDeque::from([Ok("A Rust project".to_owned())])),
        ..MockClassifier::default()
    };

    // When
    let result = classifier.describe_stream("Rust evidence");

    // Then
    assert_eq!(result.unwrap(), "A Rust project");
}

#[test]
fn a_classifier_without_a_brain_still_reports_a_missing_script() {
    // Given: the pre-agentic construction, untouched.
    let classifier = MockClassifier::default();

    // When
    let result = classifier.classify(&input("anything"), &[], None);

    // Then
    assert!(matches!(result, Err(LlmError::Api(_))));
}
