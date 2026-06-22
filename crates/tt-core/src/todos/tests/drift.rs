use super::super::{DriftError, PriorityStatus, compute_drift};
use super::support::{assert_close, priority, stream_link, stream_time};

#[test]
fn drift_reports_both_lenses_and_unattributed() -> Result<(), DriftError> {
    // Given: active priorities, linked stream time, delegated work, and off-list stream time.
    let priorities = vec![
        priority("ipi", 9, PriorityStatus::Active),
        priority("ops", 3, PriorityStatus::Active),
    ];
    let links = vec![stream_link("Fable 5 DPI", "ipi"), stream_link("Ops", "ops")];
    let stream_times = vec![
        stream_time("Fable 5 DPI", 60, 40),
        stream_time("Ops", 30, 0),
        stream_time("Slack", 10, 10),
    ];

    // When: drift shares are computed from provided per-stream time.
    let report = compute_drift(&priorities, &links, &stream_times)?;

    // Then: importance, direct-only, direct+delegated, and unattributed shares are explicit.
    assert_close(report.priorities[0].importance_share, 0.75);
    assert_close(report.priorities[0].direct_share, 0.60);
    assert_close(
        report.priorities[0].direct_plus_delegated_share,
        100.0 / 150.0,
    );
    assert_close(report.priorities[1].importance_share, 0.25);
    assert_close(report.priorities[1].direct_share, 0.30);
    assert_close(report.priorities[1].direct_plus_delegated_share, 0.20);
    assert_eq!(report.unattributed.direct_ms, 10);
    assert_eq!(report.unattributed.direct_plus_delegated_ms, 20);
    assert_close(report.unattributed.direct_share, 0.10);
    assert_close(
        report.unattributed.direct_plus_delegated_share,
        20.0 / 150.0,
    );
    Ok(())
}

#[test]
fn drift_errors_on_unresolved_stream_reference() {
    // Given: a stream-priority link for a stream absent from the report-like input.
    let priorities = vec![priority("ipi", 9, PriorityStatus::Active)];
    let links = vec![stream_link("Missing", "ipi")];
    let stream_times = vec![stream_time("Fable 5 DPI", 60, 0)];

    // When: drift attempts to resolve stream links against provided stream time records.
    let Err(error) = compute_drift(&priorities, &links, &stream_times) else {
        panic!("expected unresolved stream error");
    };

    // Then: the unresolved stream is an explicit error, not a silent omission.
    assert_eq!(
        error,
        DriftError::UnresolvedStream {
            stream: "Missing".to_string()
        }
    );
}

#[test]
fn drift_treats_inactive_priority_links_as_unattributed() -> Result<(), DriftError> {
    // Given: a stream link targets a priority slug that exists but is done.
    let priorities = vec![
        priority("active", 9, PriorityStatus::Active),
        priority("done", 3, PriorityStatus::Done),
    ];
    let links = vec![stream_link("Archived", "done")];
    let stream_times = vec![stream_time("Archived", 25, 5)];

    // When: drift computes shares for active priorities only.
    let report = compute_drift(&priorities, &links, &stream_times)?;

    // Then: inactive-linked stream time is not an error and lands in unattributed.
    assert_eq!(report.priorities.len(), 1);
    assert_eq!(report.priorities[0].priority_slug, "active");
    assert_eq!(report.priorities[0].direct_ms, 0);
    assert_eq!(report.priorities[0].direct_plus_delegated_ms, 0);
    assert_eq!(report.unattributed.direct_ms, 25);
    assert_eq!(report.unattributed.direct_plus_delegated_ms, 30);
    assert_close(report.unattributed.direct_share, 1.0);
    assert_close(report.unattributed.direct_plus_delegated_share, 1.0);
    Ok(())
}

#[test]
fn drift_errors_on_absent_priority_slug() {
    // Given: a stream link targets a priority slug absent from priorities.md.
    let priorities = vec![priority("active", 9, PriorityStatus::Active)];
    let links = vec![stream_link("Unknown", "missing")];
    let stream_times = vec![stream_time("Unknown", 10, 0)];

    // When: drift validates the stream link.
    let Err(error) = compute_drift(&priorities, &links, &stream_times) else {
        panic!("expected unresolved priority error");
    };

    // Then: genuinely absent priority slugs still error.
    assert_eq!(
        error,
        DriftError::UnresolvedPriority {
            priority: "missing".to_string()
        }
    );
}

#[test]
fn drift_errors_on_negative_stream_time() {
    // Given: a report-like stream time record has negative direct time.
    let priorities = vec![priority("active", 9, PriorityStatus::Active)];
    let links = Vec::new();
    let stream_times = vec![stream_time("Bad", -1, 0)];

    // When: drift aggregates stream times.
    let Err(error) = compute_drift(&priorities, &links, &stream_times) else {
        panic!("expected negative stream time error");
    };

    // Then: negative input is rejected explicitly.
    assert_eq!(
        error,
        DriftError::NegativeStreamTime {
            stream: "Bad".to_string()
        }
    );
}

#[test]
fn drift_errors_on_duplicate_stream_link() {
    // Given: a stream has two priority links.
    let priorities = vec![
        priority("active", 9, PriorityStatus::Active),
        priority("other", 4, PriorityStatus::Active),
    ];
    let links = vec![stream_link("Dup", "active"), stream_link("Dup", "other")];
    let stream_times = vec![stream_time("Dup", 10, 0)];

    // When: drift validates links.
    let Err(error) = compute_drift(&priorities, &links, &stream_times) else {
        panic!("expected duplicate stream link error");
    };

    // Then: duplicate links are rejected explicitly.
    assert_eq!(
        error,
        DriftError::DuplicateStreamLink {
            stream: "Dup".to_string()
        }
    );
}

#[test]
fn drift_handles_no_active_priorities_and_zero_total_time() -> Result<(), DriftError> {
    // Given: priorities are inactive and report-like stream time totals are zero.
    let priorities = vec![priority("done", 9, PriorityStatus::Done)];
    let links = Vec::new();
    let stream_times = vec![stream_time("Idle", 0, 0)];

    // When: drift computes shares.
    let report = compute_drift(&priorities, &links, &stream_times)?;

    // Then: no active priority rows are emitted and all shares are zero without division errors.
    assert!(report.priorities.is_empty());
    assert_eq!(report.total_direct_ms, 0);
    assert_eq!(report.total_direct_plus_delegated_ms, 0);
    assert_eq!(report.unattributed.direct_ms, 0);
    assert_eq!(report.unattributed.direct_plus_delegated_ms, 0);
    assert_close(report.unattributed.direct_share, 0.0);
    assert_close(report.unattributed.direct_plus_delegated_share, 0.0);
    Ok(())
}
