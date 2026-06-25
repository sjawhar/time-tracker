//! Stream classification command.
//!
//! Two modes:
//! - **Show**: Display unclassified sessions and events for LLM-based classification
//! - **Apply**: Accept JSON assignments and propagate to events

use std::collections::HashMap;
use std::io::Read;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::util::parse_datetime;

// ── Show mode ──────────────────────────────────────────────────────────────

/// Session summary for classification output.
#[derive(Debug, Serialize)]
struct SessionSummary {
    session_id: String,
    source: String,
    session_type: String,
    project_path: Option<String>,
    project_name: Option<String>,
    start_time: String,
    end_time: Option<String>,
    duration_minutes: Option<i64>,
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    starting_prompt: Option<String>,
    tool_call_count: i32,
    user_prompt_count: usize,
    stream_id: Option<String>,
    proposed_stream: Option<String>,
}

/// Non-session event cluster for classification output.
#[derive(Debug, Serialize)]
struct EventCluster {
    cwd: String,
    start_time: String,
    end_time: String,
    duration_minutes: i64,
    event_count: usize,
    event_types: Vec<String>,
    stream_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct WindowRun {
    start: String,
    end: String,
    duration_minutes: i64,
    app_id: String,
    event_ids: Vec<String>,
    titles: Vec<String>,
    machine_id: Option<String>,
    stream_id: Option<String>,
}

/// Full classification output.
#[derive(Debug, Serialize)]
struct ClassifyOutput {
    time_range: TimeRange,
    sessions: Vec<SessionSummary>,
    event_clusters: Vec<EventCluster>,
    window_runs: Vec<WindowRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gaps: Option<Vec<GapInfo>>,
    stats: ClassifyStats,
}

/// A gap between user activity events.
#[derive(Debug, Serialize)]
struct GapInfo {
    start: String,
    end: String,
    duration_minutes: i64,
}

#[derive(Debug, Serialize)]
struct TimeRange {
    start: String,
    end: String,
}

#[derive(Debug, Serialize)]
struct ClassifyStats {
    total_sessions: usize,
    unclassified_sessions: usize,
    total_event_clusters: usize,
    unclassified_event_clusters: usize,
}

/// Show unclassified sessions and events.
#[expect(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    clippy::too_many_lines,
    reason = "CLI flag passthrough; sequential phases of query, filter, format"
)]
pub fn run_show(
    db: &tt_db::Database,
    unclassified: bool,
    summary: bool,
    json: bool,
    start: Option<String>,
    end: Option<String>,
    gaps: bool,
    gap_threshold: u32,
) -> Result<()> {
    let (start_time, end_time) = resolve_time_range(start, end)?;

    // Get sessions in range
    let sessions = db
        .agent_sessions_in_range(start_time, end_time)
        .context("failed to query agent sessions")?;

    // Build CWD → stream_id mapping from existing classified events for proposals
    let classified_events = db
        .get_events_in_range(start_time, end_time)
        .context("failed to query events")?;

    let mut cwd_to_stream: HashMap<String, String> = HashMap::new();
    for event in &classified_events {
        if let (Some(cwd), Some(stream_id)) = (&event.cwd, &event.stream_id) {
            cwd_to_stream
                .entry(cwd.clone())
                .or_insert_with(|| stream_id.clone());
        }
    }

    // Get stream names for proposals
    let all_streams = db.get_streams().context("failed to query streams")?;
    let stream_names: HashMap<String, String> = all_streams
        .iter()
        .map(|s| (s.id.clone(), s.name.clone().unwrap_or_default()))
        .collect();

    // Build session summaries — filter out subagents
    let mut session_summaries: Vec<SessionSummary> = sessions
        .iter()
        .filter(|s| s.session_type.to_string() != "subagent")
        .map(|s| {
            let duration = s.end_time.map(|end| (end - s.start_time).num_minutes());
            let proposed = cwd_to_stream
                .get(&s.project_path)
                .and_then(|sid| stream_names.get(sid))
                .cloned();

            let stream_id = cwd_to_stream.get(&s.project_path).cloned();

            SessionSummary {
                session_id: s.session_id.clone(),
                source: s.source.to_string(),
                session_type: s.session_type.to_string(),
                project_path: Some(s.project_path.clone()),
                project_name: Some(s.project_name.clone()),
                start_time: s.start_time.to_rfc3339(),
                end_time: s.end_time.map(|t| t.to_rfc3339()),
                duration_minutes: duration,
                summary: s.summary.clone(),
                starting_prompt: if s.summary.is_none() {
                    s.starting_prompt.as_ref().map(|p| truncate(p, 200))
                } else {
                    None
                },
                tool_call_count: s.tool_call_count,
                user_prompt_count: s.user_prompts.len(),
                stream_id,
                proposed_stream: proposed,
            }
        })
        .collect();

    if unclassified {
        session_summaries.retain(|s| s.stream_id.is_none());
    }

    let non_session_events: Vec<_> = classified_events
        .iter()
        .filter(|e| e.session_id.is_none())
        .collect();

    let mut clusters = cluster_events(&non_session_events);
    let mut window_runs = synthesize_window_runs(&non_session_events);
    if unclassified {
        clusters.retain(|c| c.stream_id.is_none());
        window_runs.retain(|run| run.stream_id.is_none());
    }

    let stats = ClassifyStats {
        total_sessions: session_summaries.len(),
        unclassified_sessions: session_summaries
            .iter()
            .filter(|s| s.stream_id.is_none())
            .count(),
        total_event_clusters: clusters.len(),
        unclassified_event_clusters: clusters.iter().filter(|c| c.stream_id.is_none()).count(),
    };

    // Compute gaps if requested
    let gap_list = if gaps {
        let user_events: Vec<_> = classified_events
            .iter()
            .filter(|e| {
                matches!(
                    e.event_type,
                    tt_core::EventType::UserMessage
                        | tt_core::EventType::TmuxPaneFocus
                        | tt_core::EventType::TmuxScroll
                        | tt_core::EventType::WindowFocus
                        | tt_core::EventType::BrowserTab
                )
            })
            .collect();
        let threshold_ms = i64::from(gap_threshold) * 60 * 1000;
        let mut found_gaps = Vec::new();
        for window in user_events.windows(2) {
            let gap_ms = (window[1].timestamp - window[0].timestamp).num_milliseconds();
            if gap_ms >= threshold_ms {
                found_gaps.push(GapInfo {
                    start: window[0].timestamp.to_rfc3339(),
                    end: window[1].timestamp.to_rfc3339(),
                    duration_minutes: gap_ms / 60_000,
                });
            }
        }
        Some(found_gaps)
    } else {
        None
    };

    let output = ClassifyOutput {
        time_range: TimeRange {
            start: start_time.to_rfc3339(),
            end: end_time.to_rfc3339(),
        },
        sessions: session_summaries,
        event_clusters: clusters,
        window_runs,
        gaps: gap_list,
        stats,
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("failed to serialize output")?
        );
    } else if summary {
        print_summary(&output);
    } else {
        print_table(&output);
    }

    Ok(())
}

fn print_summary(output: &ClassifyOutput) {
    println!(
        "CLASSIFY: {} sessions, {} event clusters ({} unclassified sessions, {} unclassified clusters)\n",
        output.stats.total_sessions,
        output.stats.total_event_clusters,
        output.stats.unclassified_sessions,
        output.stats.unclassified_event_clusters,
    );

    println!("SESSIONS");
    println!("{}", "─".repeat(100));
    for s in &output.sessions {
        let status = if s.stream_id.is_some() { "✓" } else { "?" };
        let desc = s
            .summary
            .as_deref()
            .or(s.starting_prompt.as_deref())
            .unwrap_or("(no description)");
        println!(
            "  {status} {:<25} {:>5}m {:>4} tools  {}",
            s.session_id.get(..25).unwrap_or(&s.session_id),
            s.duration_minutes.unwrap_or(0),
            s.tool_call_count,
            truncate(desc, 60),
        );
    }

    if !output.event_clusters.is_empty() {
        println!("\nEVENT CLUSTERS");
        println!("{}", "─".repeat(100));
        for c in &output.event_clusters {
            let status = if c.stream_id.is_some() { "✓" } else { "?" };
            let cwd_short: String = c
                .cwd
                .rsplit('/')
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("/");
            println!(
                "  {status} {:<30} {:>5}m {:>4} events  {}",
                cwd_short,
                c.duration_minutes,
                c.event_count,
                c.event_types.join(", "),
            );
        }
    }

    if !output.window_runs.is_empty() {
        println!("\nWINDOW RUNS");
        println!("{}", "─".repeat(100));
        for run in &output.window_runs {
            let status = if run.stream_id.is_some() { "✓" } else { "?" };
            let title = run.titles.first().map_or("(no title)", String::as_str);
            println!(
                "  {status} {:<30} {:>5}m {:>4} events  {}",
                run.app_id,
                run.duration_minutes,
                run.event_ids.len(),
                truncate(title, 50),
            );
        }
    }
}

fn print_table(output: &ClassifyOutput) {
    print_summary(output);

    println!("\nDETAILS");
    println!("{}", "─".repeat(100));
    for s in &output.sessions {
        println!("\n  Session: {}", s.session_id);
        println!("    Source:  {} ({})", s.source, s.session_type);
        if let Some(path) = &s.project_path {
            println!("    Path:    {path}");
        }
        println!(
            "    Time:    {} → {}",
            s.start_time,
            s.end_time.as_deref().unwrap_or("running")
        );
        if let Some(d) = s.duration_minutes {
            println!("    Duration: {d}m");
        }
        println!(
            "    Tools:   {} calls, {} user prompts",
            s.tool_call_count, s.user_prompt_count
        );
        if let Some(summary) = &s.summary {
            println!("    Summary: {summary}");
        }
        if let Some(prompt) = &s.starting_prompt {
            println!("    Prompt:  {prompt}");
        }
        if let Some(proposed) = &s.proposed_stream {
            println!("    Proposed: {proposed}");
        }
    }
}

/// Cluster non-session events by CWD + temporal proximity.
fn cluster_events(events: &[&tt_db::StoredEvent]) -> Vec<EventCluster> {
    let filtered: Vec<_> = events
        .iter()
        .copied()
        .filter(|event| event.event_type != tt_core::EventType::WindowFocus)
        .collect();

    if filtered.is_empty() {
        return Vec::new();
    }

    let mut sorted = filtered;
    sorted.sort_by(|a, b| {
        let cwd_cmp = a.cwd.cmp(&b.cwd);
        if cwd_cmp == std::cmp::Ordering::Equal {
            a.timestamp.cmp(&b.timestamp)
        } else {
            cwd_cmp
        }
    });

    let gap_threshold = Duration::minutes(30);
    let mut clusters = Vec::new();
    let mut current_cwd = sorted[0].cwd.clone().unwrap_or_default();
    let mut current_start = sorted[0].timestamp;
    let mut current_end = sorted[0].timestamp;
    let mut current_count = 1usize;
    let mut current_types: Vec<String> = vec![sorted[0].event_type.to_string()];
    let mut current_stream: Option<String> = sorted[0].stream_id.clone();

    for event in &sorted[1..] {
        let event_cwd = event.cwd.clone().unwrap_or_default();
        let same_cwd = event_cwd == current_cwd;
        let within_gap = event.timestamp - current_end < gap_threshold;

        if same_cwd && within_gap {
            current_end = event.timestamp;
            current_count += 1;
            let etype = event.event_type.to_string();
            if !current_types.contains(&etype) {
                current_types.push(etype);
            }
            if current_stream.is_none() {
                current_stream.clone_from(&event.stream_id);
            }
        } else {
            clusters.push(EventCluster {
                cwd: current_cwd.clone(),
                start_time: current_start.to_rfc3339(),
                end_time: current_end.to_rfc3339(),
                duration_minutes: (current_end - current_start).num_minutes(),
                event_count: current_count,
                event_types: current_types.clone(),
                stream_id: current_stream.clone(),
            });

            current_cwd = event_cwd;
            current_start = event.timestamp;
            current_end = event.timestamp;
            current_count = 1;
            current_types = vec![event.event_type.to_string()];
            current_stream.clone_from(&event.stream_id);
        }
    }

    // Flush last cluster
    clusters.push(EventCluster {
        cwd: current_cwd,
        start_time: current_start.to_rfc3339(),
        end_time: current_end.to_rfc3339(),
        duration_minutes: (current_end - current_start).num_minutes(),
        event_count: current_count,
        event_types: current_types,
        stream_id: current_stream,
    });

    clusters
}

fn synthesize_window_runs(events: &[&tt_db::StoredEvent]) -> Vec<WindowRun> {
    let mut sorted: Vec<_> = events
        .iter()
        .copied()
        .filter(|event| event.event_type == tt_core::EventType::WindowFocus)
        .collect();
    if sorted.is_empty() {
        return Vec::new();
    }

    sorted.sort_by(|a, b| {
        let machine_cmp = a.machine_id.cmp(&b.machine_id);
        if machine_cmp == std::cmp::Ordering::Equal {
            a.timestamp.cmp(&b.timestamp)
        } else {
            machine_cmp
        }
    });

    let gap_threshold = Duration::minutes(30);
    let mut runs = Vec::new();
    let first = sorted[0];
    let mut current = WindowRunBuilder::new(first);

    for event in &sorted[1..] {
        let same_machine = event.machine_id == current.machine_id;
        let same_app = event.window_app_id.as_deref().unwrap_or("(unknown)") == current.app_id;
        let within_gap = event.timestamp - current.end < gap_threshold;

        if same_machine && same_app && within_gap {
            current.push(event);
        } else {
            runs.push(current.finish());
            current = WindowRunBuilder::new(event);
        }
    }

    runs.push(current.finish());
    runs
}

struct WindowRunBuilder {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    app_id: String,
    event_ids: Vec<String>,
    titles: Vec<String>,
    machine_id: Option<String>,
    stream_id: Option<String>,
}

impl WindowRunBuilder {
    fn new(event: &tt_db::StoredEvent) -> Self {
        let mut builder = Self {
            start: event.timestamp,
            end: event.timestamp,
            app_id: event
                .window_app_id
                .clone()
                .unwrap_or_else(|| "(unknown)".to_string()),
            event_ids: Vec::new(),
            titles: Vec::new(),
            machine_id: event.machine_id.clone(),
            stream_id: event.stream_id.clone(),
        };
        builder.push(event);
        builder
    }

    fn push(&mut self, event: &tt_db::StoredEvent) {
        const MAX_TITLES: usize = 5;

        self.end = event.timestamp;
        self.event_ids.push(event.id.clone());
        if self.stream_id.is_none() {
            self.stream_id.clone_from(&event.stream_id);
        }
        if let Some(title) = &event.window_title {
            let is_consecutive_duplicate = self.titles.last() == Some(title);
            if !is_consecutive_duplicate && self.titles.len() < MAX_TITLES {
                self.titles.push(title.clone());
            }
        }
    }

    fn finish(self) -> WindowRun {
        WindowRun {
            start: self.start.to_rfc3339(),
            end: self.end.to_rfc3339(),
            duration_minutes: (self.end - self.start).num_minutes(),
            app_id: self.app_id,
            event_ids: self.event_ids,
            titles: self.titles,
            machine_id: self.machine_id,
            stream_id: self.stream_id,
        }
    }
}

// ── Apply mode ─────────────────────────────────────────────────────────────

/// Input format for `tt classify --apply`.
#[derive(Debug, Deserialize)]
pub struct ClassifyApplyInput {
    #[serde(default)]
    pub streams: Vec<StreamDef>,
    #[serde(default)]
    pub assign_by_session: Vec<SessionAssignment>,
    #[serde(default)]
    pub assign_by_pattern: Vec<PatternAssignment>,
    #[serde(default)]
    pub assign_by_event_ids: Vec<EventIdsAssignment>,
    #[serde(default)]
    pub assign_by_time: Vec<TimeAssignment>,
}

#[derive(Debug, Deserialize)]
pub struct StreamDef {
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionAssignment {
    pub session_id: String,
    pub stream: String,
}

#[derive(Debug, Deserialize)]
pub struct PatternAssignment {
    pub cwd_like: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    pub stream: String,
}

#[derive(Debug, Deserialize)]
pub struct EventIdsAssignment {
    pub event_ids: Vec<String>,
    pub stream: String,
}

#[derive(Debug, Deserialize)]
pub struct TimeAssignment {
    pub start: String,
    pub end: String,
    pub stream: String,
}

/// Apply stream assignments from JSON input.
#[expect(
    clippy::too_many_lines,
    reason = "sequential phases of stream creation, assignment, and recompute"
)]
pub fn run_apply(db: &tt_db::Database, input_path: &str) -> Result<()> {
    let input_str = if input_path == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("failed to read from stdin")?;
        buf
    } else {
        std::fs::read_to_string(input_path)
            .with_context(|| format!("failed to read file: {input_path}"))?
    };

    let input: ClassifyApplyInput =
        serde_json::from_str(&input_str).context("failed to parse classify input JSON")?;

    // Phase 1: Create/resolve streams
    let mut stream_name_to_id: HashMap<String, String> = HashMap::new();

    let existing = db.get_streams().context("failed to query streams")?;
    for s in &existing {
        if let Some(name) = &s.name {
            stream_name_to_id.insert(name.clone(), s.id.clone());
        }
    }

    // Create new streams from definitions + resolve from assignments
    let all_stream_names: Vec<String> = input
        .streams
        .iter()
        .map(|s| s.name.clone())
        .chain(input.assign_by_session.iter().map(|a| a.stream.clone()))
        .chain(input.assign_by_pattern.iter().map(|a| a.stream.clone()))
        .chain(input.assign_by_event_ids.iter().map(|a| a.stream.clone()))
        .chain(input.assign_by_time.iter().map(|a| a.stream.clone()))
        .collect();

    for name in &all_stream_names {
        if !stream_name_to_id.contains_key(name) {
            let id = uuid::Uuid::new_v4().to_string();
            let stream = tt_db::Stream {
                id: id.clone(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                name: Some(name.clone()),
                time_direct_ms: 0,
                time_delegated_ms: 0,
                first_event_at: None,
                last_event_at: None,
                needs_recompute: true,
            };
            db.insert_stream(&stream)
                .with_context(|| format!("failed to create stream: {name}"))?;
            stream_name_to_id.insert(name.clone(), id.clone());
            println!("Created stream: {name} ({})", &id[..8]);
        }
    }

    // Apply tags from stream definitions
    for stream_def in &input.streams {
        let stream_id = &stream_name_to_id[&stream_def.name];
        for tag in &stream_def.tags {
            db.add_tag(stream_id, tag).with_context(|| {
                format!("failed to add tag {tag} to stream {}", stream_def.name)
            })?;
        }
    }

    // Phase 2: Session assignments
    let mut total_assigned = 0u64;
    for assignment in &input.assign_by_session {
        let stream_id = stream_name_to_id
            .get(&assignment.stream)
            .with_context(|| format!("unknown stream: {}", assignment.stream))?;

        let count = db
            .assign_events_by_session_id(&assignment.session_id, stream_id, "inferred")
            .with_context(|| {
                format!(
                    "failed to assign session {} to stream {}",
                    assignment.session_id, assignment.stream
                )
            })?;

        if count > 0 {
            tracing::info!(
                session_id = %assignment.session_id,
                stream = %assignment.stream,
                count,
                "assigned session events"
            );
            total_assigned += count;
        }
    }

    // Phase 3: Pattern assignments
    for assignment in &input.assign_by_pattern {
        let stream_id = stream_name_to_id
            .get(&assignment.stream)
            .with_context(|| format!("unknown stream: {}", assignment.stream))?;

        let start = assignment
            .start
            .as_ref()
            .map(|s| parse_datetime(s))
            .transpose()
            .context("invalid start time in pattern assignment")?;
        let end = assignment
            .end
            .as_ref()
            .map(|s| parse_datetime(s))
            .transpose()
            .context("invalid end time in pattern assignment")?;

        let count = db
            .assign_events_by_pattern(&assignment.cwd_like, start, end, stream_id)
            .with_context(|| {
                format!(
                    "failed to assign events matching {} to stream {}",
                    assignment.cwd_like, assignment.stream
                )
            })?;

        if count > 0 {
            tracing::info!(
                cwd_like = %assignment.cwd_like,
                stream = %assignment.stream,
                count,
                "assigned pattern events"
            );
            total_assigned += count;
        }
    }

    // Phase 4: Explicit event ID assignments
    for assignment in &input.assign_by_event_ids {
        let stream_id = stream_name_to_id
            .get(&assignment.stream)
            .with_context(|| format!("unknown stream: {}", assignment.stream))?;

        let count = db
            .assign_events_by_ids(&assignment.event_ids, stream_id, "inferred")
            .with_context(|| {
                format!(
                    "failed to assign {} explicit events to stream {}",
                    assignment.event_ids.len(),
                    assignment.stream
                )
            })?;

        if count > 0 {
            tracing::info!(
                stream = %assignment.stream,
                count,
                "assigned explicit events"
            );
            total_assigned += count;
        }
    }

    // Phase 4.5: Time-range assignments — attribute unassigned GUI/window_focus time
    // (no cwd/session) to a stream by semantic temporal judgment.
    for assignment in &input.assign_by_time {
        let stream_id = stream_name_to_id
            .get(&assignment.stream)
            .with_context(|| format!("unknown stream: {}", assignment.stream))?;

        let start = parse_datetime(&assignment.start)
            .context("invalid start time in time-range assignment")?;
        let end =
            parse_datetime(&assignment.end).context("invalid end time in time-range assignment")?;

        let count = db
            .assign_events_by_time_range(start, end, stream_id)
            .with_context(|| {
                format!(
                    "failed to assign time range to stream {}",
                    assignment.stream
                )
            })?;

        if count > 0 {
            tracing::info!(
                start = %assignment.start,
                end = %assignment.end,
                stream = %assignment.stream,
                count,
                "assigned time-range events"
            );
            total_assigned += count;
        }
    }

    // Phase 5: Recompute affected streams
    if total_assigned > 0 {
        println!("Assigned {total_assigned} events. Recomputing...");
        super::recompute::run(db, true)?;
    } else {
        println!("No events to assign.");
    }

    Ok(())
}

// ── Utilities ──────────────────────────────────────────────────────────────

fn resolve_time_range(
    start: Option<String>,
    end: Option<String>,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let end_time = match end {
        Some(s) => parse_datetime(&s).context("invalid end time")?,
        None => Utc::now(),
    };

    let start_time = match start {
        Some(s) => parse_datetime(&s).context("invalid start time")?,
        None => end_time - Duration::days(1),
    };

    Ok((start_time, end_time))
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn ts(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap() + Duration::minutes(minutes)
    }

    fn make_event(
        id: &str,
        timestamp: DateTime<Utc>,
        event_type: tt_core::EventType,
        session_id: Option<&str>,
        cwd: &str,
    ) -> tt_db::StoredEvent {
        tt_db::StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type,
            source: "remote.agent".to_string(),
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
            cwd: Some(cwd.to_string()),
            session_id: session_id.map(String::from),
            stream_id: None,
            assignment_source: None,
            data: json!({}),
        }
    }

    fn make_window_event(
        id: &str,
        timestamp: DateTime<Utc>,
        app_id: &str,
        title: &str,
        machine_id: &str,
    ) -> tt_db::StoredEvent {
        let mut event = make_event(id, timestamp, tt_core::EventType::WindowFocus, None, "");
        event.source = "local.cosmic".to_string();
        event.cwd = None;
        event.machine_id = Some(machine_id.to_string());
        event.window_app_id = Some(app_id.to_string());
        event.window_title = Some(title.to_string());
        event
    }

    #[test]
    fn test_synthesize_window_runs_groups_by_app_gap_and_machine() {
        let events = [
            make_window_event("w1", ts(0), "firefox", "Docs", "local"),
            make_window_event("w2", ts(5), "firefox", "Docs", "local"),
            make_window_event("w3", ts(10), "firefox", "Issue", "local"),
            make_window_event("w4", ts(45), "firefox", "Issue", "local"),
            make_window_event("w5", ts(46), "slack", "Team", "local"),
            make_window_event("w6", ts(47), "firefox", "Remote", "remote"),
        ];
        let refs: Vec<_> = events.iter().collect();

        let runs = synthesize_window_runs(&refs);

        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0].app_id, "firefox");
        assert_eq!(runs[0].event_ids, vec!["w1", "w2", "w3"]);
        assert_eq!(runs[0].titles, vec!["Docs", "Issue"]);
        assert_eq!(runs[0].duration_minutes, 10);
        assert_eq!(runs[0].machine_id.as_deref(), Some("local"));
        assert_eq!(runs[1].event_ids, vec!["w4"]);
        assert_eq!(runs[2].app_id, "slack");
        assert_eq!(runs[2].event_ids, vec!["w5"]);
        assert_eq!(runs[3].machine_id.as_deref(), Some("remote"));
        assert_eq!(runs[3].event_ids, vec!["w6"]);
    }

    #[test]
    fn test_cluster_events_excludes_window_focus_empty_cwd_cluster() {
        let window = make_window_event("w1", ts(0), "firefox", "Docs", "local");
        let tmux = make_event(
            "t1",
            ts(1),
            tt_core::EventType::TmuxPaneFocus,
            None,
            "/project-x",
        );
        let events = [window, tmux];
        let refs: Vec<_> = events.iter().collect();

        let clusters = cluster_events(&refs);

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].cwd, "/project-x");
    }

    #[test]
    fn test_classify_apply_assigns_window_events_by_event_ids() {
        let db = tt_db::Database::open_in_memory().unwrap();
        for event in [
            make_window_event("w1", ts(0), "firefox", "Docs", "local"),
            make_window_event("w2", ts(1), "firefox", "Docs", "local"),
            make_window_event("w3", ts(2), "slack", "Team", "local"),
        ] {
            db.insert_event(&event).unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("classify.json");
        std::fs::write(
            &input_path,
            serde_json::to_string(&json!({
                "assign_by_event_ids": [{
                    "event_ids": ["w1", "w2"],
                    "stream": "proposal"
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        run_apply(&db, input_path.to_str().unwrap()).unwrap();

        let stream = db.resolve_stream("proposal").unwrap().unwrap();
        let assigned = db.get_events_by_stream(&stream.id).unwrap();
        let assigned_ids: Vec<_> = assigned.iter().map(|event| event.id.as_str()).collect();
        assert_eq!(assigned_ids, vec!["w1", "w2"]);
        let unassigned = db.get_events_without_stream().unwrap();
        assert_eq!(unassigned.len(), 1);
        assert_eq!(unassigned[0].id, "w3");
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "integration test covering full classify workflow"
    )]
    fn test_classify_apply_session_assignment() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Create events for two sessions
        let events = vec![
            {
                let mut e = make_event(
                    "e1",
                    ts(0),
                    tt_core::EventType::AgentSession,
                    Some("sess-a"),
                    "/project-x",
                );
                e.action = Some("started".to_string());
                e
            },
            make_event(
                "e2",
                ts(5),
                tt_core::EventType::AgentToolUse,
                Some("sess-a"),
                "/project-x",
            ),
            make_event(
                "e3",
                ts(10),
                tt_core::EventType::AgentToolUse,
                Some("sess-b"),
                "/project-y",
            ),
            // Tmux event (no session)
            {
                let mut e = make_event(
                    "e4",
                    ts(2),
                    tt_core::EventType::TmuxPaneFocus,
                    None,
                    "/project-x",
                );
                e.pane_id = Some("%1".to_string());
                e
            },
        ];

        for event in &events {
            db.insert_event(event).unwrap();
        }

        // Apply assignments via JSON
        let input = ClassifyApplyInput {
            streams: vec![
                StreamDef {
                    name: "stream-x".to_string(),
                    tags: vec!["project:x".to_string()],
                },
                StreamDef {
                    name: "stream-y".to_string(),
                    tags: vec![],
                },
            ],
            assign_by_session: vec![
                SessionAssignment {
                    session_id: "sess-a".to_string(),
                    stream: "stream-x".to_string(),
                },
                SessionAssignment {
                    session_id: "sess-b".to_string(),
                    stream: "stream-y".to_string(),
                },
            ],
            assign_by_pattern: vec![PatternAssignment {
                cwd_like: "%/project-x%".to_string(),
                start: None,
                end: None,
                stream: "stream-x".to_string(),
            }],
            assign_by_event_ids: vec![],
            assign_by_time: vec![],
        };

        // Manually run the assignment logic (without recompute)
        let mut stream_name_to_id: HashMap<String, String> = HashMap::new();

        for stream_def in &input.streams {
            let id = uuid::Uuid::new_v4().to_string();
            let stream = tt_db::Stream {
                id: id.clone(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                name: Some(stream_def.name.clone()),
                time_direct_ms: 0,
                time_delegated_ms: 0,
                first_event_at: None,
                last_event_at: None,
                needs_recompute: true,
            };
            db.insert_stream(&stream).unwrap();
            stream_name_to_id.insert(stream_def.name.clone(), id.clone());

            for tag in &stream_def.tags {
                db.add_tag(&id, tag).unwrap();
            }
        }

        // Session assignments
        for assignment in &input.assign_by_session {
            let stream_id = &stream_name_to_id[&assignment.stream];
            let count = db
                .assign_events_by_session_id(&assignment.session_id, stream_id, "inferred")
                .unwrap();
            assert!(
                count > 0,
                "session {} should have events",
                assignment.session_id
            );
        }

        // Pattern assignments
        for assignment in &input.assign_by_pattern {
            let stream_id = &stream_name_to_id[&assignment.stream];
            db.assign_events_by_pattern(&assignment.cwd_like, None, None, stream_id)
                .unwrap();
        }

        // Verify: all events for sess-a are in stream-x
        let stream_x_id = &stream_name_to_id["stream-x"];
        let stream_x_events = db.get_events_by_stream(stream_x_id).unwrap();
        assert_eq!(
            stream_x_events.len(),
            3,
            "stream-x should have 3 events (2 from sess-a + 1 tmux pattern match)"
        );

        // Verify: sess-b events are in stream-y
        let y_id = &stream_name_to_id["stream-y"];
        let y_events = db.get_events_by_stream(y_id).unwrap();
        assert_eq!(
            y_events.len(),
            1,
            "stream-y should have 1 event from sess-b"
        );

        // Verify: no events are unassigned
        let unassigned = db.get_events_without_stream().unwrap();
        assert_eq!(unassigned.len(), 0, "all events should be assigned");

        // Verify: tags were applied
        let tags = db.get_tags(stream_x_id).unwrap();
        assert_eq!(tags, vec!["project:x"]);

        // Verify: no split sessions (all events for each session in one stream)
        let sess_a_count = stream_x_events
            .iter()
            .filter(|e| e.session_id.as_deref() == Some("sess-a"))
            .count();
        assert_eq!(sess_a_count, 2, "both sess-a events should be in stream-x");
    }

    #[test]
    fn test_classify_apply_preserves_user_assignments() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Create a stream and an event with user assignment
        let stream = tt_db::Stream {
            id: "user-stream".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            name: Some("user-assigned".to_string()),
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: true,
        };
        db.insert_stream(&stream).unwrap();

        let mut event = make_event(
            "e1",
            ts(0),
            tt_core::EventType::AgentToolUse,
            Some("sess-a"),
            "/project",
        );
        event.stream_id = Some("user-stream".to_string());
        event.assignment_source = Some("user".to_string());
        db.insert_event(&event).unwrap();

        // Try to reassign via session assignment
        let new_stream = tt_db::Stream {
            id: "new-stream".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            name: Some("new-stream".to_string()),
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: true,
        };
        db.insert_stream(&new_stream).unwrap();

        let count = db
            .assign_events_by_session_id("sess-a", "new-stream", "inferred")
            .unwrap();
        assert_eq!(count, 0, "user assignment should not be overwritten");

        // Verify event is still in original stream
        let e = db.get_events_by_stream("user-stream").unwrap();
        assert_eq!(e.len(), 1);
    }
}
