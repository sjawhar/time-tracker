//! Status command for showing event collection status.
//!
//! This module displays the most recent event timestamp per source,
//! helping users verify that event collection is working.

use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use tt_db::{ClassifierHealthState, Database};

use crate::Config;
use crate::commands::report::format_duration;
use crate::commands::util::format_age;
use crate::drift::{MachineFreshness, Verdict, compute_verdict};

/// Formats and prints the status output.
///
/// Returns the formatted output string (for testing).
pub fn format_status(db: &Database, config: &Config) -> Result<String> {
    let now = Utc::now();
    let verdict = compute_verdict(db, config, now).context("failed to compute status verdict")?;
    let statuses = db.get_last_event_per_source()?;

    // A one-shot invocation announces every dangling link, every time. Only the daemon
    // dedupes, because it recomputes on each `db_version` change and repeating an
    // unchanged set buried a real defect in 92% noise. Here there is no previous set to
    // compare against, and a human running `tt status` is exactly who needs telling.
    for stream in &verdict.dangling_stream_links {
        tracing::warn!(
            stream = stream.as_str(),
            "streams.md links a stream that no longer exists; skipping it in the verdict"
        );
    }

    // Same one-shot rule for a dead input, and it earns the repetition more than a stale
    // priority link does: this is an input to direct time, and the whole defect was that
    // nothing said so for nine days.
    for source in &verdict.stale_event_sources {
        tracing::warn!(
            event_type = %source.event_type,
            emitter = source.emitter,
            last_seen = %source.last_seen.to_rfc3339_opts(SecondsFormat::Secs, true),
            "a local event source has stopped reporting; direct time is missing an input"
        );
    }

    let mut output = render_verdict(&verdict, now)?;
    writeln!(output).context("failed to format status spacer")?;
    writeln!(output, "Database: {}", config.database_path.display())
        .context("failed to format database path")?;

    if statuses.is_empty() {
        output.push_str("\nNo events recorded yet.\n");
    } else {
        output.push_str("\nSources:\n");
        for status in statuses {
            let timestamp = status
                .last_timestamp
                .to_rfc3339_opts(SecondsFormat::Secs, true);
            writeln!(output, "  {}:  {}", status.source, timestamp)
                .context("failed to format source status")?;
        }
    }

    Ok(output)
}

/// Runs the status command.
pub fn run(db: &Database, config: &Config) -> Result<()> {
    let output = format_status(db, config)?;
    print!("{output}");
    Ok(())
}

fn render_verdict(verdict: &Verdict, now: DateTime<Utc>) -> Result<String> {
    let mut output = String::new();
    let current = verdict.current_stream.as_ref().map_or_else(
        || "no active stream".to_string(),
        |stream| {
            format!(
                "{} — {}",
                stream.name,
                format_duration((now - stream.since).num_milliseconds())
            )
        },
    );
    let alignment = match verdict.aligned {
        Some(true) => "   ✓ top priority",
        Some(false) => "   ⚠ not top priority",
        None => "",
    };
    writeln!(output, "NOW   {current}{alignment}").context("failed to format current stream")?;
    match &verdict.top_todo {
        Some(todo) => match &todo.stream_slug {
            Some(stream_slug) => writeln!(output, "TOP   {} ({stream_slug})", todo.text),
            None => writeln!(output, "TOP   {} (top todo has no stream link)", todo.text),
        },
        None => writeln!(output, "TOP   no top todo"),
    }
    .context("failed to format top todo")?;
    match &verdict.wip.wind_down_candidate {
        Some(candidate) => writeln!(
            output,
            "WIP   {}/{} — consider winding down: {candidate}",
            verdict.wip.in_flight.len(),
            verdict.wip.limit
        ),
        None => writeln!(
            output,
            "WIP   {}/{}",
            verdict.wip.in_flight.len(),
            verdict.wip.limit
        ),
    }
    .context("failed to format WIP status")?;
    let mut details = vec![format!("{} proposals pending", verdict.pending_proposals)];
    details.extend(
        verdict
            .machines
            .iter()
            .map(|machine| machine_freshness(machine, now))
            .collect::<Result<Vec<_>>>()?,
    );
    writeln!(output, "      {}", details.join(" · "))
        .context("failed to format verdict details")?;
    writeln!(output, "      {}", classifier_status(verdict, now))
        .context("failed to format classifier status")?;
    for source in &verdict.stale_event_sources {
        writeln!(
            output,
            "      ⚠ {} stale — {} last reported {} ago ({})",
            source.event_type,
            source.emitter,
            format_age(source.last_seen, now),
            source.last_seen.to_rfc3339_opts(SecondsFormat::Secs, true)
        )
        .context("failed to format stale event source")?;
    }
    Ok(output)
}

fn machine_freshness(machine: &MachineFreshness, now: DateTime<Utc>) -> Result<String> {
    match &machine.last_sync_at {
        Some(last_sync_at) => {
            let timestamp = DateTime::parse_from_rfc3339(last_sync_at)
                .with_context(|| format!("invalid sync timestamp for machine {}", machine.label))?
                .with_timezone(&Utc);
            Ok(format!(
                "{} {}",
                machine.label,
                format_duration((now - timestamp).num_milliseconds())
            ))
        }
        None => Ok(format!("{} never synced", machine.label)),
    }
}

fn classifier_status(verdict: &Verdict, now: DateTime<Utc>) -> String {
    // An unconfigured classifier does not fail, it simply never runs, so its
    // `consecutive_failures` stays 0 and its `last_success_at` keeps whatever the last
    // configured run left behind. Reading only those two fields therefore falls through
    // to the reassuring branch and prints `classifier ok · last run 30m ago` while nothing
    // is being classified at all — the same shape as the nine-day watcher outage, where
    // the product reported confident numbers and said nothing about a dead input.
    if verdict.classifier.state == ClassifierHealthState::Unconfigured {
        return "⚠ classifier unconfigured — no API key; nothing is being classified".to_string();
    }
    if verdict.classifier.consecutive_failures > 0 {
        let since = verdict.classifier.last_failure_at.map_or_else(
            || "an unknown time".to_string(),
            |timestamp| timestamp.with_timezone(&Local).format("%H:%M").to_string(),
        );
        return format!(
            "⚠ classifier failing — {}× since {since}",
            verdict.classifier.consecutive_failures
        );
    }
    verdict.classifier.last_success_at.map_or_else(
        || "classifier has not run yet".to_string(),
        |timestamp| {
            format!(
                "classifier ok · last run {} ago",
                format_duration((now - timestamp).num_milliseconds())
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use insta::assert_snapshot;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tt_db::StoredEvent;

    use crate::Config;

    fn make_event(id: &str, timestamp: chrono::DateTime<Utc>, source: &str) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type: tt_core::EventType::TmuxPaneFocus,
            source: source.to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: json!({}),
        }
    }

    #[test]
    fn test_status_empty_database() {
        let temp = TempDir::new().unwrap();
        let db = Database::open_in_memory().unwrap();
        let db_path = PathBuf::from("/path/to/events.db");
        let config = test_config(&temp, db_path);

        let output = format_status(&db, &config).unwrap();

        assert_snapshot!(output);
    }

    #[test]
    fn test_status_with_events() {
        let temp = TempDir::new().unwrap();
        let db = Database::open_in_memory().unwrap();
        let db_path = PathBuf::from("/path/to/events.db");
        let config = test_config(&temp, db_path);

        // Add events from multiple sources
        let ts_tmux = Utc.with_ymd_and_hms(2025, 1, 29, 10, 30, 0).unwrap();
        let ts_agent = Utc.with_ymd_and_hms(2025, 1, 29, 11, 45, 0).unwrap();

        db.insert_event(&make_event("e1", ts_tmux, "remote.tmux"))
            .unwrap();
        db.insert_event(&make_event("e2", ts_agent, "remote.agent"))
            .unwrap();

        let output = format_status(&db, &config).unwrap();

        assert_snapshot!(output);
    }

    #[test]
    fn test_status_sources_ordered_by_recency() {
        let temp = TempDir::new().unwrap();
        let db = Database::open_in_memory().unwrap();
        let db_path = PathBuf::from("/path/to/events.db");
        let config = test_config(&temp, db_path);

        // Add events - agent is most recent, then local, then tmux
        let ts_tmux = Utc.with_ymd_and_hms(2025, 1, 29, 10, 0, 0).unwrap();
        let ts_local = Utc.with_ymd_and_hms(2025, 1, 29, 11, 0, 0).unwrap();
        let ts_agent = Utc.with_ymd_and_hms(2025, 1, 29, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts_tmux, "remote.tmux"))
            .unwrap();
        db.insert_event(&make_event("e2", ts_local, "local.window"))
            .unwrap();
        db.insert_event(&make_event("e3", ts_agent, "remote.agent"))
            .unwrap();

        let output = format_status(&db, &config).unwrap();

        // Verify ordering in output - most recent first
        // Find the Sources: section and check order
        let sources_section = output
            .lines()
            .skip_while(|l| !l.contains("Sources:"))
            .skip(1) // skip "Sources:" line itself
            .take(3)
            .collect::<Vec<_>>();

        assert_eq!(sources_section.len(), 3, "expected 3 source lines");
        assert!(
            sources_section[0].contains("remote.agent"),
            "first should be remote.agent (12:00)"
        );
        assert!(
            sources_section[1].contains("local.window"),
            "second should be local.window (11:00)"
        );
        assert!(
            sources_section[2].contains("remote.tmux"),
            "third should be remote.tmux (10:00)"
        );
    }

    #[test]
    fn status_prepends_verdict_before_database_details() {
        // Given: an empty configured database and todo store.
        let temp = TempDir::new().unwrap();
        let db = Database::open_in_memory().unwrap();
        let config = test_config(&temp, PathBuf::from("/path/to/events.db"));

        // When: status output is rendered.
        let output = format_status(&db, &config).unwrap();

        // Then: the verdict block appears before the existing database details.
        assert!(output.starts_with("NOW   no active stream\nTOP   no top todo\nWIP   0/4\n"));
        assert!(output.find("NOW").unwrap() < output.find("Database:").unwrap());
    }

    #[test]
    fn status_names_a_local_event_source_that_has_gone_silent() {
        // Given: this machine's watcher last reported nine days ago while the tmux hook
        // kept flowing — the incident's shape. A human running `tt status` is exactly who
        // needs told that direct time has been missing an input for nine days, so it has to
        // reach the human output and not only the JSON the dashboard reads.
        let temp = TempDir::new().unwrap();
        let db = Database::open_in_memory().unwrap();
        let config = test_config(&temp, PathBuf::from("/path/to/events.db"));
        let mut silent = make_event(
            "focus-old",
            Utc::now() - chrono::Duration::days(9),
            "local.cosmic",
        );
        silent.event_type = tt_core::EventType::WindowFocus;
        db.insert_event(&silent).unwrap();
        let live = make_event(
            "tmux-now",
            Utc::now() - chrono::Duration::minutes(2),
            "local.tmux",
        );
        db.insert_event(&live).unwrap();

        // When: status output is rendered.
        let output = format_status(&db, &config).unwrap();

        // Then: the dead input is named alongside the process that owes it and how long it
        // has been gone, before the Sources block a reader has to interpret themselves.
        let stale_line = output
            .lines()
            .find(|line| line.contains("window_focus"))
            .expect("status must name the silent event source");
        assert!(
            stale_line.contains("tt-watcher"),
            "stale line must name the emitter to look at, got: {stale_line}"
        );
        assert!(
            stale_line.contains("9d"),
            "stale line must say how long it has been silent, got: {stale_line}"
        );
        assert!(
            output.find("window_focus").unwrap() < output.find("Sources:").unwrap(),
            "a dead input belongs in the verdict block, not buried in Sources"
        );
    }

    #[test]
    fn an_unconfigured_classifier_is_never_reported_as_ok() {
        // Given: a classifier that ran successfully, then lost its API key.
        let temp = TempDir::new().unwrap();
        let db = Database::open_in_memory().unwrap();
        let config = test_config(&temp, PathBuf::from("/path/to/events.db"));
        db.record_classifier_success(Utc.with_ymd_and_hms(2026, 8, 8, 7, 51, 0).single().unwrap())
            .unwrap();
        db.record_classifier_unconfigured("api key env var ANTHROPIC_API_KEY not set")
            .unwrap();

        // When: status is rendered.
        let output = format_status(&db, &config).unwrap();

        // Then: it says so. An unconfigured classifier never fails, so
        // `consecutive_failures` stays 0 and `last_success_at` keeps the value the last
        // configured run left; reading only those printed `classifier ok` while nothing
        // was being classified. That is the dead-watcher failure in another input.
        assert!(
            output.contains("classifier unconfigured"),
            "an unconfigured classifier must announce itself, got: {output}"
        );
        assert!(
            !output.contains("classifier ok"),
            "a stale last_success_at must not read as healthy, got: {output}"
        );
    }

    fn test_config(temp: &TempDir, database_path: PathBuf) -> Config {
        Config {
            database_path,
            todo_store_path: temp.path().join("todo-store"),
            ..Config::default()
        }
    }
}
