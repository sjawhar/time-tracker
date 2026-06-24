use chrono::NaiveDate;

use super::super::{
    AlignmentFinding, NextSection, PriorityStatus, classify_next_sections, find_alignment,
    priority_rank,
};
use super::support::{priority, section_ids, stream_link, todo};

#[test]
fn priority_rank_uses_max_active_value() {
    // Given: todos linked directly and through streams to active and inactive priorities.
    let priorities = vec![
        priority("ipi", 9, PriorityStatus::Active),
        priority("ops", 4, PriorityStatus::Active),
        priority("old", 99, PriorityStatus::Dropped),
    ];
    let links = vec![
        stream_link("Fable 5 DPI", "ops"),
        stream_link("Done stream", "old"),
    ];
    let multi = todo("td_multi", &["ipi", "ops"], None, None, None, false, false);
    let none = todo("td_none", &[], None, None, None, false, false);
    let inactive = todo(
        "td_inactive",
        &["old"],
        Some("Done stream"),
        None,
        None,
        false,
        false,
    );
    let tie = todo("td_tie", &["ipi"], None, None, None, false, false);

    // When: ranks are computed from all active links.
    let multi_rank = priority_rank(&multi, &priorities, &links);
    let none_rank = priority_rank(&none, &priorities, &links);
    let inactive_rank = priority_rank(&inactive, &priorities, &links);
    let tie_rank = priority_rank(&tie, &priorities, &links);

    // Then: max active value wins and no active links rank below active-linked todos.
    assert_eq!(multi_rank, Some(9));
    assert_eq!(none_rank, None);
    assert_eq!(inactive_rank, None);
    assert_eq!(tie_rank, Some(9));
}

#[test]
fn pin_algorithm_checks_non_pinned_subsequence() {
    // Given: canonical order has a pinned low-rank item between high-rank work and one low/high inversion.
    let priorities = vec![
        priority("high", 9, PriorityStatus::Active),
        priority("low", 2, PriorityStatus::Active),
    ];
    let todos = vec![
        todo("td_high_first", &["high"], None, None, None, false, false),
        todo("td_pinned_low", &["low"], None, None, None, true, false),
        todo("td_low", &["low"], None, None, None, false, false),
        todo("td_high_late", &["high"], None, None, None, false, false),
    ];

    // When: alignment is checked after lifting pins out of the ranked subsequence.
    let findings = find_alignment(&todos, &priorities, &[]);

    // Then: the pinned item is deliberate, and only the non-pinned inversion is misordered.
    assert!(matches!(
        findings[0],
        AlignmentFinding::Pinned { index: 1, .. }
    ));
    assert!(findings.iter().any(|finding| matches!(
        finding,
        AlignmentFinding::Misordered { todo_id, index: 3, .. } if todo_id == "td_high_late"
    )));
    assert!(!findings.iter().any(|finding| matches!(
        finding,
        AlignmentFinding::Misordered { todo_id, .. } if todo_id == "td_pinned_low"
    )));
}

#[test]
fn pin_algorithm_lifts_pinned_items_out_of_comparison_chain() {
    // Given: a high-rank pinned todo sits between two non-pinned todos in ascending rank order.
    let priorities = vec![
        priority("high", 9, PriorityStatus::Active),
        priority("medium", 5, PriorityStatus::Active),
        priority("low", 2, PriorityStatus::Active),
    ];
    let todos = vec![
        todo("td_low", &["low"], None, None, None, false, false),
        todo("td_pinned_high", &["high"], None, None, None, true, false),
        todo("td_medium", &["medium"], None, None, None, false, false),
    ];

    // When: alignment is checked with the pinned item lifted out.
    let findings = find_alignment(&todos, &priorities, &[]);

    // Then: the medium item is compared against low and is flagged despite the pinned high item.
    assert!(findings.iter().any(|finding| matches!(
        finding,
        AlignmentFinding::Misordered { todo_id, index: 2, previous_rank: Some(2), rank: Some(5) }
            if todo_id == "td_medium"
    )));
}

#[test]
fn alignment_allows_adjacent_equal_ranks() {
    // Given: adjacent non-pinned todos have the same active priority rank.
    let priorities = vec![priority("same", 5, PriorityStatus::Active)];
    let todos = vec![
        todo("td_first", &["same"], None, None, None, false, false),
        todo("td_second", &["same"], None, None, None, false, false),
    ];

    // When: alignment is checked.
    let findings = find_alignment(&todos, &priorities, &[]);

    // Then: equal-rank neighbors are not misordered.
    assert!(
        !findings
            .iter()
            .any(|finding| matches!(finding, AlignmentFinding::Misordered { .. }))
    );
}

#[test]
fn alignment_flags_only_linked_todos_without_active_priority() {
    // Given: one todo has only inactive links while another has no links at all.
    let priorities = vec![priority("old", 7, PriorityStatus::Done)];
    let links = vec![stream_link("Archived", "old")];
    let todos = vec![
        todo(
            "td_orphan",
            &["old"],
            Some("Archived"),
            None,
            None,
            false,
            false,
        ),
        todo("td_unlinked", &[], None, None, None, false, false),
    ];

    // When: alignment is checked.
    let findings = find_alignment(&todos, &priorities, &links);

    // Then: only the todo with inactive/missing references is diagnosed.
    assert!(findings.iter().any(|finding| matches!(
        finding,
        AlignmentFinding::OrphanedLinks { todo_id, index: 0 } if todo_id == "td_orphan"
    )));
    assert!(!findings.iter().any(|finding| matches!(
        finding,
        AlignmentFinding::OrphanedLinks { todo_id, .. } if todo_id == "td_unlinked"
    )));
}

#[test]
fn alignment_reports_duplicate_stream_links() {
    // Given: streams.md links the same stream name to more than one priority.
    let priorities = vec![
        priority("high", 9, PriorityStatus::Active),
        priority("low", 2, PriorityStatus::Active),
    ];
    let links = vec![
        stream_link("Shared stream", "high"),
        stream_link("Shared stream", "low"),
    ];
    let todos = vec![todo(
        "td_shared",
        &[],
        Some("Shared stream"),
        None,
        None,
        false,
        false,
    )];

    // When: alignment is checked.
    let findings = find_alignment(&todos, &priorities, &links);

    // Then: the duplicate stream link is reported with every linked priority.
    assert!(findings.iter().any(|finding| matches!(
        finding,
        AlignmentFinding::DuplicateStreamLink { stream, priorities }
            if stream == "Shared stream" && priorities == &["high".to_string(), "low".to_string()]
    )));
}

#[test]
fn due_overrides_when_into_due_section() -> Result<(), chrono::ParseError> {
    // Given: a deferred open todo is also due today.
    let today = NaiveDate::parse_from_str("2026-06-23", "%Y-%m-%d")?;
    let future = NaiveDate::parse_from_str("2026-06-30", "%Y-%m-%d")?;
    let todos = vec![
        todo("td_due", &[], None, Some(future), Some(today), false, false),
        todo("td_later", &[], None, Some(future), None, false, false),
        todo("td_main", &[], None, None, None, false, false),
        todo("td_done", &[], None, None, None, false, true),
    ];

    // When: todos are classified for the next view.
    let sections = classify_next_sections(&todos, today);

    // Then: due beats when, while other sections preserve canonical order.
    assert_eq!(section_ids(sections.get(NextSection::Due)), ["td_due"]);
    assert_eq!(section_ids(sections.get(NextSection::Later)), ["td_later"]);
    assert_eq!(section_ids(sections.get(NextSection::Main)), ["td_main"]);
    assert_eq!(section_ids(sections.get(NextSection::Done)), ["td_done"]);
    Ok(())
}

#[test]
fn next_sections_include_overdue_and_preserve_section_order() -> Result<(), chrono::ParseError> {
    // Given: multiple due, main, later, and done todos are interleaved in canonical order.
    let today = NaiveDate::parse_from_str("2026-06-23", "%Y-%m-%d")?;
    let yesterday = NaiveDate::parse_from_str("2026-06-22", "%Y-%m-%d")?;
    let tomorrow = NaiveDate::parse_from_str("2026-06-24", "%Y-%m-%d")?;
    let todos = vec![
        todo(
            "td_due_first",
            &[],
            None,
            None,
            Some(yesterday),
            false,
            false,
        ),
        todo("td_main_first", &[], None, None, None, false, false),
        todo("td_due_second", &[], None, None, Some(today), false, false),
        todo(
            "td_later_first",
            &[],
            None,
            Some(tomorrow),
            None,
            false,
            false,
        ),
        todo("td_main_second", &[], None, None, None, false, false),
        todo("td_done_first", &[], None, None, None, false, true),
    ];

    // When: todos are classified for the next view.
    let sections = classify_next_sections(&todos, today);

    // Then: overdue/today-due items are Due and every section preserves input order.
    assert_eq!(
        section_ids(sections.get(NextSection::Due)),
        ["td_due_first", "td_due_second"]
    );
    assert_eq!(
        section_ids(sections.get(NextSection::Main)),
        ["td_main_first", "td_main_second"]
    );
    assert_eq!(
        section_ids(sections.get(NextSection::Later)),
        ["td_later_first"]
    );
    assert_eq!(
        section_ids(sections.get(NextSection::Done)),
        ["td_done_first"]
    );
    Ok(())
}
