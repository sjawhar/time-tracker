//! The fetch layer, exposed to a live model as rig tools.
//!
//! Each tool is a thin adapter: it parses the model's arguments into a
//! [`FetchRequest`], hands it to the [`FetchSession`], and renders whatever comes
//! back. Budget accounting and injection filtering happen inside the session, so a
//! tool cannot skip either — and so the mock classifier, which never builds these
//! tools, still exercises the same guards.

use std::sync::Arc;

use rig_core::tool::Tool;
use serde::Deserialize;

use crate::context_provider::{ContextLookupRequest, ContextLookupSession};
use crate::fetch::{FetchRequest, FetchSession, MAX_FETCH_CALLS, MAX_MESSAGES_PER_PAGE};

/// A tool never fails: an unanswerable fetch is rendered as text the model can act on,
/// because a broken tool call would abort a classification that could still be made
/// from the payload alone.
#[derive(Debug, thiserror::Error)]
#[error("unreachable: session fetch tools report problems as text")]
pub struct ToolNeverFails;

/// Arguments for [`OverviewTool`]. The model supplies nothing; the session it is
/// classifying is fixed for the whole call.
#[derive(Debug, Deserialize)]
pub struct OverviewArgs {}

/// Fetches the session's summary, timing and counts.
pub struct OverviewTool {
    session: Arc<FetchSession>,
}

impl OverviewTool {
    pub(crate) const fn new(session: Arc<FetchSession>) -> Self {
        Self { session }
    }
}

impl Tool for OverviewTool {
    const NAME: &'static str = "session_overview";

    type Error = ToolNeverFails;
    type Args = OverviewArgs;
    type Output = String;

    fn description(&self) -> String {
        "Look up the session being classified: its own summary of what it did, when it \
         ran, which machine and directory it ran in, and how many messages and tool \
         calls it made. Use this first when the prompts you were given are short or \
         say nothing about the work. Sessions from Claude carry no summary; sessions \
         from OpenCode always do."
            .to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    // The trait requires `async fn`; clippy 1.98 flags the missing .await.
    #[allow(unknown_lints)] // older local clippy predates the 1.98 lint below
    #[allow(clippy::unused_async_trait_impl)]
    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(self
            .session
            .dispatch(&FetchRequest::Overview {
                session_id: self.session.session_id().to_owned(),
            })
            .rendered())
    }
}

/// Arguments for [`MessagesTool`].
#[derive(Debug, Deserialize)]
pub struct MessagesArgs {
    /// Index of the first message wanted; 0 is the session's opening message.
    #[serde(default)]
    pub offset: usize,
}

/// Fetches a page of the session's user messages at full stored length.
pub struct MessagesTool {
    session: Arc<FetchSession>,
}

impl MessagesTool {
    pub(crate) const fn new(session: Arc<FetchSession>) -> Self {
        Self { session }
    }
}

impl Tool for MessagesTool {
    const NAME: &'static str = "session_messages";

    type Error = ToolNeverFails;
    type Args = MessagesArgs;
    type Output = String;

    fn description(&self) -> String {
        format!(
            "Read the session's user messages at full length, {MAX_MESSAGES_PER_PAGE} at \
             a time. The prompts in your payload are truncated; these are not. Text \
             injected by the agent harness is removed before you see it, so a page may \
             return fewer messages than you asked for, or none. The reply states how \
             many messages the session holds in total."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Index of the first message to read; 0 is the opening message.",
                }
            }
        })
    }

    // The trait requires `async fn`; clippy 1.98 flags the missing .await.
    #[allow(unknown_lints)] // older local clippy predates the 1.98 lint below
    #[allow(clippy::unused_async_trait_impl)]
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(self
            .session
            .dispatch(&FetchRequest::Messages {
                session_id: self.session.session_id().to_owned(),
                offset: args.offset,
                limit: MAX_MESSAGES_PER_PAGE,
            })
            .rendered())
    }
}

/// Arguments for [`ContextLookupTool`].
#[derive(Debug, Deserialize)]
pub struct ContextLookupArgs {
    /// An unfamiliar person, organization, project, codename, or relationship.
    pub query: String,
}

/// Looks up operator-configured knowledge for the query named by the model.
pub struct ContextLookupTool {
    session: Arc<ContextLookupSession>,
}

impl ContextLookupTool {
    pub(crate) const fn new(session: Arc<ContextLookupSession>) -> Self {
        Self { session }
    }
}

impl Tool for ContextLookupTool {
    const NAME: &'static str = "context_lookup";

    type Error = ToolNeverFails;
    type Args = ContextLookupArgs;
    type Output = String;

    fn description(&self) -> String {
        "Look up operator-configured knowledge to resolve an unfamiliar person, organization, \
         project, codename, or relationship before choosing a stream. Query only when the \
         result would change the initiative you assign."
            .to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The unfamiliar name, codename, or question to resolve.",
                }
            },
            "required": ["query"],
        })
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "rig's Tool trait requires async even though dispatch is synchronous"
    )]
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(self
            .session
            .dispatch(&ContextLookupRequest { query: args.query })
            .rendered())
    }
}

/// What the model is told about its fetch tools, appended to the classification prompt.
///
/// The sentence about what happens at the bound is load-bearing and is stated as
/// withdrawal rather than as a report, because that is now the truth: once the budget is
/// spent, `WithdrawSpentTools` stops advertising these tools at all. Telling the model
/// they would merely "report the budget spent" invited it to call again and find out,
/// and each of those calls cost a model turn until rig's own bound hard-errored.
pub fn preamble() -> String {
    format!(
        "You may look further into the session before answering. `session_overview` \
         returns its summary and shape; `session_messages` returns its user messages at \
         full length. Use them when what you were given is too thin to identify the \
         work — a prompt like \"list a file\" names no initiative — and skip them when \
         it is already clear. You may call them at most {MAX_FETCH_CALLS} times in \
         total; after that they are withdrawn and you must answer from what you have. \
         If you still cannot identify the work, answer with low confidence rather than \
         inventing a stream to hold it."
    )
}

/// Appends the context-lookup instructions only when a provider is wired.
///
/// Keeping [`preamble`] separate preserves the no-provider text byte-for-byte. A
/// sessionless scope receives only these instructions, never the session-fetch preamble
/// for tools it cannot use.
pub fn preamble_with_context_lookup(has_session: bool) -> String {
    let mut text = if has_session {
        preamble()
    } else {
        String::new()
    };
    if !text.is_empty() {
        text.push(' ');
    }
    text.push_str(
        "You may resolve unfamiliar people, organizations, projects, codenames, and \
         relationships through `context_lookup` before choosing a stream. Use it only when \
         grounding would change the initiative you choose; it has a limit of four context \
         lookups, after which it is withdrawn.",
    );
    text
}
