use chrono::{TimeZone, Utc};
use tt_core::todos::{Priority, PriorityStatus, StreamPriorityLink, compute_drift};
use tt_db::Stream;

use super::*;

#[test]
fn duplicate_named_db_streams_warn_and_keep_combined_time() {
    // Given: the DB has two streams with the same display name and the period report has time
    // for both stream IDs under that shared name.
    let db = Database::open_in_memory().unwrap();
    let created_at = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
    for id in ["stream-a", "stream-b"] {
        db.insert_stream(&Stream {
            id: id.to_string(),
            name: Some("Shared stream".to_string()),
            slug: None,
            description: None,
            color: None,
            created_at,
            updated_at: created_at,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        })
        .unwrap();
    }
    let report_streams = vec![
        report::ReportStreamTime {
            id: "stream-a".to_string(),
            name: Some("Shared stream".to_string()),
            time_direct_ms: 60_000,
            time_delegated_ms: 0,
        },
        report::ReportStreamTime {
            id: "stream-b".to_string(),
            name: Some("Shared stream".to_string()),
            time_direct_ms: 120_000,
            time_delegated_ms: 0,
        },
    ];

    // When: stream times and warnings are built for drift.
    let stream_times = stream_times_with_idle_named_streams(&db, &report_streams).unwrap();
    let warnings = duplicate_stream_key_warnings(&db).unwrap();
    let drift = compute_drift(
        &[Priority {
            slug: "ipi".to_string(),
            value: 9,
            status: PriorityStatus::Active,
            description: None,
        }],
        &[StreamPriorityLink {
            stream: "Shared stream".to_string(),
            priority: "ipi".to_string(),
        }],
        &stream_times,
    )
    .unwrap();

    // Then: drift does not error, combines both streams' time, and warns about the shared name.
    assert_eq!(drift.priorities[0].direct_ms, 180_000);
    assert_eq!(
        warnings,
        vec![
            "DB stream key 'Shared stream' appears more than once; times were combined".to_string()
        ]
    );
    let rendered = render_warnings(&warnings).unwrap();
    assert!(rendered.contains(
        "WARNING: DB stream key 'Shared stream' appears more than once; times were combined"
    ));
}

#[test]
fn duplicate_display_names_with_distinct_slugs_do_not_warn() {
    // Given: separate stream keys share a legacy display name.
    let db = Database::open_in_memory().unwrap();
    let created_at = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
    for (id, slug) in [("stream-a", "slug-a"), ("stream-b", "slug-b")] {
        db.insert_stream(&Stream {
            id: id.to_string(),
            name: Some("Shared stream".to_string()),
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
        })
        .unwrap();
    }

    // When: duplicate warnings are calculated from the canonical stream keys.
    let warnings = duplicate_stream_key_warnings(&db).unwrap();

    // Then: distinct slug keys do not produce a false collision warning.
    assert!(warnings.is_empty());
}

#[test]
fn drift_matches_links_by_slug() {
    // Given: a slugged stream has report time under its display name.
    let db = Database::open_in_memory().unwrap();
    let created_at = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
    db.insert_stream(&Stream {
        id: "stream-a".to_string(),
        name: Some("agent-c: eval-3 moto".to_string()),
        slug: Some("eval3-moto".to_string()),
        description: None,
        color: None,
        created_at,
        updated_at: created_at,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })
    .unwrap();
    let report_streams = vec![report::ReportStreamTime {
        id: "stream-a".to_string(),
        name: Some("agent-c: eval-3 moto".to_string()),
        time_direct_ms: 60_000,
        time_delegated_ms: 0,
    }];
    let stream_times = stream_times_with_idle_named_streams(&db, &report_streams).unwrap();
    let links = vec![StreamPriorityLink {
        stream: "eval3-moto".to_string(),
        priority: "ipi".to_string(),
    }];

    // When: the slug reference is resolved before calculating drift.
    let (resolved_links, warnings) = resolve_stream_links(&db, links).unwrap();
    let drift = compute_drift(
        &[Priority {
            slug: "ipi".to_string(),
            value: 9,
            status: PriorityStatus::Active,
            description: None,
        }],
        &resolved_links,
        &stream_times,
    )
    .unwrap();

    // Then: the slug links to the report time without a warning.
    assert_eq!(drift.priorities[0].direct_ms, 60_000);
    assert!(warnings.is_empty());
}

#[test]
fn drift_name_reference_falls_back_with_deprecation_warning() {
    // Given: a slugged stream has report time under its display name.
    let db = Database::open_in_memory().unwrap();
    let created_at = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
    db.insert_stream(&Stream {
        id: "stream-a".to_string(),
        name: Some("agent-c: eval-3 moto".to_string()),
        slug: Some("eval3-moto".to_string()),
        description: None,
        color: None,
        created_at,
        updated_at: created_at,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })
    .unwrap();
    let report_streams = vec![report::ReportStreamTime {
        id: "stream-a".to_string(),
        name: Some("agent-c: eval-3 moto".to_string()),
        time_direct_ms: 60_000,
        time_delegated_ms: 0,
    }];
    let stream_times = stream_times_with_idle_named_streams(&db, &report_streams).unwrap();
    let links = vec![StreamPriorityLink {
        stream: "agent-c: eval-3 moto".to_string(),
        priority: "ipi".to_string(),
    }];

    // When: the legacy display-name reference is resolved before calculating drift.
    let (resolved_links, warnings) = resolve_stream_links(&db, links).unwrap();
    let drift = compute_drift(
        &[Priority {
            slug: "ipi".to_string(),
            value: 9,
            status: PriorityStatus::Active,
            description: None,
        }],
        &resolved_links,
        &stream_times,
    )
    .unwrap();

    // Then: it is rewritten to the slug and emits an upgrade warning.
    assert_eq!(resolved_links[0].stream, "eval3-moto");
    assert_eq!(
        warnings,
        vec!["streams.md reference 'agent-c: eval-3 moto' matches by name; update to slug 'eval3-moto'"
            .to_string()]
    );
    assert_eq!(
        render_warnings(&warnings).unwrap(),
        "WARNING: streams.md reference 'agent-c: eval-3 moto' matches by name; update to slug 'eval3-moto'\n"
    );
    assert_eq!(drift.priorities[0].direct_ms, 60_000);
}

fn drift_row(slug: &str, direct_ms: i64, delegated_ms: i64) -> tt_core::todos::PriorityDrift {
    tt_core::todos::PriorityDrift {
        priority_slug: slug.to_string(),
        priority_value: 5,
        importance_share: 0.5,
        direct_ms,
        direct_plus_delegated_ms: direct_ms + delegated_ms,
        direct_share: 0.5,
        direct_plus_delegated_share: 0.5,
    }
}

#[test]
fn the_drift_table_never_reports_a_share_of_direct_plus_delegated() {
    // Direct is wall-clock hours; delegated is machine-hours summed across parallel
    // agents. Their sum has no denominator, so a percentage of it equates one human hour
    // with one agent hour -- which is why this table used to show `wo-005` at 6.0% of
    // attention and 0.5% of "all time", two figures that cannot both be a share of effort.
    let drift = tt_core::todos::DriftReport {
        priorities: vec![
            drift_row("hands-on", 2 * 3_600_000, 600_000),
            drift_row("delegated-heavy", 3_600_000, 20 * 3_600_000),
        ],
        unattributed: tt_core::todos::UnattributedDrift {
            direct_ms: 0,
            direct_plus_delegated_ms: 5 * 3_600_000,
            direct_share: 0.0,
            direct_plus_delegated_share: 0.4,
        },
        total_direct_ms: 3 * 3_600_000,
        total_direct_plus_delegated_ms: 26 * 3_600_000 + 600_000,
        dangling_stream_links: Vec::new(),
    };

    let rendered = render_human(&drift, &[]).unwrap();

    // The forbidden columns are gone, and the honest pair is present.
    assert!(!rendered.contains("Direct+Del"), "{rendered}");
    assert!(!rendered.contains("All time"), "{rendered}");
    assert!(rendered.contains("Delegated"), "{rendered}");
    assert!(rendered.contains("Leverage"), "{rendered}");

    // Delegated is reported on its own, and leverage distinguishes hands-on work from
    // heavily delegated work -- the distinction the summed column destroyed.
    let hands_on = rendered
        .lines()
        .find(|line| line.starts_with("hands-on"))
        .expect("the hands-on row");
    assert!(
        hands_on.contains("10m"),
        "delegated shown on its own: {hands_on}"
    );
    assert!(hands_on.contains("0.1x"), "{hands_on}");
    let heavy = rendered
        .lines()
        .find(|line| line.starts_with("delegated-heavy"))
        .expect("the delegated-heavy row");
    assert!(heavy.contains("20.0x"), "{heavy}");
}

#[test]
fn leverage_is_not_invented_for_a_priority_with_no_attention() {
    // Dividing by zero direct time would print an enormous ratio for work the user never
    // touched, which reads as spectacular leverage rather than as unattributed machine time.
    let drift = tt_core::todos::DriftReport {
        priorities: vec![
            drift_row("untouched-with-agents", 0, 40 * 3_600_000),
            drift_row("untouched-entirely", 0, 0),
        ],
        unattributed: tt_core::todos::UnattributedDrift {
            direct_ms: 0,
            direct_plus_delegated_ms: 0,
            direct_share: 0.0,
            direct_plus_delegated_share: 0.0,
        },
        total_direct_ms: 0,
        total_direct_plus_delegated_ms: 40 * 3_600_000,
        dangling_stream_links: Vec::new(),
    };

    let rendered = render_human(&drift, &[]).unwrap();

    let with_agents = rendered
        .lines()
        .find(|line| line.starts_with("untouched-with-agents"))
        .expect("the row");
    assert!(with_agents.contains("n/a"), "{with_agents}");
    let entirely = rendered
        .lines()
        .find(|line| line.starts_with("untouched-entirely"))
        .expect("the row");
    assert!(entirely.contains('-'), "{entirely}");
}
