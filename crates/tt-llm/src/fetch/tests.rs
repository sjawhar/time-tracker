use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{FetchOutcome, FetchRequest, MAX_FETCH_CALLS, MAX_MESSAGES_PER_PAGE, SessionTools};
use crate::session_detail::{MessagePage, SessionDetail, SessionDetailError, SessionOverview};

/// A provider that counts how often it was actually consulted.
#[derive(Default)]
struct FakeDetail {
    messages: Vec<String>,
    reads: AtomicUsize,
    missing: bool,
    last_limit: AtomicUsize,
}

impl SessionDetail for FakeDetail {
    fn overview(&self, session_id: &str) -> Result<SessionOverview, SessionDetailError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.missing {
            return Err(SessionDetailError::NotFound(session_id.to_owned()));
        }
        Ok(SessionOverview {
            summary: Some("Rotation bug fix".to_owned()),
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
        self.last_limit.store(limit, Ordering::SeqCst);
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

/// Stands in for `tt_core::injection::is_injected`, which this crate cannot reach.
fn is_injected(message: &str) -> bool {
    message.trim_start().starts_with("<system-reminder>")
}

fn overview_request() -> FetchRequest {
    FetchRequest::Overview {
        session_id: "ses-1".to_owned(),
    }
}

#[test]
fn the_budget_stops_cleanly_once_the_call_limit_is_reached() {
    // Given: a session whose provider records every read it is asked for.
    let detail = Arc::new(FakeDetail::default());
    let session = SessionTools::new(Arc::clone(&detail) as Arc<dyn SessionDetail>, is_injected)
        .begin("ses-1");

    // When: the classifier spends its whole budget and then asks once more.
    let spent: Vec<FetchOutcome> = (0..MAX_FETCH_CALLS)
        .map(|_| session.dispatch(&overview_request()))
        .collect();
    let over_budget = session.dispatch(&overview_request());

    // Then: every call inside the budget was served.
    assert!(
        spent
            .iter()
            .all(|outcome| matches!(outcome, FetchOutcome::Fetched(_))),
        "calls within the budget must be served: {spent:?}"
    );
    // And: the one past it stops cleanly rather than erroring.
    assert_eq!(over_budget, FetchOutcome::BudgetExhausted);
    // And: the stop is real — the provider was never consulted for it.
    assert_eq!(detail.reads.load(Ordering::SeqCst), MAX_FETCH_CALLS);
    assert_eq!(session.calls_used(), MAX_FETCH_CALLS);
}

#[test]
fn an_exhausted_budget_tells_the_model_to_answer_from_what_it_has() {
    // Given / When
    let rendered = FetchOutcome::BudgetExhausted.rendered();

    // Then: the model is directed to a verdict, not left guessing why a tool failed.
    assert!(
        rendered.contains("Classify from what you have"),
        "{rendered}"
    );
    assert!(rendered.contains("low confidence"), "{rendered}");
}

#[test]
fn an_injected_message_never_reaches_the_rendered_page() {
    // Given: a session whose stored messages include harness-injected text.
    let detail = Arc::new(FakeDetail {
        messages: vec![
            "Fix the DisplayLink rotation bug".to_owned(),
            "<system-reminder>the user opened a new file</system-reminder>".to_owned(),
            "and add a regression test".to_owned(),
        ],
        ..FakeDetail::default()
    });
    let session = SessionTools::new(detail as Arc<dyn SessionDetail>, is_injected).begin("ses-1");

    // When
    let outcome = session.dispatch(&FetchRequest::Messages {
        session_id: "ses-1".to_owned(),
        offset: 0,
        limit: MAX_MESSAGES_PER_PAGE,
    });

    // Then: the human intent survives.
    let rendered = outcome.rendered();
    assert!(rendered.contains("DisplayLink rotation bug"), "{rendered}");
    assert!(rendered.contains("regression test"), "{rendered}");
    // And: the injected text is gone, so it can never be read as attention.
    assert!(!rendered.contains("system-reminder"), "{rendered}");
    assert!(!rendered.contains("opened a new file"), "{rendered}");
}

#[test]
fn a_page_of_nothing_but_injections_says_so_rather_than_going_blank() {
    // Given: every stored message in range is injected text.
    let detail = Arc::new(FakeDetail {
        messages: vec!["<system-reminder>compacting</system-reminder>".to_owned()],
        ..FakeDetail::default()
    });
    let session = SessionTools::new(detail as Arc<dyn SessionDetail>, is_injected).begin("ses-1");

    // When
    let rendered = session
        .dispatch(&FetchRequest::Messages {
            session_id: "ses-1".to_owned(),
            offset: 0,
            limit: 3,
        })
        .rendered();

    // Then: the model learns the range is empty of human text, not that a tool broke.
    assert!(rendered.contains("No human messages"), "{rendered}");
    assert!(!rendered.contains("compacting"), "{rendered}");
}

#[test]
fn a_page_larger_than_the_maximum_is_capped() {
    // Given: a model asking for far more than a page may carry.
    let detail = Arc::new(FakeDetail {
        messages: (0..40).map(|index| format!("message-{index}")).collect(),
        ..FakeDetail::default()
    });
    let session = SessionTools::new(Arc::clone(&detail) as Arc<dyn SessionDetail>, is_injected)
        .begin("ses-1");

    // When
    session.dispatch(&FetchRequest::Messages {
        session_id: "ses-1".to_owned(),
        offset: 0,
        limit: 500,
    });

    // Then: the page stays bounded in tokens.
    assert_eq!(
        detail.last_limit.load(Ordering::SeqCst),
        MAX_MESSAGES_PER_PAGE
    );
}

#[test]
fn a_session_the_store_cannot_find_is_stated_not_raised() {
    // Given: a provider that does not know this session.
    let detail = Arc::new(FakeDetail {
        missing: true,
        ..FakeDetail::default()
    });
    let session = SessionTools::new(detail as Arc<dyn SessionDetail>, is_injected).begin("ses-1");

    // When
    let outcome = session.dispatch(&overview_request());

    // Then: the classifier can carry on and answer from the payload it already has.
    assert!(
        matches!(outcome, FetchOutcome::Unavailable(_)),
        "{outcome:?}"
    );
    assert!(outcome.rendered().contains("not indexed"));
}

#[test]
fn every_served_request_is_logged_in_order() {
    // Given
    let detail = Arc::new(FakeDetail::default());
    let session = SessionTools::new(detail as Arc<dyn SessionDetail>, is_injected).begin("ses-1");
    let page = FetchRequest::Messages {
        session_id: "ses-1".to_owned(),
        offset: 0,
        limit: 3,
    };

    // When
    session.dispatch(&overview_request());
    session.dispatch(&page);

    // Then
    assert_eq!(session.log(), vec![overview_request(), page]);
}
