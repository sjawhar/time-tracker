//! Claude API integration for the time tracker.
//!
//! Provides LLM-powered features such as:
//! - Automatic time entry generation from events
//! - Natural language queries over activity data

use std::fmt;
use std::time::Duration;

use thiserror::Error;

/// Default request timeout for API calls.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

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
}
