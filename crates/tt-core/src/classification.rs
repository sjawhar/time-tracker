//! Structural predicates deciding what the classifier is asked and what it is
//! allowed to answer.
//!
//! # Junk before asking
//!
//! A session that ran no tool and holds at most one exchange did nothing and
//! discussed nothing, so no LLM call can find work in it. The depth bound is
//! load-bearing: among sessions with no tool calls and three or more messages,
//! about half are real work — a work-order review, a vendor pricing evaluation,
//! a discussion of eval technique — and half are `"Hello"`. Those go to the LLM.
//!
//! # Shape, never substring
//!
//! Three name shapes are rejected because they describe a posture, a date, or a
//! leftover instead of the work: there is no transitional time, and a classifier
//! that cannot identify the work must leave the session unassigned rather than
//! invent a container for it.
//!
//! Detecting them by substring destroys real work. A `%nav%` rule written during
//! remediation matched `agent-c: calendar navigation debugging`, and a bare
//! `%misc%` rule matches `misc: webcam troubleshooting` — both genuine
//! initiatives. The discriminator is not which generic words a name *contains*
//! but whether anything is left once they are removed: a name fails only
//! when **every** token in it is generic, so a single subject token
//! (`calendar`, `webcam`, `ghost-wispr`) rescues the name. A namespace prefix
//! (`misc:`, `other:`) is stripped first, because the prefix groups streams and
//! the body is what names the work.
//!
//! Dates are the exception: the suffix `(Jun14-20)` marks a week bucket whatever
//! the body says, so `workorder-5: IPI envs + wo-005 (Jun14-20)` is rejected even
//! though its subject is real. One day buckets exactly as a week does — a stream
//! is a unit of work, not a date — so `infra: devbox-mx recovery (Jun6)` is
//! rejected on the same grounds, and the next incident cannot mint its own dated
//! twin. That costs nothing here — this predicate only gates stream *creation*,
//! so a rejection leaves the session unassigned, which reads as classification
//! lag. Feeding the same pattern to a bulk dissolution is what would destroy the
//! ~25k events those dated streams already hold, and that is a different
//! operation entirely.
//!
//! Both word lists are grown from measured stream names, the same discipline
//! `injection::INJECTION_MARKERS` follows.

/// What a name describes instead of the work it was supposed to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MisnamedReason {
    /// Names a posture or a surface — `other: shell / nav / transitional`.
    ActivityType,
    /// Buckets work into a date — `misc (Jun14-20)`, `infra: recovery (Jun6)`.
    DateRange,
    /// Names the leftovers — `misc: stragglers`.
    CatchAll,
}

/// Words that name a leftover rather than a subject.
const CATCH_ALL_WORDS: &[&str] = &[
    "misc",
    "miscellaneous",
    "other",
    "others",
    "straggler",
    "stragglers",
    "leftover",
    "leftovers",
    "remainder",
    "remainders",
    "assorted",
    "various",
    "sundry",
    "unsorted",
    "uncategorized",
    "uncategorised",
    "unclassified",
    "general",
    "trivial",
    "stuff",
    "random",
    "session",
    "sessions",
    "etc",
    "tbd",
    "unknown",
];

/// Words naming a posture or the surface it happened on, never the work itself.
const POSTURE_WORDS: &[&str] = &[
    "shell",
    "nav",
    "navigation",
    "navigating",
    "terminal",
    "terminals",
    "console",
    "tmux",
    "context",
    "switch",
    "switching",
    "transition",
    "transitions",
    "transitional",
    "transient",
    "browse",
    "browsing",
    "browser",
    "idle",
    "afk",
    "overhead",
    "housekeeping",
    "devbox",
    "laptop",
    "desktop",
    "machine",
    "host",
    "remote",
    "window",
    "windows",
    "tab",
    "tabs",
];

/// Month names and the abbreviations that appear in real stream names.
const MONTH_NAMES: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sept",
    "sep",
    "oct",
    "nov",
    "dec",
];

/// Returns `true` when a session provably carries no work to attribute.
///
/// Both clauses are required: a session that called a tool did something, and a
/// tool-free session with real depth may still be a discussion worth a stream.
///
/// **`tt_db::Database::structurally_junk_sessions` mirrors this predicate in SQL** so a
/// pass can route junk in bulk before it spends a bounded budget selecting candidates.
/// That query is a pre-filter, not a second rule: its caller re-checks every row against
/// this function, so the two cannot drift into disagreeing about what junk is. Changing
/// the rule here means changing that `WHERE` clause in the same edit.
#[must_use]
pub const fn is_structurally_junk(tool_call_count: i32, message_count: i32) -> bool {
    tool_call_count == 0 && message_count <= 2
}

/// The form of a stream name that decides whether two names are the same name.
///
/// Leading and trailing whitespace is dropped and every internal run of
/// whitespace collapses to one space. Nothing else changes.
///
/// A stream name carries no uniqueness constraint, so nothing in the database
/// stops a second row from holding a name a first row already holds, and the
/// classifier's reuse check is a string comparison. One leading space defeats it:
/// the live table holds `" agent-c: eval-3 prometheus test-stage (round 2)"` beside
/// its unspaced twin, minted three minutes apart, because the model emitted the
/// space and the comparison saw two different names. Comparing normalized forms is
/// what closes that, and normalizing on write is what stops the whitespace-bearing
/// form from being stored in the first place.
///
/// **Case is deliberately preserved.** Case-folding would make `DPI: ingest` and
/// `dpi: ingest` the same name, and the live table holds 13 streams under the first
/// prefix and 7 under the second. Whether those are one initiative is a judgement,
/// and acting on it is `tt streams merge` — an operator command with an audit trail —
/// not a silent side effect of the classifier writing a name. Normalization may only
/// erase differences that no human could have meant.
#[must_use]
pub fn normalize_stream_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    for word in name.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

/// Returns what a name describes instead of work, or `None` when it names the
/// work.
///
/// See the module docs for why this matches on shape rather than substring.
#[must_use]
pub fn is_misnamed_stream(name: &str) -> Option<MisnamedReason> {
    let lowered = name.to_ascii_lowercase();
    if contains_date(&lowered) {
        return Some(MisnamedReason::DateRange);
    }
    let mut saw_posture = false;
    for token in strip_namespace_prefix(&lowered).split(|c: char| !c.is_ascii_alphanumeric()) {
        // A bare number names no work, so it neither rescues nor condemns a name.
        if token.is_empty() || token.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if POSTURE_WORDS.contains(&token) {
            saw_posture = true;
        } else if !CATCH_ALL_WORDS.contains(&token) {
            return None;
        }
    }
    Some(if saw_posture {
        MisnamedReason::ActivityType
    } else {
        MisnamedReason::CatchAll
    })
}

/// Drops a leading single-token namespace (`misc:`, `agent-c:`) so the check
/// runs against the part of the name that is supposed to identify the work.
fn strip_namespace_prefix(name: &str) -> &str {
    match name.split_once(':') {
        Some((prefix, body)) if is_namespace(prefix) => body,
        _ => name,
    }
}

fn is_namespace(prefix: &str) -> bool {
    let prefix = prefix.trim();
    !prefix.is_empty() && !prefix.contains(char::is_whitespace)
}

/// Detects `Jun6` / `Jun14-20` / `Jul 5 - 11`, and any name carrying two full ISO
/// dates.
///
/// A month and a day are the whole test: every range spelling opens with one, so
/// requiring the range as well is what let `(Jun6)` through.
fn contains_date(lowered: &str) -> bool {
    let bytes = lowered.as_bytes();
    let starts = || (0..bytes.len()).filter(|&at| is_word_start(bytes, at));
    starts().any(|at| month_day_at(bytes, at))
        || starts().filter(|&at| is_iso_date_at(bytes, at)).count() >= 2
}

fn is_word_start(bytes: &[u8], at: usize) -> bool {
    at == 0 || !bytes[at - 1].is_ascii_alphanumeric()
}

fn month_day_at(bytes: &[u8], at: usize) -> bool {
    month_token_end(bytes, at)
        .map(|after_month| skip_separators(bytes, after_month))
        .and_then(|at| day_number_end(bytes, at))
        .is_some()
}

/// Returns the end of a month token at `at`, requiring that no letter follows —
/// otherwise `mar` would match inside `marathon14-20`.
fn month_token_end(bytes: &[u8], at: usize) -> Option<usize> {
    MONTH_NAMES.iter().find_map(|month| {
        let end = at + month.len();
        (bytes.len() >= end
            && &bytes[at..end] == month.as_bytes()
            && !bytes.get(end).is_some_and(u8::is_ascii_alphabetic))
        .then_some(end)
    })
}

fn day_number_end(bytes: &[u8], at: usize) -> Option<usize> {
    let end = (at..bytes.len().min(at + 2))
        .take_while(|&index| bytes[index].is_ascii_digit())
        .count()
        + at;
    (end > at).then_some(end)
}

fn skip_separators(bytes: &[u8], at: usize) -> usize {
    let mut at = at;
    while bytes
        .get(at)
        .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'.')
    {
        at += 1;
    }
    at
}

fn is_iso_date_at(bytes: &[u8], at: usize) -> bool {
    let digits = |from: usize, len: usize| {
        bytes.len() >= from + len && bytes[from..from + len].iter().all(u8::is_ascii_digit)
    };
    digits(at, 4)
        && bytes.get(at + 4) == Some(&b'-')
        && digits(at + 5, 2)
        && bytes.get(at + 7) == Some(&b'-')
        && digits(at + 8, 2)
        && !bytes.get(at + 10).is_some_and(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_with_no_tools_and_one_exchange_is_junk() {
        // Given: a session that ran no tools and holds a single exchange.
        // When/Then: it is structurally junk — nothing was done, nothing discussed.
        assert!(is_structurally_junk(0, 2));
        assert!(is_structurally_junk(0, 1));
        assert!(is_structurally_junk(0, 0));
    }

    #[test]
    fn a_tool_free_session_with_depth_is_not_junk() {
        // Given: a session with no tool calls but several exchanges — the shape of a
        // contract review or a vendor pricing discussion.
        // When/Then: structure cannot judge it, so it goes to the classifier.
        assert!(!is_structurally_junk(0, 3));
        assert!(!is_structurally_junk(0, 6));
    }

    #[test]
    fn any_tool_call_disqualifies_junk_regardless_of_depth() {
        // Given: a session that used a tool.
        // When/Then: work was done, so it is never structurally junk.
        assert!(!is_structurally_junk(1, 1));
        assert!(!is_structurally_junk(1, 2));
    }

    #[test]
    fn activity_type_stream_names_are_rejected() {
        // Given: names describing a posture rather than work.
        for name in [
            "other: shell / nav / transitional",
            "Devbox terminal context-switching + transient browser nav",
        ] {
            // When/Then
            assert_eq!(
                is_misnamed_stream(name),
                Some(MisnamedReason::ActivityType),
                "{name} must be rejected as an activity type"
            );
        }
    }

    #[test]
    fn date_range_stream_names_are_rejected() {
        // Given: names bucketing work into a week.
        for name in ["ops: devbox terminal nav (Jul5-11)", "misc (Jun14-20)"] {
            // When/Then
            assert_eq!(
                is_misnamed_stream(name),
                Some(MisnamedReason::DateRange),
                "{name} must be rejected as a date range"
            );
        }
    }

    #[test]
    fn a_real_initiative_carrying_a_week_suffix_is_still_misnamed() {
        // Given: names whose subject is genuine work but whose suffix buckets it into
        // a week. `workorder-5: IPI envs + wo-005 (Jun14-20)` holds 4,347 real events
        // and `dpi: cybertasks verifier + techniques (Jun14-20)` holds 3,223.
        //
        // Rejecting the *name* stops week 52 from being minted and destroys nothing,
        // because this predicate only ever gates stream creation. Feeding the same
        // pattern to a bulk dissolution is what would have released those ~25k events,
        // and that is a different operation with a different blast radius.
        for name in [
            "workorder-5: IPI envs + wo-005 (Jun14-20)",
            "dpi: cybertasks verifier + techniques (Jun14-20)",
        ] {
            // When/Then
            assert_eq!(
                is_misnamed_stream(name),
                Some(MisnamedReason::DateRange),
                "{name} must be rejected as a date range"
            );
        }
    }

    #[test]
    fn catch_all_stream_names_are_rejected() {
        // Given: names for leftovers.
        for name in [
            "misc: stragglers",
            "other: Misc (trivial sessions)",
            "misc",
            "Other",
        ] {
            // When/Then
            assert_eq!(
                is_misnamed_stream(name),
                Some(MisnamedReason::CatchAll),
                "{name} must be rejected as a catch-all"
            );
        }
    }

    #[test]
    fn real_work_sharing_a_generic_word_is_allowed() {
        // Given: real streams that contain a generic word in a legitimate context.
        // A `%nav%` substring rule caught `calendar navigation debugging` during
        // remediation and would have destroyed thousands of correct assignments.
        for name in [
            "agent-c: calendar navigation debugging",
            "misc: webcam troubleshooting",
            "misc: ghost-wispr infra",
            "misc: startup credits",
            "Misc: Raspberry Pi setup",
            "xmodel-eval: Anthropic cross-model eval - runs/scoring/flow",
        ] {
            // When/Then
            assert_eq!(
                is_misnamed_stream(name),
                None,
                "{name} is real work and must be allowed"
            );
        }
    }

    #[test]
    fn a_week_written_in_iso_dates_is_a_rejected_date_range() {
        // Given: the same week bucket in a different notation. Rejecting only the
        // `MonDD-DD` spelling would leave the guard open to this one.
        for name in [
            "misc (2026-06-14 to 2026-06-20)",
            "ops: devbox work 2026-06-14..2026-06-20",
        ] {
            // When/Then
            assert_eq!(
                is_misnamed_stream(name),
                Some(MisnamedReason::DateRange),
                "{name} must be rejected as a date range"
            );
        }
    }

    #[test]
    fn one_iso_date_names_an_artifact_rather_than_a_week() {
        // Given: a stream named for something dated, not for a span of days.
        // When/Then
        assert_eq!(
            is_misnamed_stream("hawk: 2026-06-14 incident postmortem"),
            None
        );
    }

    #[test]
    fn a_month_word_inside_a_longer_word_is_not_a_date_range() {
        // Given: names where a month abbreviation only appears as a substring.
        for name in [
            "marathon14-20 telemetry",
            "decoder5-11 rewrite",
            "sepsis1-2 model",
        ] {
            // When/Then
            assert_eq!(
                is_misnamed_stream(name),
                None,
                "{name} contains no date range"
            );
        }
    }

    #[test]
    fn a_single_date_is_a_rejected_date() {
        // Given: names whose suffix is one day rather than a span of them. A stream is
        // a unit of work, not a day, so a lone date buckets exactly as a week does —
        // `infra: devbox-mx recovery (Jun6)` was minted while the guard still required
        // a range, and the next incident would have minted its own dated twin.
        for name in [
            "infra: devbox-mx recovery (Jun6)",
            "release: Mar31 cutover",
            "ops: Jul 5 rollout",
        ] {
            // When/Then
            assert_eq!(
                is_misnamed_stream(name),
                Some(MisnamedReason::DateRange),
                "{name} must be rejected as a date"
            );
        }
    }

    #[test]
    fn a_name_is_normalized_by_trimming_and_collapsing_whitespace() {
        // Given: the whitespace shapes a model actually emitted. A leading space is
        // what minted ` agent-c: eval-3 prometheus test-stage (round 2)` alongside its
        // unspaced twin, three minutes apart.
        for (raw, expected) in [
            (
                " agent-c: eval-3 prometheus test-stage (round 2)",
                "agent-c: eval-3 prometheus test-stage (round 2)",
            ),
            ("time-tracker: roster cap \n", "time-tracker: roster cap"),
            ("agent-c:  eval-3  saleor", "agent-c: eval-3 saleor"),
            ("legion:\tworker dispatch", "legion: worker dispatch"),
        ] {
            // When/Then
            assert_eq!(normalize_stream_name(raw), expected, "normalizing {raw:?}");
        }
    }

    #[test]
    fn normalizing_a_clean_name_changes_nothing() {
        // Given: the shape almost every real stream already has.
        let name = "agent-c: eval-3 mayan LIMS environment (eval-3 integration)";

        // When/Then
        assert_eq!(normalize_stream_name(name), name);
    }

    #[test]
    fn normalization_never_folds_case() {
        // Given: two prefixes the live table carries as separate streams — 13 under
        // `DPI:` and 7 under `dpi:`. Folding case would silently merge them, and
        // merging streams is an operator's judgement (`tt streams merge`), not a
        // side effect of the classifier writing a name.
        // When/Then
        assert_ne!(normalize_stream_name("DPI: ingest"), "dpi: ingest");
    }

    #[test]
    fn a_normalized_name_is_judged_exactly_as_its_raw_form_was() {
        // Given: normalization must not become a way past `is_misnamed_stream`.
        for name in [" misc: stragglers ", "other:  shell", "misc  (Jun14-20)"] {
            // When/Then
            assert!(
                is_misnamed_stream(&normalize_stream_name(name)).is_some(),
                "{name} must still be refused after normalization"
            );
        }
    }
}
