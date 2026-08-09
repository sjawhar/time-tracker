use std::cell::{Cell, RefCell};
use std::time::Duration;

use super::{
    AttemptFailure, Deadline, HttpFailure, MAX_BACKOFF_STEP_MS, MAX_CLASSIFICATION_MS,
    MAX_PARSE_ATTEMPTS, MAX_TOTAL_BACKOFF_MS, MAX_TRANSPORT_RETRIES, retrying,
};
use crate::LlmError;

/// Collects the waits a run asked for instead of sleeping through them.
struct Clock(RefCell<Vec<Duration>>);

impl Clock {
    fn new() -> Self {
        Self(RefCell::new(Vec::new()))
    }

    fn wait(&self) -> impl FnMut(Duration) + '_ {
        |pause| self.0.borrow_mut().push(pause)
    }

    fn waits(&self) -> Vec<Duration> {
        self.0.borrow().clone()
    }

    fn total(&self) -> Duration {
        self.0.borrow().iter().sum()
    }
}

fn overloaded() -> AttemptFailure {
    AttemptFailure::Http(HttpFailure::new(529, "overloaded_error".to_owned()))
}

fn timed_out() -> AttemptFailure {
    AttemptFailure::Timeout("request timed out".to_owned())
}

#[test]
fn an_overload_followed_by_an_answer_yields_the_answer() {
    // Given: the production failure — Anthropic answers 529 once, then serves the
    // call. Treating that as terminal is what cost a session its classification and
    // held the live drain at 0.65 sessions a minute.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            return Err(overloaded());
        }
        Ok("a verdict")
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: the verdict survives, having cost exactly one wait.
    assert_eq!(answer.unwrap(), "a verdict");
    assert_eq!(attempts.get(), 2);
    assert_eq!(clock.waits().len(), 1);
}

#[test]
fn a_provider_that_stays_overloaded_gives_up_at_the_bound() {
    // Given: an outage rather than a blip.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(overloaded())
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: bounded in calls and in patience, and reported as what it was. A
    // retry-exhausted failure drains on its own; a generic API error may not, and an
    // operator reading the pass summary has to be able to tell them apart.
    assert_eq!(
        attempts.get(),
        usize::try_from(MAX_TRANSPORT_RETRIES).unwrap() + 1
    );
    assert_eq!(
        clock.waits().len(),
        usize::try_from(MAX_TRANSPORT_RETRIES).unwrap()
    );
    assert!(clock.total() <= Duration::from_millis(MAX_TOTAL_BACKOFF_MS));
    assert!(matches!(
        answer,
        Err(LlmError::Overloaded(failure)) if failure.status == 529
    ));
}

#[test]
fn a_bad_request_is_returned_without_a_retry() {
    // Given: a 400. The request is wrong, so every later call is wrong the same way.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(AttemptFailure::Http(HttpFailure::new(
            400,
            "invalid_request_error".to_owned(),
        )))
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: one call, no wait, and not counted as an overload.
    assert_eq!(attempts.get(), 1);
    assert!(clock.waits().is_empty());
    assert!(matches!(
        answer,
        Err(LlmError::Api(message)) if message.contains("400")
    ));
}

#[test]
fn an_authentication_failure_is_returned_without_a_retry() {
    // Given: a rejected key — the one 4xx most likely to be mistaken for a blip,
    // because it arrives in bulk exactly like an outage does.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(AttemptFailure::Http(HttpFailure::new(
            401,
            "authentication_error".to_owned(),
        )))
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then
    assert_eq!(attempts.get(), 1);
    assert!(clock.waits().is_empty());
    assert!(matches!(answer, Err(LlmError::Api(_))));
}

#[test]
fn a_rate_limit_naming_its_own_delay_is_taken_at_its_word() {
    // Given: a 429 that says how long to wait. The provider knows when it will serve
    // again and this module only guesses, so the guess must yield.
    let asked = Duration::from_secs(7);
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            return Err(AttemptFailure::Http(HttpFailure {
                retry_after: Some(asked),
                ..HttpFailure::new(429, "rate_limit_error".to_owned())
            }));
        }
        Ok("a verdict")
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: exactly what was asked for, not the default ladder — whose first rung is
    // capped well below this and would send the call back while still limited.
    assert_eq!(answer.unwrap(), "a verdict");
    assert_eq!(clock.waits(), vec![asked]);
    assert!(asked > Duration::from_millis(MAX_BACKOFF_STEP_MS / 4));
}

#[test]
fn a_delay_longer_than_the_allowance_ends_the_call_instead_of_stalling_it() {
    // Given: a provider asking for more patience than a session is worth. Honouring
    // it would let one candidate hold a whole pass.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(AttemptFailure::Http(HttpFailure {
            retry_after: Some(Duration::from_millis(MAX_TOTAL_BACKOFF_MS + 1)),
            ..HttpFailure::new(429, "rate_limit_error".to_owned())
        }))
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: refused immediately rather than slept on, and still reported as an
    // exhausted retry rather than a terminal error.
    assert_eq!(attempts.get(), 1);
    assert!(clock.waits().is_empty());
    assert!(matches!(answer, Err(LlmError::Overloaded(_))));
}

#[test]
fn the_wait_allowance_is_spent_across_the_whole_classification() {
    // Given: two delays that each fit alone but not together. The allowance is per
    // classification, not per call, so the second must not reset it.
    let half = Duration::from_millis(MAX_TOTAL_BACKOFF_MS * 3 / 4);
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(AttemptFailure::Http(HttpFailure {
            retry_after: Some(half),
            ..HttpFailure::new(503, "overloaded_error".to_owned())
        }))
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: one wait fitted, the second did not, and the retry count never reached
    // its own bound — the time cap bound first, which is the point of having both.
    assert_eq!(clock.waits(), vec![half]);
    assert_eq!(attempts.get(), 2);
    assert!(matches!(answer, Err(LlmError::Overloaded(_))));
}

#[test]
fn the_backoff_ladder_climbs_and_stays_inside_its_step_ceiling() {
    // Given: a persistent overload, so every rung of the ladder is walked.
    let clock = Clock::new();
    let attempt = |_remaining| Err::<&str, _>(overloaded());

    // When
    let _ = retrying(attempt, clock.wait());

    // Then: every wait is a real pause and none exceeds the per-step ceiling, so a
    // long outage cannot turn one candidate into an unbounded sleep.
    let waits = clock.waits();
    assert!(!waits.is_empty());
    for wait in &waits {
        assert!(*wait > Duration::ZERO);
        assert!(*wait <= Duration::from_millis(MAX_BACKOFF_STEP_MS));
    }
    // The ladder doubles, so the last rung must outgrow the first's ceiling.
    assert!(waits[waits.len() - 1] > waits[0]);
}

#[test]
fn a_malformed_answer_is_retried_and_the_next_one_is_taken() {
    // Given: a model that answers off-schema once, then answers properly. Each
    // attempt draws a fresh sample, so the second may well fit where the first
    // did not.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            return Err(AttemptFailure::Malformed("missing field".to_owned()));
        }
        Ok("a verdict")
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: the good answer is taken, and redrawing a sample costs no wait — the
    // model is available, it simply missed.
    assert_eq!(answer.unwrap(), "a verdict");
    assert_eq!(attempts.get(), 2);
    assert!(clock.waits().is_empty());
}

#[test]
fn persistently_malformed_output_gives_up_at_the_bound() {
    // Given: a model that never fits the schema.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(AttemptFailure::Malformed(format!("bad {}", attempts.get())))
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: bounded exactly as before, and the surviving message is the last seen.
    assert_eq!(attempts.get(), usize::try_from(MAX_PARSE_ATTEMPTS).unwrap());
    assert!(matches!(answer, Err(LlmError::Parse(message)) if message == "bad 2"));
}

#[test]
fn a_failure_carrying_no_status_is_not_retried() {
    // Given: a call that failed on something with no status behind it — a transport
    // error, an empty response, the agent loop giving out. Nothing here can say
    // whether another call would differ, so nothing here spends one.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(AttemptFailure::Failed("connection reset".to_owned()))
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: one attempt, reported as what it was.
    assert_eq!(attempts.get(), 1);
    assert!(clock.waits().is_empty());
    assert!(matches!(answer, Err(LlmError::Api(message)) if message == "connection reset"));
}

#[test]
fn waiting_out_a_provider_does_not_spend_the_schema_retries() {
    // Given: an overload, then an off-schema sample, then an answer. The two bounds
    // are independent, so a slow provider must not eat the draws meant for a model
    // that missed the schema.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        match attempts.get() {
            1 => Err(overloaded()),
            2 => Err(AttemptFailure::Malformed("missing field".to_owned())),
            _ => Ok("a verdict"),
        }
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: both recoveries happened within one classification.
    assert_eq!(answer.unwrap(), "a verdict");
    assert_eq!(attempts.get(), 3);
}

#[test]
fn only_a_not_now_status_is_worth_waiting_out() {
    // Given/When/Then: the statuses this classifier actually meets. 429 and every
    // 5xx mean the provider would serve this later; the rest of 4xx mean it would
    // refuse this identically forever.
    for status in [429, 500, 502, 503, 529] {
        assert!(
            HttpFailure::new(status, String::new()).is_transient(),
            "{status} should be waited out"
        );
    }
    for status in [400, 401, 403, 404, 413, 422] {
        assert!(
            !HttpFailure::new(status, String::new()).is_transient(),
            "{status} should fail fast"
        );
    }
}

#[test]
fn a_request_that_never_answered_is_retried_rather_than_lost() {
    // Given: the production defect. A connection the provider accepted and never
    // answered blocked one classification — and with it the whole single-threaded
    // pass — for 3,077 seconds at 0.0% CPU. With a request timeout that hang becomes
    // a failure, and the only question left is whether the failure is worth another
    // call. It is: nothing about the request was refused, so the next one may be
    // served.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            return Err(timed_out());
        }
        Ok("a verdict")
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: the session keeps its classification, having paid one wait for it.
    assert_eq!(answer.unwrap(), "a verdict");
    assert_eq!(attempts.get(), 2);
    assert_eq!(clock.waits().len(), 1);
}

#[test]
fn a_provider_that_never_answers_gives_up_at_the_bound_instead_of_hanging() {
    // Given: the defect at its worst — every request times out. The point of the
    // bound is that this now *ends*. Before the timeout existed the first request
    // never returned at all, so there was no bound to reach.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(timed_out())
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: bounded in calls and in patience, exactly as a persistent overload is.
    assert_eq!(
        attempts.get(),
        usize::try_from(MAX_TRANSPORT_RETRIES).unwrap() + 1
    );
    assert_eq!(
        clock.waits().len(),
        usize::try_from(MAX_TRANSPORT_RETRIES).unwrap()
    );
    assert!(clock.total() <= Duration::from_millis(MAX_TOTAL_BACKOFF_MS));
    // And: reported as a timeout rather than as an overload. The two recover the
    // same way, but only one of them means the provider ever spoke.
    assert!(matches!(answer, Err(LlmError::Timeout(_))), "{answer:?}");
}

#[test]
fn timeouts_and_overloads_draw_on_one_allowance() {
    // Given: a provider that times out, then says 529, then serves the call. Both
    // are the same class of condition — *not now* — so they share one retry budget.
    // Giving a timeout its own counter would let a provider that alternates between
    // the two spend twice the patience a session is worth.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        match attempts.get() {
            1 => Err(timed_out()),
            2 => Err(overloaded()),
            _ => Ok("a verdict"),
        }
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: two waits off one allowance, and the verdict survives.
    assert_eq!(answer.unwrap(), "a verdict");
    assert_eq!(attempts.get(), 3);
    assert_eq!(clock.waits().len(), 2);
    assert!(clock.total() <= Duration::from_millis(MAX_TOTAL_BACKOFF_MS));
}

#[test]
fn waiting_out_a_silent_provider_does_not_spend_the_schema_retries() {
    // Given: a timeout, then an off-schema sample, then an answer. A timeout is a
    // call that never happened, so it must not consume a draw meant for a model that
    // answered and missed.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        match attempts.get() {
            1 => Err(timed_out()),
            2 => Err(AttemptFailure::Malformed("missing field".to_owned())),
            _ => Ok("a verdict"),
        }
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then
    assert_eq!(answer.unwrap(), "a verdict");
    assert_eq!(attempts.get(), 3);
}

#[test]
fn a_timeout_does_not_soften_the_statuses_that_must_fail_fast() {
    // Given: the regression the new variant could cause. A timeout is retried
    // because nothing was refused; a 4xx is refused, and adding a second transient
    // class must not blur that line.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let attempt = |_remaining| {
        attempts.set(attempts.get() + 1);
        Err::<&str, _>(AttemptFailure::Http(HttpFailure::new(
            401,
            "authentication_error".to_owned(),
        )))
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: still one call, still no wait, still terminal.
    assert_eq!(attempts.get(), 1);
    assert!(clock.waits().is_empty());
    assert!(matches!(answer, Err(LlmError::Api(_))));
}

#[test]
fn an_answer_costs_nothing_now_that_a_silence_costs_something() {
    // Given: the ordinary case, which is the one a new bound is most likely to
    // damage. A model that answers on the first call must still pay nothing.
    let clock = Clock::new();
    let attempts = Cell::new(0_usize);
    let deadline = Deadline::start();
    let attempt = |remaining: Duration| {
        // The allowance really is handed to the attempt, not merely tracked beside it:
        // an attempt that is not told its bound cannot honour it, which is how six
        // requests inside their own timeouts once outlived the classification.
        assert!(remaining > Duration::ZERO);
        attempts.set(attempts.get() + 1);
        Ok::<_, AttemptFailure>("a verdict")
    };

    // When
    let answer = retrying(attempt, clock.wait());

    // Then: one call, no wait, and the classification allowance untouched.
    assert_eq!(answer.unwrap(), "a verdict");
    assert_eq!(attempts.get(), 1);
    assert!(clock.waits().is_empty());
    assert!(deadline.remaining().is_some());
}

#[test]
fn a_fresh_classification_has_its_whole_allowance_to_spend() {
    // Given/When: a deadline as one classification starts.
    let deadline = Deadline::start();

    // Then: nearly all of it is left, and it is the documented allowance rather than
    // whatever a single request is bounded at — the two are different budgets.
    let remaining = deadline
        .remaining()
        .expect("a fresh deadline has time left");
    assert!(remaining <= Duration::from_millis(MAX_CLASSIFICATION_MS));
    assert!(remaining > Duration::from_millis(MAX_CLASSIFICATION_MS / 2));
}

#[test]
fn a_spent_classification_allowance_reports_nothing_left() {
    // Given: the multiplication this bounds. Five transport attempts of six model
    // calls each, every call sitting on the request timeout, is 36 requests of
    // patience for one session — which is why the per-request bound is not on its
    // own a per-classification bound.
    let deadline = Deadline::spent();

    // When/Then: no remaining time at all, which is what makes the next attempt
    // terminal rather than merely shorter.
    assert!(deadline.remaining().is_none());
}

#[test]
fn a_classification_allowance_shrinks_as_it_is_spent() {
    // Given: a deadline part-way through a classification. The remaining time is
    // what bounds the *next* attempt, so it has to actually decrease — a deadline
    // that always reported the full allowance would bound nothing.
    let deadline = Deadline::start();
    let first = deadline.remaining().expect("time left at the start");

    // When
    std::thread::sleep(Duration::from_millis(20));

    // Then
    let second = deadline.remaining().expect("time left after 20ms");
    assert!(second < first, "{second:?} should be less than {first:?}");
}
