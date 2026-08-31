//! The fetch layer: budget, injection filtering, and rendering, in one place.
//!
//! Both classifiers drive this. [`crate::RigClassifier`] exposes it to a real model as
//! rig tools; [`crate::MockClassifier`] drives it from a scripted brain. Because the
//! budget and the injection filter live here rather than in either caller, a test that
//! exercises the mock exercises the same guards the real model meets.
//!
//! This budget mirrors [`crate::context_provider`] deliberately. Two tool families do not earn
//! a shared abstraction; unify their mechanics when a third family makes the rule of three.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::session_detail::{MessagePage, SessionDetail, SessionDetailError, SessionOverview};

/// Fetches one classification may spend.
///
/// Four, because three is the most a classification can usefully spend and the fourth
/// is slack rather than room to loop: the tool set is two, an overview is worth asking
/// for once, and the extractors store at most five user messages, which
/// [`MAX_MESSAGES_PER_PAGE`] covers in two pages. The bound is what keeps an agentic
/// classifier from being an unbounded bill — it caps a classification at five model
/// round trips against today's one, and only sessions whose payload is thin spend
/// anything at all.
///
/// Exceeding it is a clean stop, not an error: [`FetchSession::dispatch`] answers
/// [`FetchOutcome::BudgetExhausted`], which tells the model to decide on what it has.
/// A verdict reached under a spent budget is still a verdict, and a model that cannot
/// reach one answers with low confidence, which leaves the session unassigned where it
/// registers as classification lag.
///
/// That answer is a *tool result*, though, so it stops nothing on its own — the model
/// read it and called again, and each extra call spent one of rig's own model turns
/// until the run hard-errored as `MaxTurnsError`. The real stop is
/// `WithdrawSpentTools`, which un-advertises the tools for every turn after the budget
/// is spent. This outcome remains the within-turn backstop: a single turn may emit
/// several tool calls at once, and the last of those can still cross the bound.
pub const MAX_FETCH_CALLS: usize = 4;

/// Messages one page may carry, so a page stays bounded in tokens.
pub const MAX_MESSAGES_PER_PAGE: usize = 3;

/// Decides whether a message is harness-injected text rather than human intent.
///
/// A function pointer supplied by the caller, because this crate cannot depend on
/// `tt-core` to call `tt_core::injection::is_injected` itself. It is a required
/// argument of [`SessionTools::new`] rather than an option with a default, so no
/// caller can wire up fetching and forget the filter.
pub type InjectionFilter = fn(&str) -> bool;

/// Something the classifier asked to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchRequest {
    /// The session's summary, timing and counts.
    Overview {
        /// Session to describe.
        session_id: String,
    },
    /// A slice of the session's stored user messages.
    Messages {
        /// Session to read.
        session_id: String,
        /// Index of the first message wanted.
        offset: usize,
        /// How many are wanted, capped at [`MAX_MESSAGES_PER_PAGE`].
        limit: usize,
    },
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Model-facing text for the request.
    Fetched(String),
    /// The budget is spent; the model should answer from what it has.
    ///
    /// Reached when a turn's own calls cross the bound. Between turns the tools are
    /// withdrawn instead, so a model that reads this has already been told the truth
    /// twice over.
    BudgetExhausted,
    /// The store could not answer, stated plainly rather than as a failure.
    Unavailable(String),
}

impl FetchOutcome {
    /// The text a model would see for this outcome.
    #[must_use]
    pub fn rendered(&self) -> String {
        match self {
            Self::Fetched(text) => text.clone(),
            Self::BudgetExhausted => format!(
                "No fetches remain (limit {MAX_FETCH_CALLS}); these tools are now \
                 withdrawn. Classify from what you have already seen, or answer with low \
                 confidence if you cannot identify the work."
            ),
            Self::Unavailable(reason) => format!("Unavailable: {reason}"),
        }
    }
}

/// A provider plus the injection filter that guards everything it returns.
pub struct SessionTools {
    detail: Arc<dyn SessionDetail>,
    is_injected: InjectionFilter,
}

/// The provider for a classifier that was given none.
struct NoDetail;

impl SessionDetail for NoDetail {
    fn overview(&self, _session_id: &str) -> Result<SessionOverview, SessionDetailError> {
        Err(no_provider())
    }

    fn messages(
        &self,
        _session_id: &str,
        _offset: usize,
        _limit: usize,
    ) -> Result<MessagePage, SessionDetailError> {
        Err(no_provider())
    }
}

fn no_provider() -> SessionDetailError {
    SessionDetailError::Backend("this classifier was not given session access".to_owned())
}

impl SessionTools {
    /// Binds a provider to the filter every message it returns must pass.
    #[must_use]
    pub fn new(detail: Arc<dyn SessionDetail>, is_injected: InjectionFilter) -> Arc<Self> {
        Arc::new(Self {
            detail,
            is_injected,
        })
    }

    /// Tools for a classifier wired up without a provider.
    ///
    /// Every fetch reports itself unavailable, so the classifier still runs its whole
    /// decision procedure and simply learns nothing extra. That keeps "can fetch" a
    /// single variable a test can toggle.
    #[must_use]
    pub fn unavailable() -> Arc<Self> {
        Self::new(Arc::new(NoDetail), |_| false)
    }

    /// Opens a fetch budget for one classification of `session_id`.
    ///
    /// The session under judgement is fixed here rather than asked of the model, so a
    /// classification can never read its way into a different session.
    #[must_use]
    pub fn begin(self: &Arc<Self>, session_id: &str) -> Arc<FetchSession> {
        Arc::new(FetchSession {
            tools: Arc::clone(self),
            session_id: session_id.to_owned(),
            used: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        })
    }
}

/// One classification's worth of fetching, with its own budget.
pub struct FetchSession {
    tools: Arc<SessionTools>,
    session_id: String,
    used: AtomicUsize,
    log: Mutex<Vec<FetchRequest>>,
}

impl FetchSession {
    /// The session this budget was opened for.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Serves a request, spending one call of the budget.
    ///
    /// Stops cleanly once [`MAX_FETCH_CALLS`] is reached: the provider is not consulted
    /// and no error is raised.
    /// Every outcome is logged at `debug`, and an exhausted budget at `warn`.
    ///
    /// Fetching existed for some time with no instrumentation at all, and the cost was
    /// not cosmetic: whether the model ever fetches decides whether a high
    /// `undetermined` count means thin payloads or genuine ambiguity, and that question
    /// was unanswerable from outside the process. Absence of log lines was mistaken for
    /// absence of fetching, which is a conclusion the silence could never support.
    pub fn dispatch(&self, request: &FetchRequest) -> FetchOutcome {
        let spent = self.used.fetch_add(1, Ordering::SeqCst);
        if spent >= MAX_FETCH_CALLS {
            // Not an error -- a verdict reached under a spent budget is still a verdict --
            // but worth saying, because a session that wanted more than four looks is a
            // session the fixed payload was far from describing.
            tracing::warn!(
                session_id = %self.session_id,
                budget = MAX_FETCH_CALLS,
                "fetch budget exhausted; the model must answer from what it has"
            );
            return FetchOutcome::BudgetExhausted;
        }
        tracing::debug!(
            session_id = %self.session_id,
            call = spent + 1,
            budget = MAX_FETCH_CALLS,
            request = ?request,
            "classifier fetching more of a session"
        );
        if let Ok(mut log) = self.log.lock() {
            log.push(request.clone());
        }
        match request {
            FetchRequest::Overview { session_id } => self.overview(session_id),
            FetchRequest::Messages {
                session_id,
                offset,
                limit,
            } => self.messages(session_id, *offset, *limit),
        }
    }

    fn overview(&self, session_id: &str) -> FetchOutcome {
        match self.tools.detail.overview(session_id) {
            Ok(overview) => FetchOutcome::Fetched(render_overview(&overview)),
            Err(error) => unavailable(&error),
        }
    }

    fn messages(&self, session_id: &str, offset: usize, limit: usize) -> FetchOutcome {
        let limit = limit.clamp(1, MAX_MESSAGES_PER_PAGE);
        match self.tools.detail.messages(session_id, offset, limit) {
            Ok(page) => {
                let kept: Vec<&String> = page
                    .messages
                    .iter()
                    .filter(|message| !(self.tools.is_injected)(message))
                    .collect();
                FetchOutcome::Fetched(render_messages(&kept, page.offset, page.total))
            }
            Err(error) => unavailable(&error),
        }
    }

    /// How many calls have been spent.
    #[must_use]
    pub fn calls_used(&self) -> usize {
        self.used.load(Ordering::SeqCst).min(MAX_FETCH_CALLS)
    }

    /// Requests that reached the provider, in order — each having spent a call.
    ///
    /// A request the budget refused is absent: it cost nothing and asked nothing. One
    /// the provider could not answer is present, because it did spend a call.
    #[must_use]
    pub fn log(&self) -> Vec<FetchRequest> {
        self.log.lock().map(|log| log.clone()).unwrap_or_default()
    }
}

fn unavailable(error: &SessionDetailError) -> FetchOutcome {
    FetchOutcome::Unavailable(error.to_string())
}

fn render_overview(overview: &SessionOverview) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    let summary = overview.summary.as_deref().unwrap_or(
        "(none — this source does not write summaries; judge from the messages instead)",
    );
    let _ = writeln!(text, "Summary: {summary}");
    for (label, value) in [
        ("Source", overview.source.as_deref()),
        ("Project path", overview.project_path.as_deref()),
        ("Machine", overview.machine.as_deref()),
    ] {
        let _ = writeln!(text, "{label}: {}", value.unwrap_or("(unknown)"));
    }
    for (label, value) in [
        ("Started", overview.started_at),
        ("Ended", overview.ended_at),
    ] {
        let rendered = value.map_or_else(|| "(unknown)".to_owned(), |at| at.to_rfc3339());
        let _ = writeln!(text, "{label}: {rendered}");
    }
    let _ = writeln!(
        text,
        "Messages: {} ({} from the assistant); tool calls: {}",
        overview.message_count, overview.assistant_message_count, overview.tool_call_count
    );
    text
}

fn render_messages(messages: &[&String], offset: usize, total: usize) -> String {
    use std::fmt::Write as _;

    if messages.is_empty() {
        return format!(
            "No human messages in this range (offset {offset}, {total} stored in total)."
        );
    }
    let mut text = String::new();
    let _ = writeln!(
        text,
        "User messages {offset}..{} of {total}:",
        offset + messages.len()
    );
    for (index, message) in messages.iter().enumerate() {
        let _ = writeln!(text, "[{}] {message}", offset + index);
    }
    text
}

#[cfg(test)]
mod tests;
