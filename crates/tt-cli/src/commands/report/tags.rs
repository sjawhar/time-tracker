//! Tag-level time aggregation shared by the human-readable and JSON views.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::ReportStreamTime;

/// Time attributed to one tag within the report period.
#[derive(Debug, Clone)]
pub struct TagTime {
    pub tag: String,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub streams: Vec<String>,
}

#[derive(Debug, Default)]
struct TagAggregate {
    time_direct_ms: i64,
    time_delegated_ms: i64,
    streams: BTreeSet<String>,
}

/// Builds tag-level time aggregation from stream data.
///
/// Takes a slice of *references* so a caller can roll up a filtered view of the
/// period's streams without cloning them — the human-readable report lifts the
/// reserved junk stream out before aggregating, while the JSON contract does not.
///
/// **Multi-tag attribution**: Streams with multiple tags have their FULL time
/// attributed to EACH tag. This means `sum(by_tag.time_direct_ms)` may exceed
/// `totals.time_direct_ms` when streams have overlapping tags. This is intentional —
/// tags represent orthogonal dimensions (e.g., project + activity), so each dimension
/// should reflect the complete time spent.
pub fn build_tag_times(
    streams: &[&ReportStreamTime],
    tags_by_stream: &HashMap<String, Vec<String>>,
) -> Vec<TagTime> {
    let mut by_tag: BTreeMap<String, TagAggregate> = BTreeMap::new();

    for stream in streams {
        if let Some(tags) = tags_by_stream.get(&stream.id) {
            for tag in tags {
                let entry = by_tag.entry(tag.clone()).or_default();
                entry.time_direct_ms += stream.time_direct_ms;
                entry.time_delegated_ms += stream.time_delegated_ms;
                entry.streams.insert(stream.id.clone());
            }
        }
    }

    by_tag
        .into_iter()
        .map(|(tag, aggregate)| TagTime {
            tag,
            time_direct_ms: aggregate.time_direct_ms,
            time_delegated_ms: aggregate.time_delegated_ms,
            streams: aggregate.streams.into_iter().collect(),
        })
        .collect()
}

/// Sums the time of streams carrying no tag at all.
pub fn untagged_totals(
    streams: &[&ReportStreamTime],
    tags_by_stream: &HashMap<String, Vec<String>>,
) -> (i64, i64) {
    let mut direct_ms = 0;
    let mut delegated_ms = 0;
    for stream in streams.iter().filter(|s| !is_tagged(tags_by_stream, &s.id)) {
        direct_ms += stream.time_direct_ms;
        delegated_ms += stream.time_delegated_ms;
    }
    (direct_ms, delegated_ms)
}

/// Whether a stream carries at least one tag.
pub fn is_tagged(tags_by_stream: &HashMap<String, Vec<String>>, stream_id: &str) -> bool {
    tags_by_stream
        .get(stream_id)
        .is_some_and(|tags| !tags.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(id: &str, direct_ms: i64, delegated_ms: i64) -> ReportStreamTime {
        ReportStreamTime {
            id: id.to_string(),
            name: Some(id.to_string()),
            time_direct_ms: direct_ms,
            time_delegated_ms: delegated_ms,
        }
    }

    #[test]
    fn multi_tagged_stream_contributes_to_every_tag() {
        let mut tags_by_stream = HashMap::new();
        tags_by_stream.insert(
            "s1".to_string(),
            vec!["project:a".to_string(), "activity:dev".to_string()],
        );

        let tags = build_tag_times(&[&stream("s1", 600_000, 1_200_000)], &tags_by_stream);

        assert_eq!(tags.len(), 2);
        for tag in &tags {
            assert_eq!(tag.time_direct_ms, 600_000);
            assert_eq!(tag.time_delegated_ms, 1_200_000);
            assert_eq!(tag.streams, vec!["s1".to_string()]);
        }
    }

    #[test]
    fn untagged_totals_ignore_tagged_streams() {
        let mut tags_by_stream = HashMap::new();
        tags_by_stream.insert("tagged".to_string(), vec!["project:a".to_string()]);
        // An empty tag list must count as untagged, not as tagged.
        tags_by_stream.insert("empty".to_string(), Vec::new());

        let streams = [
            stream("tagged", 600_000, 0),
            stream("empty", 300_000, 60_000),
            stream("missing", 120_000, 30_000),
        ];
        let streams: Vec<&ReportStreamTime> = streams.iter().collect();

        assert_eq!(
            untagged_totals(&streams, &tags_by_stream),
            (420_000, 90_000)
        );
        assert!(is_tagged(&tags_by_stream, "tagged"));
        assert!(!is_tagged(&tags_by_stream, "empty"));
        assert!(!is_tagged(&tags_by_stream, "missing"));
    }
}
