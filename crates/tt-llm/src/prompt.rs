//! The payload a classification starts from, and the roster it chooses within.
//!
//! # The roster is selected, not dumped
//!
//! Every stream in the database used to be written into every prompt, one labelled line
//! per stream. That is a feedback loop, not a listing: each stream created enlarges the
//! roster, a larger roster makes the stream that should be reused harder to find, and a
//! model that cannot find it creates a neighbour instead. Measured on the live table it
//! had reached **1,018 streams / 314 KB per classification**, growing at ~101 new streams
//! an hour — roughly one per session — with families like
//! `agent-c: eval-3 <app> environment (eval-3 integration)` minting a row per application.
//!
//! Two independent changes cut it, and both are needed:
//!
//! - **Ordering by proximity to the session.** Each stream carries the period it was
//!   active over ([`StreamSummary::first_active`]..[`StreamSummary::last_active`]) and the
//!   roster is sorted by how far [`ClassificationInput::started_at`] falls from it, ties
//!   broken by the tighter period. Replaying 1,165 reuse decisions against the live
//!   corpus, the stream the classifier actually reused ranked **1st by median, 13th at
//!   p95, 26th at p99**, and fell inside the top 200 **99.91%** of the time — against
//!   **99.48%** for recency-before-the-session and **54.76%** for the shipped
//!   `updated_at DESC`, which mostly records when `tt recompute` last ran.
//!
//!   Proximity to the *session*, not to now, and that distinction is the whole thing.
//!   The classifier drains a backlog months deep: the 23
//!   `agent-c: eval-3 <app> environment` streams were created on one August morning and
//!   hold April events. Ordering by absolute recency ranked all 23 between 495 and 860, so
//!   a 200-stream roster showed **0 of them** — each one's obvious reuse target hidden
//!   from every other. Session-relative ordering shows **20 of 21**. A replay cannot catch
//!   that, because "newest event" leaks assignments made after the decision; only the live
//!   check does.
//! - **A compressed line.** `id  name — description  [tags]`, against the old
//!   `id: …; slug: …; name: …; description: …; tags: …; last active: …`. The labels, the
//!   RFC 3339 timestamp (NULL for 758 of 1,018 streams, and now conveyed by the ordering
//!   itself), and the slug (25 streams have one, and the model answers with an id) were
//!   all overhead. Descriptions average 428 bytes and reach 910, so they are truncated —
//!   but never dropped, because they are what the model recognises a stream by.
//!
//! # Why a cap is safe
//!
//! Capping means a session's stream can be off the list, and the model will then propose
//! its name again. `tt_db::find_stream_by_normalized_name` turns that into reuse of the
//! existing row, so **the cap can only ever cost a semantically near-duplicate, never an
//! exact one**. The two fixes are designed to compose that way; neither is sufficient
//! alone.
//!
//! This is presentation, not attribution. Which streams are *shown*, and in what order,
//! is a rendering decision; which stream the work belongs to stays the model's judgement.
//! Nothing here maps a cwd, a title, or an app to a stream — see `tt-core`'s `AGENTS.md`,
//! "Streams are semantic".

use std::cmp::Reverse;
use std::fmt::Write;

use chrono::{DateTime, Utc};

use crate::{ClassificationInput, StreamSummary};

const MAX_USER_PROMPTS: usize = 5;
const USER_PROMPT_BUDGET: usize = 500;

/// How many streams a prompt lists.
///
/// Measured against the live corpus rather than guessed. Replaying 1,165 reuse decisions
/// under a recency ordering, the stream the classifier reused fell inside the top N for:
/// 97.85% at 25, 99.14% at 100, **99.48% at 200**, 99.74% at 250. The p99 rank is 94, so
/// 200 is a little over twice the depth all but one classification in a hundred needs, and
/// the curve past it is flat.
///
/// It costs ~33 KB of prompt on that corpus against the 314 KB the uncapped roster had
/// reached — and unlike that number, this one does not grow as streams accumulate, which
/// is the whole point.
///
/// Raising it buys ~0.3 percentage points for a third more bytes per 100 streams. Lowering
/// it below ~100 starts hiding real reuse targets, and a hidden target is what mints a
/// duplicate.
pub const ROSTER_LIMIT: usize = 200;

/// How much of a stream's description a roster line carries.
///
/// Descriptions average 428 bytes on the live table and reach 910, which made them the
/// dominant cost of the roster. A description's job here is recognition, and the opening
/// clause does that; the rest is detail the model does not need to match on.
pub const ROSTER_DESCRIPTION_BUDGET: usize = 160;

/// Builds the classifier prompt without optional context lookup.
///
/// This is intentionally the original rendering path: without a context provider, both
/// the prompt bytes and available tools stay exactly as they were before lookup support.
pub fn build(input: &ClassificationInput, roster: &[StreamSummary]) -> String {
    build_inner(input, roster, false)
}

/// Builds the classifier prompt for an agent with a context provider.
pub fn build_with_context_provider(
    input: &ClassificationInput,
    roster: &[StreamSummary],
) -> String {
    build_inner(input, roster, true)
}

fn build_inner(
    input: &ClassificationInput,
    roster: &[StreamSummary],
    has_context_provider: bool,
) -> String {
    let mut prompt = String::new();
    let _ = writeln!(prompt, "Session ID: {}", input.session_id);
    append_option(&mut prompt, "Machine", input.machine.as_deref());
    append_option(&mut prompt, "Working directory", input.cwd.as_deref());
    append_option(
        &mut prompt,
        "Starting prompt",
        input.starting_prompt.as_deref(),
    );

    let _ = writeln!(prompt, "Window titles:");
    for title in &input.window_titles {
        let _ = writeln!(prompt, "{title}");
    }

    let _ = writeln!(prompt, "Recent user prompts:");
    for user_prompt in input.user_prompts.iter().rev().take(MAX_USER_PROMPTS).rev() {
        let truncated: String = user_prompt.chars().take(USER_PROMPT_BUDGET).collect();
        let _ = writeln!(prompt, "{truncated}");
    }

    append_roster(&mut prompt, roster, input.started_at);

    let _ = writeln!(
        prompt,
        "Prefer an existing stream. A stream is one initiative that spans many sessions \
         over days or weeks, not one row per task instance: the sessions that stood up \
         six applications for the same integration all belong to that one integration \
         stream, not to six streams named after the applications. If this session is \
         another task within work a listed stream already covers, answer with that \
         stream_id even when the wording differs. Only create a new stream when the work \
         genuinely belongs to no listed initiative, and then give it a name and a \
         description that identify the initiative rather than this session's task. \
         Set throwaway when the session holds no work worth attributing at all. \
         Never name a stream after an activity or posture (shell, navigation, context-switching), \
         a date range, or a leftover bucket (misc, other, stragglers): those describe how the user \
         was sitting, not what they were doing, and such a name is refused. \
         If you cannot identify the work, leave stream_id, new_stream_name and throwaway \
         all unset and answer with low confidence, rather than inventing a container for it. \
         That is not the same as a throwaway: throwaway asserts this session holds no work, \
         while leaving all three unset says only that the work was not identified. \
         Whenever you can identify the work, name a choice even when you are unsure of it, \
         and say how sure you are in confidence. Confidence must be within 0..1."
    );
    if has_context_provider {
        append_context_lookup_grounding(&mut prompt);
    }

    prompt
}

fn append_context_lookup_grounding(prompt: &mut String) {
    let _ = writeln!(
        prompt,
        "Grounding and stream taxonomy: You MAY resolve unfamiliar people, organizations, \
         projects, codenames, and their relationships through an operator-configured knowledge \
         lookup before choosing. A stream names ONE initiative: a deliverable, customer, or \
         program. NEVER name a stream after a session, a dispatch mechanism, an activity \
         (navigation, ops, admin, coordination, or meetings), a date range, or a catch-all. \
         Name a stream \"A + B\" only when A and B are facets of one deliverable. Strongly \
         prefer reusing an existing roster stream over minting a near-duplicate."
    );
}

/// Writes the streams the model may choose from, closest to the session's moment first.
///
/// Ordering and capping happen here rather than in the caller because this is where what
/// the model sees is decided. See the module docs for the measurements behind both.
fn append_roster(prompt: &mut String, roster: &[StreamSummary], at: Option<DateTime<Utc>>) {
    let mut ordered: Vec<&StreamSummary> = roster.iter().collect();
    match at {
        Some(at) => ordered.sort_by_key(|stream| proximity(stream, at)),
        None => ordered
            .sort_by_key(|stream| (stream.last_active.is_none(), Reverse(stream.last_active))),
    }

    let _ = writeln!(
        prompt,
        "Available streams, closest to this session's own activity first (this is not \
         every stream that exists, so a stream being absent is not evidence against it):"
    );
    for stream in ordered.into_iter().take(ROSTER_LIMIT) {
        let name = stream.name.as_deref().unwrap_or("(unnamed)");
        let _ = write!(prompt, "{}  {name}", stream.id);
        if let Some(description) = stream.description.as_deref() {
            let truncated: String = description
                .chars()
                .take(ROSTER_DESCRIPTION_BUDGET)
                .collect();
            let _ = write!(prompt, " — {truncated}");
        }
        if !stream.tags.is_empty() {
            let _ = write!(prompt, "  [{}]", stream.tags.join(", "));
        }
        prompt.push('\n');
    }
}

/// How well a stream's active period answers for a session at `at`, smallest is best.
///
/// Three components, in order:
///
/// 1. **Whether it has ever been active.** A stream with no events at all sorts behind
///    every stream that has some, however distant — 171 of the live table's 1,018 streams
///    are in that state and none of them is a likelier answer than a stream that ran.
/// 2. **Distance from the session to the period**, zero when the period contains it.
///    Symmetric on purpose: a stream that continued *after* this session is as much a
///    reuse candidate as one that preceded it, and a backlog pass classifies sessions from
///    the middle of the corpus where both directions exist.
/// 3. **The length of the period**, as a tie-break among the streams underway. Both are
///    equally present, so the tighter initiative wins — a stream scoped to this week
///    describes this moment better than one spanning four months that merely covers it.
///    Measured: this tie-break lifts p95 rank from 26 to 13 and cap-25 coverage from
///    93.65% to 98.71%.
fn proximity(stream: &StreamSummary, at: DateTime<Utc>) -> (bool, i64, i64) {
    let (Some(first), Some(last)) = (stream.first_active, stream.last_active) else {
        return (true, 0, 0);
    };
    let distance = if at < first {
        (first - at).num_milliseconds()
    } else if at > last {
        (at - last).num_milliseconds()
    } else {
        0
    };
    (false, distance, (last - first).num_milliseconds())
}

fn append_option(prompt: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        let _ = writeln!(prompt, "{label}: {value}");
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{ROSTER_DESCRIPTION_BUDGET, ROSTER_LIMIT, build, build_with_context_provider};
    use crate::{ClassificationInput, StreamSummary};

    fn at(minutes: i64) -> chrono::DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 4, 25, 12, 0, 0).unwrap()
            + chrono::Duration::minutes(minutes)
    }

    fn input(user_prompts: Vec<String>) -> ClassificationInput {
        ClassificationInput {
            has_session: true,
            session_id: "session-1".to_owned(),
            machine: Some("laptop".to_owned()),
            cwd: Some("/work/project".to_owned()),
            starting_prompt: Some("Classify my work".to_owned()),
            user_prompts,
            window_titles: Vec::new(),
            started_at: Some(at(0)),
        }
    }

    /// A stream active over `[from, to]` minutes on the test clock.
    fn active(id: &str, name: &str, from: i64, to: i64) -> StreamSummary {
        StreamSummary {
            slug: None,
            id: id.to_owned(),
            name: Some(name.to_owned()),
            description: Some(format!("work on {name}")),
            tags: Vec::new(),
            first_active: Some(at(from)),
            last_active: Some(at(to)),
        }
    }

    /// A stream active for one moment, `minutes` before the session.
    fn stream(id: &str, name: &str, minutes: i64) -> StreamSummary {
        active(id, name, -minutes, -minutes)
    }

    fn roster() -> Vec<StreamSummary> {
        vec![
            StreamSummary {
                slug: Some("time-tracker".to_owned()),
                id: "stream-1".to_owned(),
                name: Some("Time Tracker".to_owned()),
                description: Some("Build the tracker".to_owned()),
                tags: vec!["rust".to_owned()],
                first_active: Some(at(-60)),
                last_active: Some(at(0)),
            },
            StreamSummary {
                slug: None,
                id: "stream-2".to_owned(),
                name: Some("Documentation".to_owned()),
                description: None,
                tags: Vec::new(),
                first_active: None,
                last_active: None,
            },
        ]
    }

    #[test]
    fn includes_every_roster_identifier_once() {
        // Given
        let input = input(Vec::new());
        let roster = roster();

        // When
        let prompt = build(&input, &roster);

        // Then: the id is what the model answers with, so it is the one identifier the
        // rendering owes every stream.
        for stream in roster {
            assert_eq!(prompt.matches(&stream.id).count(), 1);
            if let Some(name) = stream.name {
                assert_eq!(prompt.matches(&name).count(), 1);
            }
        }
    }

    #[test]
    fn the_roster_is_capped() {
        // Given: more streams than the prompt will carry. The live table reached 1,018 at
        // ~101 new streams an hour, and rendering all of them cost 314 KB of prompt — the
        // loop this cap exists to break, since a roster the model cannot read is a roster
        // it cannot reuse from.
        let roster: Vec<_> = (0..ROSTER_LIMIT + 50)
            .map(|index| stream(&format!("stream-{index}"), &format!("work {index}"), 0))
            .collect();

        // When
        let prompt = build(&input(Vec::new()), &roster);

        // Then
        let listed = roster
            .iter()
            .filter(|stream| prompt.contains(&format!("{}  ", stream.id)))
            .count();
        assert_eq!(listed, ROSTER_LIMIT);
    }

    #[test]
    fn the_roster_lists_the_streams_active_nearest_the_session_first() {
        // Given: streams handed over in the least helpful order, and a session in the
        // middle of the corpus rather than at the end of it. Proximity to *this session*
        // is the ordering, not proximity to now, because the classifier drains a backlog:
        // on the live DB the 23 `agent-c: eval-3 <app> environment` streams were created
        // today but hold April events, so ordering by absolute recency ranked all of them
        // 495–860 and showed **0 of 23**. Ordering by distance to the session's own moment
        // shows **20 of 21**.
        let roster = vec![
            active("ancient", "ancient work", -10_000, -9_000),
            active("stale", "stale work", -900, -800),
            active("neighbour", "neighbour work", -30, -10),
            active("distant_future", "distant future work", 9_500, 10_500),
            active("near_future", "near future work", 100, 200),
        ];

        // When
        let prompt = build(&input(Vec::new()), &roster);

        // Then: nearest in time first, and distance decides rather than direction — a
        // stream that continued *after* this session is as much a reuse candidate as one
        // that preceded it, so a close future stream outranks a distant past one and a
        // distant future one falls behind both.
        let position = |needle: &str| prompt.find(needle).expect("every stream is listed");
        assert!(position("neighbour") < position("near_future"));
        assert!(position("near_future") < position("stale"));
        assert!(position("stale") < position("ancient"));
        assert!(position("ancient") < position("distant_future"));
    }

    #[test]
    fn a_stream_whose_activity_brackets_the_session_outranks_one_merely_nearby() {
        // Given: a stream that was already running when this session happened, and one
        // that stopped shortly before it. Being underway is the strongest signal there is
        // that this session is another task within that same initiative.
        let roster = vec![
            active("nearby", "nearby work", -20, -5),
            active("underway", "underway work", -600, 600),
        ];

        // When
        let prompt = build(&input(Vec::new()), &roster);

        // Then
        let position = |needle: &str| prompt.find(needle).expect("every stream is listed");
        assert!(position("underway") < position("nearby"));
    }

    #[test]
    fn among_streams_spanning_the_session_the_tighter_one_comes_first() {
        // Given: two streams both underway, one scoped to this week and one spanning
        // months. Both are equally "present", so the tie is broken by span — a tightly
        // scoped initiative that covers this moment describes it better than a
        // long-running one that happens to. Measured: breaking the tie this way lifts
        // p95 rank from 26 to 13 and cap-25 coverage from 93.65% to 98.71%.
        let roster = vec![
            active("sprawling", "sprawling work", -100_000, 100_000),
            active("focused", "focused work", -120, 120),
        ];

        // When
        let prompt = build(&input(Vec::new()), &roster);

        // Then
        let position = |needle: &str| prompt.find(needle).expect("every stream is listed");
        assert!(position("focused") < position("sprawling"));
    }

    #[test]
    fn a_stream_that_has_never_been_active_sorts_last_but_is_still_listed() {
        // Given: a stream with no activity at all. 171 of the live table's streams have
        // no events, and a stream nobody has touched is the least likely reuse target —
        // but it is still a real stream, so it goes to the tail rather than being hidden.
        let mut never = stream("never", "never active", 0);
        never.first_active = None;
        never.last_active = None;
        let roster = vec![never, active("distant", "distant work", -90_000, -80_000)];

        // When
        let prompt = build(&input(Vec::new()), &roster);

        // Then: even activity from long ago beats no activity at all.
        let position = |needle: &str| prompt.find(needle).expect("every stream is listed");
        assert!(position("distant") < position("never"));
    }

    #[test]
    fn without_a_session_time_the_roster_falls_back_to_plain_recency() {
        // Given: a candidate whose moment is unknown. There is no distance to measure, so
        // the roster orders by recency instead — degraded, never arbitrary, and never
        // input order, which is `updated_at DESC` and mostly records when `tt recompute`
        // last ran (it put the reused stream in the top 200 just 54.76% of the time).
        let mut undated = input(Vec::new());
        undated.started_at = None;
        let roster = vec![
            active("ancient", "ancient work", -10_000, -9_000),
            active("newest", "newest work", -30, -10),
            active("stale", "stale work", -900, -800),
        ];

        // When
        let prompt = build(&undated, &roster);

        // Then
        let position = |needle: &str| prompt.find(needle).expect("every stream is listed");
        assert!(position("newest") < position("stale"));
        assert!(position("stale") < position("ancient"));
    }

    #[test]
    fn a_cap_never_drops_a_stream_ahead_of_a_less_relevant_one() {
        // Given: exactly one stream too many, the closest of them handed over last.
        // Capping the *input* order rather than the proximity order would drop it, which is
        // precisely the reuse target the cap must never hide.
        let mut roster: Vec<_> = (0..ROSTER_LIMIT)
            .map(|index| stream(&format!("old-{index}"), &format!("old work {index}"), 900))
            .collect();
        roster.push(stream("freshest", "freshest work", 1));

        // When
        let prompt = build(&input(Vec::new()), &roster);

        // Then
        assert!(prompt.contains("freshest"));
    }

    #[test]
    fn a_long_description_is_truncated_but_the_name_never_is() {
        // Given: descriptions average 428 bytes across the live table and reach 910, so
        // they dominate the roster's size. The name is the recognition signal the model
        // matches on, and must survive whole.
        let name = "agent-c: eval-3 traccar environment (eval-3 integration)";
        let mut only = stream("s1", name, 1);
        only.description = Some("d".repeat(ROSTER_DESCRIPTION_BUDGET * 3));

        // When
        let prompt = build(&input(Vec::new()), &[only]);

        // Then
        assert!(prompt.contains(name));
        assert!(!prompt.contains(&"d".repeat(ROSTER_DESCRIPTION_BUDGET + 1)));
        assert!(prompt.contains(&"d".repeat(ROSTER_DESCRIPTION_BUDGET)));
    }

    #[test]
    fn the_rendering_keeps_the_signal_a_model_recognises_a_stream_by() {
        // Given: a fully populated stream. Dropping descriptions and tags would save bytes
        // and cost reuse, which is the failure the cap is meant to prevent.
        let described = StreamSummary {
            slug: None,
            id: "s1".to_owned(),
            name: Some("agent-c: eval-3 integration".to_owned()),
            description: Some("Standing up eval-3 application environments".to_owned()),
            tags: vec!["eval-3".to_owned(), "infra".to_owned()],
            first_active: Some(at(-60)),
            last_active: Some(at(0)),
        };

        // When
        let prompt = build(&input(Vec::new()), &[described]);

        // Then
        assert!(prompt.contains("agent-c: eval-3 integration"));
        assert!(prompt.contains("Standing up eval-3 application environments"));
        assert!(prompt.contains("eval-3, infra"));
    }

    #[test]
    fn the_roster_is_rendered_far_more_compactly_than_one_line_per_field() {
        // Given: the shipped rendering spent an average of 315 bytes a stream on
        // `id: …; slug: …; name: …; description: …; tags: …; last active: …`, of which the
        // labels, an RFC 3339 timestamp that was NULL for 758 of 1,018 streams, and a slug
        // 25 streams have were pure overhead.
        let roster: Vec<_> = (0..100)
            .map(|index| stream(&format!("stream-{index}"), &format!("work {index}"), 0))
            .collect();

        // When
        let prompt = build(&input(Vec::new()), &roster);

        // Then: well under the old per-stream cost, without dropping name or description.
        let bytes_per_stream = prompt.len() / roster.len();
        assert!(
            bytes_per_stream < 200,
            "{bytes_per_stream} bytes per stream is no better than the rendering it replaced"
        );
    }

    #[test]
    fn keeps_only_the_five_most_recent_user_prompts_and_truncates_each() {
        // Given
        let user_prompts = (0..6)
            .map(|index| format!("prompt-{index}:{}", "x".repeat(600)))
            .collect();
        let input = input(user_prompts);

        // When
        let prompt = build(&input, &[]);

        // Then
        assert!(!prompt.contains("prompt-0:"));
        for index in 1..6 {
            let marker = format!("prompt-{index}:");
            let start = prompt.find(&marker).unwrap();
            let content = &prompt[start..].lines().next().unwrap();
            assert!(content.len() <= 500);
        }
    }

    #[test]
    fn requires_new_stream_details_and_bounded_confidence() {
        // Given
        let input = input(Vec::new());

        // When
        let prompt = build(&input, &[]);

        // Then
        assert!(prompt.contains("name"));
        assert!(prompt.contains("description"));
        assert!(prompt.contains("Confidence"));
        assert!(prompt.contains("0..1"));
    }

    #[test]
    fn the_instructions_make_reuse_the_default_and_creation_the_exception() {
        // Given: wording that was even-handed between the two ("Choose an existing stream
        // by stream_id, or create a new stream…") while the classifier minted ~101 streams
        // an hour, roughly one per session.
        // When
        let prompt = build(&input(Vec::new()), &[]);

        // Then: reuse must be stated as the default and creation as the fallback.
        let lowered = prompt.to_lowercase();
        assert!(lowered.contains("prefer"));
        assert!(lowered.contains("only create a new stream"));
    }

    #[test]
    fn the_instructions_state_what_granularity_a_stream_has() {
        // Given: the concrete failure. Six streams named
        // `agent-c: eval-3 <app> environment (eval-3 integration)` were created inside one
        // hour — one initiative split per application, one row per task instance. Nothing
        // in the prompt said that was wrong, and `is_misnamed_stream` deliberately does
        // not judge granularity.
        // When
        let prompt = build(&input(Vec::new()), &[]);

        // Then
        let lowered = prompt.to_lowercase();
        assert!(lowered.contains("initiative"));
        assert!(lowered.contains("many sessions"));
        assert!(
            lowered.contains("task"),
            "the instructions must say a stream is not one row per task instance"
        );
    }

    #[test]
    fn the_instructions_say_the_roster_is_ordered_and_partial() {
        // Given: the roster is capped, so a stream the session belongs to may be absent.
        // The model has to know the ordering it is reading and that absence from the list
        // is not evidence the stream does not exist.
        // When
        let prompt = build(&input(Vec::new()), &[]);

        // Then
        let lowered = prompt.to_lowercase();
        assert!(lowered.contains("closest to this session"));
        assert!(lowered.contains("not every stream"));
    }

    #[test]
    fn context_lookup_is_the_only_prompt_change_when_a_provider_is_wired() {
        // Given: identical classification evidence, with and without the optional
        // provider. Leaving it unwired must retain the byte-for-byte prompt that was
        // already in production.
        let input = input(vec!["Identify the customer for project Apollo".to_owned()]);
        let roster = roster();

        // When / Then
        insta::assert_snapshot!(
            "classification_prompt_without_context_provider",
            build(&input, &roster)
        );
        insta::assert_snapshot!(
            "classification_prompt_with_context_provider",
            build_with_context_provider(&input, &roster)
        );
    }
}
