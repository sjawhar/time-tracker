use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, Utc};
use tt_core::todos::{StreamTimeInput, Todo, compute_drift, priority_rank};
use tt_core::{AllocationConfig, EventType, StreamTime};
use tt_db::{
    ClassifierHealth, Database, LocalEventTypeStatus, ProposalStatus, StoredEvent, Stream,
    allocate_for_period,
};

use crate::Config;
use crate::commands::todo::{TopTodoView, top_todo_view};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Verdict {
    pub current_stream: Option<CurrentStream>,
    pub top_todo: Option<TopTodo>,
    pub aligned: Option<bool>,
    pub wip: WipStatus,
    pub alignment_share: Option<f64>,
    pub pending_proposals: u64,
    pub machines: Vec<MachineFreshness>,
    pub classifier: ClassifierHealth,
    /// `streams.md` links naming a stream that no longer exists.
    ///
    /// Carried on the verdict rather than logged and forgotten so `tt status` and the
    /// dashboard can say the priority mapping has fallen behind, instead of the whole
    /// verdict failing over one stale hand-edited line.
    pub dangling_stream_links: Vec<String>,
    /// Local event sources that used to report and have since gone quiet.
    ///
    /// Carried on the verdict for the same reason as `dangling_stream_links`, one step
    /// closer to the bone: an input to *direct time* can die while every number the product
    /// prints stays confident. The released `tt-watcher` did exactly that — it refused a
    /// schema it did not know, correctly, and `window_focus` and `afk_change` both stopped
    /// at 2026-07-29T05:01:20Z for nine days while `tmux_pane_focus` flowed normally. The
    /// verdict is the one place every status surface already reads, so a dead input has to
    /// arrive as data here rather than as a log line someone might grep for.
    pub stale_event_sources: Vec<StaleEventSource>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CurrentStream {
    pub stream_id: String,
    pub name: String,
    pub since: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub active: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TopTodo {
    pub id: String,
    pub text: String,
    pub stream_slug: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WipStatus {
    pub in_flight: Vec<StreamActivity>,
    pub limit: u32,
    pub wind_down_candidate: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StreamActivity {
    pub stream_id: String,
    pub name: String,
    pub direct_ms: i64,
    pub delegated_ms: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MachineFreshness {
    pub label: String,
    pub last_sync_at: Option<String>,
}

/// A local event source that used to report and has since fallen silent.
///
/// Both fields beyond the type are what make the warning actionable: `emitter` names the
/// process to go and look at, and `last_seen` bounds how much attention has already been
/// lost. A bare "`window_focus` is stale" would send the reader hunting for which of several
/// capture mechanisms owns it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct StaleEventSource {
    pub event_type: EventType,
    pub emitter: &'static str,
    pub last_seen: DateTime<Utc>,
}

/// A local event source worth noticing the absence of, and the process that emits it.
///
/// A table rather than a hardcoded pair of `if`s so adding a source is one line: name the
/// type and its emitter and the rule, the threshold, the CLI warning and the daemon's
/// change-detection all pick it up. Naming the watcher's two signature types explicitly is
/// deliberate — the alternative, monitoring *every* type this machine has ever produced,
/// would flag `browser_tab` (an unimplemented input) and every agent type the instant a
/// quiet week passed, which is how a real alarm becomes noise nobody reads.
struct MonitoredSource {
    event_type: EventType,
    emitter: &'static str,
}

const MONITORED_LOCAL_SOURCES: &[MonitoredSource] = &[
    MonitoredSource {
        event_type: EventType::WindowFocus,
        emitter: "tt-watcher",
    },
    MonitoredSource {
        event_type: EventType::AfkChange,
        emitter: "tt-watcher",
    },
    // The tmux hook, which is a separate mechanism from the watcher and fails separately.
    // It runs `tt` out of a `run-shell`, so it breaks on anything that makes that command
    // exit non-zero — `tt` missing from the hook's PATH, a schema the binary refuses, or a
    // config passing a flag the installed binary does not know. That last one happened:
    // deploying a hook config carrying `--pane-pid` to a host whose `tt` predated the flag
    // killed every pane-focus event there for two and a half hours, and the only trace was
    // `hook.log`, which nothing reads. Same shape as the nine-day watcher outage — capture
    // stops, every number stays confident, and nobody is told.
    //
    // `tmux_scroll` is deliberately not monitored: it fires only on entering copy-mode, so
    // a fortnight of ordinary work can pass without one and it would cry wolf.
    MonitoredSource {
        event_type: EventType::TmuxPaneFocus,
        emitter: "tmux hook (config/tmux-hook.conf)",
    },
];

/// A monitored local source silent for this many hours has stopped reporting.
///
/// Deliberately a constant rather than a config key, exactly like `DARK_AFTER_DAYS`: this
/// is a "something is broken" alarm, not a preference worth tuning away.
///
/// 72 hours is the smallest threshold that clears a healthy machine's longest silence. The
/// watcher emits on window changes and AFK transitions, so a machine nobody touches emits
/// nothing at all — silence means *away*, not *dead*. A night is ~10h, and the long case is
/// a weekend: stopping Friday 18:00 and resuming Monday 09:00 is 63 hours of legitimate
/// quiet, which rules out 24h and 48h. 72h leaves 9 hours of margin over that and would
/// have caught this incident on day 3 of 9, turning nine days of invisible attention into
/// three.
///
/// The asymmetry justifies erring short rather than long. A false positive costs one line
/// of status output saying, truthfully, that nothing has arrived in three days; a false
/// negative cost nine days of direct time that silently did not exist. A four-day holiday
/// weekend will therefore say so once, which is the right trade and not a reason to widen
/// this to a week — at a week, this incident would have been reported on day 7 of 9.
const STALE_LOCAL_SOURCE_HOURS: i64 = 72;

#[derive(Debug)]
struct InFlightStream {
    activity: StreamActivity,
    slug: Option<String>,
}

pub fn compute_verdict(db: &Database, config: &Config, now: DateTime<Utc>) -> Result<Verdict> {
    let allocation_config = AllocationConfig::default();
    let drift_start = now - Duration::minutes(i64::from(config.drift_window_min));
    let allocation = allocate_for_period(db, drift_start, now, Some(now), &allocation_config)
        .context("failed to allocate time for drift verdict")?;
    let events = db
        .get_events_in_range(drift_start, now)
        .context("failed to load focus events for drift verdict")?;
    let current_stream = current_stream(db, &events, now)?;
    let todo_view = top_todo_view(config, now.with_timezone(&Local).date_naive())
        .context("failed to load top todo for drift verdict")?;
    let top_todo = todo_view.top.clone();
    let top_stream_id = resolved_top_stream_id(db, top_todo.as_ref())?;
    let streams = db
        .get_streams()
        .context("failed to load streams for drift verdict")?;
    let in_flight = in_flight_streams(&allocation.stream_times, &streams);
    let wip_limit = usize::try_from(config.wip_limit).context("WIP limit does not fit in usize")?;
    let wind_down_candidate = (in_flight.len() > wip_limit)
        .then(|| weakest_priority_candidate(&in_flight, &todo_view))
        .flatten();
    let stream_times = stream_time_inputs(&streams, &allocation.stream_times);
    let drift = compute_drift(
        &todo_view.priorities,
        &todo_view.stream_links,
        &stream_times,
    )
    .context("failed to compute priority drift for verdict")?;
    let alignment_share = drift
        .priorities
        .iter()
        .max_by_key(|priority| priority.priority_value)
        .map(|priority| priority.direct_share);
    let dangling_stream_links = drift.dangling_stream_links;
    let pending_proposals = u64::try_from(
        db.get_proposals(Some(ProposalStatus::Pending))
            .context("failed to load pending proposals for drift verdict")?
            .len(),
    )
    .context("pending proposal count does not fit in u64")?;
    let machines = db
        .list_machines()
        .context("failed to load machines for drift verdict")?
        .into_iter()
        .map(|machine| MachineFreshness {
            label: machine.label,
            last_sync_at: machine.last_sync_at,
        })
        .collect();
    let classifier = db
        .get_classifier_health()
        .context("failed to load classifier health for drift verdict")?;
    let stale_event_sources = stale_event_sources(
        &db.last_local_event_per_type()
            .context("failed to load local event source freshness for drift verdict")?,
        now,
    );

    Ok(Verdict {
        aligned: current_stream
            .as_ref()
            .filter(|c| c.active)
            .zip(top_stream_id.as_ref())
            .map(|(current, top_stream_id)| current.stream_id == *top_stream_id),
        current_stream,
        top_todo,
        wip: WipStatus {
            in_flight: in_flight
                .into_iter()
                .map(|stream| stream.activity)
                .collect(),
            limit: config.wip_limit,
            wind_down_candidate,
        },
        alignment_share,
        pending_proposals,
        machines,
        classifier,
        dangling_stream_links,
        stale_event_sources,
    })
}

/// Local event sources that used to report on this machine and have gone quiet.
///
/// The expectation comes from the data: `last_local_event_per_type` returns a type only
/// once this machine has produced one, so a machine that never ran a watcher has nothing to
/// fall behind and is never flagged. That is why the rule reads the observed set and
/// filters it by [`MONITORED_LOCAL_SOURCES`], rather than walking the monitored list and
/// treating a missing type as broken.
///
/// Ordered by event type so the daemon's change detection and the CLI's output are stable.
fn stale_event_sources(
    observed: &[LocalEventTypeStatus],
    now: DateTime<Utc>,
) -> Vec<StaleEventSource> {
    let mut stale = observed
        .iter()
        .filter_map(|status| {
            let monitored = MONITORED_LOCAL_SOURCES
                .iter()
                .find(|source| source.event_type == status.event_type)?;
            (now - status.last_timestamp >= Duration::hours(STALE_LOCAL_SOURCE_HOURS)).then_some(
                StaleEventSource {
                    event_type: status.event_type,
                    emitter: monitored.emitter,
                    last_seen: status.last_timestamp,
                },
            )
        })
        .collect::<Vec<_>>();
    stale.sort_by_key(|source| source.event_type.to_string());
    stale
}

/// Determines whether the set of dangling stream links has changed.
///
/// Returns true if the set of dangling links differs between the previous and current
/// verdict. Order does not matter — only the set membership. This is used by the daemon
/// to avoid logging the same dangling links repeatedly.
pub fn should_announce_dangling_links(previous: &[String], current: &[String]) -> bool {
    warning_set_changed(previous, current)
}

/// Determines whether the set of stale local event sources has changed.
///
/// Compares whole entries, `last_seen` included, which is what the daemon wants: a silent
/// source's `last_seen` is frozen by definition, so it moves only when the source came back
/// and stopped again — worth saying out loud.
pub fn should_announce_stale_sources(
    previous: &[StaleEventSource],
    current: &[StaleEventSource],
) -> bool {
    warning_set_changed(previous, current)
}

/// Whether a set of verdict warnings differs from the one last announced.
///
/// Shared by both announcement helpers because the split they implement is one rule, not
/// two: a one-shot CLI invocation announces everything every time, while the daemon
/// recomputes on every `db_version` change and must only speak when the set moves. Measured
/// on the live daemon, repeating an unchanged set was 92% of all log lines — roughly one
/// every nine seconds — which buried the real defects.
fn warning_set_changed<T: Eq + Hash>(previous: &[T], current: &[T]) -> bool {
    if previous.len() != current.len() {
        return true;
    }
    let previous_set: HashSet<_> = previous.iter().collect();
    let current_set: HashSet<_> = current.iter().collect();
    previous_set != current_set
}

fn current_stream(
    db: &Database,
    events: &[StoredEvent],
    now: DateTime<Utc>,
) -> Result<Option<CurrentStream>> {
    let attention_start =
        now - Duration::milliseconds(AllocationConfig::default().attention_window_ms);
    let Some((latest_index, latest)) = events
        .iter()
        .enumerate()
        .rev()
        .find(|(_, event)| is_focus_event(event) && event.stream_id.is_some())
    else {
        return Ok(None);
    };
    let Some(stream_id) = latest.stream_id.as_deref() else {
        return Ok(None);
    };
    let active = latest.timestamp >= attention_start;
    let mut since = latest.timestamp;
    for event in events[..latest_index]
        .iter()
        .rev()
        .filter(|event| is_focus_event(event))
    {
        // An event naming no stream is absence of evidence, not evidence of a different
        // stream, so it neither extends the run nor ends it. Breaking here instead would
        // truncate almost every run to nothing: `tmux_pane_focus` is 86.7% unattributable
        // and interleaves constantly with the `window_focus` events that do carry a stream.
        if event.stream_id.is_none() {
            continue;
        }
        if event.stream_id.as_deref() != Some(stream_id) {
            break;
        }
        since = event.timestamp;
    }
    let name = db
        .get_stream(stream_id)
        .context("failed to resolve current focus stream")?
        .map_or_else(
            || stream_id.to_string(),
            |stream| stream_display_name(&stream),
        );
    Ok(Some(CurrentStream {
        stream_id: stream_id.to_string(),
        name,
        since,
        last_seen: latest.timestamp,
        active,
    }))
}

const fn is_focus_event(event: &StoredEvent) -> bool {
    matches!(
        event.event_type,
        EventType::TmuxPaneFocus | EventType::WindowFocus
    )
}

fn resolved_top_stream_id(db: &Database, top_todo: Option<&TopTodo>) -> Result<Option<String>> {
    let Some(stream_slug) = top_todo.and_then(|todo| todo.stream_slug.as_deref()) else {
        return Ok(None);
    };
    Ok(db
        .get_stream_by_slug(stream_slug)
        .context("failed to resolve top todo stream")?
        .map(|stream| stream.id))
}

fn in_flight_streams(stream_times: &[StreamTime], streams: &[Stream]) -> Vec<InFlightStream> {
    let streams_by_id = streams
        .iter()
        .map(|stream| (stream.id.as_str(), stream))
        .collect::<HashMap<_, _>>();
    let mut in_flight = stream_times
        .iter()
        .filter(|time| time.time_direct_ms > 0 || time.time_delegated_ms > 0)
        // Junk is the reserved home for sessions with no attributable work, so it is by
        // definition not work in progress. Counting it inflated WIP against the limit on
        // the dashboard's own panel and made it eligible as a wind-down candidate, which
        // would advise dropping the one stream that holds nothing. `tt report` already
        // renders junk on its own unranked line for the same reason.
        .filter(|time| {
            streams_by_id
                .get(time.stream_id.as_str())
                .and_then(|stream| stream.slug.as_deref())
                != Some(tt_db::JUNK_STREAM_SLUG)
        })
        .map(|time| {
            let stream = streams_by_id.get(time.stream_id.as_str()).copied();
            InFlightStream {
                activity: StreamActivity {
                    stream_id: time.stream_id.clone(),
                    name: stream.map_or_else(|| time.stream_id.clone(), stream_display_name),
                    direct_ms: time.time_direct_ms,
                    delegated_ms: time.time_delegated_ms,
                },
                slug: stream.and_then(|stream| stream.slug.clone()),
            }
        })
        .collect::<Vec<_>>();
    in_flight.sort_by(|left, right| {
        left.activity
            .name
            .cmp(&right.activity.name)
            .then(left.activity.stream_id.cmp(&right.activity.stream_id))
    });
    in_flight
}

fn stream_time_inputs(streams: &[Stream], stream_times: &[StreamTime]) -> Vec<StreamTimeInput> {
    let times_by_stream_id = stream_times
        .iter()
        .map(|time| (time.stream_id.as_str(), time))
        .collect::<HashMap<_, _>>();
    streams
        .iter()
        .filter_map(|stream| {
            stream
                .slug
                .clone()
                .or_else(|| stream.name.clone())
                .map(|stream_name| {
                    let time = times_by_stream_id.get(stream.id.as_str()).copied();
                    StreamTimeInput {
                        stream_name,
                        direct_ms: time.map_or(0, |time| time.time_direct_ms),
                        delegated_ms: time.map_or(0, |time| time.time_delegated_ms),
                    }
                })
        })
        .collect()
}

fn weakest_priority_candidate(
    in_flight: &[InFlightStream],
    todo_view: &TopTodoView,
) -> Option<String> {
    in_flight
        .iter()
        .min_by(|left, right| priority_linkage_order(left, right, todo_view))
        .map(|stream| {
            stream
                .slug
                .clone()
                .unwrap_or_else(|| stream.activity.name.clone())
        })
}

fn priority_linkage_order(
    left: &InFlightStream,
    right: &InFlightStream,
    todo_view: &TopTodoView,
) -> Ordering {
    match (
        stream_priority_rank(left.slug.as_deref(), todo_view),
        stream_priority_rank(right.slug.as_deref(), todo_view),
    ) {
        (None, None) => left.activity.stream_id.cmp(&right.activity.stream_id),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left_rank), Some(right_rank)) => left_rank
            .cmp(&right_rank)
            .then_with(|| left.activity.stream_id.cmp(&right.activity.stream_id)),
    }
}

fn stream_priority_rank(stream_slug: Option<&str>, todo_view: &TopTodoView) -> Option<i32> {
    let stream = stream_slug?;
    priority_rank(
        &Todo {
            id: String::new(),
            text: String::new(),
            priority: Vec::new(),
            stream: Some(stream.to_string()),
            when: None,
            due: None,
            pin: false,
            quick: false,
            done: false,
            block: None,
            sessions: Vec::new(),
        },
        &todo_view.priorities,
        &todo_view.stream_links,
    )
}

fn stream_display_name(stream: &Stream) -> String {
    stream
        .name
        .clone()
        .or_else(|| stream.slug.clone())
        .unwrap_or_else(|| stream.id.clone())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;
    use tempfile::TempDir;
    use tt_core::EventType;
    use tt_db::{Database, StoredEvent, Stream};

    use super::{
        StaleEventSource, compute_verdict, should_announce_dangling_links,
        should_announce_stale_sources,
    };
    use crate::Config;

    #[test]
    fn verdict_aligns_current_stream_with_linked_top_todo() -> anyhow::Result<()> {
        // Given: the top todo and current focus are linked to the same stream.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        write_todo_store(&config, "alpha", "high", 10, "Ship alpha")?;
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();
        insert_stream(&db, "stream-alpha", "alpha", "Alpha work", now)?;
        insert_focus(
            &db,
            "focus-alpha",
            now - Duration::minutes(2),
            "stream-alpha",
        )?;

        // When: the status verdict is computed for the active window.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: the current stream is aligned with the linked top todo.
        assert_eq!(
            verdict
                .current_stream
                .as_ref()
                .map(|stream| stream.stream_id.as_str()),
            Some("stream-alpha")
        );
        assert_eq!(
            verdict.top_todo.as_ref().map(|todo| todo.text.as_str()),
            Some("Ship alpha")
        );
        assert_eq!(verdict.aligned, Some(true));
        assert_eq!(verdict.alignment_share, Some(1.0));
        Ok(())
    }

    #[test]
    fn verdict_marks_drift_when_current_stream_differs_from_top_todo() -> anyhow::Result<()> {
        // Given: the top todo is linked to alpha while current focus is beta.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        write_store(
            &config,
            "- [ ] High <!-- tt-priority:{\"slug\":\"high\",\"value\":10,\"status\":\"active\"} -->\n- [ ] Low <!-- tt-priority:{\"slug\":\"low\",\"value\":1,\"status\":\"active\"} -->\n",
            "- [ ] Ship alpha <!-- tt-todo:{\"id\":\"td_top000001\",\"priority\":[],\"stream\":\"alpha\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
            "- alpha <!-- tt-stream:{\"priority\":\"high\"} -->\n- beta <!-- tt-stream:{\"priority\":\"low\"} -->\n",
        )?;
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();
        insert_stream(&db, "stream-alpha", "alpha", "Alpha work", now)?;
        insert_stream(&db, "stream-beta", "beta", "Beta work", now)?;
        insert_focus(
            &db,
            "focus-alpha",
            now - Duration::minutes(4),
            "stream-alpha",
        )?;
        insert_focus(&db, "focus-beta", now - Duration::minutes(2), "stream-beta")?;

        // When: the status verdict is computed for the active window.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: the current stream is explicitly marked as drifting.
        assert_eq!(
            verdict
                .current_stream
                .as_ref()
                .map(|stream| stream.stream_id.as_str()),
            Some("stream-beta")
        );
        assert_eq!(verdict.aligned, Some(false));
        Ok(())
    }

    #[test]
    fn verdict_selects_lowest_priority_in_flight_stream_to_wind_down() -> anyhow::Result<()> {
        // Given: two active streams exceed the WIP limit and beta has the lower priority.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 1);
        write_store(
            &config,
            "- [ ] High <!-- tt-priority:{\"slug\":\"high\",\"value\":10,\"status\":\"active\"} -->\n- [ ] Low <!-- tt-priority:{\"slug\":\"low\",\"value\":1,\"status\":\"active\"} -->\n",
            "- [ ] Ship alpha <!-- tt-todo:{\"id\":\"td_top000001\",\"priority\":[],\"stream\":\"alpha\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
            "- alpha <!-- tt-stream:{\"priority\":\"high\"} -->\n- beta <!-- tt-stream:{\"priority\":\"low\"} -->\n",
        )?;
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();
        insert_stream(&db, "stream-alpha", "alpha", "Alpha work", now)?;
        insert_stream(&db, "stream-beta", "beta", "Beta work", now)?;
        insert_focus(
            &db,
            "focus-alpha-1",
            now - Duration::minutes(4),
            "stream-alpha",
        )?;
        insert_focus(&db, "focus-beta", now - Duration::minutes(3), "stream-beta")?;
        insert_focus(
            &db,
            "focus-alpha-2",
            now - Duration::minutes(2),
            "stream-alpha",
        )?;

        // When: the status verdict is computed for the active window.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: the lower-priority beta stream is the wind-down candidate.
        assert_eq!(verdict.wip.in_flight.len(), 2);
        assert_eq!(verdict.wip.limit, 1);
        assert_eq!(verdict.wip.wind_down_candidate.as_deref(), Some("beta"));
        Ok(())
    }

    #[test]
    fn the_junk_stream_is_never_work_in_progress() -> anyhow::Result<()> {
        // Given: real work and the reserved junk stream are both active.
        //
        // Junk holds sessions with no attributable work, so listing it as WIP put
        // "junk: no attributable work" on the dashboard's own in-flight panel, counted it
        // against the WIP limit, and made it selectable as the thing to wind down.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        write_store(
            &config,
            "- [ ] High <!-- tt-priority:{\"slug\":\"high\",\"value\":10,\"status\":\"active\"} -->\n",
            "",
            "- alpha <!-- tt-stream:{\"priority\":\"high\"} -->\n",
        )?;
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();
        insert_stream(&db, "stream-alpha", "alpha", "Alpha work", now)?;
        insert_stream(
            &db,
            "stream-junk",
            tt_db::JUNK_STREAM_SLUG,
            "junk: no attributable work",
            now,
        )?;
        insert_focus(
            &db,
            "focus-alpha",
            now - Duration::minutes(4),
            "stream-alpha",
        )?;
        insert_focus(&db, "focus-junk", now - Duration::minutes(3), "stream-junk")?;

        // When: the status verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: only the real work is in flight.
        assert!(
            !verdict
                .wip
                .in_flight
                .iter()
                .any(|stream| stream.stream_id == "stream-junk"),
            "junk must never be listed as work in progress"
        );
        assert_eq!(verdict.wip.in_flight.len(), 1);
        assert_eq!(verdict.wip.in_flight[0].stream_id, "stream-alpha");
        Ok(())
    }

    #[test]
    fn verdict_is_empty_when_database_has_no_activity() -> anyhow::Result<()> {
        // Given: an empty database and todo store.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();

        // When: the status verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: every optional activity signal is absent.
        assert!(verdict.current_stream.is_none());
        assert!(verdict.top_todo.is_none());
        assert!(verdict.aligned.is_none());
        assert!(verdict.alignment_share.is_none());
        assert!(verdict.wip.in_flight.is_empty());
        assert!(verdict.wip.wind_down_candidate.is_none());
        assert_eq!(verdict.pending_proposals, 0);
        assert!(verdict.machines.is_empty());
        assert!(verdict.classifier.last_success_at.is_none());
        assert!(verdict.dangling_stream_links.is_empty());
        assert!(verdict.stale_event_sources.is_empty());
        Ok(())
    }

    #[test]
    fn verdict_survives_a_stream_link_naming_a_dissolved_stream() -> anyhow::Result<()> {
        // Given: streams.md links a live stream and two that have since been dissolved,
        // the shape that took /api/status down with an HTTP 500.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        write_store(
            &config,
            "- [ ] High <!-- tt-priority:{\"slug\":\"high\",\"value\":10,\"status\":\"active\"} -->\n",
            "- [ ] Ship alpha <!-- tt-todo:{\"id\":\"td_top000001\",\"priority\":[],\"stream\":\"alpha\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
            "- alpha <!-- tt-stream:{\"priority\":\"high\"} -->\n\
             - meetings-coord <!-- tt-stream:{\"priority\":\"high\"} -->\n\
             - team-coordination <!-- tt-stream:{\"priority\":\"high\"} -->\n",
        )?;
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();
        insert_stream(&db, "stream-alpha", "alpha", "Alpha work", now)?;
        insert_focus(
            &db,
            "focus-alpha",
            now - Duration::minutes(2),
            "stream-alpha",
        )?;

        // When: the status verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: a verdict is produced from the links that resolve, and the two that do not
        // are named on it rather than aborting the computation.
        assert_eq!(
            verdict.dangling_stream_links,
            vec![
                "meetings-coord".to_string(),
                "team-coordination".to_string()
            ]
        );
        assert_eq!(verdict.aligned, Some(true));
        assert_eq!(verdict.alignment_share, Some(1.0));
        Ok(())
    }

    #[test]
    fn verdict_flags_a_local_event_source_that_has_gone_silent() -> anyhow::Result<()> {
        // Given: this machine produced watcher events until nine days ago and none since,
        // while the tmux hook — a separate mechanism — kept reporting normally. That is the
        // incident: the released `tt-watcher` failed closed on a schema it did not know,
        // systemd restarted it 35,051 times, and every status surface stayed confident.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).single().unwrap();
        let went_silent = now - Duration::days(9);
        insert_local_event(&db, "focus-old", EventType::WindowFocus, went_silent)?;
        insert_local_event(&db, "afk-old", EventType::AfkChange, went_silent)?;
        insert_local_event(
            &db,
            "tmux-now",
            EventType::TmuxPaneFocus,
            now - Duration::minutes(2),
        )?;

        // When: the status verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: both watcher types are named with when they were last seen, and the
        // still-reporting tmux hook is not among them.
        assert_eq!(
            verdict.stale_event_sources,
            vec![
                StaleEventSource {
                    event_type: EventType::AfkChange,
                    emitter: "tt-watcher",
                    last_seen: went_silent,
                },
                StaleEventSource {
                    event_type: EventType::WindowFocus,
                    emitter: "tt-watcher",
                    last_seen: went_silent,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn verdict_never_flags_a_source_this_machine_has_not_produced() -> anyhow::Result<()> {
        // Given: a machine that has only ever produced tmux events — a server, or a fresh
        // install. Absence is not staleness: nothing here was ever expected to emit window
        // focus, so flagging it would make every headless box cry wolf forever.
        //
        // The tmux event is recent because `TmuxPaneFocus` is itself monitored; the property
        // under test is that the *absent* type is not flagged, not that a stale one escapes.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).single().unwrap();
        insert_local_event(
            &db,
            "tmux-recent",
            EventType::TmuxPaneFocus,
            now - Duration::hours(1),
        )?;

        // When: the status verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: nothing is flagged, despite never having produced a single window_focus.
        assert!(
            verdict.stale_event_sources.is_empty(),
            "a type this machine never produced must not be flagged: {:?}",
            verdict.stale_event_sources
        );
        Ok(())
    }

    #[test]
    fn a_dead_tmux_hook_announces_itself() -> anyhow::Result<()> {
        // The tmux hook is a separate mechanism from the watcher and fails separately: it
        // runs `tt` from a `run-shell`, so anything making that exit non-zero stops capture
        // silently. Deploying a hook config carrying `--pane-pid` to a host whose `tt`
        // predated the flag killed every pane-focus event there for two and a half hours,
        // and the only trace was `hook.log`, which nothing reads.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).single().unwrap();
        insert_local_event(
            &db,
            "tmux-stale",
            EventType::TmuxPaneFocus,
            now - Duration::days(5),
        )?;

        let verdict = compute_verdict(&db, &config, now)?;

        let flagged = verdict
            .stale_event_sources
            .iter()
            .find(|source| source.event_type == EventType::TmuxPaneFocus)
            .expect("a tmux hook silent for five days is a dead input");
        assert!(
            flagged.emitter.contains("tmux"),
            "the report must name what owes the events: {flagged:?}"
        );
        Ok(())
    }

    #[test]
    fn verdict_does_not_flag_a_source_still_producing_events() -> anyhow::Result<()> {
        // Given: a watcher that emitted moments ago.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).single().unwrap();
        insert_local_event(
            &db,
            "focus-now",
            EventType::WindowFocus,
            now - Duration::minutes(2),
        )?;
        insert_local_event(
            &db,
            "afk-now",
            EventType::AfkChange,
            now - Duration::minutes(3),
        )?;

        // When: the status verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: a live source is never named.
        assert!(verdict.stale_event_sources.is_empty());
        Ok(())
    }

    #[test]
    fn a_weekend_of_silence_is_not_a_dead_source() -> anyhow::Result<()> {
        // Given: a watcher last seen Friday 18:00, read on Monday 09:00 — 63 hours, the
        // longest silence a healthy watcher produces, because it emits on window changes
        // and a machine nobody touches changes no windows. It must not be flagged; one
        // hour later past the threshold it must be.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let friday_evening = Utc.with_ymd_and_hms(2026, 8, 7, 18, 0, 0).single().unwrap();
        insert_local_event(&db, "focus-fri", EventType::WindowFocus, friday_evening)?;

        // When / Then: a normal weekend is silent, and 73 hours is not.
        let monday_morning = friday_evening + Duration::hours(63);
        assert!(
            compute_verdict(&db, &config, monday_morning)?
                .stale_event_sources
                .is_empty(),
            "a 63-hour weekend must not be reported as a dead input"
        );
        assert_eq!(
            compute_verdict(&db, &config, friday_evening + Duration::hours(73))?
                .stale_event_sources
                .len(),
            1,
            "silence past the threshold must be reported"
        );
        Ok(())
    }

    fn test_config(temp: &TempDir, wip_limit: u32) -> Config {
        Config {
            database_path: temp.path().join("tt.db"),
            todo_store_path: temp.path().join("todos"),
            wip_limit,
            ..Config::default()
        }
    }

    fn write_todo_store(
        config: &Config,
        stream_slug: &str,
        priority_slug: &str,
        priority_value: i32,
        todo_text: &str,
    ) -> anyhow::Result<()> {
        write_store(
            config,
            &format!(
                "- [ ] {priority_slug} <!-- tt-priority:{{\"slug\":\"{priority_slug}\",\"value\":{priority_value},\"status\":\"active\"}} -->\n"
            ),
            &format!(
                "- [ ] {todo_text} <!-- tt-todo:{{\"id\":\"td_top000001\",\"priority\":[],\"stream\":\"{stream_slug}\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false}} -->\n"
            ),
            &format!("- {stream_slug} <!-- tt-stream:{{\"priority\":\"{priority_slug}\"}} -->\n"),
        )
    }

    fn write_store(
        config: &Config,
        priorities: &str,
        todos: &str,
        stream_links: &str,
    ) -> anyhow::Result<()> {
        std::fs::create_dir_all(&config.todo_store_path)?;
        std::fs::write(config.todo_store_path.join("priorities.md"), priorities)?;
        std::fs::write(config.todo_store_path.join("todos.md"), todos)?;
        std::fs::write(config.todo_store_path.join("streams.md"), stream_links)?;
        Ok(())
    }

    fn insert_stream(
        db: &Database,
        id: &str,
        slug: &str,
        name: &str,
        created_at: chrono::DateTime<Utc>,
    ) -> anyhow::Result<()> {
        db.insert_stream(&Stream {
            id: id.to_string(),
            name: Some(name.to_string()),
            slug: Some(slug.to_string()),
            description: None,
            color: None,
            created_at,
            updated_at: created_at,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        })?;
        Ok(())
    }

    fn insert_focus(
        db: &Database,
        id: &str,
        timestamp: chrono::DateTime<Utc>,
        stream_id: &str,
    ) -> anyhow::Result<()> {
        db.insert_event(&StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type: EventType::TmuxPaneFocus,
            source: "test".to_string(),
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
            stream_id: Some(stream_id.to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
        Ok(())
    }

    /// A focus event carrying no stream, which is what 86.7% of `tmux_pane_focus`
    /// rows are: they expose no title, no app id and no session, so nothing in the
    /// tree can legitimately attribute one.
    fn insert_unattributed_focus(
        db: &Database,
        id: &str,
        timestamp: chrono::DateTime<Utc>,
    ) -> anyhow::Result<()> {
        db.insert_event(&StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type: EventType::TmuxPaneFocus,
            source: "test".to_string(),
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
        })?;
        Ok(())
    }

    #[test]
    fn an_unattributable_pane_does_not_blank_the_current_stream() -> anyhow::Result<()> {
        // Given: attributed focus, then a later pane nothing can attribute.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();
        insert_stream(&db, "stream-alpha", "alpha", "Alpha work", now)?;
        insert_focus(
            &db,
            "focus-alpha",
            now - Duration::minutes(3),
            "stream-alpha",
        )?;
        insert_unattributed_focus(&db, "pane-blank", now - Duration::seconds(10))?;

        // When: the verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: the rail still names the newest stream anything could attribute.
        // Reading only the single newest focus event blanked it 77% of the time,
        // because panes fire more often than window focus and almost never resolve.
        assert_eq!(
            verdict
                .current_stream
                .as_ref()
                .map(|stream| stream.stream_id.as_str()),
            Some("stream-alpha")
        );
        Ok(())
    }

    #[test]
    fn the_current_stream_is_absent_when_no_focus_in_the_window_resolves() -> anyhow::Result<()> {
        // Given: recent focus activity, none of which names a stream.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();
        insert_unattributed_focus(&db, "pane-one", now - Duration::minutes(2))?;
        insert_unattributed_focus(&db, "pane-two", now - Duration::seconds(20))?;

        // When: the verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: it stays absent. Unattributable focus is left unassigned rather than
        // given a container, so the rail reports nothing instead of inventing one.
        assert!(verdict.current_stream.is_none());
        Ok(())
    }

    #[test]
    fn an_unattributable_pane_does_not_truncate_the_focus_run() -> anyhow::Result<()> {
        // Given: one stream's focus run interrupted by a pane nothing can attribute.
        let temp = TempDir::new()?;
        let config = test_config(&temp, 4);
        let db = Database::open_in_memory()?;
        let now = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 0, 0)
            .single()
            .unwrap();
        insert_stream(&db, "stream-alpha", "alpha", "Alpha work", now)?;
        let run_start = now - Duration::minutes(4);
        insert_focus(&db, "focus-early", run_start, "stream-alpha")?;
        insert_unattributed_focus(&db, "pane-mid", now - Duration::minutes(2))?;
        insert_focus(
            &db,
            "focus-late",
            now - Duration::minutes(1),
            "stream-alpha",
        )?;

        // When: the verdict is computed.
        let verdict = compute_verdict(&db, &config, now)?;

        // Then: the run spans the gap. Absence of evidence is not evidence of a
        // different stream, so a blank pane neither extends the run nor ends it.
        assert_eq!(
            verdict.current_stream.as_ref().map(|stream| stream.since),
            Some(run_start)
        );
        Ok(())
    }

    fn insert_local_event(
        db: &Database,
        id: &str,
        event_type: EventType,
        timestamp: chrono::DateTime<Utc>,
    ) -> anyhow::Result<()> {
        db.insert_event(&StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type,
            source: "local.cosmic".to_string(),
            machine_id: Some("local-uuid".to_string()),
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
        })?;
        Ok(())
    }

    #[test]
    fn test_should_announce_dangling_links_unchanged_set_is_silent() {
        let previous = vec![
            "meetings-coord".to_string(),
            "team-coordination".to_string(),
        ];
        let current = vec![
            "meetings-coord".to_string(),
            "team-coordination".to_string(),
        ];
        assert!(!should_announce_dangling_links(&previous, &current));
    }

    #[test]
    fn test_should_announce_dangling_links_new_link_announces() {
        let previous = vec!["meetings-coord".to_string()];
        let current = vec![
            "meetings-coord".to_string(),
            "team-coordination".to_string(),
        ];
        assert!(should_announce_dangling_links(&previous, &current));
    }

    #[test]
    fn test_should_announce_dangling_links_removed_link_announces() {
        let previous = vec![
            "meetings-coord".to_string(),
            "team-coordination".to_string(),
        ];
        let current = vec!["meetings-coord".to_string()];
        assert!(should_announce_dangling_links(&previous, &current));
    }

    #[test]
    fn test_should_announce_dangling_links_empty_to_nonempty_announces() {
        let previous: Vec<String> = vec![];
        let current = vec!["meetings-coord".to_string()];
        assert!(should_announce_dangling_links(&previous, &current));
    }

    #[test]
    fn test_should_announce_dangling_links_nonempty_to_empty_announces() {
        let previous = vec!["meetings-coord".to_string()];
        let current: Vec<String> = vec![];
        assert!(should_announce_dangling_links(&previous, &current));
    }

    #[test]
    fn test_should_announce_dangling_links_different_order_same_set_is_silent() {
        let previous = vec![
            "meetings-coord".to_string(),
            "team-coordination".to_string(),
        ];
        let current = vec![
            "team-coordination".to_string(),
            "meetings-coord".to_string(),
        ];
        assert!(!should_announce_dangling_links(&previous, &current));
    }

    #[test]
    fn test_should_announce_stale_sources_unchanged_set_is_silent() {
        let last_seen = Utc
            .with_ymd_and_hms(2026, 7, 29, 5, 1, 20)
            .single()
            .unwrap();
        let previous = vec![stale(EventType::WindowFocus, last_seen)];
        let current = vec![stale(EventType::WindowFocus, last_seen)];
        assert!(!should_announce_stale_sources(&previous, &current));
    }

    #[test]
    fn test_should_announce_stale_sources_new_source_announces() {
        let last_seen = Utc
            .with_ymd_and_hms(2026, 7, 29, 5, 1, 20)
            .single()
            .unwrap();
        let previous = vec![stale(EventType::WindowFocus, last_seen)];
        let current = vec![
            stale(EventType::WindowFocus, last_seen),
            stale(EventType::AfkChange, last_seen),
        ];
        assert!(should_announce_stale_sources(&previous, &current));
    }

    #[test]
    fn test_should_announce_stale_sources_recovered_source_announces() {
        let last_seen = Utc
            .with_ymd_and_hms(2026, 7, 29, 5, 1, 20)
            .single()
            .unwrap();
        let previous = vec![stale(EventType::WindowFocus, last_seen)];
        let current: Vec<StaleEventSource> = vec![];
        assert!(should_announce_stale_sources(&previous, &current));
    }

    #[test]
    fn test_should_announce_stale_sources_different_order_same_set_is_silent() {
        let last_seen = Utc
            .with_ymd_and_hms(2026, 7, 29, 5, 1, 20)
            .single()
            .unwrap();
        let previous = vec![
            stale(EventType::WindowFocus, last_seen),
            stale(EventType::AfkChange, last_seen),
        ];
        let current = vec![
            stale(EventType::AfkChange, last_seen),
            stale(EventType::WindowFocus, last_seen),
        ];
        assert!(!should_announce_stale_sources(&previous, &current));
    }

    fn stale(event_type: EventType, last_seen: chrono::DateTime<Utc>) -> StaleEventSource {
        StaleEventSource {
            event_type,
            emitter: "tt-watcher",
            last_seen,
        }
    }
}
