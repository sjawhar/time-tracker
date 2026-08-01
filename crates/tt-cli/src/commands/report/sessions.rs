//! Agent-session roll-up for the report period, in both JSON and text form.
//!
//! Sessions are described by the activity they emitted inside the period, never
//! by the nominal span of their `agent_sessions` row — see the module docs on
//! `tt_db::session_activity` for the measurements behind that. Two consequences
//! are visible here: a session that merely spanned the period never reaches this
//! module at all, and `duration_ms` is an activity span rather than a
//! window-clamped one, so it cannot silently report the period's own length.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::Serialize;
use tt_db::WindowedAgentSession;

use super::format::format_duration;
use super::render::short_id;

const STARTING_PROMPT_MAX_CHARS: usize = 100;

#[derive(Debug, Serialize)]
pub struct JsonAgentSessionSummary {
    pub total: usize,
    pub by_source: BTreeMap<String, usize>,
    pub by_type: BTreeMap<String, usize>,
    pub top_sessions: Vec<JsonAgentSessionEntry>,
}

#[derive(Debug, Serialize)]
pub struct JsonAgentSessionEntry {
    pub session_id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub session_type: String,
    pub duration_ms: i64,
    pub starting_prompt: String,
}

fn truncate_starting_prompt(prompt: &str) -> String {
    // Collapse whitespace first. A prompt is free-form text that routinely contains
    // newlines, and this string is rendered as the last column of a one-line row, so a
    // raw newline breaks out of the table: the live report showed
    // `ses_03  opencode/user  106h 31m  # Generate Improvement Ideas` followed by a blank
    // line and then the rest of the prompt in column zero. Collapsing also makes the
    // character budget mean what it says, since a prompt whose first 100 bytes are mostly
    // newlines rendered as several near-empty lines.
    let collapsed = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= STARTING_PROMPT_MAX_CHARS {
        return collapsed;
    }

    let end = collapsed
        .char_indices()
        .nth(STARTING_PROMPT_MAX_CHARS)
        .map_or(collapsed.len(), |(index, _)| index);

    format!("{}...", &collapsed[..end])
}

/// Rolls the period's sessions up into counts and the five longest-active.
///
/// `sessions` has already been scoped to the period by activity, so `total` is a
/// count of sessions that *did something* in it rather than of sessions whose
/// nominal span happened to cover it.
pub fn build_agent_session_summary(sessions: &[WindowedAgentSession]) -> JsonAgentSessionSummary {
    let mut by_source: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();

    let mut top_sessions: Vec<JsonAgentSessionEntry> = sessions
        .iter()
        .map(|active| {
            let session = &active.session;
            let duration_ms = active.active_ms();
            let starting_prompt = session
                .starting_prompt
                .as_deref()
                .map(truncate_starting_prompt)
                .unwrap_or_default();

            *by_source
                .entry(session.source.as_str().to_string())
                .or_insert(0) += 1;
            *by_type
                .entry(session.session_type.as_str().to_string())
                .or_insert(0) += 1;

            JsonAgentSessionEntry {
                session_id: session.session_id.clone(),
                source: session.source.as_str().to_string(),
                session_type: session.session_type.as_str().to_string(),
                duration_ms,
                starting_prompt,
            }
        })
        .collect();

    top_sessions.sort_by(|a, b| {
        b.duration_ms
            .cmp(&a.duration_ms)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    top_sessions.truncate(5);

    JsonAgentSessionSummary {
        total: sessions.len(),
        by_source,
        by_type,
        top_sessions,
    }
}

pub fn write_agent_session_summary(output: &mut String, summary: &JsonAgentSessionSummary) {
    writeln!(output).unwrap();
    writeln!(output, "AGENT SESSIONS").unwrap();
    writeln!(output, "──────────────").unwrap();

    if summary.total == 0 {
        writeln!(output, "No agent sessions recorded.").unwrap();
        return;
    }

    writeln!(output, "Total sessions: {}", summary.total).unwrap();

    if !summary.by_source.is_empty() {
        let by_source = summary
            .by_source
            .iter()
            .map(|(source, count)| format!("{source}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "By source: {by_source}").unwrap();
    }

    if !summary.by_type.is_empty() {
        let by_type = summary
            .by_type
            .iter()
            .map(|(session_type, count)| format!("{session_type}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "By type: {by_type}").unwrap();
    }

    // The figure is `active_ms`, which is agent-active time and therefore *delegated*: it
    // sums across parallel subagents and routinely exceeds the period's wall clock (a
    // single session read 106h 31m in a 168h week). Every other section of this report
    // labels Direct and Delegated explicitly, and this column carried no label at all, so
    // a reader had no way to tell it was not attention.
    writeln!(output, "Top sessions (by delegated time):").unwrap();
    if summary.top_sessions.is_empty() {
        writeln!(output, "  (none)").unwrap();
        return;
    }

    for session in &summary.top_sessions {
        let id_short = short_id(&session.session_id);
        let duration = format_duration(session.duration_ms);
        let prompt = if session.starting_prompt.is_empty() {
            "(no prompt)"
        } else {
            session.starting_prompt.as_str()
        };
        writeln!(
            output,
            "  {id_short}  {}/{}  {duration:>6}  {prompt}",
            session.source, session.session_type
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multiline_prompt_never_breaks_out_of_its_row() {
        // The live report rendered `# Generate Improvement Ideas`, then a blank line, then
        // the rest of the prompt in column zero -- because this string is the last column
        // of a one-line row and the prompt contained newlines.
        let rendered =
            truncate_starting_prompt("# Generate Improvement Ideas\n\n**Note.** Use this");
        assert!(!rendered.contains('\n'), "got: {rendered:?}");
        assert_eq!(
            rendered, "# Generate Improvement Ideas **Note.** Use this",
            "runs of whitespace collapse to one space"
        );
    }

    #[test]
    fn the_character_budget_counts_visible_text_not_newlines() {
        // A prompt whose leading bytes are mostly newlines used to spend its budget on
        // them and render as several near-empty lines.
        let padded = format!("{}real content here", "\n".repeat(80));
        let rendered = truncate_starting_prompt(&padded);
        assert_eq!(rendered, "real content here");
    }

    #[test]
    fn a_long_prompt_is_truncated_with_an_ellipsis() {
        let rendered = truncate_starting_prompt(&"x".repeat(STARTING_PROMPT_MAX_CHARS + 50));
        assert!(rendered.ends_with("..."), "got: {rendered:?}");
        assert_eq!(rendered.chars().count(), STARTING_PROMPT_MAX_CHARS + 3);
    }

    #[test]
    fn truncation_does_not_split_a_multibyte_character() {
        // Counting chars rather than bytes: 100 CJK glyphs are 300 bytes, and slicing at
        // byte 100 would land mid-character and panic.
        let rendered = truncate_starting_prompt(&"\u{8a08}".repeat(STARTING_PROMPT_MAX_CHARS + 20));
        assert!(rendered.ends_with("..."));
        assert_eq!(rendered.chars().count(), STARTING_PROMPT_MAX_CHARS + 3);
    }
}
