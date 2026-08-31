use std::sync::Arc;
use std::time::Duration;

use rig_core::agent::{AgentHook, Flow, HookContext, RequestPatch, StepEvent, StepEventKind};
use rig_core::client::CompletionClient;
use rig_core::completion::CompletionModel;
use rig_core::completion::request::{PromptError, StructuredOutputError, TypedPrompt};
use rig_core::extractor::ExtractionError;
use rig_core::providers::anthropic;
use rig_core::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::context_provider::{
    ContextLookupSession, ContextProviderTools, MAX_CONTEXT_LOOKUP_CALLS,
};
use crate::fetch::{FetchSession, MAX_FETCH_CALLS, SessionTools};
use crate::rig_tools::{
    ContextLookupTool, MessagesTool, OverviewTool, preamble, preamble_with_context_lookup,
};
use crate::transport::{AttemptFailure, HttpFailure, MAX_CLASSIFICATION_MS, retrying};
use crate::{
    ClassificationExtract, ClassificationInput, ClassificationOutput, Classifier, LlmError,
    StreamChoice, StreamSummary, prompt,
};

pub const DEFAULT_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Wall clock one HTTP request to the provider may take before it is abandoned.
///
/// Two minutes, and the reason a number is needed at all is that there was none: rig's
/// `anthropic::Client::new` takes `reqwest::Client::default()`, whose timeout is `None`,
/// so a connection the provider accepted and never answered blocked its classification
/// forever. Because a pass classifies serially, that one socket stalled everything: the
/// live daemon was measured 3,077 seconds alive at 0.0% CPU, one open TCP connection, 3
/// sessions classified in 20 minutes against a healthy 3.9 a minute.
///
/// A bound too tight is worse than none, because it would abandon completions the
/// provider is genuinely serving — so the number is set above the slowest real work ever
/// observed rather than near the typical. The slowest *whole* classification seen to
/// complete ran about 92 s (0.65 a minute), and a single request cannot exceed the
/// classification containing it, so 120 s sits above every request that has ever been
/// served here while still cutting the measured 3,077-second hang by 25x.
///
/// This bounds one **request**, not one classification. An agentic attempt makes up to
/// [`MAX_MODEL_TURNS`] requests and a classification may retry, so the product needs its
/// own bound — [`crate::transport::MAX_CLASSIFICATION_MS`], which carries the arithmetic.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Model calls one classification *attempt* may make.
///
/// Ten: one turn for every session fetch and context lookup the two independent budgets
/// allow, one to answer with, and one spare. A turn ends either by calling tools — which
/// spends at least one call from one budget, since Anthropic may pack several calls into a
/// turn — or by answering, which ends the run. So the longest honest attempt consumes both
/// allowances and then answers.
///
/// A tool budget's exhausted response is text, not a stop: a model can read it and call an
/// advertised tool again. [`WithdrawSpentTools`] removes each spent tool family from the next
/// request instead, preserving the spare for rig's own turn accounting rather than buying a
/// non-answer with more model calls.
///
/// Per attempt, not per classification: a retry is a fresh conversation with fresh session
/// and context budgets, so the bounds in [`crate::transport`] decide how many of these a
/// single classification can cost.
const MAX_MODEL_TURNS: usize = MAX_FETCH_CALLS + MAX_CONTEXT_LOOKUP_CALLS + 2;

/// The turn bound has to leave room for both whole tool budgets and the turn that answers.
///
/// A compile error rather than a test, because it is a property of constants and not of any
/// run: below this a model that spends both budgets can never speak, and the only thing it
/// could then produce is the `MaxTurnsError` all of the above exists to prevent.
const _: () = assert!(MAX_MODEL_TURNS > MAX_FETCH_CALLS + MAX_CONTEXT_LOOKUP_CALLS);

/// Withdraws every spent tool family from the next model turn.
///
/// A budget's own stop is a *tool result*, not a stop: the model can read
/// [`crate::FetchOutcome::BudgetExhausted`] or [`crate::ContextOutcome::BudgetExhausted`],
/// see the same tool still advertised, and call it again. rig resolves the advertised tool
/// set per turn from a [`StepEvent::CompletionCall`] hook's [`RequestPatch`], so removing
/// names makes the budget a property of the request rather than advice the model may decline.
///
/// The in-process guards remain necessary: one turn can emit several tool calls, and the
/// final one can cross a budget before the following turn sees this hook. No `tool_choice`
/// is patched alongside the allow-list; the agent sets none, so an empty list advertises
/// nothing without making rig fail a request closed against the tools it withdraws.
struct WithdrawSpentTools {
    session: Option<Arc<FetchSession>>,
    context_lookup: Option<Arc<ContextLookupSession>>,
}

impl WithdrawSpentTools {
    const fn new(
        session: Option<Arc<FetchSession>>,
        context_lookup: Option<Arc<ContextLookupSession>>,
    ) -> Self {
        Self {
            session,
            context_lookup,
        }
    }

    /// Active names to put into the next request after a budget is spent.
    ///
    /// `None` leaves rig's original tool set untouched. The vector is structural: a spent
    /// tool no longer appears in the provider request, rather than merely telling the model
    /// it ought not call it.
    fn active_tools(&self) -> Option<Vec<String>> {
        let session_spent = self
            .session
            .as_ref()
            .is_some_and(|session| session.calls_used() >= MAX_FETCH_CALLS);
        let context_lookup_spent = self
            .context_lookup
            .as_ref()
            .is_some_and(|session| session.calls_used() >= MAX_CONTEXT_LOOKUP_CALLS);
        if !session_spent && !context_lookup_spent {
            return None;
        }

        let mut active = Vec::with_capacity(3);
        if self.session.is_some() && !session_spent {
            active.push(OverviewTool::NAME.to_owned());
            active.push(MessagesTool::NAME.to_owned());
        }
        if self.context_lookup.is_some() && !context_lookup_spent {
            active.push(ContextLookupTool::NAME.to_owned());
        }
        Some(active)
    }

    /// Whether any budget changes the next request's tool allow-list.
    #[cfg(test)]
    fn withdraws(&self) -> bool {
        self.active_tools().is_some()
    }
}

impl<M> AgentHook<M> for WithdrawSpentTools
where
    M: CompletionModel,
{
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "rig's AgentHook trait requires async even though this hook only patches"
    )]
    async fn on_event(&self, _hook_context: &HookContext, event: StepEvent<'_, M>) -> Flow {
        if matches!(event, StepEvent::CompletionCall { .. })
            && let Some(active_tools) = self.active_tools()
        {
            return Flow::patch_request(RequestPatch::new().active_tools(active_tools));
        }
        Flow::cont()
    }

    fn observes(&self, kind: StepEventKind) -> bool {
        matches!(kind, StepEventKind::CompletionCall)
    }
}

/// What one agentic attempt came back with.
#[derive(Debug)]
enum Attempted {
    /// The model delivered something to read.
    Answer(ClassificationExtract),
    /// It spent every model call it was allowed without delivering anything.
    TurnsExhausted,
}

/// Sorts one agentic attempt, keeping a spent turn budget out of the failures.
///
/// Turn exhaustion is an `Ok` rather than an [`AttemptFailure`] for two reasons. Nothing
/// failed: the provider answered every call, so counting it as a failure armed a
/// backoff built for a classifier that is broken or unreachable. And nothing a redraw
/// would fix: a fresh attempt starts a fresh conversation with a fresh fetch budget, so
/// a model that did not converge would be paid another whole turn budget to not
/// converge again.
///
/// Read structurally, like the status extraction below — the variant is matched, never
/// rig's rendered `MaxTurnsError: reached max turns limit: 6`, which is rig's prose to
/// reword.
fn attempted(
    result: Result<ClassificationExtract, StructuredOutputError>,
) -> Result<Attempted, AttemptFailure> {
    match result {
        Ok(extract) => Ok(Attempted::Answer(extract)),
        Err(StructuredOutputError::PromptError(prompt))
            if matches!(*prompt, PromptError::MaxTurnsError { .. }) =>
        {
            Ok(Attempted::TurnsExhausted)
        }
        Err(error) => Err(AttemptFailure::from(error)),
    }
}

/// Sorts one typed-prompt failure by the only thing that tells retries apart: the HTTP
/// status the provider set.
///
/// Structural, not textual. `provider_response_status` walks rig's own chain —
/// `StructuredOutputError` → `PromptError` → `CompletionError` → `http_client::Error`
/// — and hands back the `http::StatusCode` the Anthropic provider stored when it built
/// the error from a non-2xx response. Reading `529` out of the rendered message instead
/// would key this crate's retry policy to prose the provider is free to reword, which is
/// the failure the typed status exists to remove.
///
/// No status is a real answer rather than a gap: a deserialization failure, an empty
/// response, and a transport error that never reached a response all carry none, and
/// each is sorted by its variant instead.
impl From<StructuredOutputError> for AttemptFailure {
    fn from(error: StructuredOutputError) -> Self {
        let message = error.to_string();
        if let Some(status) = error.provider_response_status() {
            return Self::Http(HttpFailure::new(status.as_u16(), message));
        }
        if timed_out(&error) {
            return Self::Timeout(message);
        }
        match error {
            StructuredOutputError::DeserializationError(_) => Self::Malformed(message),
            StructuredOutputError::PromptError(_) | StructuredOutputError::EmptyResponse => {
                Self::Failed(message)
            }
        }
    }
}

/// Whether a failure is a request this crate abandoned rather than one the provider
/// refused.
///
/// Structural, on the same principle as the status extraction above, and structural twice
/// over. The chain is walked through [`std::error::Error::source`] rather than by matching
/// rig's variants, because the shape of that chain is rig's to rearrange and a timeout can
/// surface from any link that performs I/O — sending the request or reading the body. And
/// the verdict at the end is `reqwest::Error::is_timeout`, which walks its *own* sources
/// for a `TimedOut` marker, a timed-out hyper error, or `io::ErrorKind::TimedOut`.
///
/// The alternative was reading rig's rendered `Http client error: operation timed out`,
/// which is prose two crates are free to reword, and keying a retry policy to prose is
/// precisely what `provider_response_status` was adopted to stop.
///
/// A downcast is the only way in: rig erases the error to
/// `http_client::Error::Instance(Box<dyn Error + Send + Sync>)`, and reqwest's own
/// `TimedOut` marker is private, so nothing shallower can answer this. It is also why
/// `reqwest` is a direct dependency pinned to the 0.13 rig resolves — two copies in the
/// graph would make the downcast silently fail, which the timeout tests would catch
/// because they build the error through rig's real transport.
fn timed_out(error: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(error.source(), |link| link.source())
        .filter_map(|link| link.downcast_ref::<reqwest::Error>())
        .any(reqwest::Error::is_timeout)
}

/// Sorts one extraction failure the same way.
///
/// `ExtractionError` carries no forwarding helper of its own, so the chain is entered
/// one link down, at the variant that owns the provider's response. The two paths must
/// not disagree about what a retry is for.
impl From<ExtractionError> for AttemptFailure {
    fn from(error: ExtractionError) -> Self {
        let message = error.to_string();
        if let ExtractionError::CompletionError(completion) = &error {
            if let Some(status) = completion.provider_response_status() {
                return Self::Http(HttpFailure::new(status.as_u16(), message));
            }
        }
        if timed_out(&error) {
            return Self::Timeout(message);
        }
        match error {
            ExtractionError::DeserializationError(_) => Self::Malformed(message),
            ExtractionError::NoData | ExtractionError::CompletionError(_) => Self::Failed(message),
        }
    }
}

pub struct RigClassifier {
    client: anthropic::Client,
    model: String,
    runtime: tokio::runtime::Runtime,
    session_tools: Option<Arc<SessionTools>>,
    context_provider: Option<Arc<ContextProviderTools>>,
}

impl RigClassifier {
    pub fn from_config(model: &str, api_key_env: &str) -> Result<Self, LlmError> {
        let api_key = std::env::var(api_key_env)
            .map_err(|_| LlmError::MissingApiKey(api_key_env.to_owned()))?;
        let client = timed_client(api_key)?;
        let runtime =
            tokio::runtime::Runtime::new().map_err(|error| LlmError::Api(error.to_string()))?;

        Ok(Self {
            client,
            model: model.to_owned(),
            runtime,
            session_tools: None,
            context_provider: None,
        })
    }

    /// Lets the classifier look further into a session than its payload shows.
    ///
    /// Without this it behaves exactly as before: one call, one answer, whatever the
    /// payload carried. With it, a thin payload becomes a question the model can
    /// resolve instead of a guess it has to make.
    #[must_use]
    pub fn with_session_detail(mut self, tools: Arc<SessionTools>) -> Self {
        self.session_tools = Some(tools);
        self
    }

    /// Lets the classifier resolve unfamiliar terms through optional external knowledge.
    ///
    /// Without this, the prompt and tool set are identical to the session-detail-only
    /// classifier; with it, the model gains one read-only `context_lookup` tool and an
    /// independent budget.
    #[must_use]
    pub fn with_context_provider(mut self, provider: Arc<ContextProviderTools>) -> Self {
        self.context_provider = Some(provider);
        self
    }

    /// Runs one classification with whichever optional tool families are available.
    fn classify_with_tools(
        &self,
        input: &ClassificationInput,
        roster: &[StreamSummary],
    ) -> Result<ClassificationOutput, LlmError> {
        // A window run has no session by design. It must never be offered session tools:
        // those would report `not indexed`, which reads as a broken system rather than a
        // scope that never had a session. Context lookup is independent of that scope and
        // remains available when configured.
        let session_tools = self.session_tools.as_ref().filter(|_| input.has_session);
        let context_provider = self.context_provider.as_ref();
        let prompt = if context_provider.is_some() {
            prompt::build_with_context_provider(input, roster)
        } else {
            prompt::build(input, roster)
        };
        if session_tools.is_none() && context_provider.is_none() {
            return self.extract::<ClassificationExtract>(&prompt)?.try_into();
        }

        let attempt = retrying(
            |remaining| {
                // Rig starts a fresh conversation for a retry, so every attempt also gets
                // fresh independent budgets. Reusing a spent one would make a retry blind
                // while costing the same bounded `MAX_MODEL_TURNS` round trips.
                let session = session_tools.map(|tools| tools.begin(&input.session_id));
                let context_lookup = context_provider.map(ContextProviderTools::begin);
                let hook = WithdrawSpentTools::new(session.clone(), context_lookup.clone());
                let result = match (&session, &context_lookup) {
                    (Some(session), Some(context_lookup)) => {
                        let agent = self
                            .client
                            .agent(&self.model)
                            .preamble(&preamble_with_context_lookup(true))
                            .tool(OverviewTool::new(Arc::clone(session)))
                            .tool(MessagesTool::new(Arc::clone(session)))
                            .tool(ContextLookupTool::new(Arc::clone(context_lookup)))
                            .build();
                        self.within(
                            remaining,
                            std::future::IntoFuture::into_future(
                                agent
                                    .prompt_typed::<ClassificationExtract>(prompt.clone())
                                    .max_turns(MAX_MODEL_TURNS)
                                    .add_hook(hook),
                            ),
                        )
                    }
                    (Some(session), None) => {
                        let agent = self
                            .client
                            .agent(&self.model)
                            .preamble(&preamble())
                            .tool(OverviewTool::new(Arc::clone(session)))
                            .tool(MessagesTool::new(Arc::clone(session)))
                            .build();
                        self.within(
                            remaining,
                            std::future::IntoFuture::into_future(
                                agent
                                    .prompt_typed::<ClassificationExtract>(prompt.clone())
                                    .max_turns(MAX_MODEL_TURNS)
                                    .add_hook(hook),
                            ),
                        )
                    }
                    (None, Some(context_lookup)) => {
                        let agent = self
                            .client
                            .agent(&self.model)
                            .preamble(&preamble_with_context_lookup(false))
                            .tool(ContextLookupTool::new(Arc::clone(context_lookup)))
                            .build();
                        self.within(
                            remaining,
                            std::future::IntoFuture::into_future(
                                agent
                                    .prompt_typed::<ClassificationExtract>(prompt.clone())
                                    .max_turns(MAX_MODEL_TURNS)
                                    .add_hook(hook),
                            ),
                        )
                    }
                    (None, None) => unreachable!("tool-less classifications returned before retry"),
                };
                attempted(result?)
            },
            std::thread::sleep,
        )?;
        match attempt {
            Attempted::Answer(extract) => extract.try_into(),
            // A verdict-less outcome, not a failure. The reasoning is what an operator
            // reads if this is ever logged, so it names the bound that was hit.
            Attempted::TurnsExhausted => Ok(ClassificationOutput {
                choice: StreamChoice::TurnsExhausted,
                confidence: 0.0,
                reasoning: format!("the model made {MAX_MODEL_TURNS} calls without answering"),
            }),
        }
    }

    fn extract<T>(&self, prompt: &str) -> Result<T, LlmError>
    where
        T: JsonSchema + for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
    {
        retrying(
            |remaining| {
                let extractor = self.client.extractor::<T>(&self.model).build();
                self.within(remaining, extractor.extract(prompt))?
                    .map_err(AttemptFailure::from)
            },
            std::thread::sleep,
        )
    }

    /// Runs one model call, abandoning it if the classification's allowance runs out.
    ///
    /// The per-request timeout on the HTTP backend bounds a *request*; this bounds the
    /// attempt containing it, and the two are not the same size. An agentic attempt makes
    /// up to [`MAX_MODEL_TURNS`] requests, so without this a single attempt could spend
    /// `MAX_MODEL_TURNS × REQUEST_TIMEOUT` — twelve minutes — while every individual
    /// request stayed inside its bound.
    ///
    /// Reported as [`AttemptFailure::Timeout`], which is honest twice over: the request
    /// really was outstanding when it was abandoned, and the retry it invites terminates
    /// at once, because [`retrying`] re-checks the allowance before starting an attempt
    /// and this branch can only be reached with the allowance spent.
    ///
    /// The timeout is constructed *inside* the block rather than handed to `block_on`
    /// already built. `tokio::time::timeout` registers a timer with the reactor as it is
    /// created, and an argument is evaluated before the call it is passed to, so building
    /// it outside panics with "there is no reactor running" — on every classification,
    /// since nothing upstream of this crate is async.
    fn within<T>(
        &self,
        remaining: Duration,
        future: impl Future<Output = T>,
    ) -> Result<T, AttemptFailure> {
        self.runtime
            .block_on(async move { tokio::time::timeout(remaining, future).await })
            .map_err(|_elapsed| {
                AttemptFailure::Timeout(format!(
                    "abandoned after the classification's {MAX_CLASSIFICATION_MS}ms allowance"
                ))
            })
    }
}

/// Builds the Anthropic client on an HTTP backend that gives up on a silent request.
///
/// The seam is rig's own, and rig documents it: `Client::new` is pinned to
/// `reqwest::Client::default()`, and its doc comment says callers wanting a different
/// backend "should go through `Client::builder` and chain `ClientBuilder::http_client`
/// before `ClientBuilder::build`". That is all this does — the same default backend, with
/// the one setting that was missing.
///
/// `reqwest::Client::default()` has no timeout at all, which is where the stall came from.
/// Only the overall request timeout is set: it already covers connect, response, and body,
/// so a separate `connect_timeout` would be a second knob answering the same question.
fn timed_client(api_key: String) -> Result<anthropic::Client, LlmError> {
    let http = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| LlmError::Api(error.to_string()))?;
    anthropic::Client::builder()
        .api_key(api_key)
        .http_client(http)
        .build()
        .map_err(|error| LlmError::Api(error.to_string()))
}

impl Classifier for RigClassifier {
    fn classify(
        &self,
        input: &ClassificationInput,
        roster: &[StreamSummary],
    ) -> Result<ClassificationOutput, LlmError> {
        self.classify_with_tools(input, roster)
    }

    fn describe_stream(&self, evidence: &str) -> Result<String, LlmError> {
        self.extract(evidence)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rig_core::completion::request::{CompletionError, PromptError, StructuredOutputError};
    use rig_core::extractor::ExtractionError;
    use rig_core::http_client::{HttpClientExt, Request, ReqwestClient, Response};

    use super::{
        AttemptFailure, Attempted, HttpFailure, MAX_CLASSIFICATION_MS, MAX_FETCH_CALLS,
        MAX_MODEL_TURNS, REQUEST_TIMEOUT, WithdrawSpentTools, anthropic, attempted, retrying,
    };
    use crate::fetch::{FetchRequest, SessionTools};
    use crate::transport::{MAX_TOTAL_BACKOFF_MS, MAX_TRANSPORT_RETRIES};
    use crate::{
        ClassificationExtract, ClassificationInput, Classifier, ContextLookupRequest,
        ContextProvider, ContextProviderError, ContextProviderTools, LlmError,
        MAX_CONTEXT_LOOKUP_CALLS, RigClassifier, StreamSummary,
    };

    fn deserialization_error() -> serde_json::Error {
        serde_json::from_str::<i32>("not a number").unwrap_err()
    }

    /// The error rig builds from a non-2xx Anthropic response.
    ///
    /// Built through rig's own constructor rather than by hand, so the test exercises
    /// the real storage path: `from_http_response` is what the Anthropic completion
    /// model calls, and it is what decides that a failed status lands in
    /// `HttpError::InvalidStatusCodeWithMessage` where the status stays typed.
    fn provider_refusal(status: u16) -> CompletionError {
        // A `StatusCode` without adding an `http` dependency for one constructor.
        let status = Response::builder()
            .status(status)
            .body(())
            .expect("a status code the test named itself is valid")
            .status();
        CompletionError::from_http_response(
            status,
            r#"{"type":"error","error":{"type":"overloaded_error"}}"#,
        )
    }

    fn typed_prompt_refusal(status: u16) -> StructuredOutputError {
        StructuredOutputError::PromptError(Box::new(PromptError::CompletionError(
            provider_refusal(status),
        )))
    }

    /// A socket that accepts a connection and answers nothing, and its base URL.
    ///
    /// The production defect in miniature: the live process sat on exactly one such
    /// connection for 3,077 seconds. Local only — no provider, no key, no packet leaves
    /// the loopback interface.
    ///
    /// The listener is returned so the caller keeps it alive; dropping it would close the
    /// port and turn the hang into a connection refusal, which is a different failure and
    /// would make these tests pass for the wrong reason.
    fn silent_server() -> (std::net::TcpListener, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("the loopback interface accepts an ephemeral port");
        let address = listener
            .local_addr()
            .expect("a bound listener has an address");
        let accepting = listener
            .try_clone()
            .expect("a listener can be cloned for the accept loop");
        // Accepts connections and answers nothing, holding them open well past any
        // timeout under test. Detached: the tests need the socket alive, not the thread.
        std::thread::spawn(move || {
            #[expect(
                clippy::collection_is_never_read,
                reason = "holding the streams is the point: dropping one closes the \
                          connection, which the client sees as an EOF rather than as the \
                          silence these tests exist to reproduce"
            )]
            let mut held = Vec::new();
            while let Ok((stream, _)) = accepting.accept() {
                held.push(stream);
            }
        });
        (listener, format!("http://{address}"))
    }

    /// A rig transport error from a request that really did time out.
    ///
    /// Nothing here is hand-rolled, because the hand-rolled version is what would hide
    /// the bug: the detection has to survive rig's own wrapping of reqwest's own error,
    /// and both of those are outside this crate. A socket that accepts and never answers
    /// is the production defect in miniature — the live process sat on exactly one such
    /// connection for 3,077 seconds — and `HttpClientExt::send` is the same call the
    /// Anthropic completion model makes, so the error arrives through the same
    /// `instance_error` path that buries the timeout in a `Box<dyn Error>`.
    ///
    /// Local only. No provider, no key, no packet leaves the loopback interface.
    fn real_timeout_error() -> rig_core::http_client::Error {
        let (_listener, base) = silent_server();
        let uri = format!("{base}/v1/messages");

        let client = ReqwestClient::builder()
            .timeout(Duration::from_millis(150))
            .build()
            .expect("a reqwest client with only a timeout set is buildable");
        let request = Request::post(uri)
            .body(Vec::<u8>::new())
            .expect("a POST with an empty body is a valid request");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime is buildable");

        runtime
            .block_on(HttpClientExt::send::<_, Vec<u8>>(&client, request))
            .err()
            .expect("a server that never answers cannot produce a response")
    }

    #[test]
    fn a_request_the_provider_never_answered_is_recognised_as_a_timeout() {
        // Given: the production defect, reproduced. Before a timeout was configured
        // this call simply never returned — the live drain sat at 0.0% CPU on one open
        // connection for 3,077 seconds, having classified 3 sessions in 20 minutes.
        let error = real_timeout_error();

        // When: sorted by the same seam that sorts a 529.
        let failure = AttemptFailure::from(StructuredOutputError::PromptError(Box::new(
            PromptError::CompletionError(CompletionError::HttpError(error)),
        )));

        // Then: a timeout, not an untyped failure. Read structurally — `is_timeout`
        // walks the error's own source chain — rather than by matching rig's or
        // reqwest's rendered prose, which is theirs to reword.
        assert!(matches!(failure, AttemptFailure::Timeout(_)), "{failure:?}");
    }

    #[test]
    fn a_timeout_reaching_the_extraction_path_is_recognised_there_too() {
        // Given: the same silence meeting `describe_stream`, which has no forwarding
        // helper and enters the chain a link lower. The two paths must not disagree
        // about what a retry is for.
        let error = real_timeout_error();

        // When
        let failure = AttemptFailure::from(ExtractionError::CompletionError(
            CompletionError::HttpError(error),
        ));

        // Then
        assert!(matches!(failure, AttemptFailure::Timeout(_)), "{failure:?}");
    }

    #[test]
    fn a_provider_that_never_answers_ends_the_classification_instead_of_holding_it() {
        // Given: the whole production seam with nothing hand-rolled between its halves
        // — a genuine timed-out request, sorted by the real conversion, handed to the
        // same `retrying` the classifier calls. This is the assertion the defect
        // report asks for: the loop *terminates*, in a bounded number of attempts,
        // where before it terminated never.
        let mut waits = Vec::new();
        let mut attempts = 0_usize;

        // When
        let answer = retrying::<&str>(
            |_remaining| {
                attempts += 1;
                Err(AttemptFailure::from(StructuredOutputError::PromptError(
                    Box::new(PromptError::CompletionError(CompletionError::HttpError(
                        real_timeout_error(),
                    ))),
                )))
            },
            |pause| waits.push(pause),
        );

        // Then: bounded, and reported as a timeout rather than as breakage — the
        // session is still classifiable and the next pass will reach it.
        assert_eq!(attempts, MAX_TRANSPORT_RETRIES as usize + 1);
        assert_eq!(waits.len(), MAX_TRANSPORT_RETRIES as usize);
        assert!(matches!(answer, Err(LlmError::Timeout(_))), "{answer:?}");
    }

    #[test]
    fn a_statusless_transport_error_that_is_not_a_timeout_stays_terminal() {
        // Given: the discrimination the new variant must not lose. A connection reset
        // carries no status and is not a timeout, so nothing here can say another call
        // would differ — and nothing here spends one.
        let error = ExtractionError::CompletionError(CompletionError::ProviderError(
            "connection reset".to_owned(),
        ));

        // When/Then
        assert!(matches!(
            AttemptFailure::from(error),
            AttemptFailure::Failed(_)
        ));
    }

    #[test]
    fn a_refusal_carrying_a_status_is_still_sorted_by_that_status() {
        // Given: the precedence. A provider that answered has a status, and the status
        // is the better evidence — the timeout check must not shadow it.
        // When/Then
        let failure = AttemptFailure::from(typed_prompt_refusal(529));
        let AttemptFailure::Http(failure) = failure else {
            panic!("a status the provider set must still be sorted as an HTTP failure");
        };
        assert_eq!(failure.status, 529);
    }

    /// The failure rig raises when an agentic run runs out of model calls.
    ///
    /// Built as the variant rather than as a rendered string, because the whole point
    /// is that the outcome is read structurally: `MaxTurnsError: reached max turns
    /// limit: 10` is rig's prose to reword.
    fn turn_budget_exhausted() -> StructuredOutputError {
        StructuredOutputError::PromptError(Box::new(PromptError::MaxTurnsError {
            max_turns: MAX_MODEL_TURNS,
            chat_history: Box::new(Vec::new()),
            prompt: Box::new("classify this session".into()),
        }))
    }

    struct UnavailableContextProvider;

    impl ContextProvider for UnavailableContextProvider {
        fn lookup(&self, _query: &str) -> Result<String, ContextProviderError> {
            Err(ContextProviderError::Backend(
                "test provider unavailable".to_owned(),
            ))
        }
    }

    /// A gate over a context-lookup budget that has served `spent` calls.
    fn context_lookup_gate_after(spent: usize) -> WithdrawSpentTools {
        let provider = ContextProviderTools::new(std::sync::Arc::new(UnavailableContextProvider));
        let session = provider.begin();
        for _ in 0..spent {
            session.dispatch(&ContextLookupRequest {
                query: "Apollo".to_owned(),
            });
        }
        WithdrawSpentTools::new(None, Some(session))
    }

    #[test]
    fn a_spent_context_lookup_budget_withdraws_only_that_tool_from_the_next_request() {
        // Given: session fetches still have room, while context lookups are spent.
        let session = SessionTools::unavailable().begin("ses-1");
        let context_lookup =
            ContextProviderTools::new(std::sync::Arc::new(UnavailableContextProvider)).begin();
        for _ in 0..MAX_CONTEXT_LOOKUP_CALLS {
            context_lookup.dispatch(&ContextLookupRequest {
                query: "Apollo".to_owned(),
            });
        }
        let gate = WithdrawSpentTools::new(Some(session), Some(context_lookup));

        // Then: this is the request's actual allow-list, not prose that merely asks the
        // model not to call the lookup again. Session tools remain available.
        assert_eq!(
            gate.active_tools(),
            Some(vec![
                "session_overview".to_owned(),
                "session_messages".to_owned(),
            ])
        );
        assert_eq!(context_lookup_gate_after(0).active_tools(), None);
        assert_eq!(
            context_lookup_gate_after(MAX_CONTEXT_LOOKUP_CALLS).active_tools(),
            Some(Vec::new())
        );
    }

    /// A gate over a budget that has served `spent` calls.
    ///
    /// Driven through real dispatches rather than by setting a counter, so the gate is
    /// held against the same accounting the live tools spend.
    fn gate_after(spent: usize) -> WithdrawSpentTools {
        let session = SessionTools::unavailable().begin("ses-1");
        for _ in 0..spent {
            session.dispatch(&FetchRequest::Overview {
                session_id: "ses-1".to_owned(),
            });
        }
        WithdrawSpentTools::new(Some(session), None)
    }

    #[test]
    fn the_fetch_tools_stay_advertised_while_the_budget_has_anything_left() {
        // Given / When / Then: a model with a fetch left must keep both tools, or the
        // whole point of an agentic classification is gone. The last call inside the
        // budget is the interesting one — off by one here and the fourth fetch is never
        // reachable.
        assert!(!gate_after(0).withdraws());
        assert!(!gate_after(MAX_FETCH_CALLS - 1).withdraws());
    }

    #[test]
    fn the_fetch_tools_are_withdrawn_from_every_turn_after_the_budget_is_spent() {
        // Given: a budget spent exactly, and one spent past its bound — the state a
        // turn that emitted several tool calls at once leaves behind.
        // When / Then: the next turn advertises nothing, so `BudgetExhausted` stops
        // being advice the model may decline and read as an invitation to call again.
        // Each of those calls spent a model turn until rig raised `MaxTurnsError`.
        assert!(gate_after(MAX_FETCH_CALLS).withdraws());
        assert!(gate_after(MAX_FETCH_CALLS + 3).withdraws());
    }

    #[test]
    fn a_spent_turn_budget_is_an_answerless_outcome_rather_than_a_failure() {
        // Given: the production failure. `classifier_last_error` read
        // `PromptError: MaxTurnsError: reached max turns`, and every occurrence cost a
        // session its classification *and* incremented
        // `classifier_consecutive_failures`, whose exponential backoff throttled the
        // drain to 0.17 classifications a minute.
        let mut waits = Vec::new();
        let mut attempts = 0_usize;

        // When
        let answer = retrying(
            |_remaining| {
                attempts += 1;
                attempted(Err(turn_budget_exhausted()))
            },
            |pause| waits.push(pause),
        );

        // Then: an outcome, not a failure. Nothing failed — the model was reached and
        // answered, it simply never converged on a verdict.
        assert!(
            matches!(answer, Ok(Attempted::TurnsExhausted)),
            "{answer:?}"
        );
        // And: not redrawn. Another attempt would buy the same non-answer for another
        // whole turn budget, so it is returned at once and costs no wait.
        assert_eq!(attempts, 1);
        assert!(waits.is_empty());
    }

    #[test]
    fn a_provider_that_stays_overloaded_still_backs_off_through_the_same_seam() {
        // Given: the transport failure the new seam must not swallow. It reaches
        // `attempted` exactly where a spent turn budget does.
        let mut waits = Vec::new();
        let mut attempts = 0_usize;

        // When
        let answer = retrying(
            |_remaining| {
                attempts += 1;
                attempted(Err(typed_prompt_refusal(529)))
            },
            |pause| waits.push(pause),
        );

        // Then: still an error, still waited out first — the session is worth retrying
        // because the provider said *not now*.
        assert!(matches!(answer, Err(LlmError::Overloaded(_))), "{answer:?}");
        assert_eq!(attempts, MAX_TRANSPORT_RETRIES as usize + 1);
        assert_eq!(waits.len(), MAX_TRANSPORT_RETRIES as usize);
    }

    #[test]
    fn a_rejected_request_reaching_the_same_seam_stays_terminal() {
        // Given: the status that must not be waited out.
        let mut waits = Vec::new();

        // When
        let answer = retrying(
            |_remaining| attempted(Err(typed_prompt_refusal(400))),
            |pause| {
                waits.push(pause);
            },
        );

        // Then
        assert!(matches!(answer, Err(LlmError::Api(_))), "{answer:?}");
        assert!(waits.is_empty());
    }

    #[test]
    fn an_answer_that_arrived_is_carried_through_untouched() {
        // Given: the ordinary case — a model that answered on schema.
        let extract = ClassificationExtract {
            stream_id: Some("stream-a".to_owned()),
            new_stream_name: None,
            new_stream_description: None,
            throwaway: None,
            confidence: 0.9,
            reasoning: "names the work".to_owned(),
        };

        // When
        let answer = attempted(Ok(extract));

        // Then: the new seam is a sorter, not a filter.
        let Ok(Attempted::Answer(carried)) = answer else {
            panic!("an on-schema answer must arrive as an answer");
        };
        assert_eq!(carried.stream_id.as_deref(), Some("stream-a"));
    }

    #[test]
    fn an_overload_is_read_out_of_the_agentic_paths_error_chain_as_a_number() {
        // Given: the production failure. Its rendered form is
        // `PromptError: CompletionError: HttpError: Invalid status code 529 ...`,
        // and the whole point is to not read it that way.
        let error = typed_prompt_refusal(529);

        // When
        let failure = AttemptFailure::from(error);

        // Then: 529 arrives as a number rig kept, and is recognised as worth waiting
        // out. Before this, it was an untyped string that cost the session its
        // classification.
        let AttemptFailure::Http(failure) = failure else {
            panic!("a status the provider set must be sorted as an HTTP failure");
        };
        assert_eq!(failure.status, 529);
        assert!(failure.is_transient());
    }

    #[test]
    fn an_overload_is_read_out_of_the_extraction_paths_error_chain_too() {
        // Given: the same refusal reaching the non-agentic path, which has no
        // forwarding helper and must enter the chain a link lower.
        let error = ExtractionError::CompletionError(provider_refusal(529));

        // When
        let failure = AttemptFailure::from(error);

        // Then: the two paths agree, which is the only reason `describe_stream`
        // survives an overload as well.
        let AttemptFailure::Http(failure) = failure else {
            panic!("a status the provider set must be sorted as an HTTP failure");
        };
        assert_eq!(failure.status, 529);
        assert!(failure.is_transient());
    }

    #[test]
    fn a_rejected_request_is_read_as_a_status_and_left_terminal() {
        // Given: a 400. It reaches the same sorting as the 529 — the status is what
        // is extracted, never whether to retry, so a single rule decides that later.
        let failure = AttemptFailure::from(typed_prompt_refusal(400));

        // When/Then
        let AttemptFailure::Http(failure) = failure else {
            panic!("a status the provider set must be sorted as an HTTP failure");
        };
        assert_eq!(failure.status, 400);
        assert!(!failure.is_transient());
    }

    #[test]
    fn only_a_typed_prompts_deserialization_failure_is_worth_another_sample() {
        // Given: the statusless failures the agentic path can meet.
        // When/Then: output that missed the schema is worth another sample; an empty
        // response is the call failing, and is not retried.
        assert!(matches!(
            AttemptFailure::from(StructuredOutputError::DeserializationError(
                deserialization_error()
            )),
            AttemptFailure::Malformed(_)
        ));
        assert!(matches!(
            AttemptFailure::from(StructuredOutputError::EmptyResponse),
            AttemptFailure::Failed(_)
        ));
    }

    #[test]
    fn only_an_extractions_deserialization_failure_is_worth_another_sample() {
        // Given: the statusless failures the non-agentic path can meet, sorted the
        // same way — the two paths must not disagree about what a retry is for.
        // When/Then
        assert!(matches!(
            AttemptFailure::from(ExtractionError::DeserializationError(
                deserialization_error()
            )),
            AttemptFailure::Malformed(_)
        ));
        assert!(matches!(
            AttemptFailure::from(ExtractionError::NoData),
            AttemptFailure::Failed(_)
        ));
    }

    #[test]
    fn a_statusless_provider_error_stays_untyped_rather_than_being_guessed_at() {
        // Given: a provider error rig recorded without a response — no status ever
        // reached this layer.
        let error = ExtractionError::CompletionError(CompletionError::ProviderError(
            "connection reset".to_owned(),
        ));

        // When/Then: reported as a plain failure. Inventing a status here, or
        // sniffing one out of the message, is what the typed path exists to avoid.
        assert!(matches!(
            AttemptFailure::from(error),
            AttemptFailure::Failed(_)
        ));
    }

    #[test]
    fn an_overloaded_verdict_renders_its_status_for_the_operator() {
        // Given: the error a spent retry budget produces.
        let error = LlmError::Overloaded(HttpFailure::new(529, "overloaded_error".to_owned()));

        // When/Then: the log line names the status, so `classifier_last_error` still
        // says what happened without anyone parsing it back out.
        let rendered = error.to_string();
        assert!(rendered.contains("529"), "{rendered}");
        assert!(rendered.contains("overloaded"), "{rendered}");
    }

    #[test]
    fn a_real_529_from_rig_is_waited_out_and_then_answered() {
        // Given: the whole production seam, with nothing hand-rolled between the two
        // halves — a genuine rig error built the way the Anthropic provider builds it,
        // handed to the same `retrying` the classifier calls. Testing the conversion
        // and the policy only in isolation would leave exactly this join untested,
        // and the join is where a 529 was being thrown away.
        let mut waits = Vec::new();
        let mut attempts = 0_usize;

        // When
        let answer = retrying(
            |_remaining| {
                attempts += 1;
                if attempts == 1 {
                    return Err(AttemptFailure::from(typed_prompt_refusal(529)));
                }
                Ok("a verdict")
            },
            |pause| waits.push(pause),
        );

        // Then: the session keeps its classification, having paid one wait for it.
        assert_eq!(answer.unwrap(), "a verdict");
        assert_eq!(attempts, 2);
        assert_eq!(waits.len(), 1);
    }

    #[test]
    fn a_real_400_from_rig_is_returned_without_a_wait() {
        // Given: the same seam for the status that must not cost anything.
        let mut waits = Vec::new();
        let mut attempts = 0_usize;

        // When
        let answer = retrying::<&str>(
            |_remaining| {
                attempts += 1;
                Err(AttemptFailure::from(typed_prompt_refusal(400)))
            },
            |pause| waits.push(pause),
        );

        // Then: one call, no wait, and not counted as an overload — a rejected request
        // is rejected identically forever.
        assert_eq!(attempts, 1);
        assert!(waits.is_empty());
        assert!(matches!(answer, Err(LlmError::Api(_))));
    }

    /// A classifier whose provider is a socket that answers nothing.
    ///
    /// Assembled through `timed_client`'s own builder chain rather than through
    /// `from_config`, which would read a real API key and talk to the real provider. Only
    /// two things differ: the base URL points at the silent socket, and the timeout is
    /// milliseconds instead of [`REQUEST_TIMEOUT`] so the test finishes. The seam under
    /// test — `http_client` on rig's builder — is identical.
    fn classifier_against(base_url: &str, timeout: Duration) -> RigClassifier {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("a reqwest client with only a timeout set is buildable");
        let client = anthropic::Client::builder()
            .api_key("not-a-real-key")
            .http_client(http)
            .base_url(base_url)
            .build()
            .expect("a client with a base URL and a backend is buildable");
        RigClassifier {
            client,
            model: "claude-haiku-4-5".to_owned(),
            runtime: tokio::runtime::Runtime::new().expect("a runtime is buildable"),
            session_tools: None,
            context_provider: None,
        }
    }

    #[test]
    fn a_classifier_whose_provider_never_answers_returns_instead_of_hanging() {
        // Given: the entire production stack against the production defect, with nothing
        // stubbed between the halves — a real rig client on a real reqwest backend, a
        // real socket that accepts and never answers, and the real `retrying`. This is
        // the assertion the defect report asks for and the one no unit test can make:
        // before the timeout was configured this call *did not return*, so there was no
        // outcome to assert on at all. The live process was measured 3,077 seconds into
        // exactly this state at 0.0% CPU.
        let (_listener, base) = silent_server();
        let classifier = classifier_against(&base, Duration::from_millis(120));
        let started = std::time::Instant::now();

        // When: the extraction path, which `describe_stream` uses and which
        // `classify` also falls back to when no fetch tools are registered.
        let answer = classifier.describe_stream("evidence for a stream nobody will name");

        // Then: it came back, and came back as a timeout — not as breakage, because the
        // work is still classifiable and the next pass will reach it.
        let elapsed = started.elapsed();
        assert!(matches!(answer, Err(LlmError::Timeout(_))), "{answer:?}");
        // And: inside the bound rather than merely eventually. Five attempts of 120ms
        // plus the module's 20s cumulative wait allowance is the whole budget; anything
        // beyond it means a bound is not being enforced.
        assert!(
            elapsed < Duration::from_millis(MAX_TOTAL_BACKOFF_MS + 5_000),
            "gave up only after {elapsed:?}"
        );
    }

    #[test]
    fn an_attempt_cannot_outlive_the_allowance_it_was_given() {
        // Given: a request bound so wide it cannot be what stops anything (ten minutes),
        // and a call that never completes. This isolates the outer bound: if the
        // classification allowance were only checked *between* attempts, one attempt
        // could spend `MAX_MODEL_TURNS × REQUEST_TIMEOUT` — twelve minutes — with every
        // individual request still inside its own bound, which is the same mistake as
        // bounding requests but not classifications.
        let classifier = classifier_against("http://127.0.0.1:1", Duration::from_secs(600));
        let started = std::time::Instant::now();

        // When: given 50ms of allowance for work that never finishes.
        let outcome = classifier.within(Duration::from_millis(50), std::future::pending::<()>());

        // Then: abandoned by the allowance, not by the request timeout, and promptly.
        assert!(
            matches!(outcome, Err(AttemptFailure::Timeout(_))),
            "{outcome:?}"
        );
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
    }

    #[test]
    fn the_request_timeout_is_set_above_the_slowest_classification_ever_served() {
        // Given/When/Then: the number itself, because it is the one thing here that a
        // behavioural test cannot hold. A bound tighter than real work would abandon
        // completions the provider is genuinely serving, which is worse than no bound at
        // all — so 120s sits above the slowest whole classification ever observed
        // completing (~92s at 0.65/min), and a single request cannot exceed the
        // classification containing it.
        assert!(REQUEST_TIMEOUT >= Duration::from_secs(92));
        // And: a request may not outlast the classification that contains it, or the
        // outer bound would be unreachable and the inner one would decide alone.
        assert!(REQUEST_TIMEOUT < Duration::from_millis(MAX_CLASSIFICATION_MS));
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY"]
    fn classifies_a_hardcoded_input_against_anthropic() {
        // Given
        let classifier =
            RigClassifier::from_config("claude-haiku-4-5", "ANTHROPIC_API_KEY").unwrap();
        let input = ClassificationInput {
            has_session: true,
            session_id: "manual-verification".to_owned(),
            machine: None,
            cwd: Some("/tmp".to_owned()),
            starting_prompt: Some("Investigate a Rust project".to_owned()),
            user_prompts: vec!["Add a test".to_owned()],
            window_titles: Vec::new(),
            started_at: None,
        };

        // When
        let result = classifier.classify(&input, &Vec::<StreamSummary>::new());

        // Then
        assert!(result.is_ok());
    }
}
