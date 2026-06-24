use std::collections::{HashMap, HashSet};

use serde::Serialize;
use thiserror::Error;

use super::model::{Priority, PriorityStatus, StreamPriorityLink};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamTimeInput {
    pub stream_name: String,
    pub direct_ms: i64,
    pub delegated_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriftReport {
    pub priorities: Vec<PriorityDrift>,
    pub unattributed: UnattributedDrift,
    pub total_direct_ms: i64,
    pub total_direct_plus_delegated_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PriorityDrift {
    pub priority_slug: String,
    pub priority_value: i32,
    pub importance_share: f64,
    pub direct_ms: i64,
    pub direct_plus_delegated_ms: i64,
    pub direct_share: f64,
    pub direct_plus_delegated_share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnattributedDrift {
    pub direct_ms: i64,
    pub direct_plus_delegated_ms: i64,
    pub direct_share: f64,
    pub direct_plus_delegated_share: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DriftError {
    #[error("stream link references unknown stream: {stream}")]
    UnresolvedStream { stream: String },
    #[error("stream link references unknown priority: {priority}")]
    UnresolvedPriority { priority: String },
    #[error("stream has multiple priority links: {stream}")]
    DuplicateStreamLink { stream: String },
    #[error("stream time for {stream} is negative")]
    NegativeStreamTime { stream: String },
}

pub fn compute_drift(
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
    stream_times: &[StreamTimeInput],
) -> Result<DriftReport, DriftError> {
    let active_priorities = active_priorities(priorities);
    let time_by_stream = aggregate_stream_times(stream_times)?;
    let priority_statuses = priority_statuses(priorities);
    let linked_streams = validate_links(stream_links, &time_by_stream, &priority_statuses)?;
    let totals = total_time(&time_by_stream);
    let priority_totals = totals_by_priority(&linked_streams);
    let importance_total: i32 = active_priorities
        .iter()
        .map(|priority| priority.value)
        .sum();
    let priority_drifts = active_priorities
        .iter()
        .map(|priority| priority_drift(priority, importance_total, totals, &priority_totals))
        .collect();
    let unattributed = unattributed_drift(&linked_streams, &time_by_stream, totals);

    Ok(DriftReport {
        priorities: priority_drifts,
        unattributed,
        total_direct_ms: totals.direct_ms,
        total_direct_plus_delegated_ms: totals.direct_plus_delegated_ms,
    })
}

fn active_priorities(priorities: &[Priority]) -> Vec<&Priority> {
    priorities
        .iter()
        .filter(|priority| priority.status == PriorityStatus::Active)
        .collect()
}

fn priority_statuses(priorities: &[Priority]) -> HashMap<&str, PriorityStatus> {
    priorities
        .iter()
        .map(|priority| (priority.slug.as_str(), priority.status))
        .collect()
}

#[derive(Debug, Clone, Copy, Default)]
struct TimeTotals {
    direct_ms: i64,
    direct_plus_delegated_ms: i64,
}

#[derive(Debug, Clone, Copy)]
struct ActiveStreamLink<'a> {
    priority: &'a str,
    time: TimeTotals,
}

fn aggregate_stream_times(
    stream_times: &[StreamTimeInput],
) -> Result<HashMap<&str, TimeTotals>, DriftError> {
    let mut time_by_stream = HashMap::new();
    for stream_time in stream_times {
        if stream_time.direct_ms < 0 || stream_time.delegated_ms < 0 {
            return Err(DriftError::NegativeStreamTime {
                stream: stream_time.stream_name.clone(),
            });
        }
        let entry = time_by_stream
            .entry(stream_time.stream_name.as_str())
            .or_insert_with(TimeTotals::default);
        entry.direct_ms += stream_time.direct_ms;
        entry.direct_plus_delegated_ms += stream_time.direct_ms + stream_time.delegated_ms;
    }
    Ok(time_by_stream)
}

fn validate_links<'a>(
    stream_links: &'a [StreamPriorityLink],
    time_by_stream: &HashMap<&str, TimeTotals>,
    priority_statuses: &HashMap<&str, PriorityStatus>,
) -> Result<HashMap<&'a str, ActiveStreamLink<'a>>, DriftError> {
    let mut linked_streams = HashMap::new();
    let mut seen_streams = HashSet::new();
    for link in stream_links {
        if !seen_streams.insert(link.stream.as_str()) {
            return Err(DriftError::DuplicateStreamLink {
                stream: link.stream.clone(),
            });
        }
        let Some(stream_time) = time_by_stream.get(link.stream.as_str()).copied() else {
            return Err(DriftError::UnresolvedStream {
                stream: link.stream.clone(),
            });
        };
        match priority_status(link, priority_statuses)? {
            PriorityStatus::Active => {
                linked_streams.insert(
                    link.stream.as_str(),
                    ActiveStreamLink {
                        priority: link.priority.as_str(),
                        time: stream_time,
                    },
                );
            }
            PriorityStatus::Done | PriorityStatus::Dropped => {}
        }
    }
    Ok(linked_streams)
}

fn priority_status(
    link: &StreamPriorityLink,
    priority_statuses: &HashMap<&str, PriorityStatus>,
) -> Result<PriorityStatus, DriftError> {
    priority_statuses
        .get(link.priority.as_str())
        .copied()
        .ok_or_else(|| DriftError::UnresolvedPriority {
            priority: link.priority.clone(),
        })
}

fn total_time(time_by_stream: &HashMap<&str, TimeTotals>) -> TimeTotals {
    time_by_stream
        .values()
        .fold(TimeTotals::default(), |total, stream| TimeTotals {
            direct_ms: total.direct_ms + stream.direct_ms,
            direct_plus_delegated_ms: total.direct_plus_delegated_ms
                + stream.direct_plus_delegated_ms,
        })
}

fn totals_by_priority<'a>(
    linked_streams: &HashMap<&'a str, ActiveStreamLink<'a>>,
) -> HashMap<&'a str, TimeTotals> {
    let mut priority_totals = HashMap::new();
    for link in linked_streams.values() {
        let entry = priority_totals
            .entry(link.priority)
            .or_insert_with(TimeTotals::default);
        entry.direct_ms += link.time.direct_ms;
        entry.direct_plus_delegated_ms += link.time.direct_plus_delegated_ms;
    }
    priority_totals
}

fn priority_drift(
    priority: &Priority,
    importance_total: i32,
    totals: TimeTotals,
    priority_totals: &HashMap<&str, TimeTotals>,
) -> PriorityDrift {
    let time = priority_totals
        .get(priority.slug.as_str())
        .copied()
        .unwrap_or_default();
    PriorityDrift {
        priority_slug: priority.slug.clone(),
        priority_value: priority.value,
        importance_share: share_i32(priority.value, importance_total),
        direct_ms: time.direct_ms,
        direct_plus_delegated_ms: time.direct_plus_delegated_ms,
        direct_share: share_i64(time.direct_ms, totals.direct_ms),
        direct_plus_delegated_share: share_i64(
            time.direct_plus_delegated_ms,
            totals.direct_plus_delegated_ms,
        ),
    }
}

fn unattributed_drift(
    linked_streams: &HashMap<&str, ActiveStreamLink<'_>>,
    time_by_stream: &HashMap<&str, TimeTotals>,
    totals: TimeTotals,
) -> UnattributedDrift {
    let linked: HashSet<&str> = linked_streams.keys().copied().collect();
    let unattributed = time_by_stream
        .iter()
        .filter(|(stream, _)| !linked.contains(**stream))
        .map(|(_, time)| *time)
        .fold(TimeTotals::default(), |total, time| TimeTotals {
            direct_ms: total.direct_ms + time.direct_ms,
            direct_plus_delegated_ms: total.direct_plus_delegated_ms
                + time.direct_plus_delegated_ms,
        });
    UnattributedDrift {
        direct_ms: unattributed.direct_ms,
        direct_plus_delegated_ms: unattributed.direct_plus_delegated_ms,
        direct_share: share_i64(unattributed.direct_ms, totals.direct_ms),
        direct_plus_delegated_share: share_i64(
            unattributed.direct_plus_delegated_ms,
            totals.direct_plus_delegated_ms,
        ),
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "drift shares are presentation ratios"
)]
fn share_i64(part: i64, total: i64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

fn share_i32(part: i32, total: i32) -> f64 {
    if total == 0 {
        0.0
    } else {
        f64::from(part) / f64::from(total)
    }
}
