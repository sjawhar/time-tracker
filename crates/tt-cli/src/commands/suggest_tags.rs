//! Suggest tags for a stream using the Claude API.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::Value;
use tt_db::{Database, EventRecord};
use tt_llm::{Client, TagSuggestionRequest};

use crate::Config;

const DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";
const MAX_SAMPLE_COUNT: usize = 8;
const MAX_COUNTED_ITEMS: usize = 6;

#[derive(Debug, Args)]
pub struct SuggestTagsArgs {
    /// Stream ID to tag.
    pub stream_id: String,
}

pub fn run<W: Write>(writer: &mut W, args: &SuggestTagsArgs, config: &Config) -> Result<()> {
    let api_key = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing Claude API key (set TT_API_KEY or config.toml)"))?;

    let db = Database::open(&config.database_path)
        .with_context(|| format!("failed to open {}", config.database_path.display()))?;
    let events = db.list_events_for_stream(&args.stream_id)?;
    if events.is_empty() {
        bail!("no events found for stream {}", args.stream_id);
    }

    let stream_name = db
        .list_streams()?
        .into_iter()
        .find(|stream| stream.id == args.stream_id)
        .and_then(|stream| stream.name);

    let request = build_request(&args.stream_id, stream_name, &events);
    let client = Client::new(api_key.to_string()).context("failed to create LLM client")?;
    let runtime = tokio::runtime::Runtime::new().context("failed to initialize tokio runtime")?;
    let suggestion = runtime
        .block_on(client.suggest_tags(DEFAULT_MODEL, &request))
        .context("failed to suggest tags")?;

    let output = serde_json::json!({
        "stream_id": args.stream_id,
        "summary": suggestion.summary,
        "tags": suggestion.tags,
    });
    writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
    Ok(())
}

fn build_request(
    stream_id: &str,
    stream_name: Option<String>,
    events: &[EventRecord],
) -> TagSuggestionRequest {
    let mut event_counts: HashMap<String, usize> = HashMap::new();
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    let mut cwd_counts: HashMap<String, usize> = HashMap::new();
    let mut file_counts: HashMap<String, usize> = HashMap::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut session_counts: HashMap<String, usize> = HashMap::new();

    for event in events {
        *event_counts.entry(event.kind.clone()).or_insert(0) += 1;
        *source_counts.entry(event.source.clone()).or_insert(0) += 1;

        if let Some(cwd) = event.cwd.as_deref() {
            record_sample(&mut cwd_counts, cwd);
        }
        if let Some(session_id) = event.session_id.as_deref() {
            record_sample(&mut session_counts, session_id);
        }

        let data: Value = serde_json::from_str(&event.data).unwrap_or(Value::Null);
        if let Some(cwd) = data.get("cwd").and_then(Value::as_str) {
            record_sample(&mut cwd_counts, cwd);
        }
        if let Some(file) = data.get("file").and_then(Value::as_str) {
            record_sample(&mut file_counts, file);
        }
        if let Some(path) = data.get("path").and_then(Value::as_str) {
            record_sample(&mut file_counts, path);
        }
        if let Some(tool) = data.get("tool").and_then(Value::as_str) {
            record_sample(&mut tool_counts, tool);
        }
        if let Some(session_id) = data.get("session_id").and_then(Value::as_str) {
            record_sample(&mut session_counts, session_id);
        }
    }

    TagSuggestionRequest {
        stream_id: stream_id.to_string(),
        stream_name,
        time_range: events
            .first()
            .zip(events.last())
            .map(|(first, last)| (first.timestamp.clone(), last.timestamp.clone())),
        event_kinds: top_counts(event_counts, MAX_COUNTED_ITEMS),
        sources: top_counts(source_counts, MAX_COUNTED_ITEMS),
        cwd_samples: top_samples(cwd_counts, MAX_SAMPLE_COUNT),
        file_samples: top_samples(file_counts, MAX_SAMPLE_COUNT),
        tool_samples: top_samples(tool_counts, MAX_SAMPLE_COUNT),
        session_ids: top_samples(session_counts, MAX_SAMPLE_COUNT),
    }
}

fn record_sample(counts: &mut HashMap<String, usize>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    *counts.entry(trimmed.to_string()).or_insert(0) += 1;
}

fn top_counts(mut counts: HashMap<String, usize>, limit: usize) -> Vec<(String, usize)> {
    let mut entries: Vec<(String, usize)> = counts.drain().collect();
    entries.sort_by(|(a_key, a_count), (b_key, b_count)| {
        b_count.cmp(a_count).then_with(|| a_key.cmp(b_key))
    });
    entries.truncate(limit);
    entries
}

fn top_samples(counts: HashMap<String, usize>, limit: usize) -> Vec<String> {
    top_counts(counts, limit)
        .into_iter()
        .map(|(value, _)| value)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_collects_signal_fields() {
        let events = vec![
            EventRecord {
                id: "event-1".to_string(),
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                kind: "tmux_pane_focus".to_string(),
                source: "remote.tmux".to_string(),
                schema_version: 1,
                data: r#"{"cwd":"/repo","file":"src/lib.rs"}"#.to_string(),
                cwd: Some("/repo".to_string()),
                session_id: Some("sess-1".to_string()),
                stream_id: Some("stream-1".to_string()),
                assignment_source: None,
            },
            EventRecord {
                id: "event-2".to_string(),
                timestamp: "2025-01-01T00:01:00Z".to_string(),
                kind: "agent_tool_use".to_string(),
                source: "remote.agent".to_string(),
                schema_version: 1,
                data: r#"{"tool":"rg","session_id":"sess-2"}"#.to_string(),
                cwd: None,
                session_id: None,
                stream_id: Some("stream-1".to_string()),
                assignment_source: None,
            },
        ];

        let request = build_request("stream-1", Some("Auth".to_string()), &events);
        assert_eq!(request.stream_id, "stream-1");
        assert_eq!(request.stream_name.as_deref(), Some("Auth"));
        assert_eq!(
            request.time_range.as_ref().unwrap().0,
            "2025-01-01T00:00:00Z"
        );
        assert!(
            request
                .event_kinds
                .iter()
                .any(|(kind, count)| kind == "tmux_pane_focus" && *count == 1)
        );
        assert!(
            request
                .sources
                .iter()
                .any(|(source, count)| source == "remote.tmux" && *count == 1)
        );
        assert!(request.cwd_samples.contains(&"/repo".to_string()));
        assert!(request.file_samples.contains(&"src/lib.rs".to_string()));
        assert!(request.tool_samples.contains(&"rg".to_string()));
        assert!(request.session_ids.contains(&"sess-1".to_string()));
        assert!(request.session_ids.contains(&"sess-2".to_string()));
    }
}
