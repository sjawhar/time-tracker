//! Optional operator-provided knowledge lookup for classification.
//!
//! `tt-llm` owns the agentic budget and model-facing rendering, but it does not know where
//! knowledge resides. [`ContextProvider`] is deliberately small and read-only; `tt-cli`
//! supplies a command-backed implementation while tests supply literals. The provider is
//! optional, so an unwired classifier keeps its previous prompt and tool set exactly.
//!
//! Context lookup has its own allowance rather than sharing [`crate::MAX_FETCH_CALLS`].
//! Session detail and external knowledge answer different questions, and sharing a counter
//! lets a thin session consume the calls needed to resolve a customer or project codename.
//! The limit is enforced in the same two places as session fetching:
//! [`ContextLookupSession`] returns [`ContextLookupOutcome::BudgetExhausted`] to stop calls
//! packed into one model turn, and `RigClassifier` withdraws the spent tool before its next
//! turn.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Why a context lookup could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum ContextProviderError {
    /// The configured provider could not complete the lookup.
    #[error("context lookup failed: {0}")]
    Backend(String),
}

/// Read-only access to knowledge that grounds a stream decision.
///
/// Sync on purpose. [`crate::Classifier`] is sync and [`crate::RigClassifier`] drives rig on
/// its own runtime, so an async method here would force every caller async for no gain.
pub trait ContextProvider: Send + Sync {
    /// Resolves an unfamiliar name, organization, project, codename, or relationship.
    ///
    /// # Errors
    /// When the provider cannot complete the lookup.
    fn lookup(&self, query: &str) -> Result<String, ContextProviderError>;
}

/// Context lookups one classification may spend.
///
/// Four, matching the session-fetch budget but tracked independently. It gives an agent room
/// to resolve an unfamiliar term without letting a single ambiguous classification turn into
/// an unbounded sequence of external lookups.
pub const MAX_CONTEXT_LOOKUP_CALLS: usize = 4;

/// A question the classifier asks the configured knowledge provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextLookupRequest {
    /// The unfamiliar term or question to resolve.
    pub query: String,
}

/// What came back from a context lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextLookupOutcome {
    /// Model-facing text from the provider.
    Fetched(String),
    /// The budget is spent; the model should decide from the information it has.
    BudgetExhausted,
    /// The provider could not answer, stated as model-facing text rather than an error.
    Unavailable(String),
}

impl ContextLookupOutcome {
    /// The text a model sees for this result.
    #[must_use]
    pub fn rendered(&self) -> String {
        match self {
            Self::Fetched(text) => text.clone(),
            Self::BudgetExhausted => format!(
                "No context lookups remain (limit {MAX_CONTEXT_LOOKUP_CALLS}); this tool is \
                 now withdrawn. Classify from what you have already seen, or answer with low \
                 confidence if you cannot identify the work."
            ),
            Self::Unavailable(reason) => format!("Unavailable: {reason}"),
        }
    }
}

/// A knowledge provider, ready to open one classification's budget.
pub struct ContextProviderTools {
    provider: Arc<dyn ContextProvider>,
}

impl ContextProviderTools {
    /// Binds a provider to the classifier's context-lookup tool.
    #[must_use]
    pub fn new(provider: Arc<dyn ContextProvider>) -> Arc<Self> {
        Arc::new(Self { provider })
    }

    /// Opens an independent context-lookup budget for one classification.
    #[must_use]
    pub fn begin(self: &Arc<Self>) -> Arc<ContextLookupSession> {
        Arc::new(ContextLookupSession {
            tools: Arc::clone(self),
            used: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        })
    }
}

/// One classification's worth of context lookups, with its own budget.
pub struct ContextLookupSession {
    tools: Arc<ContextProviderTools>,
    used: AtomicUsize,
    log: Mutex<Vec<ContextLookupRequest>>,
}

impl ContextLookupSession {
    /// Serves one lookup request, spending one call of the context-lookup budget.
    ///
    /// A provider failure remains a tool result: the classification can still reach a verdict
    /// from its payload, session detail, and any knowledge it already read.
    pub fn dispatch(&self, request: &ContextLookupRequest) -> ContextLookupOutcome {
        let spent = self.used.fetch_add(1, Ordering::SeqCst);
        if spent >= MAX_CONTEXT_LOOKUP_CALLS {
            tracing::warn!(
                budget = MAX_CONTEXT_LOOKUP_CALLS,
                "context-lookup budget exhausted; the model must answer from what it has"
            );
            return ContextLookupOutcome::BudgetExhausted;
        }
        tracing::debug!(
            call = spent + 1,
            budget = MAX_CONTEXT_LOOKUP_CALLS,
            request = ?request,
            "classifier performing a context lookup"
        );
        if let Ok(mut log) = self.log.lock() {
            log.push(request.clone());
        }
        self.tools.provider.lookup(&request.query).map_or_else(
            |error| ContextLookupOutcome::Unavailable(error.to_string()),
            ContextLookupOutcome::Fetched,
        )
    }

    /// How many context lookup calls have been spent.
    #[must_use]
    pub fn calls_used(&self) -> usize {
        self.used
            .load(Ordering::SeqCst)
            .min(MAX_CONTEXT_LOOKUP_CALLS)
    }

    /// Requests that reached the provider, in order.
    #[must_use]
    pub fn log(&self) -> Vec<ContextLookupRequest> {
        self.log.lock().map(|log| log.clone()).unwrap_or_default()
    }
}
