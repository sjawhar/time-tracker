//! Stream classification command.
//!
//! Two modes:
//! - **Show**: Display unclassified sessions and events for LLM-based classification
//! - **Apply**: Accept JSON assignments and propagate to events

use std::collections::{HashMap, HashSet};
use std::io::Read;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tt_core::slug::validate_slug;
use tt_core::todos::TodoFileItem;

use super::util::parse_datetime;
use crate::Config;
use crate::todo_store::{LoadedTodoStore, load_mutating, load_read_only, write_todos};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    linked_todo: Option<LinkedTodo>,
}

#[derive(Clone, Debug, Serialize)]
struct LinkedTodo {
    id: String,
    text: String,
    stream_slug: Option<String>,
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
    streams: Vec<StreamRef>,
    sessions: Vec<SessionSummary>,
    event_clusters: Vec<EventCluster>,
    window_runs: Vec<WindowRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gaps: Option<Vec<GapInfo>>,
    stats: ClassifyStats,
}

#[derive(Debug, Serialize)]
struct StreamRef {
    id: String,
    slug: Option<String>,
    name: Option<String>,
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

fn build_session_todo_index(loaded: &LoadedTodoStore) -> HashMap<String, LinkedTodo> {
    let mut index: HashMap<String, LinkedTodo> = HashMap::new();

    for line in &loaded.store.todos.items {
        let TodoFileItem::Todo(todo) = &line.item else {
            continue;
        };
        if todo.done {
            continue;
        }

        let linked_todo = LinkedTodo {
            id: todo.id.clone(),
            text: todo.text.clone(),
            stream_slug: todo.stream.clone(),
        };
        for session_id in &todo.sessions {
            if let Some(first_todo) = index.get(session_id) {
                if first_todo.id != todo.id {
                    eprintln!(
                        "todo {} duplicates session {session_id} linked by todo {}; keeping first link",
                        todo.id, first_todo.id
                    );
                }
                continue;
            }
            index.insert(session_id.clone(), linked_todo.clone());
        }
    }

    index
}

fn auto_assign_linked_sessions(
    db: &tt_db::Database,
    index: &HashMap<String, LinkedTodo>,
) -> Result<Vec<String>> {
    let mut session_ids: Vec<_> = index.keys().collect();
    session_ids.sort_unstable();

    let mut notes = Vec::new();
    for session_id in session_ids {
        let linked_todo = &index[session_id];
        let Some(slug) = &linked_todo.stream_slug else {
            continue;
        };

        match db
            .get_stream_by_slug(slug)
            .with_context(|| format!("failed to query stream slug '{slug}'"))?
        {
            Some(stream) => {
                db.assign_events_by_session_id(session_id, &stream.id, "todo_link")
                    .with_context(|| {
                        format!(
                            "failed to assign events for linked session {session_id} to stream '{slug}'"
                        )
                    })?;
            }
            None => notes.push(format!(
                "todo {} references slug '{slug}' with no matching stream; session {session_id} left unclassified",
                linked_todo.id
            )),
        }
    }

    Ok(notes)
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
    config: &Config,
    unclassified: bool,
    summary: bool,
    json: bool,
    start: Option<String>,
    end: Option<String>,
    gaps: bool,
    gap_threshold: u32,
) -> Result<()> {
    let loaded = load_read_only(config)?;
    let session_todo_index = build_session_todo_index(&loaded);
    for note in auto_assign_linked_sessions(db, &session_todo_index)? {
        eprintln!("{note}");
    }

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
    let streams = streams_referenced_by_events(&classified_events, &all_streams);

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
                linked_todo: session_todo_index.get(&s.session_id).cloned(),
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
        streams,
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

fn streams_referenced_by_events(
    events: &[tt_db::StoredEvent],
    all_streams: &[tt_db::Stream],
) -> Vec<StreamRef> {
    let stream_ids: HashSet<&str> = events
        .iter()
        .filter_map(|event| event.stream_id.as_deref())
        .collect();
    let mut streams: Vec<_> = all_streams
        .iter()
        .filter(|stream| stream_ids.contains(stream.id.as_str()))
        .map(|stream| StreamRef {
            id: stream.id.clone(),
            slug: stream.slug.clone(),
            name: stream.name.clone(),
        })
        .collect();
    streams.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    streams
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
    pub slug: String,
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

#[derive(Debug)]
pub struct ApplyOutcome {
    pub session_stream_slugs: HashMap<String, String>,
}

/// Apply stream assignments from JSON input.
pub fn run_apply(db: &tt_db::Database, config: &Config, input_path: &str) -> Result<()> {
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

    let outcome = apply_input(db, input)?;
    for line in backfill_todo_streams(config, &outcome.session_stream_slugs)? {
        println!("{line}");
    }
    Ok(())
}

fn backfill_todo_streams(
    config: &Config,
    session_stream_slugs: &HashMap<String, String>,
) -> Result<Vec<String>> {
    if session_stream_slugs.is_empty() {
        return Ok(Vec::new());
    }

    let mut loaded = load_mutating(config)?;
    let mut lines = Vec::new();
    for file_line in &mut loaded.store.todos.items {
        let TodoFileItem::Todo(todo) = &mut file_line.item else {
            continue;
        };
        if todo.stream.is_some() || todo.done {
            continue;
        }
        let Some(slug) = todo
            .sessions
            .iter()
            .find_map(|session| session_stream_slugs.get(session))
        else {
            continue;
        };

        todo.stream = Some(slug.clone());
        lines.push(format!("Backfilled stream '{slug}' → {}", todo.id));
    }

    if !lines.is_empty() {
        write_todos(config, &loaded.store.todos)?;
    }
    Ok(lines)
}

#[expect(
    clippy::too_many_lines,
    reason = "sequential phases of stream creation, assignment, and recompute"
)]
fn apply_input(db: &tt_db::Database, input: ClassifyApplyInput) -> Result<ApplyOutcome> {
    let ClassifyApplyInput {
        streams,
        assign_by_session,
        assign_by_pattern,
        assign_by_event_ids,
        assign_by_time,
    } = input;

    // Phase 1: Create/resolve streams
    let mut ref_to_id: HashMap<String, String> = HashMap::new();
    let mut stream_slug_by_id: HashMap<String, String> = HashMap::new();

    let existing = db.get_streams().context("failed to query streams")?;
    for s in &existing {
        if let Some(slug) = &s.slug {
            ref_to_id.insert(slug.clone(), s.id.clone());
            stream_slug_by_id.insert(s.id.clone(), slug.clone());
        }
        if let Some(name) = &s.name {
            ref_to_id
                .entry(name.clone())
                .or_insert_with(|| s.id.clone());
        }
    }

    for def in &streams {
        validate_slug(&def.slug)?;
        let stream_id = if let Some(existing) = db
            .get_stream_by_slug(&def.slug)
            .context("failed to query stream by slug")?
        {
            if existing.name.as_deref() != Some(def.name.as_str()) {
                bail!(
                    "slug '{}' already belongs to stream '{}'; refusing to reuse it for '{}'",
                    def.slug,
                    existing.name.as_deref().unwrap_or("(unnamed)"),
                    def.name
                );
            }
            existing.id
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            let stream = tt_db::Stream {
                id: id.clone(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                name: Some(def.name.clone()),
                slug: Some(def.slug.clone()),
                time_direct_ms: 0,
                time_delegated_ms: 0,
                first_event_at: None,
                last_event_at: None,
                needs_recompute: true,
            };
            db.insert_stream(&stream)
                .with_context(|| format!("failed to create stream: {}", def.name))?;
            println!("Created stream: {} [{}] ({})", def.name, def.slug, &id[..8]);
            id
        };
        ref_to_id.insert(def.slug.clone(), stream_id.clone());
        ref_to_id
            .entry(def.name.clone())
            .or_insert_with(|| stream_id.clone());
        stream_slug_by_id.insert(stream_id, def.slug.clone());
    }

    // Apply tags from stream definitions
    for stream_def in &streams {
        let stream_id = resolve_stream_id(&ref_to_id, &stream_def.slug)?;
        for tag in &stream_def.tags {
            db.add_tag(stream_id, tag).with_context(|| {
                format!("failed to add tag {tag} to stream {}", stream_def.name)
            })?;
        }
    }

    // Phase 2: Session assignments
    let mut total_assigned = 0u64;
    let mut session_stream_slugs = HashMap::new();
    for assignment in &assign_by_session {
        let stream_id = resolve_stream_id(&ref_to_id, &assignment.stream)?;

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
            if let Some(slug) = stream_slug_by_id.get(stream_id) {
                session_stream_slugs.insert(assignment.session_id.clone(), slug.clone());
            }
        }
    }

    // Phase 3: Pattern assignments
    for assignment in &assign_by_pattern {
        let stream_id = resolve_stream_id(&ref_to_id, &assignment.stream)?;

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
    for assignment in &assign_by_event_ids {
        let stream_id = resolve_stream_id(&ref_to_id, &assignment.stream)?;

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
    for assignment in &assign_by_time {
        let stream_id = resolve_stream_id(&ref_to_id, &assignment.stream)?;

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

    Ok(ApplyOutcome {
        session_stream_slugs,
    })
}

fn resolve_stream_id<'a>(
    ref_to_id: &'a HashMap<String, String>,
    stream_ref: &str,
) -> Result<&'a str> {
    ref_to_id
        .get(stream_ref)
        .map(String::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown stream: '{stream_ref}' — define it in \"streams\" or use an existing slug"
            )
        })
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

    #[test]
    fn auto_assign_matches_existing_slug_only() {
        let db = tt_db::Database::open_in_memory().unwrap();
        let stream = tt_db::Stream {
            id: "proj-x".to_string(),
            created_at: ts(0),
            updated_at: ts(0),
            name: Some("Project X".to_string()),
            slug: Some("proj-x".to_string()),
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: true,
        };
        db.insert_stream(&stream).unwrap();

        for event in [
            make_event(
                "e1",
                ts(0),
                tt_core::EventType::AgentToolUse,
                Some("sess-1"),
                "/project-x",
            ),
            make_event(
                "e2",
                ts(1),
                tt_core::EventType::AgentToolUse,
                Some("sess-1"),
                "/project-x",
            ),
            make_event(
                "e3",
                ts(2),
                tt_core::EventType::AgentToolUse,
                Some("sess-2"),
                "/project-y",
            ),
        ] {
            db.insert_event(&event).unwrap();
        }

        let mut index = HashMap::new();
        index.insert(
            "sess-1".to_string(),
            LinkedTodo {
                id: "td_1".to_string(),
                text: "do the thing".to_string(),
                stream_slug: Some("proj-x".to_string()),
            },
        );
        index.insert(
            "sess-2".to_string(),
            LinkedTodo {
                id: "td_2".to_string(),
                text: "other".to_string(),
                stream_slug: Some("no-such-stream".to_string()),
            },
        );

        let notes = auto_assign_linked_sessions(&db, &index).unwrap();

        let assigned = db.get_events_by_stream(&stream.id).unwrap();
        assert_eq!(assigned.len(), 2);
        assert!(assigned.iter().all(|event| {
            event.session_id.as_deref() == Some("sess-1")
                && event.assignment_source.as_deref() == Some("todo_link")
        }));
        let unassigned = db.get_events_without_stream().unwrap();
        assert_eq!(unassigned.len(), 1);
        assert_eq!(unassigned[0].id, "e3");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("no-such-stream"));
    }

    #[test]
    fn session_todo_index_reads_sessions_from_store() {
        let loaded = crate::todo_store::parse_store_contents(
            "",
            "- [ ] Fix it <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[],\"stream\":\"proj-x\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"ses_abc\"]} -->\n",
            "",
        );

        let index = build_session_todo_index(&loaded);

        assert_eq!(index["ses_abc"].stream_slug.as_deref(), Some("proj-x"));
    }

    #[test]
    fn backfill_sets_stream_on_streamless_linked_todos_only() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = temp.path().join("todo-store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("todos.md"),
            concat!(
                "- [ ] Backfill me <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"sess-1\"]} -->\n",
                "- [ ] Already assigned <!-- tt-todo:{\"id\":\"td_2\",\"priority\":[],\"stream\":\"existing\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"sess-1\"]} -->\n",
                "- [ ] Different session <!-- tt-todo:{\"id\":\"td_3\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"sess-9\"]} -->\n",
            ),
        )
        .unwrap();
        let config = Config {
            database_path: temp.path().join("tt.db"),
            todo_store_path: store,
        };
        let session_stream_slugs = HashMap::from([("sess-1".to_string(), "proj-x".to_string())]);

        let lines = backfill_todo_streams(&config, &session_stream_slugs).unwrap();

        assert_eq!(lines, ["Backfilled stream 'proj-x' → td_1"]);
        let loaded = crate::todo_store::load_read_only(&config).unwrap();
        let todos: HashMap<_, _> = loaded
            .store
            .todos
            .items
            .iter()
            .filter_map(|file_line| {
                let TodoFileItem::Todo(todo) = &file_line.item else {
                    return None;
                };
                Some((todo.id.clone(), todo.stream.clone()))
            })
            .collect();
        assert_eq!(todos["td_1"].as_deref(), Some("proj-x"));
        assert_eq!(todos["td_2"].as_deref(), Some("existing"));
        assert_eq!(todos["td_3"].as_deref(), None);
    }

    #[test]
    fn backfill_skips_todo_store_when_no_sessions_were_assigned() {
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            database_path: temp.path().join("tt.db"),
            todo_store_path: temp.path().join("missing-store"),
        };

        let lines = backfill_todo_streams(&config, &HashMap::new()).unwrap();

        assert!(lines.is_empty());
        assert!(!config.todo_store_path.exists());
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
    fn stream_refs_include_only_streams_referenced_by_period_events() {
        let mut event = make_event(
            "e1",
            ts(0),
            tt_core::EventType::AgentToolUse,
            Some("sess-1"),
            "/project",
        );
        event.stream_id = Some("referenced".to_string());
        let streams = vec![
            tt_db::Stream {
                id: "referenced".to_string(),
                created_at: ts(0),
                updated_at: ts(0),
                name: Some("Referenced stream".to_string()),
                slug: Some("referenced-stream".to_string()),
                time_direct_ms: 0,
                time_delegated_ms: 0,
                first_event_at: None,
                last_event_at: None,
                needs_recompute: true,
            },
            tt_db::Stream {
                id: "other".to_string(),
                created_at: ts(0),
                updated_at: ts(0),
                name: Some("Other stream".to_string()),
                slug: Some("other-stream".to_string()),
                time_direct_ms: 0,
                time_delegated_ms: 0,
                first_event_at: None,
                last_event_at: None,
                needs_recompute: true,
            },
        ];

        let refs = streams_referenced_by_events(&[event], &streams);

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "referenced");
        assert_eq!(refs[0].slug.as_deref(), Some("referenced-stream"));
        assert_eq!(refs[0].name.as_deref(), Some("Referenced stream"));
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
                }],
                "streams": [{
                    "name": "proposal",
                    "slug": "proposal"
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let config = Config {
            database_path: dir.path().join("tt.db"),
            todo_store_path: dir.path().join("todo-store"),
        };
        run_apply(&db, &config, input_path.to_str().unwrap()).unwrap();

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
                    slug: "stream-x".to_string(),
                    tags: vec!["project:x".to_string()],
                },
                StreamDef {
                    name: "stream-y".to_string(),
                    slug: "stream-y".to_string(),
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
                slug: None,
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

    fn empty_apply_input() -> ClassifyApplyInput {
        ClassifyApplyInput {
            streams: vec![],
            assign_by_session: vec![],
            assign_by_pattern: vec![],
            assign_by_event_ids: vec![],
            assign_by_time: vec![],
        }
    }

    fn insert_session_event(db: &tt_db::Database, session_id: &str) {
        let event = make_event(
            "session-event",
            ts(0),
            tt_core::EventType::AgentToolUse,
            Some(session_id),
            "/project",
        );
        db.insert_event(&event).unwrap();
    }

    #[test]
    fn apply_rejects_unknown_assignment_stream() {
        let db = tt_db::Database::open_in_memory().unwrap();
        insert_session_event(&db, "sess-1");
        let input = ClassifyApplyInput {
            streams: vec![],
            assign_by_session: vec![SessionAssignment {
                session_id: "sess-1".to_string(),
                stream: "never-defined".to_string(),
            }],
            assign_by_pattern: vec![],
            assign_by_event_ids: vec![],
            assign_by_time: vec![],
        };

        let err = apply_input(&db, input).unwrap_err();

        assert!(err.to_string().contains("unknown stream"));
    }

    #[test]
    fn apply_creates_stream_with_slug_and_resolves_assignment_by_slug() {
        let db = tt_db::Database::open_in_memory().unwrap();
        insert_session_event(&db, "sess-1");
        let input = ClassifyApplyInput {
            streams: vec![StreamDef {
                name: "agent-c: eval-3 moto".to_string(),
                slug: "eval3-moto".to_string(),
                tags: vec![],
            }],
            assign_by_session: vec![SessionAssignment {
                session_id: "sess-1".to_string(),
                stream: "eval3-moto".to_string(),
            }],
            assign_by_pattern: vec![],
            assign_by_event_ids: vec![],
            assign_by_time: vec![],
        };

        let outcome = apply_input(&db, input).unwrap();

        let stream = db.get_stream_by_slug("eval3-moto").unwrap().unwrap();
        assert_eq!(stream.name.as_deref(), Some("agent-c: eval-3 moto"));
        assert_eq!(
            outcome.session_stream_slugs.get("sess-1"),
            Some(&"eval3-moto".to_string())
        );
    }

    #[test]
    fn apply_rejects_invalid_slug() {
        let db = tt_db::Database::open_in_memory().unwrap();
        let input = ClassifyApplyInput {
            streams: vec![StreamDef {
                name: "x".to_string(),
                slug: "Not A Slug".to_string(),
                tags: vec![],
            }],
            ..empty_apply_input()
        };

        assert!(apply_input(&db, input).is_err());
    }

    #[test]
    fn apply_reapply_same_slug_and_name_is_idempotent() {
        let db = tt_db::Database::open_in_memory().unwrap();
        let def = || StreamDef {
            name: "x".to_string(),
            slug: "x-slug".to_string(),
            tags: vec![],
        };

        apply_input(
            &db,
            ClassifyApplyInput {
                streams: vec![def()],
                ..empty_apply_input()
            },
        )
        .unwrap();
        apply_input(
            &db,
            ClassifyApplyInput {
                streams: vec![def()],
                ..empty_apply_input()
            },
        )
        .unwrap();

        assert_eq!(db.get_streams().unwrap().len(), 1);
    }

    #[test]
    fn apply_rejects_slug_collision_with_different_name() {
        let db = tt_db::Database::open_in_memory().unwrap();
        apply_input(
            &db,
            ClassifyApplyInput {
                streams: vec![StreamDef {
                    name: "x".to_string(),
                    slug: "shared".to_string(),
                    tags: vec![],
                }],
                ..empty_apply_input()
            },
        )
        .unwrap();

        let err = apply_input(
            &db,
            ClassifyApplyInput {
                streams: vec![StreamDef {
                    name: "different".to_string(),
                    slug: "shared".to_string(),
                    tags: vec![],
                }],
                ..empty_apply_input()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("shared"));
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
            slug: None,
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
            slug: None,
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
