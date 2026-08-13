//! LLM-backed stream classification.
//!
//! # Sync facade, async engine
//!
//! [`Classifier`] is sync. [`RigClassifier`] drives async rig-core on a runtime it owns
//! and blocks, so nothing upstream has to become async — `tt-cli` and `tt-db` are sync
//! because rusqlite is.
//!
//! # Agentic classification
//!
//! A classification is not one shot at a fixed payload. The model may fetch more of the
//! session while it decides, because choosing in advance how much of a session is enough
//! is what produced bad verdicts on ambiguous input: 601 unclassified sessions share the
//! byte-identical prompt `The following tool was executed by the user`, and 566 distinct
//! summaries between them. From the payload alone those 601 are one string.
//!
//! Three pieces make that work while keeping this crate free of internal dependencies
//! (see the dependency graph in the root `AGENTS.md`):
//!
//! - [`SessionDetail`] declares *what* may be fetched. This crate cannot read the
//!   database, so `tt-cli` implements the trait over `tt_db::Database`.
//! - [`SessionTools`] binds a provider to an [`InjectionFilter`], and [`FetchSession`]
//!   is one classification's budget. Every fetch passes through both, so neither the
//!   live model nor a test can route around them.
//! - The rig tools are a thin adapter over that session, which is why [`MockClassifier`]
//!   — driving the same [`FetchSession`] from a scripted brain — exercises the same
//!   budget and the same filter with no network.
//!
//! Fetching is bounded ([`MAX_FETCH_CALLS`]), filtered, and optional: a payload that
//! already names the work costs exactly what it cost before.
//!
//! The bound is enforced twice, and the second half was missing at first. Inside a turn,
//! a call past the budget answers [`FetchOutcome::BudgetExhausted`]. Between turns, the
//! tools are withdrawn from the request entirely. Only the first existed to begin with,
//! and it is a tool *result* rather than a stop: the model read it, called again anyway,
//! and each extra call spent one of rig's `max_turns` until the run hard-errored as
//! `MaxTurnsError`. On a live daemon that read `classifier_last_error: PromptError:
//! MaxTurnsError: reached max turns` with four consecutive failures behind it, and the
//! backoff those failures armed had cut the drain from 1.67 to 0.17 classifications a
//! minute against a 3,900-session backlog.
//!
//! # The roster is a selection, and reuse is the default
//!
//! Every classification carries a list of streams the model may choose from. That list
//! used to be the whole `streams` table, one labelled line each, which is a feedback loop:
//! each stream created makes the next reuse target harder to find, and a model that cannot
//! find it creates a neighbour instead. It reached **1,018 streams / 329 KB per prompt**,
//! growing ~101 streams an hour — about one per session — with families like
//! `agent-c: eval-3 <app> environment (eval-3 integration)` taking a row per application.
//!
//! [`prompt`] now selects: [`ROSTER_LIMIT`] streams, ordered by how close each stream's
//! activity falls to [`ClassificationInput::started_at`], rendered compactly. 33 KB, and it
//! stops growing with the table. The measurements, the cap's justification, and why
//! proximity is measured against the *session* rather than against now all live in that
//! module's docs.
//!
//! Two things make the cap safe to have, and they are not in this crate:
//!
//! - `tt_db::find_stream_by_normalized_name` turns a name the model proposes for a stream
//!   it was never shown into **reuse of that row**. So a cap can only ever cost a
//!   *semantically* near-duplicate, never an exact one.
//! - The prompt states reuse as the default and creation as the exception, and says what
//!   granularity a stream has: one initiative spanning many sessions, not one row per task
//!   instance.
//!
//! Selecting what the model is *shown* is presentation. Which stream the work belongs to
//! stays the model's judgement — see `tt-core`'s `AGENTS.md`, "Streams are semantic".
//!
//! # What counts as an answer
//!
//! [`StreamChoice`] has five shapes, and the last three are the easy ones to conflate.
//! `Existing` and `New` place the work. `Throwaway` asserts there is no work to place.
//! `Undetermined` asserts nothing at all: the model was reached, it answered, and its
//! answer was that it could not identify the work — which is what the `prompt` module
//! asks for, in preference to inventing a container. `TurnsExhausted` is not an answer
//! of any kind: the model spent every call it was allowed and never delivered one.
//!
//! That last one is a verdict, not a failure, and it used to be treated as one. An
//! all-null extract was rejected as [`LlmError::Parse`], so every obedient decline
//! counted as a failed call; `classifier_consecutive_failures` reached 53 and the
//! daemon's exponential backoff throttled its classify loop to silence against a
//! backlog of thousands. What stays an error is output that never deserialized — there
//! is no answer to read, and `transport` redraws it because the next sample may fit.
//!
//! `Undetermined` must not be folded into `Throwaway`. Throwaway routes a session to
//! the junk stream; a declined session is left unassigned, where it reads as
//! classification lag and stays reachable by a later pass. Junking it would file real
//! work as nothing.
//!
//! `TurnsExhausted` rests in that same unassigned place and must not be folded into
//! `Undetermined` either, because the two name different things to fix. A decline asks
//! for nothing — the prompt worked on input it could not place. A spent turn budget says
//! the agentic loop did not converge inside its bound, which is a bound or a prompt to
//! look at, and `tt-cli` tallies it separately so the pass summary keeps saying which
//! happened. What both share is that no call failed, so neither may reach the error
//! tally or the failure backoff.
//!
//! # Retrying, and the line it does not cross
//!
//! Three failures get another call, for three different reasons, and everything else gets
//! none. An off-schema sample is retried because the next draw may fit. A 429 or a 5xx
//! — Anthropic's 529 overload above all — is retried because the provider said *not
//! now*, and a live drain running at 0.65 sessions a minute was losing a session's
//! classification outright every time it heard that. A request the provider never
//! answered is retried because nothing about it was refused.
//!
//! Retry used to be refused wholesale, and the refusal was right at the time: the error
//! carried the provider's detail as a string, so nothing could tell an overload from a
//! bad request, and retrying blindly multiplies the bill without changing the answer.
//! What changed is the evidence, not the appetite. rig exposes the status structurally
//! through `provider_response_status`, so [`HttpFailure`] carries the HTTP status as
//! a number and [`HttpFailure::is_transient`] draws the line once: 429 and 5xx are worth
//! waiting out, every other 4xx is the provider refusing this request rather than this
//! moment, and a failure carrying no status at all is not guessed at.
//!
//! # Nothing may take forever, and everything here once could
//!
//! The third retryable failure is the newest, and until it existed it was not a failure
//! at all. `anthropic::Client::new` takes rig's default HTTP backend, `reqwest` defaults
//! to no request timeout, and this crate configured none — so a connection the provider
//! accepted and never answered blocked its classification *indefinitely*, and because a
//! pass classifies serially, blocked the pass with it. The live daemon was measured 3,077
//! seconds alive at 0.0% CPU, 40 MB RSS, one open TCP connection, 3 sessions classified
//! in 20 minutes against a healthy 3.9 a minute — which put the remaining 3,687 sessions
//! roughly 410 hours out.
//!
//! Three bounds now nest, and each answers a question the one inside it cannot:
//!
//! | bound | scope | value |
//! |---|---|---|
//! | `REQUEST_TIMEOUT` | one HTTP request | 120 s |
//! | `MAX_MODEL_TURNS` | requests per attempt | 6 |
//! | `transport::MAX_CLASSIFICATION_MS` | wall clock per classification | 300 s |
//!
//! The middle row is why the outer one is needed: bounding a request does not bound a
//! classification, because an agentic attempt is many requests and a classification is
//! many attempts. The full arithmetic, and the measurements each number is chosen
//! against, are on those constants.
//!
//! All three bounds are stated in the `transport` module and `rig_classifier`, and the two
//! outer ones are per classification, so a stalled provider costs a bounded amount of
//! patience and a bounded amount of wall clock, and never blocks a pass.

/// Which classifier authored a verdict, so a queued one can be told from a fresh one.
///
/// A weak verdict lands in the review queue as a proposal, and a reviewer may never come
/// — so the queue must not stop the classifier re-answering. It also must not make it
/// pay to re-ask a question it has already answered identically. Both are true at once,
/// and this number is what tells them apart: a proposal carries the generation that most
/// recently answered it, and `tt-cli` spends no model call re-asking a window run whose
/// queued answer already carries **this** one.
///
/// # Bump this whenever the classifier materially changes
///
/// The prompt, the roster construction, the model, or the fetch behaviour — any of those
/// can turn a verdict this classifier could not reach into one it can, and bumping is how
/// every earlier refusal gets asked exactly once more. Not bumping after such a change is
/// the failure this constant exists to prevent: 731 proposals authored before the roster
/// was cut from 1,018 streams to 200 ordered by proximity sat in the queue blocking all
/// 37 August candidate sessions and 157 of 185 July ones, and replaying the improved
/// classifier over 17 of them placed 13 confidently. The classifier had got better and
/// nothing said so.
///
/// Bumping costs one extra pass over the queued questions and nothing after that: a
/// re-asked proposal is stamped with the new generation whether or not the answer
/// improved. Cosmetic edits — a reworded log line, a refactor with no behavioural
/// difference — do not warrant one.
///
/// Generations start at 1. Zero is not a generation; it would read as "unstamped", which
/// is what `NULL` already means for the rows written before this existed.
///
/// ## 1 -> 2: the roster stopped showing the model its own duplicates
///
/// Two changes to roster construction, either of which alone would warrant this. Name
/// resolution gained a near-miss fallback, so a name the model proposes for a stream the
/// capped roster never showed it now reuses that row instead of minting a neighbour. And
/// 291 duplicate streams were collapsed on the live table, which changes what the 200
/// rows a prompt actually carries *are*: a run whose work belonged to `dpi: hosted-task
/// lambda` was previously offered up to sixteen rows spelling that one initiative, and
/// `agent-c: WO-005 environment typing` existed as 189. Confidence split across
/// near-identical candidates is exactly the shape that lands under the 0.80 threshold
/// while the model is not actually confused about the work, so the queued refusals are
/// worth asking once more against a roster that names each initiative once.
/// ## 2 -> 3: a window run stopped being told its missing session was a broken system
///
/// A material change to fetch behaviour, which is one of the four things this constant
/// tracks. A window run is a set of focus events with no session at all, yet it was handed
/// the same preamble ("You may look further into the session") and the same
/// `session_overview` / `session_messages` tools as a real session, against a synthetic
/// `window:<event_id>`. The model called them and was answered `session window:<id> is not
/// indexed` -- a message about system health, not about the scope, and one it paid fetch
/// calls to receive. Live, that reached **161 of 518 pending proposals, every one of them
/// window-scoped**, and those average **0.491 confidence against 0.597** for the rest of
/// the queue.
///
/// So every queued window-run refusal was authored by a classifier that had been told a
/// falsehood about its own inputs, which is exactly the case this constant exists to
/// re-open: the tools are now withheld for a sessionless scope, and each of those
/// questions is asked once more without the lie.
pub const CLASSIFIER_GENERATION: u32 = 3;

mod fetch;
mod prompt;
mod rig_classifier;
mod rig_tools;
mod session_detail;
mod transport;
mod types;

pub use fetch::{
    FetchOutcome, FetchRequest, FetchSession, InjectionFilter, MAX_FETCH_CALLS,
    MAX_MESSAGES_PER_PAGE, SessionTools,
};
pub use prompt::{ROSTER_DESCRIPTION_BUDGET, ROSTER_LIMIT};
pub use rig_classifier::{DEFAULT_API_KEY_ENV, RigClassifier};
pub use session_detail::{MessagePage, SessionDetail, SessionDetailError, SessionOverview};
pub use transport::HttpFailure;
pub use types::{
    ClassificationExtract, ClassificationInput, ClassificationOutput, Classifier, LlmError,
    MockBrain, MockClassifier, StreamChoice, StreamSummary,
};

#[cfg(test)]
mod tests;
