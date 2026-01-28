//! Claude API integration for the time tracker.
//!
//! Provides LLM-powered features such as:
//! - Automatic time entry generation from events
//! - Natural language queries over activity data

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default request timeout for API calls.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const TAG_SUGGESTION_MAX_TOKENS: u32 = 400;
const TAG_SUGGESTION_TEMPERATURE: f32 = 0.2;

/// LLM client errors.
#[derive(Debug, Error)]
pub enum LlmError {
    /// The provided API key was invalid.
    #[error("invalid API key: {reason}")]
    InvalidApiKey { reason: &'static str },
    /// Failed to build HTTP client.
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(#[source] reqwest::Error),
    /// HTTP request failed.
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// API returned an error response.
    #[error("API error: {message}")]
    Api { message: String },
    /// Failed to parse response.
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

/// Claude API client.
///
/// # Thread Safety
///
/// The client is safe to clone and share across threads. Each clone shares
/// the underlying HTTP connection pool.
pub struct Client {
    http: reqwest::Client,
    api_key: String,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("api_key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates a new client with the given API key.
    ///
    /// # Errors
    ///
    /// Returns an error if the API key is empty or whitespace-only, or if
    /// the HTTP client fails to build.
    pub fn new(api_key: impl Into<String>) -> Result<Self, LlmError> {
        let api_key = api_key.into();

        // Validate API key
        if api_key.is_empty() {
            return Err(LlmError::InvalidApiKey {
                reason: "API key cannot be empty",
            });
        }
        if api_key.trim().is_empty() {
            return Err(LlmError::InvalidApiKey {
                reason: "API key cannot be whitespace-only",
            });
        }

        // Build HTTP client with timeout
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(LlmError::ClientBuild)?;

        Ok(Self { http, api_key })
    }

    /// Suggest tags for a stream using the Claude API.
    pub async fn suggest_tags(
        &self,
        model: &str,
        input: &TagSuggestionRequest,
    ) -> Result<TagSuggestion, LlmError> {
        let prompt = build_tag_prompt(input);
        let request = MessageRequest {
            model: model.to_string(),
            max_tokens: TAG_SUGGESTION_MAX_TOKENS,
            temperature: TAG_SUGGESTION_TEMPERATURE,
            messages: vec![Message {
                role: "user",
                content: prompt,
            }],
        };

        let response = self
            .http
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(parse_api_error(&body).unwrap_or_else(|| LlmError::Api {
                message: format!("status {status}: {body}"),
            }));
        }

        let payload: MessageResponse = serde_json::from_str(&body)
            .map_err(|err| LlmError::InvalidResponse(err.to_string()))?;
        let text = extract_text(payload.content)?;
        let suggestion = parse_tag_suggestion(&text)?;
        Ok(normalize_suggestion(suggestion))
    }
}

/// Input context for tag suggestions.
#[derive(Debug, Clone)]
pub struct TagSuggestionRequest {
    pub stream_id: String,
    pub stream_name: Option<String>,
    pub time_range: Option<(String, String)>,
    pub event_kinds: Vec<(String, usize)>,
    pub sources: Vec<(String, usize)>,
    pub cwd_samples: Vec<String>,
    pub file_samples: Vec<String>,
    pub tool_samples: Vec<String>,
    pub session_ids: Vec<String>,
}

/// Suggested tags and summary for a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSuggestion {
    pub summary: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MessageRequest {
    model: String,
    max_tokens: u32,
    temperature: f32,
    messages: Vec<Message>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
}

fn extract_text(blocks: Vec<ContentBlock>) -> Result<String, LlmError> {
    let mut pieces = Vec::new();
    for block in blocks {
        let ContentBlock::Text { text } = block;
        pieces.push(text);
    }
    if pieces.is_empty() {
        return Err(LlmError::InvalidResponse(
            "missing text content".to_string(),
        ));
    }
    Ok(pieces.join("\n"))
}

fn parse_api_error(body: &str) -> Option<LlmError> {
    #[derive(Deserialize)]
    struct ErrorPayload {
        error: ErrorDetails,
    }

    #[derive(Deserialize)]
    struct ErrorDetails {
        message: String,
    }

    serde_json::from_str::<ErrorPayload>(body)
        .ok()
        .map(|payload| LlmError::Api {
            message: payload.error.message,
        })
}

fn build_tag_prompt(input: &TagSuggestionRequest) -> String {
    let mut lines = Vec::new();
    lines.push(
        "You are a time-tracking assistant. Suggest concise tags for a work stream.".to_string(),
    );
    lines
        .push("Return strict JSON: {\"summary\":\"...\",\"tags\":[\"tag1\",\"tag2\"]}".to_string());
    lines.push("Rules:".to_string());
    lines.push(
        "- Tags are short, lower-case, kebab-case or colon-delimited (e.g. project:foo)."
            .to_string(),
    );
    lines.push("- Provide 2-6 tags. Avoid duplicates.".to_string());
    lines.push(
        "- Do not include secrets, credentials, or file contents in the summary.".to_string(),
    );
    lines.push(String::new());
    lines.push(format!("stream_id: {}", input.stream_id));
    if let Some(name) = &input.stream_name {
        lines.push(format!("stream_name: {name}"));
    }
    if let Some((start, end)) = &input.time_range {
        lines.push(format!("time_range: {start}..{end}"));
    }
    if !input.event_kinds.is_empty() {
        let rendered = input
            .event_kinds
            .iter()
            .map(|(kind, count)| format!("{kind}({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("event_kinds: {rendered}"));
    }
    if !input.sources.is_empty() {
        let rendered = input
            .sources
            .iter()
            .map(|(source, count)| format!("{source}({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("sources: {rendered}"));
    }
    if !input.cwd_samples.is_empty() {
        lines.push(format!("cwd_samples: {}", input.cwd_samples.join(", ")));
    }
    if !input.file_samples.is_empty() {
        lines.push(format!("file_samples: {}", input.file_samples.join(", ")));
    }
    if !input.tool_samples.is_empty() {
        lines.push(format!("tool_samples: {}", input.tool_samples.join(", ")));
    }
    if !input.session_ids.is_empty() {
        lines.push(format!("session_ids: {}", input.session_ids.join(", ")));
    }
    lines.join("\n")
}

fn parse_tag_suggestion(text: &str) -> Result<TagSuggestion, LlmError> {
    #[derive(serde::Deserialize)]
    struct Payload {
        summary: String,
        tags: Vec<String>,
    }

    let payload: Payload =
        serde_json::from_str(text).map_err(|err| LlmError::InvalidResponse(err.to_string()))?;
    Ok(TagSuggestion {
        summary: payload.summary,
        tags: payload.tags,
    })
}

fn normalize_suggestion(mut suggestion: TagSuggestion) -> TagSuggestion {
    let mut seen = HashSet::new();
    let mut tags = Vec::new();
    for tag in suggestion.tags.drain(..) {
        let trimmed = tag.trim().to_lowercase();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.clone()) {
            tags.push(trimmed);
        }
    }
    suggestion.summary = suggestion.summary.trim().to_string();
    suggestion.tags = tags;
    suggestion
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_rejects_empty_api_key() {
        assert!(matches!(
            Client::new(""),
            Err(LlmError::InvalidApiKey { .. })
        ));
    }

    #[test]
    fn client_rejects_whitespace_api_key() {
        assert!(matches!(
            Client::new("   "),
            Err(LlmError::InvalidApiKey { .. })
        ));
    }

    #[test]
    fn client_accepts_valid_api_key() {
        assert!(Client::new("sk-ant-api03-valid-key").is_ok());
    }

    #[test]
    fn client_debug_redacts_api_key() {
        let client = Client::new("secret-key").unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("secret-key"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn build_tag_prompt_includes_context_fields() {
        let input = TagSuggestionRequest {
            stream_id: "stream-123".to_string(),
            stream_name: Some("Auth flow".to_string()),
            time_range: Some((
                "2025-01-01T00:00:00Z".to_string(),
                "2025-01-01T01:00:00Z".to_string(),
            )),
            event_kinds: vec![("tmux_pane_focus".to_string(), 3)],
            sources: vec![("remote.tmux".to_string(), 3)],
            cwd_samples: vec!["/repo/auth".to_string()],
            file_samples: vec!["src/login.rs".to_string()],
            tool_samples: vec!["rg".to_string()],
            session_ids: vec!["session-1".to_string()],
        };
        let prompt = build_tag_prompt(&input);
        assert!(prompt.contains("stream_id: stream-123"));
        assert!(prompt.contains("stream_name: Auth flow"));
        assert!(prompt.contains("time_range: 2025-01-01T00:00:00Z..2025-01-01T01:00:00Z"));
        assert!(prompt.contains("event_kinds: tmux_pane_focus(3)"));
        assert!(prompt.contains("sources: remote.tmux(3)"));
        assert!(prompt.contains("cwd_samples: /repo/auth"));
        assert!(prompt.contains("file_samples: src/login.rs"));
        assert!(prompt.contains("tool_samples: rg"));
        assert!(prompt.contains("session_ids: session-1"));
    }

    #[test]
    fn parse_tag_suggestion_accepts_json() {
        let input = r#"{"summary":"Worked on login flow","tags":["auth","project:time-tracker"]}"#;
        let parsed = parse_tag_suggestion(input).unwrap();
        assert_eq!(parsed.summary, "Worked on login flow");
        assert_eq!(parsed.tags, vec!["auth", "project:time-tracker"]);
    }

    #[test]
    fn parse_tag_suggestion_rejects_invalid_json() {
        let err = parse_tag_suggestion("not-json").unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)));
    }
}
