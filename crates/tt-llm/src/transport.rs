//! Waiting out a provider that refused, or never answered, a call it would serve in a
//! moment.
//!
//! # Why this exists now and did not before
//!
//! Retrying a failed *call* used to be impossible to do responsibly: [`LlmError::Api`]
//! carried a string, so nothing could tell a 529 overload from a 400 bad request, and
//! retrying blindly multiplies the bill without changing the answer. rig exposes the
//! status structurally — `provider_response_status` walks its own error chain and hands
//! back an `http::StatusCode` — so the distinction is now a number rather than prose,
//! and the retry can be confined to the statuses that mean *not now*.
//!
//! # Silence is a refusal too, and it used to be unbounded
//!
//! A provider that answers nothing at all says *not now* just as plainly as one that
//! answers 529, and for a long time this module could not hear it. Nothing configured a
//! request timeout, and reqwest's default is none, so a connection Anthropic accepted
//! and never answered blocked its classification forever — and with it the whole
//! single-threaded pass. The live daemon was caught doing exactly that: 3,077 seconds
//! alive at 0.0% CPU on one open socket, 3 sessions classified in 20 minutes against a
//! measured 3.9, which put the remaining 3,687 sessions ~410 hours out.
//!
//! [`crate::RigClassifier`] now bounds each request, so that hang arrives here as
//! [`AttemptFailure::Timeout`] and is waited out on the same allowance as a 529. It is
//! held apart from [`AttemptFailure::Http`] rather than given a synthetic status because
//! the provider set no status: inventing a 504 it never sent is the prose-matching sin
//! in reverse, and [`LlmError::Timeout`] is what tells an operator the provider never
//! spoke at all.
//!
//! # The three axes are separate on purpose
//!
//! [`retrying`] runs one loop over three independent bounds, because the failures are
//! not the same kind of thing. An off-schema sample is an *answer* that missed; another
//! draw may fit, and [`MAX_PARSE_ATTEMPTS`] bounds how many draws are worth paying for.
//! A transient status or a timeout is a call that never landed; the only cure is time,
//! so it is bounded by [`MAX_TRANSPORT_RETRIES`] *and* by [`MAX_TOTAL_BACKOFF_MS`].
//! Sharing one counter between those two would let a slow provider eat the schema
//! retries, or an off-schema model eat the patience meant for an overloaded one.
//!
//! The third bound is wall clock, and it exists because the first two multiply. A
//! per-*request* timeout is not a per-*classification* budget: an agentic attempt is up
//! to ten requests when both tool families are enabled, and a classification is up to five
//! attempts, so a 120-second request bound alone permits 5 × 10 × 120 s ≈ 100 minutes on one
//! session — and 200 of those is a 333-hour pass. [`Deadline`] caps the whole classification
//! at [`MAX_CLASSIFICATION_MS`] instead: every attempt is told what is left and may not outlive
//! it, and an attempt is never started with nothing left. The arithmetic that picks the number
//! is on [`MAX_CLASSIFICATION_MS`].
//!
//! # The bounds are per classification, not per attempt
//!
//! The fetch budget is deliberately per *attempt* (a retry is a fresh conversation that
//! remembers nothing). Waiting is the opposite: the point of the caps is that no single
//! session can stall a pass, so both the wait allowance and the wall-clock allowance are
//! spent across the whole classification and never reset.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::LlmError;

/// Samples one model call may draw before its output is called unusable.
///
/// Two, because the failure this recovers from is a single off-schema sample: each
/// attempt is a fresh draw, so a second one usually fits where the first did not, and
/// a third buys little against a model that is misreading the schema outright.
pub const MAX_PARSE_ATTEMPTS: u32 = 2;

/// Calls one classification may retry after a transient refusal.
///
/// Four, so a classification makes at most five transport attempts. Anthropic's 529
/// clears on a timescale of seconds, and the nominal ladder below spans that; a fifth
/// retry would only lengthen the tail on an outage the daemon's own classifier backoff
/// already handles at the loop level.
pub const MAX_TRANSPORT_RETRIES: u32 = 4;

/// Wait before the first retry. Each later retry doubles it.
const BASE_BACKOFF_MS: u64 = 1_000;

/// Ceiling on any single wait, so doubling cannot run away.
const MAX_BACKOFF_STEP_MS: u64 = 8_000;

/// Ceiling on everything one classification may wait, summed.
///
/// Twenty seconds against a baseline session that takes roughly ninety, and against the
/// alternative of losing that session's classification outright. It also bounds the bad
/// case: a wholly unreachable provider adds at most this per candidate, so a pass of
/// `SESSIONS_PER_PASS` still terminates instead of blocking on a stalled provider.
pub const MAX_TOTAL_BACKOFF_MS: u64 = 20_000;

/// Ceiling on the wall clock one classification may consume, waits included.
///
/// Five minutes, and the number is chosen against three measurements rather than taste.
/// A healthy drain ran at 3.9 classifications a minute — about 15 s each. The slowest
/// rate ever observed *while still completing* was 0.65 a minute, about 92 s. So 300 s
/// is 3.3x the slowest classification the provider has actually served, and it still
/// accommodates three consecutive attempts at that slowest observed rate; nothing the
/// provider is genuinely serving gets cut short.
///
/// What it does cut short is the multiplication. The per-request timeout bounds one
/// request, and a classification is up to ten requests per attempt when both tool families
/// are enabled, across up to `1 + MAX_TRANSPORT_RETRIES` attempts:
///
/// | bound | worst case per classification | worst case per 200-session pass |
/// |---|---|---|
/// | request timeout alone | 5 × 10 × 120 s ≈ 100 min | ≈ 333 h |
/// | plus this ceiling | 300 s | ≈ 16.7 h |
/// | before either | unbounded (3,077 s and counting, measured) | never terminates |
///
/// A classification cut off here leaves its session unassigned, which is exactly where a
/// failed classification already leaves it: still a candidate, re-reached next pass. So
/// abandoning one costs a session, while blocking on it costs the pass — which is why
/// the ceiling is deliberately a backstop against pathology and not a service target.
pub const MAX_CLASSIFICATION_MS: u64 = 300_000;

/// A model call the provider answered with an HTTP status.
///
/// The status is the whole reason this type exists: it is what separates a condition
/// another call could clear from one that will fail identically forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFailure {
    /// Status the provider answered with.
    pub status: u16,
    /// Wait the provider explicitly asked for.
    ///
    /// Always `None` from [`crate::RigClassifier`] today: rig 0.40 keeps a failed
    /// response's status and body but drops its headers, so `Retry-After` never
    /// survives to this layer. Carried anyway because it is the provider's own answer
    /// to the question this module otherwise guesses at, and the retry budget prefers
    /// it to the guess whenever a source for it exists.
    pub retry_after: Option<Duration>,
    /// What the provider said, kept for the operator reading the log.
    pub message: String,
}

impl HttpFailure {
    /// A failure carrying only what rig structurally exposes.
    ///
    /// `retry_after` is left unset because rig 0.40 has no header to give it; a caller
    /// with a real `Retry-After` sets the field directly.
    #[must_use]
    pub const fn new(status: u16, message: String) -> Self {
        Self {
            status,
            retry_after: None,
            message,
        }
    }

    /// Whether another call could plausibly answer differently.
    ///
    /// 429 and every 5xx are the provider saying *not now* — a rate limit, a bad
    /// gateway, or Anthropic's 529 overload. Every other 4xx is the provider saying
    /// *not this*: a malformed request, a rejected key, a spent quota. Retrying those
    /// cannot change the answer and only multiplies the bill, so the split is drawn
    /// here once and nowhere else.
    pub const fn is_transient(&self) -> bool {
        self.status == 429 || (self.status >= 500 && self.status < 600)
    }
}

impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status, self.message)
    }
}

/// One failed attempt, sorted into the classes retry can act on.
///
/// Four, because there are three cures and two ways to earn the middle one: draw another
/// sample, wait, or stop — and waiting is warranted both when the provider refused
/// retryably and when it said nothing at all.
#[derive(Debug)]
pub enum AttemptFailure {
    /// The model's output did not fit the schema. Another sample may.
    Malformed(String),
    /// The provider answered with an HTTP status. Whether that is worth waiting out
    /// is [`HttpFailure::is_transient`]'s call and nothing else's, so this variant
    /// carries every status rig recovered rather than pre-judging it here.
    Http(HttpFailure),
    /// The request was abandoned because the provider never answered it.
    ///
    /// Always worth another call, and for a stronger reason than a 5xx: a 5xx is the
    /// provider declining, whereas this is the provider not having spoken, so nothing
    /// about the request has been refused. Kept out of [`Self::Http`] because there is
    /// no status to put there — a synthetic 504 would be this crate inventing provider
    /// evidence, which is the failure the typed status exists to remove.
    Timeout(String),
    /// The call failed on something no retry can change.
    Failed(String),
}

/// How much wall clock one classification has left.
///
/// The other two bounds count events — samples drawn, retries taken — and events are the
/// wrong unit for a provider that is slow rather than wrong. Five attempts of six
/// requests each are all within their event bounds and can still take over an hour if
/// every request sits near the request timeout.
///
/// Handed to every attempt so the attempt can bound *itself*: checking only between
/// attempts would leave one attempt free to outlive the whole allowance, which is the
/// same mistake as bounding requests but not classifications.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    expires_at: Instant,
}

impl Deadline {
    /// Opens an allowance for one classification.
    #[must_use]
    pub fn start() -> Self {
        Self {
            expires_at: Instant::now() + Duration::from_millis(MAX_CLASSIFICATION_MS),
        }
    }

    /// What is left, or `None` once the allowance is spent.
    ///
    /// Zero reads as spent rather than as a zero-length attempt: an attempt granted no
    /// time cannot answer, and would burn a model call to discover that.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        let left = self.expires_at.checked_duration_since(Instant::now())?;
        (!left.is_zero()).then_some(left)
    }

    /// An allowance with nothing left, for holding the spent branch against.
    #[cfg(test)]
    fn spent() -> Self {
        Self {
            expires_at: Instant::now(),
        }
    }
}

/// What one classification has left to spend on waiting.
struct TransportBudget {
    retries: u32,
    waited_ms: u64,
}

impl TransportBudget {
    const fn new() -> Self {
        Self {
            retries: 0,
            waited_ms: 0,
        }
    }

    /// How long to wait before trying again, or `None` once the allowance is spent.
    ///
    /// A `Retry-After` is taken verbatim rather than jittered — the provider named a
    /// time, and second-guessing it is how a thundering herd re-forms. It is still
    /// refused when it would not fit the remaining allowance, because a provider
    /// asking for longer than a session is worth is exactly the stall this bounds.
    fn next_wait(&mut self, asked: Option<Duration>) -> Option<Duration> {
        if self.retries >= MAX_TRANSPORT_RETRIES {
            return None;
        }
        let remaining_ms = MAX_TOTAL_BACKOFF_MS.saturating_sub(self.waited_ms);
        let wait_ms = asked.map_or_else(|| jittered_backoff_ms(self.retries), duration_as_ms);
        if wait_ms > remaining_ms {
            return None;
        }
        self.retries += 1;
        self.waited_ms += wait_ms;
        Some(Duration::from_millis(wait_ms))
    }
}

fn duration_as_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The nominal wait before retry number `retry`, doubling and then capped.
fn backoff_step_ms(retry: u32) -> u64 {
    let doubling = 1_u64.checked_shl(retry).unwrap_or(u64::MAX);
    BASE_BACKOFF_MS
        .saturating_mul(doubling)
        .min(MAX_BACKOFF_STEP_MS)
}

/// Equal jitter: half the step, plus a random share of the other half.
///
/// Half rather than the whole, so a retry still backs off meaningfully; jittered at all,
/// so a pass that met one outage does not send every queued session back in lockstep.
fn jittered_backoff_ms(retry: u32) -> u64 {
    let half = backoff_step_ms(retry) / 2;
    half + coarse_entropy() % (half + 1)
}

/// A coarse random source for spreading retries.
///
/// The clock's nanosecond field, because a `rand` dependency would be a lot of supply
/// chain for scattering a handful of sleeps, and nothing here is security-sensitive.
fn coarse_entropy() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::from(since.subsec_nanos()))
}

/// Runs `attempt` until it answers, redrawing off-schema output and waiting out a
/// provider that refused retryably or answered nothing at all.
///
/// Each attempt is handed the wall clock its classification has left, because an attempt
/// is the only thing that can bound itself: this loop can refuse to *start* one past the
/// allowance, but only the attempt can stop halfway through.
///
/// `wait` is injected so a test can prove the schedule without sleeping through it;
/// production passes [`std::thread::sleep`].
///
/// Shared by both classification paths on purpose. The agentic path was once added
/// without the schema retry the extraction path already had, which is how parse
/// failures accumulated into the hundreds before anyone could see them.
pub fn retrying<T>(
    mut attempt: impl FnMut(Duration) -> Result<T, AttemptFailure>,
    mut wait: impl FnMut(Duration),
) -> Result<T, LlmError> {
    let mut samples = 0_u32;
    let mut budget = TransportBudget::new();
    let deadline = Deadline::start();
    loop {
        // Checked before the attempt rather than after: a call that cannot finish
        // inside the allowance is a call not worth paying for.
        let Some(remaining) = deadline.remaining() else {
            return Err(LlmError::Timeout(format!(
                "classification exceeded its {MAX_CLASSIFICATION_MS}ms allowance"
            )));
        };
        match attempt(remaining) {
            Ok(answer) => return Ok(answer),
            Err(AttemptFailure::Malformed(error)) => {
                samples += 1;
                // The last complaint is the one worth reporting: it is the freshest
                // evidence of how the model is misreading the schema.
                if samples >= MAX_PARSE_ATTEMPTS {
                    return Err(LlmError::Parse(error));
                }
            }
            Err(AttemptFailure::Http(failure)) => {
                if !failure.is_transient() {
                    return Err(LlmError::Api(failure.to_string()));
                }
                match budget.next_wait(failure.retry_after) {
                    Some(pause) => wait(pause),
                    None => return Err(LlmError::Overloaded(failure)),
                }
            }
            Err(AttemptFailure::Timeout(error)) => {
                // No `retry_after` to consult: a provider that said nothing said
                // nothing about when to come back, so the ladder decides.
                match budget.next_wait(None) {
                    Some(pause) => wait(pause),
                    None => return Err(LlmError::Timeout(error)),
                }
            }
            Err(AttemptFailure::Failed(error)) => return Err(LlmError::Api(error)),
        }
    }
}

#[cfg(test)]
mod tests;
