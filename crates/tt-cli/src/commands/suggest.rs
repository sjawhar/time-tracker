//! Suggest command implementation.

use anyhow::{Context, Result, bail};
use chrono::SecondsFormat;
use serde::Serialize;
use tt_core::{Suggestion, is_metadata_ambiguous, suggest_from_metadata};
use tt_db::{Database, StoredEvent, Stream};
use tt_llm::{Client, LlmSuggestion, SummarizeEvent};

/// Result of the suggest command for JSON output.
#[derive(Serialize)]
struct SuggestOutput {
    stream_id: String,
    stream_name: Option<String>,
    existing_tags: Vec<String>,
    suggestion: Option<SuggestionJson>,
    source: SuggestionSource,
}

#[derive(Serialize)]
struct SuggestionJson {
    tag: String,
    reason: String,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum SuggestionSource {
    Metadata,
    Llm,
    None,
}

/// Run the suggest command.
///
/// Suggests a tag for a stream based on working directory metadata.
/// When metadata is ambiguous, uses LLM analysis if `ANTHROPIC_API_KEY` is set.
#[allow(clippy::future_not_send)] // Database uses RefCell internally
pub async fn run(db: &Database, stream_id: &str, json: bool) -> Result<()> {
    // Resolve stream
    let stream = db
        .resolve_stream(stream_id)
        .context("failed to query streams")?;

    let Some(stream) = stream else {
        bail!(
            "Stream '{stream_id}' not found.\n\nHint: Use 'tt streams' to see available stream IDs."
        );
    };

    // Get existing tags
    let existing_tags = db.get_tags(&stream.id).context("failed to get tags")?;

    // Get events for this stream
    let events = db
        .get_events_by_stream(&stream.id)
        .context("failed to get events")?;

    // Extract cwds from events
    let cwds: Vec<&str> = events.iter().filter_map(|e| e.cwd.as_deref()).collect();

    // Try metadata-based suggestion first
    let mut suggestion = suggest_from_metadata(&cwds);
    let mut source = if suggestion.is_some() {
        SuggestionSource::Metadata
    } else {
        SuggestionSource::None
    };

    // If metadata is ambiguous, try LLM
    if suggestion.is_none() && is_metadata_ambiguous(&cwds) {
        if let Some(llm_suggestion) = try_llm_suggestion(&events).await {
            suggestion = Some(Suggestion {
                tag: llm_suggestion.tags.into_iter().next().unwrap_or_default(),
                reason: llm_suggestion.reason,
            });
            source = SuggestionSource::Llm;
        }
    }

    if json {
        output_json(&stream, &existing_tags, suggestion.as_ref(), source)?;
    } else {
        output_human(&stream, &existing_tags, suggestion.as_ref(), source);
    }

    Ok(())
}

/// Try to get a suggestion from the LLM.
///
/// Returns `None` if:
/// - `ANTHROPIC_API_KEY` is not set
/// - The API call fails
/// - The response contains no tags
async fn try_llm_suggestion(events: &[StoredEvent]) -> Option<LlmSuggestion> {
    // Check for API key
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            eprintln!("Note: Set ANTHROPIC_API_KEY for LLM-powered suggestions");
            return None;
        }
    };

    // Convert events to summarizable format
    let summarize_events: Vec<SummarizeEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e.event_type.as_str(),
                "agent_tool_use" | "tmux_pane_focus" | "agent_session"
            )
        })
        .take(1000) // Limit for context window
        .map(|e| SummarizeEvent {
            event_type: e.event_type.clone(),
            timestamp: e.timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
            tool: e
                .data
                .get("tool")
                .and_then(|v| v.as_str())
                .map(String::from),
            file: e
                .data
                .get("file")
                .and_then(|v| v.as_str())
                .map(String::from),
            cwd: e.cwd.clone(),
        })
        .collect();

    if summarize_events.is_empty() {
        return None;
    }

    eprintln!("Analyzing {} events with LLM...", summarize_events.len());

    // Create client and call API
    let client = match Client::new(api_key) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: Failed to create LLM client: {e}");
            return None;
        }
    };

    match client.suggest_tags(&summarize_events).await {
        Ok(suggestion) if !suggestion.tags.is_empty() => Some(suggestion),
        Ok(_) => {
            eprintln!("LLM returned no tags");
            None
        }
        Err(e) => {
            eprintln!("Warning: LLM suggestion failed: {e}");
            None
        }
    }
}

fn output_json(
    stream: &Stream,
    existing_tags: &[String],
    suggestion: Option<&Suggestion>,
    source: SuggestionSource,
) -> Result<()> {
    let output = SuggestOutput {
        stream_id: stream.id.clone(),
        stream_name: stream.name.clone(),
        existing_tags: existing_tags.to_vec(),
        suggestion: suggestion.as_ref().map(|s| SuggestionJson {
            tag: s.tag.clone(),
            reason: s.reason.clone(),
        }),
        source,
    };

    let json_str = serde_json::to_string_pretty(&output).context("failed to serialize JSON")?;
    println!("{json_str}");
    Ok(())
}

fn output_human(
    stream: &Stream,
    existing_tags: &[String],
    suggestion: Option<&Suggestion>,
    source: SuggestionSource,
) {
    let stream_name = stream.name.as_deref().unwrap_or("<unnamed>");
    println!("Stream: {} ({})", stream.id, stream_name);

    if !existing_tags.is_empty() {
        println!("Current tags: {}", existing_tags.join(", "));
    }

    println!();

    if let Some(s) = suggestion {
        let source_label = match source {
            SuggestionSource::Metadata => "(from working directories)",
            SuggestionSource::Llm => "(from LLM analysis)",
            SuggestionSource::None => "",
        };
        println!("Suggested tag: {} {}", s.tag, source_label);
        println!("Reason: {}", s.reason);
        println!();
        println!("To apply: tt tag {} {}", stream.id, s.tag);
    } else {
        println!("No suggestion available.");
        println!("The stream has no analyzable data.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn make_stream(id: &str, name: Option<&str>) -> Stream {
        let now = Utc::now();
        Stream {
            id: id.to_string(),
            name: name.map(String::from),
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        }
    }

    fn make_event(id: &str, cwd: &str, stream_id: &str) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp: Utc::now(),
            event_type: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: json!({}),
            cwd: Some(cwd.to_string()),
            session_id: None,
            stream_id: Some(stream_id.to_string()),
            assignment_source: Some("inferred".to_string()),
        }
    }

    #[tokio::test]
    async fn test_suggest_from_stream_with_clear_project() {
        let db = Database::open_in_memory().unwrap();

        // Create a stream
        db.insert_stream(&make_stream("stream-1", Some("test")))
            .unwrap();

        // Create events with consistent cwd
        let e1 = make_event("e1", "/home/user/projects/acme-webapp/src", "stream-1");
        let e2 = make_event("e2", "/home/user/projects/acme-webapp/tests", "stream-1");
        let e3 = make_event("e3", "/home/user/projects/acme-webapp", "stream-1");

        db.insert_events(&[e1, e2, e3]).unwrap();

        // Assign events to stream
        db.assign_events_to_stream(
            &[
                ("e1".to_string(), "stream-1".to_string()),
                ("e2".to_string(), "stream-1".to_string()),
                ("e3".to_string(), "stream-1".to_string()),
            ],
            "inferred",
        )
        .unwrap();

        // Run suggest (capture output by checking JSON mode)
        run(&db, "stream-1", true).await.unwrap();
    }

    #[tokio::test]
    async fn test_suggest_nonexistent_stream() {
        let db = Database::open_in_memory().unwrap();
        let result = run(&db, "nonexistent", false).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
