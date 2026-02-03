//! Claude API integration for the time tracker.
//!
//! Provides LLM-powered features such as:
//! - Tag suggestion based on session events
//! - Automatic time entry generation from events
//! - Natural language queries over activity data

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

/// Default request timeout for API calls.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Claude API endpoint.
const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Model to use for tag suggestions.
const MODEL: &str = "claude-sonnet-4-20250514";

/// System prompt for tag suggestion.
const SYSTEM_PROMPT: &str = r#"You analyze coding session events to suggest project tags.

Given a list of events (tool usage, file edits, directory changes), suggest 1-3 project tags.
Tags should be project names derived from directories or files. Look for:
- Repository or project directory names
- Package names from file paths
- Consistent patterns in working directories

Respond ONLY with valid JSON in this exact format:
{"tags": ["tag1"], "reason": "brief explanation"}

Do not include any text before or after the JSON."#;

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

/// Event metadata for LLM summarization (privacy-filtered).
///
/// Contains only structural information about events, not actual content.
#[derive(Debug, Clone, Serialize)]
pub struct SummarizeEvent {
    /// Event type (e.g., "`agent_tool_use`", "`tmux_pane_focus`").
    pub event_type: String,
    /// When the event occurred (ISO 8601).
    pub timestamp: String,
    /// Tool name if this is a tool use event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// File path if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Working directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// LLM-generated tag suggestion.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmSuggestion {
    /// Suggested tags (usually 1-3).
    pub tags: Vec<String>,
    /// Explanation for why these tags were suggested.
    pub reason: String,
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

    /// Suggests tags based on session events.
    ///
    /// Sends event metadata to Claude API and returns suggested tags.
    /// Events are privacy-filtered to only include structural information.
    ///
    /// # Errors
    ///
    /// Returns an error if the API call fails or the response cannot be parsed.
    pub async fn suggest_tags(&self, events: &[SummarizeEvent]) -> Result<LlmSuggestion, LlmError> {
        // Build user prompt with event data
        let user_prompt = build_user_prompt(events);

        // Make API request
        let request_body = json!({
            "model": MODEL,
            "max_tokens": 256,
            "system": SYSTEM_PROMPT,
            "messages": [{
                "role": "user",
                "content": user_prompt
            }]
        });

        let response = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        // Check for error status
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                message: format!("{status}: {error_text}"),
            });
        }

        // Parse response
        let response_body: serde_json::Value = response.json().await?;

        // Extract text content from Claude's response
        let content = response_body["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item["text"].as_str())
            .ok_or_else(|| LlmError::InvalidResponse("missing content text".to_string()))?;

        // Parse the JSON from Claude's response
        parse_suggestion_response(content)
    }
}

/// Build the user prompt from event data.
fn build_user_prompt(events: &[SummarizeEvent]) -> String {
    let mut lines = Vec::new();
    lines.push("Here are the session events:".to_string());
    lines.push(String::new());

    for event in events.iter().take(100) {
        // Limit to 100 events for context
        let mut parts = vec![event.timestamp.clone(), event.event_type.clone()];

        if let Some(tool) = &event.tool {
            parts.push(format!("tool={tool}"));
        }
        if let Some(file) = &event.file {
            parts.push(format!("file={file}"));
        }
        if let Some(cwd) = &event.cwd {
            parts.push(format!("cwd={cwd}"));
        }

        lines.push(parts.join(" | "));
    }

    if events.len() > 100 {
        lines.push(format!("... and {} more events", events.len() - 100));
    }

    lines.push(String::new());
    lines.push("Suggest project tags based on these events.".to_string());

    lines.join("\n")
}

/// Parse Claude's response text into an `LlmSuggestion`.
fn parse_suggestion_response(content: &str) -> Result<LlmSuggestion, LlmError> {
    // Try direct JSON parse first
    if let Ok(suggestion) = serde_json::from_str::<LlmSuggestion>(content) {
        return Ok(suggestion);
    }

    // Try to extract JSON from markdown code block
    let json_content = if content.contains("```json") {
        content
            .split("```json")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .map(str::trim)
    } else if content.contains("```") {
        content
            .split("```")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .map(str::trim)
    } else {
        None
    };

    if let Some(json_str) = json_content {
        if let Ok(suggestion) = serde_json::from_str::<LlmSuggestion>(json_str) {
            return Ok(suggestion);
        }
    }

    // Try to find JSON-like content with braces
    if let Some(start) = content.find('{') {
        if let Some(end) = content.rfind('}') {
            let json_str = &content[start..=end];
            if let Ok(suggestion) = serde_json::from_str::<LlmSuggestion>(json_str) {
                return Ok(suggestion);
            }
        }
    }

    Err(LlmError::InvalidResponse(format!(
        "could not parse JSON from response: {content}"
    )))
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

    // ========== Response Parsing Tests ==========

    #[test]
    fn parse_valid_json_response() {
        let response =
            r#"{"tags": ["acme-webapp"], "reason": "Most events in /projects/acme-webapp"}"#;
        let result = parse_suggestion_response(response).unwrap();
        assert_eq!(result.tags, vec!["acme-webapp"]);
        assert!(result.reason.contains("acme-webapp"));
    }

    #[test]
    fn parse_json_in_markdown_code_block() {
        let response = r#"Here's my analysis:

```json
{"tags": ["time-tracker"], "reason": "Working directory pattern"}
```

Let me know if you need more details."#;
        let result = parse_suggestion_response(response).unwrap();
        assert_eq!(result.tags, vec!["time-tracker"]);
    }

    #[test]
    fn parse_json_in_plain_code_block() {
        let response = r#"```
{"tags": ["my-project"], "reason": "Based on file paths"}
```"#;
        let result = parse_suggestion_response(response).unwrap();
        assert_eq!(result.tags, vec!["my-project"]);
    }

    #[test]
    fn parse_json_with_surrounding_text() {
        let response = r#"Based on the events, I suggest: {"tags": ["project-x", "client-work"], "reason": "Multiple directories but project-x is most common"}"#;
        let result = parse_suggestion_response(response).unwrap();
        assert_eq!(result.tags, vec!["project-x", "client-work"]);
    }

    #[test]
    fn parse_multiple_tags() {
        let response =
            r#"{"tags": ["tag1", "tag2", "tag3"], "reason": "Multiple projects detected"}"#;
        let result = parse_suggestion_response(response).unwrap();
        assert_eq!(result.tags.len(), 3);
    }

    #[test]
    fn parse_empty_tags() {
        let response = r#"{"tags": [], "reason": "No clear project detected"}"#;
        let result = parse_suggestion_response(response).unwrap();
        assert!(result.tags.is_empty());
    }

    #[test]
    fn parse_invalid_response_returns_error() {
        let response = "I couldn't determine any tags from this data.";
        let result = parse_suggestion_response(response);
        assert!(result.is_err());
    }

    // ========== Prompt Building Tests ==========

    #[test]
    fn build_prompt_includes_events() {
        let events = vec![
            SummarizeEvent {
                event_type: "agent_tool_use".to_string(),
                timestamp: "2025-01-15T10:00:00Z".to_string(),
                tool: Some("Edit".to_string()),
                file: Some("/project/src/main.rs".to_string()),
                cwd: Some("/project".to_string()),
            },
            SummarizeEvent {
                event_type: "tmux_pane_focus".to_string(),
                timestamp: "2025-01-15T10:01:00Z".to_string(),
                tool: None,
                file: None,
                cwd: Some("/project".to_string()),
            },
        ];

        let prompt = build_user_prompt(&events);
        assert!(prompt.contains("agent_tool_use"));
        assert!(prompt.contains("tmux_pane_focus"));
        assert!(prompt.contains("/project"));
        assert!(prompt.contains("tool=Edit"));
        assert!(prompt.contains("Suggest project tags"));
    }

    #[test]
    fn build_prompt_truncates_at_100_events() {
        let events: Vec<SummarizeEvent> = (0..150)
            .map(|i| SummarizeEvent {
                event_type: "test".to_string(),
                timestamp: format!("2025-01-15T10:{i:02}:00Z"),
                tool: None,
                file: None,
                cwd: Some("/project".to_string()),
            })
            .collect();

        let prompt = build_user_prompt(&events);
        assert!(prompt.contains("... and 50 more events"));
    }
}
