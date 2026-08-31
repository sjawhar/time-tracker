use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::context_provider::{ContextLookupRequest, ContextLookupSession, ContextProviderTools};
use crate::fetch::{FetchRequest, FetchSession, SessionTools};
use crate::transport::HttpFailure;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationInput {
    /// Identifies the work under classification, and keys the fetch budget.
    ///
    /// This is **not** always a real session: a window run is a set of focus events with
    /// no session at all, and passes a synthetic `window:<event_id>`. Consult
    /// [`Self::has_session`] before treating it as one.
    pub session_id: String,
    /// Whether [`Self::session_id`] names a session the `SessionDetail` provider can read.
    ///
    /// The agentic fetch path is only offered when this is true, and the reason is a
    /// measured defect rather than tidiness. A window run has no session by design, but it
    /// was handed the same preamble ("You may look further into the session") and the same
    /// `session_overview` / `session_messages` tools as a real session. The model duly
    /// called them, and `SessionDetail` answered `session window:<id> is not indexed` --
    /// a message that reads as *the system is broken* rather than *this scope has no
    /// session*. Live, that reached **161 of 518 pending proposals (31%), every one of them
    /// window-scoped**, and they average **0.491 confidence against 0.597** for the rest of
    /// the queue. The model was being told a lie about system state and answering more
    /// cautiously because of it, having also spent fetch calls to learn it.
    ///
    /// Declared per input rather than sniffed from the `window:` prefix, because that
    /// prefix is a format `tt-cli` invents and `tt-llm` must not come to depend on it.
    pub has_session: bool,
    pub machine: Option<String>,
    pub cwd: Option<String>,
    pub starting_prompt: Option<String>,
    pub user_prompts: Vec<String>,
    pub window_titles: Vec<String>,
    /// When the work under classification happened.
    ///
    /// Orders the roster around *this* moment rather than around now. The classifier
    /// drains a backlog, so the two are months apart: the 23
    /// `agent-c: eval-3 <app> environment` streams were created on one August morning and
    /// hold April events, and ordering by absolute recency ranked every one of them
    /// 495–860 — outside a 200-stream roster, so the model could not have reused any of
    /// them even though they are each other's obvious reuse target.
    ///
    /// `None` degrades to plain recency rather than to input order.
    pub started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSummary {
    pub slug: Option<String>,
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// The stream's earliest activity.
    ///
    /// Paired with [`Self::last_active`] it gives the stream a *period*, which is what
    /// `prompt::build` measures the session against. A single timestamp cannot express
    /// "was already underway when this session ran", and that is the strongest reuse
    /// signal there is.
    pub first_active: Option<DateTime<Utc>>,
    /// The stream's most recent activity.
    pub last_active: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamChoice {
    Existing {
        stream_id: String,
    },
    New {
        name: String,
        /// Absent when the model named the work but did not describe it.
        ///
        /// Left empty rather than filled with a placeholder, because
        /// `tt streams describe --backfill` selects on a missing description:
        /// inventing one would file the stream as described and exile it from the
        /// command built to finish the job, besides feeding the fabrication back to
        /// the classifier in every later roster.
        description: Option<String>,
    },
    /// The session carries no work worth a stream, judged on inspection.
    Throwaway,
    /// The model answered, and its answer was that it could not identify the work.
    ///
    /// Held apart from [`Self::Throwaway`] because the two say opposite things about
    /// whether work exists. Throwaway is a judgement that there is nothing to attribute,
    /// and it routes the session to the junk stream. Undetermined is the absence of a
    /// judgement: work may well exist and this classification did not identify it, so
    /// the session is left unassigned, where it reads as classification lag. Routing an
    /// undetermined answer to junk would file real work as nothing.
    Undetermined,
    /// The model spent every call it was allowed without ever delivering an answer.
    ///
    /// Not a choice at all, which is why it is held apart from the three that are and
    /// from [`Self::Undetermined`], which *is* an answer. rig bounds an agentic run at
    /// `MAX_MODEL_TURNS` model calls and hard-errors past it, and that error used to
    /// reach `tt-cli` as [`LlmError::Api`]: every occurrence cost a session its
    /// classification and incremented `classifier_consecutive_failures`, whose
    /// exponential backoff throttled the daemon's drain to 0.17 classifications a
    /// minute. Nothing had failed — the provider answered every call — so the failure
    /// tally was the wrong place for it.
    ///
    /// Resting place is the same as [`Self::Undetermined`]: unassigned, where it reads
    /// as classification lag and stays reachable by a later pass. The counter is not,
    /// because the two name different things to fix. A decline is the prompt working on
    /// input it cannot place and asks for nothing; a spent turn budget says the agentic
    /// fetch loop failed to converge, which is a bound or a prompt to look at.
    TurnsExhausted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassificationOutput {
    pub choice: StreamChoice,
    pub confidence: f64,
    pub reasoning: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("api key env var {0} not set")]
    MissingApiKey(String),
    /// The call failed on something no retry could change.
    ///
    /// A status outside the retryable set, or a failure carrying no status at all.
    #[error("model call failed: {0}")]
    Api(String),
    /// The provider kept refusing with a status that meant *not now*.
    ///
    /// Held apart from [`Self::Api`] because the two ask different things of whoever
    /// reads the pass summary: a run degraded by provider overload recovers by itself
    /// and its candidates are worth re-reaching next pass, while a run hitting real
    /// errors will hit them again identically.
    #[error("provider stayed overloaded across every retry: {0}")]
    Overloaded(HttpFailure),
    /// The provider never answered, or the classification ran out of wall clock.
    ///
    /// A fourth bucket rather than part of [`Self::Overloaded`] because the provider
    /// answering 529 and the provider saying nothing at all are different observations,
    /// and only the first one proves anything reached a model. It is not part of
    /// [`Self::Api`] for the reason `Overloaded` is not either: this recovers by itself
    /// and its candidate is worth re-reaching next pass, so folding it into breakage is
    /// how a silent socket comes to look like a classifier defect. One live process sat
    /// on such a socket for 3,077 seconds and classified 3 sessions in 20 minutes.
    ///
    /// Both shapes share the bucket because they ask the same thing of an operator
    /// — *try later* — and the message names which bound was hit.
    #[error("model call timed out: {0}")]
    Timeout(String),
    #[error("unparseable model output: {0}")]
    Parse(String),
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClassificationExtract {
    pub stream_id: Option<String>,
    pub new_stream_name: Option<String>,
    pub new_stream_description: Option<String>,
    /// Set when the session is trivial enough that no stream should hold it.
    pub throwaway: Option<bool>,
    pub confidence: f64,
    pub reasoning: String,
}

impl TryFrom<ClassificationExtract> for ClassificationOutput {
    type Error = LlmError;

    fn try_from(value: ClassificationExtract) -> Result<Self, Self::Error> {
        if value.throwaway == Some(true) {
            return Ok(Self {
                choice: StreamChoice::Throwaway,
                confidence: value.confidence,
                reasoning: value.reasoning,
            });
        }
        // The name is the whole verdict: it is what `is_misnamed_stream`
        // judges and what the stream is filed under. A missing description is a
        // gap `tt streams describe --backfill` was built to close, so discarding a
        // named stream over it throws away work the model actually identified.
        let choice = match (value.stream_id, value.new_stream_name) {
            (Some(stream_id), _) => StreamChoice::Existing { stream_id },
            (None, Some(name)) => StreamChoice::New {
                name,
                description: value.new_stream_description,
            },
            // Naming neither an existing stream nor a new one, having been asked to
            // decline rather than invent a container, is that instruction being obeyed.
            // It determines nothing, which is a verdict about this classification and
            // not a defect in it.
            //
            // A stray `new_stream_description` does not change that. This arm used to
            // split on it and raise a hard parse error, reasoning that a description is
            // the dependent half of a choice whose load-bearing half is missing and that
            // nothing invites the shape. The premise was measured false: the model emits
            // it in 6% of live classifications (10 of 167 over twelve hours), because the
            // schema advertises the field and a declining model fills it with prose that
            // belongs in `reasoning`. The load-bearing half is missing in *both* arms --
            // there is no name, so no stream can be minted either way -- so the presence
            // of a field that names nothing cannot be what separates a decline from a
            // malformation. Erroring instead spent up to `MAX_PARSE_ATTEMPTS` redraws on
            // a shape no redraw fixes, counted a decline among `errors`, and armed the
            // failure backoff against a model that was answering as instructed.
            //
            // This is the same correction this repo already made for the arm above, applied
            // to the case it missed. The guard that matters is untouched: creating a stream
            // still requires a name, and `is_misnamed_stream` still judges it.
            (None, None) => StreamChoice::Undetermined,
        };

        Ok(Self {
            choice,
            confidence: value.confidence,
            reasoning: value.reasoning,
        })
    }
}

pub trait Classifier: Send + Sync {
    fn classify(
        &self,
        input: &ClassificationInput,
        roster: &[StreamSummary],
    ) -> Result<ClassificationOutput, LlmError>;

    fn describe_stream(&self, evidence: &str) -> Result<String, LlmError>;
}

/// A scripted stand-in for a model, including its decisions to fetch and look up knowledge.
///
/// A [`brain`](MockClassifier::brain) sees the same session and context-lookup budgets a live
/// classifier would expose. The context argument is `None` without a configured provider,
/// exactly matching `RigClassifier`: it is not a fake unavailable tool that a model can waste
/// a turn on. Leave a brain unset to retain the older scripted-answer behavior.
pub type MockBrain = Box<
    dyn Fn(
            &ClassificationInput,
            &FetchSession,
            Option<&ContextLookupSession>,
        ) -> Result<ClassificationOutput, LlmError>
        + Send
        + Sync,
>;

pub struct MockClassifier {
    pub scripted: Mutex<VecDeque<Result<ClassificationOutput, LlmError>>>,
    pub descriptions: Mutex<VecDeque<Result<String, LlmError>>>,
    /// How this mock decides, when it is allowed to look further than the payload.
    pub brain: Option<MockBrain>,
    /// What it may read from a session. A sessionless scope gets an unavailable session
    /// budget, preserving the established mock control arm.
    pub tools: Option<Arc<SessionTools>>,
    /// Optional context provider. Its budget is withheld entirely when no provider is wired.
    pub context_provider: Option<Arc<ContextProviderTools>>,
    /// Session requests the last classification actually served, in order.
    pub fetches: Mutex<Vec<FetchRequest>>,
    /// Context lookup requests the last classification actually served, in order.
    pub context_lookups: Mutex<Vec<ContextLookupRequest>>,
}

impl Default for MockClassifier {
    fn default() -> Self {
        Self {
            scripted: Mutex::new(VecDeque::new()),
            descriptions: Mutex::new(VecDeque::new()),
            brain: None,
            tools: None,
            context_provider: None,
            fetches: Mutex::new(Vec::new()),
            context_lookups: Mutex::new(Vec::new()),
        }
    }
}

impl MockClassifier {
    /// Session requests the last classification served.
    #[must_use]
    pub fn fetches(&self) -> Vec<FetchRequest> {
        self.fetches
            .lock()
            .map(|fetches| fetches.clone())
            .unwrap_or_default()
    }

    /// Context lookup requests the last classification served.
    #[must_use]
    pub fn context_lookups(&self) -> Vec<ContextLookupRequest> {
        self.context_lookups
            .lock()
            .map(|lookups| lookups.clone())
            .unwrap_or_default()
    }
}
impl Classifier for MockClassifier {
    fn classify(
        &self,
        input: &ClassificationInput,
        _roster: &[StreamSummary],
    ) -> Result<ClassificationOutput, LlmError> {
        let Some(brain) = self.brain.as_ref() else {
            return self
                .scripted
                .lock()
                .map_err(|error| LlmError::Api(error.to_string()))?
                .pop_front()
                .ok_or_else(|| LlmError::Api("no scripted classification result".to_owned()))?;
        };
        // Withheld for a scope with no session, exactly as `RigClassifier` withholds them:
        // a window run has nothing for a provider to read, and offering the tools anyway
        // earned 161 live proposals a `not indexed` answer that reads as a broken system.
        // The mock must model the same rule or no test can catch its absence.
        let tools = self
            .tools
            .clone()
            .filter(|_| input.has_session)
            .unwrap_or_else(SessionTools::unavailable);
        // Fresh budgets per classification, exactly as `RigClassifier` does. A context
        // lookup is absent rather than unavailable when no provider was configured, because
        // the live request never advertises that tool in that state.
        let session = tools.begin(&input.session_id);
        let context = self
            .context_provider
            .as_ref()
            .map(ContextProviderTools::begin);
        let output = brain(input, &session, context.as_deref());
        if let Ok(mut fetches) = self.fetches.lock() {
            *fetches = session.log();
        }
        if let Ok(mut lookups) = self.context_lookups.lock() {
            *lookups = context.map_or_else(Vec::new, |context| context.log());
        }
        output
    }

    fn describe_stream(&self, _evidence: &str) -> Result<String, LlmError> {
        self.descriptions
            .lock()
            .map_err(|error| LlmError::Api(error.to_string()))?
            .pop_front()
            .ok_or_else(|| LlmError::Api("no scripted description result".to_owned()))?
    }
}

#[cfg(test)]
mod tests {
    use super::{ClassificationExtract, ClassificationOutput, StreamChoice};

    fn extract() -> ClassificationExtract {
        ClassificationExtract {
            stream_id: None,
            new_stream_name: None,
            new_stream_description: None,
            throwaway: None,
            confidence: 0.9,
            reasoning: "test reasoning".to_owned(),
        }
    }

    #[test]
    fn a_throwaway_verdict_becomes_a_throwaway_choice() {
        // Given: the classifier judged the session trivial on inspection.
        let extracted = ClassificationExtract {
            throwaway: Some(true),
            ..extract()
        };

        // When
        let output = ClassificationOutput::try_from(extracted).unwrap();

        // Then
        assert_eq!(output.choice, StreamChoice::Throwaway);
    }

    #[test]
    fn a_throwaway_verdict_wins_over_a_stream_the_same_answer_also_names() {
        // Given: a contradictory answer naming both a stream and a throwaway verdict.
        let extracted = ClassificationExtract {
            stream_id: Some("stream-a".to_owned()),
            throwaway: Some(true),
            ..extract()
        };

        // When
        let output = ClassificationOutput::try_from(extracted).unwrap();

        // Then: junking a session is recoverable; attributing it wrongly is not.
        assert_eq!(output.choice, StreamChoice::Throwaway);
    }

    #[test]
    fn a_declined_throwaway_still_resolves_the_named_stream() {
        // Given: an answer that explicitly rejects the throwaway verdict.
        let extracted = ClassificationExtract {
            stream_id: Some("stream-a".to_owned()),
            throwaway: Some(false),
            ..extract()
        };

        // When
        let output = ClassificationOutput::try_from(extracted).unwrap();

        // Then
        assert_eq!(
            output.choice,
            StreamChoice::Existing {
                stream_id: "stream-a".to_owned()
            }
        );
    }

    #[test]
    fn a_new_stream_named_without_a_description_is_still_a_usable_verdict() {
        // Given: the model identified the work and named a stream for it, but left
        // the description out. The name is the load-bearing half — it is what
        // `is_misnamed_stream` judges and what the stream is filed under.
        let extracted = ClassificationExtract {
            new_stream_name: Some("time-tracker: classifier retry".to_owned()),
            ..extract()
        };

        // When
        let output = ClassificationOutput::try_from(extracted)
            .expect("a named stream is a usable verdict even without a description");

        // Then: the verdict survives, and the missing half stays missing. Discarding
        // it counted the session as an error and left it unassigned, which is how one
        // live pass lost 315 sessions. A placeholder description would keep the stream
        // but hide it from `tt streams describe --backfill`, so the gap is recorded as
        // a gap.
        assert_eq!(
            output.choice,
            StreamChoice::New {
                name: "time-tracker: classifier retry".to_owned(),
                description: None,
            }
        );
    }

    #[test]
    fn a_new_stream_carrying_both_halves_keeps_its_description() {
        // Given: the answer the schema asks for.
        let extracted = ClassificationExtract {
            new_stream_name: Some("time-tracker: classifier retry".to_owned()),
            new_stream_description: Some("Repairing the automatic classifier".to_owned()),
            ..extract()
        };

        // When
        let output = ClassificationOutput::try_from(extracted).unwrap();

        // Then
        assert_eq!(
            output.choice,
            StreamChoice::New {
                name: "time-tracker: classifier retry".to_owned(),
                description: Some("Repairing the automatic classifier".to_owned()),
            }
        );
    }

    #[test]
    fn a_described_stream_with_no_name_is_a_decline_rather_than_a_failure() {
        // Given: a declining answer that still filled the description field — the shape
        // the live model emits in 6% of classifications (10 of 167 over twelve hours),
        // because the schema advertises the field.
        let extracted = ClassificationExtract {
            new_stream_description: Some("Work on the classifier".to_owned()),
            ..extract()
        };

        // When
        let output = ClassificationOutput::try_from(extracted)
            .expect("a decline carrying a stray description is still a decline");

        // Then: undetermined, not an error. The load-bearing half — the name — is absent
        // in this arm and in the all-null one alike, so no stream can be minted either
        // way and the stray field cannot be what separates a decline from a malformation.
        // Erroring spent up to `MAX_PARSE_ATTEMPTS` redraws on a shape no redraw fixes,
        // counted a decline among `errors`, and armed the failure backoff.
        assert_eq!(output.choice, StreamChoice::Undetermined);
    }

    #[test]
    fn an_answer_naming_nothing_is_undetermined_rather_than_a_failure() {
        // Given: no stream, no new name, no throwaway verdict — the answer the prompt
        // asks for when the work cannot be identified.
        let extracted = extract();

        // When
        let output = ClassificationOutput::try_from(extracted)
            .expect("a model that answered `I cannot tell` answered");

        // Then: a verdict, not an error. Rejecting it counted every such answer as a
        // classifier failure, and the daemon's consecutive-failure backoff throttled
        // the classify loop to silence against a backlog of thousands.
        assert_eq!(output.choice, StreamChoice::Undetermined);
    }

    #[test]
    fn an_undetermined_verdict_is_not_a_throwaway() {
        // Given: the same answer that names nothing.
        let extracted = extract();

        // When
        let output = ClassificationOutput::try_from(extracted).unwrap();

        // Then: kept apart from `Throwaway`, which routes to the junk stream. Throwaway
        // means no attributable work exists; undetermined means work may exist and was
        // not identified. Conflating them junks real work.
        assert_ne!(output.choice, StreamChoice::Throwaway);
    }

    #[test]
    fn the_wire_payload_of_a_declined_answer_survives_the_whole_trip() {
        // Given: what the model actually sends when the prompt's own instruction —
        // answer with low confidence rather than inventing a container — is obeyed.
        // Every choice field is simply absent.
        let payload = r#"{
            "confidence": 0.1,
            "reasoning": "the prompts name no identifiable work"
        }"#;

        // When
        let extracted: ClassificationExtract = serde_json::from_str(payload).unwrap();
        let output = ClassificationOutput::try_from(extracted).unwrap();

        // Then
        assert_eq!(output.choice, StreamChoice::Undetermined);
        assert!((output.confidence - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn a_payload_missing_a_required_field_never_becomes_a_verdict_at_all() {
        // Given: output omitting `confidence`, which the schema requires. This is the
        // shape that must stay an error, and it is a different thing from a declined
        // answer: nothing the model said arrived intact, so there is no verdict to
        // read. `transport::retrying` redraws it because the next sample may fit.
        let payload = r#"{ "reasoning": "no idea" }"#;

        // When/Then: it never reaches `try_from` at all — serde refuses it first,
        // which is exactly what `AttemptFailure::Malformed` is built from.
        assert!(serde_json::from_str::<ClassificationExtract>(payload).is_err());
    }

    #[test]
    fn a_wire_payload_omitting_the_description_survives_the_whole_trip() {
        // Given: what a model actually sends when it names the work and stops. The
        // schema never asked for the pair together — schemars marks every `Option`
        // optional — so tightening it would take a `oneOf` the model would still be
        // free to answer around. The answer it did send has to survive either way.
        let payload = r#"{
            "new_stream_name": "time-tracker: classifier retry",
            "confidence": 0.9,
            "reasoning": "repairing the automatic classifier"
        }"#;

        // When
        let extracted: ClassificationExtract = serde_json::from_str(payload).unwrap();
        let output = ClassificationOutput::try_from(extracted).unwrap();

        // Then
        assert_eq!(
            output.choice,
            StreamChoice::New {
                name: "time-tracker: classifier retry".to_owned(),
                description: None,
            }
        );
    }
}
