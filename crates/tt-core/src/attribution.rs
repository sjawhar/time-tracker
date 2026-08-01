//! Attributing laptop window focus that carries no `cwd`.
//!
//! `window_focus` events come from the COSMIC watcher and are the closest thing
//! to a ground-truth attention signal, but they carry `window_app_id` and
//! `window_title` and **no `cwd`**. Stream attribution keys on `cwd`, so every
//! hour of GUI attention fell through to whatever container a classifier had
//! invented for it.
//!
//! A terminal focus is recoverable without content understanding: when the user
//! looks at a terminal attached to a remote host, the work is whatever that host
//! was doing at the time, and the host's own events carry a `cwd` that is
//! already classified. This module decides, for one focus event, which stream
//! that is.
//!
//! # Either signal is enough to call it a terminal
//!
//! A known terminal `app_id` alone suffices, and a title opening with a
//! remote-shell command alone suffices. Requiring both would silently drop
//! attention: the app set grows the moment the terminal is swapped, and the
//! title set grows the moment a new remote tool is used. An unrecognised
//! terminal resolves to `None`, which leaves the event unassigned — a visible,
//! safe failure, unlike a dropped hour that merely looks like a quiet day.
//!
//! # Ties resolve to nothing
//!
//! Resolution is a *strict* plurality of the streams active nearby. Two streams
//! tied for the lead is a coin flip, and a coin flip is an invented answer. The
//! event stays unassigned, where it registers as classification lag rather than
//! as false attribution.
//!
//! # Where ±60 seconds came from
//!
//! Measured over 1,581 terminal-focus events across 2026-07-15..21: ±15s
//! resolves 92.0%, ±30s 97.5%, ±60s 99.4%, ±120s 99.8%, ±600s 100%. The curve
//! is flat past 60s, so a wider window buys 0.6% of coverage at the cost of
//! letting a focus event be adopted by work the user had already left.
//!
//! # A temporal *hint* for the classifier was built here and removed
//!
//! A `streams_active_near` tallied the classified activity around a window run and fed
//! it to `tt classify --auto` as evidence — every nearby stream with its share, naming
//! no winner, so the model still chose. It measured negative and is gone. Do not
//! rebuild it from the intuition that the classifier merely lacks context: the numbers,
//! including a Google Meet room code whose confidence the evidence pushed to 0.75, are
//! in the root `AGENTS.md` under "Giving a window run temporal evidence".

use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};

/// Correlation half-width: activity this far either side of a focus event counts.
pub const TERMINAL_CORRELATION_WINDOW_MS: i64 = 60_000;

/// Window app IDs that are terminals.
///
/// `com.mitchellh.ghostty` is the only one present in real data; the rest are
/// the terminals a swap would plausibly land on, listed so that swapping does
/// not silently stop attributing attention. `org.wezfurlong.wezterm` is
/// deliberately absent: it is the tests' stand-in for an unrecognised terminal,
/// and listing it here would make the title rule untested.
const TERMINAL_APP_IDS: &[&str] = &[
    "com.mitchellh.ghostty",
    "com.system76.CosmicTerm",
    "Alacritty",
    "kitty",
    "org.gnome.Terminal",
];

/// Title openings that mean the window is running a shell on a remote host.
///
/// `tmux ` subsumes `tmux attach`, so only the shorter form is listed.
const REMOTE_SHELL_PREFIXES: &[&str] = &["tmux ", "mosh ", "ssh "];

/// One classified event from a remote host, used as a correlation candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteActivity {
    pub timestamp: DateTime<Utc>,
    pub stream_id: String,
}

/// Returns `true` when a focused window is a terminal.
///
/// Either signal suffices; see the module docs for why.
#[must_use]
pub fn is_terminal_focus(app_id: Option<&str>, title: Option<&str>) -> bool {
    let known_app = app_id.is_some_and(|id| TERMINAL_APP_IDS.contains(&id));
    let remote_shell_title = title.is_some_and(|title| {
        REMOTE_SHELL_PREFIXES
            .iter()
            .any(|prefix| title.starts_with(prefix))
    });
    known_app || remote_shell_title
}

/// Returns the stream that dominated remote activity around `focus_at`.
///
/// `sorted` must be ordered by timestamp ascending; the window is located by
/// binary search so one pre-loaded slice serves every focus event. Returns
/// `None` when nothing was active nearby, or when no single stream leads.
#[must_use]
pub fn resolve_terminal_focus(
    focus_at: DateTime<Utc>,
    sorted: &[RemoteActivity],
    window_ms: i64,
) -> Option<String> {
    let focus_ms = focus_at.timestamp_millis();
    let start_ms = focus_ms.saturating_sub(window_ms);
    let end_ms = focus_ms.saturating_add(window_ms);

    let first = sorted.partition_point(|activity| activity.timestamp.timestamp_millis() < start_ms);
    let last = sorted.partition_point(|activity| activity.timestamp.timestamp_millis() <= end_ms);

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for activity in &sorted[first..last] {
        *counts.entry(activity.stream_id.as_str()).or_default() += 1;
    }

    let (winner, top) = counts.iter().max_by_key(|(_, count)| **count)?;
    let leaders = counts.values().filter(|count| *count == top).count();
    (leaders == 1).then(|| (*winner).to_string())
}

// ---------------------------------------------------------------------------
// Artifact-reference attribution (non-terminal focus)
// ---------------------------------------------------------------------------

/// The `·` GitHub puts between the parts of a page title.
const DOT: &str = "·";

/// The longest issue or pull-request number treated as plausible.
///
/// Five digits covers every number in the corpus (the largest is `#13986`); the
/// sixth is slack. A longer `#`-prefixed run of digits is far more likely to be
/// an identifier of some other kind than an issue reference.
const MAX_ARTIFACT_NUMBER_DIGITS: usize = 6;

/// Characters GitHub permits in an owner or repository name.
const fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'
}

/// A durable work artifact: one pull request or issue in one repository.
///
/// `owner` is `None` when the reference was written as a bare `#123`, which names
/// a number within some repository without naming that repository's owner. Two
/// references describe the same artifact when repo and number agree and the owners
/// do not *disagree* — see [`ArtifactRef::is_same_artifact_as`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactRef {
    pub owner: Option<String>,
    pub repo: String,
    pub number: String,
}

impl ArtifactRef {
    /// Returns `true` when both references name the same pull request or issue.
    ///
    /// A missing owner is *compatible*, not wildcard-equal: it lets a bare `#123`
    /// written inside a project bind to that project's artifact, while two
    /// **known** and different owners never bind. Forks make that distinction
    /// load-bearing — `METR/hawk` and `trajectory-labs-pbc/hawk` share a repo name
    /// and a number space but are different work.
    #[must_use]
    pub fn is_same_artifact_as(&self, other: &Self) -> bool {
        if self.repo != other.repo || self.number != other.number {
            return false;
        }
        match (self.owner.as_deref(), other.owner.as_deref()) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => true,
        }
    }
}

/// One artifact reference found in already-classified work, with that work's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMention {
    pub artifact: ArtifactRef,
    pub stream_id: String,
}

/// Returns the pull request or issue a window title is displaying.
///
/// Only a title naming a *specific* artifact qualifies. A repository-wide listing
/// (`Pull requests · sjawhar/time-tracker`), an account-wide one (`Work · Pull
/// requests`), and every title that names no artifact at all (`New Tab - Brave`)
/// return `None`: a repository hosts many streams, so a title that reaches no
/// further than a repository has identified no work.
#[must_use]
pub fn artifact_in_title(title: Option<&str>) -> Option<ArtifactRef> {
    let title = title?;
    // Last occurrence: a subject line may itself quote `#123`, but the trailing
    // `<dot> Pull Request #N <dot> owner/repo` is what GitHub appends.
    let tail = ["Pull Request #", "Issue #"]
        .iter()
        .filter_map(|kind| {
            let marker = format!("{DOT} {kind}");
            title.rfind(&marker).map(|at| (at, at + marker.len()))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, from)| &title[from..])?;

    let number: String = tail.chars().take_while(char::is_ascii_digit).collect();
    if number.is_empty() || number.len() > MAX_ARTIFACT_NUMBER_DIGITS {
        return None;
    }
    let slug = tail[number.len()..]
        .split_once(DOT)?
        .1
        .split_whitespace()
        .next()?;
    parse_owner_repo(slug).map(|(owner, repo)| ArtifactRef {
        owner: Some(owner),
        repo,
        number,
    })
}

/// Splits `owner/repo`, rejecting anything that is not exactly that shape.
fn parse_owner_repo(slug: &str) -> Option<(String, String)> {
    let (owner, repo) = slug.split_once('/')?;
    let ok = |part: &str| !part.is_empty() && part.chars().all(is_name_char);
    (ok(owner) && ok(repo)).then(|| (owner.to_string(), repo.to_string()))
}

/// Returns every artifact the text of one piece of classified work refers to.
///
/// Both recognised forms are *identifiers*, never prose. A GitHub URL is
/// self-scoping. A bare `#123` is scoped by `project`, the repository the work was
/// done in, and is ignored entirely when that is unknown — a number belonging to no
/// repository names nothing.
///
/// Prose matching was measured and rejected: scoring a title's subject words against
/// this same text bound `API Keys | Claude Platform` to `agent-c: rl-eval postgres`.
/// See the module docs.
#[must_use]
pub fn artifact_refs_in_text(text: &str, project: Option<&str>) -> Vec<ArtifactRef> {
    let mut found = BTreeSet::new();

    let mut rest = text;
    while let Some(at) = rest.find("github.com/") {
        rest = &rest[at + "github.com/".len()..];
        if let Some(artifact) = parse_artifact_url_path(rest) {
            found.insert(artifact);
        }
    }

    if let Some(project) = project {
        for (at, _) in text.match_indices('#') {
            if let Some(number) = parse_hash_number(&text[at + 1..]) {
                found.insert(ArtifactRef {
                    owner: None,
                    repo: project.to_string(),
                    number,
                });
            }
        }
    }

    found.into_iter().collect()
}

/// Reads the digits of a `#123` reference, requiring a word boundary after them.
///
/// The boundary is what keeps `#ff0000` and `#47abc` out: an identifier that
/// continues into letters was never an issue number.
fn parse_hash_number(tail: &str) -> Option<String> {
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    let bounded = tail[digits.len()..]
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
    (!digits.is_empty() && digits.len() <= MAX_ARTIFACT_NUMBER_DIGITS && bounded).then_some(digits)
}

/// Parses `owner/repo/pull/123` or `owner/repo/issues/123` off the front of `path`.
fn parse_artifact_url_path(path: &str) -> Option<ArtifactRef> {
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if !matches!(parts.next()?, "pull" | "issues") {
        return None;
    }
    let number = parse_hash_number(parts.next()?)?;
    let (owner, repo) = parse_owner_repo(&format!("{owner}/{repo}"))?;
    Some(ArtifactRef {
        owner: Some(owner),
        repo,
        number,
    })
}

/// Returns the stream whose work referred to `target`, or `None`.
///
/// Strict plurality over the mentions of *that one artifact*, on the same
/// no-coin-flips discipline as [`resolve_terminal_focus`]. Mentions of any other
/// artifact are not candidates at all, which is what stops this degenerating into
/// adopting whichever stream happened to be active nearby.
#[must_use]
pub fn resolve_artifact_focus(
    target: &ArtifactRef,
    mentions: &[ArtifactMention],
) -> Option<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for mention in mentions
        .iter()
        .filter(|mention| mention.artifact.is_same_artifact_as(target))
    {
        *counts.entry(mention.stream_id.as_str()).or_default() += 1;
    }

    let (winner, top) = counts.iter().max_by_key(|(_, count)| **count)?;
    let leaders = counts.values().filter(|count| *count == top).count();
    (leaders == 1).then(|| (*winner).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn at(s: i64) -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + s, 0).unwrap()
    }

    fn act(s: i64, id: &str) -> RemoteActivity {
        RemoteActivity {
            timestamp: at(s),
            stream_id: id.to_string(),
        }
    }

    #[test]
    fn ghostty_is_a_terminal() {
        assert!(is_terminal_focus(
            Some("com.mitchellh.ghostty"),
            Some("tmux attach -t dev -d")
        ));
    }

    #[test]
    fn an_unknown_app_running_a_remote_shell_is_still_a_terminal() {
        assert!(is_terminal_focus(
            Some("org.wezfurlong.wezterm"),
            Some("mosh devbox")
        ));
    }

    #[test]
    fn a_browser_is_not_a_terminal() {
        assert!(!is_terminal_focus(
            Some("brave-browser"),
            Some("Work · Pull requests - Brave")
        ));
    }

    #[test]
    fn resolves_to_the_dominant_concurrent_stream() {
        let acts = vec![
            act(-30, "eval"),
            act(-5, "eval"),
            act(10, "dpi"),
            act(40, "eval"),
        ];
        assert_eq!(
            resolve_terminal_focus(at(0), &acts, 60_000),
            Some("eval".to_string())
        );
    }

    #[test]
    fn activity_outside_the_window_is_ignored() {
        let acts = vec![act(-90, "stale"), act(-90, "stale"), act(-10, "live")];
        assert_eq!(
            resolve_terminal_focus(at(0), &acts, 60_000),
            Some("live".to_string())
        );
    }

    #[test]
    fn a_tie_resolves_to_nothing_rather_than_guessing() {
        let acts = vec![act(-10, "a"), act(10, "b")];
        assert_eq!(resolve_terminal_focus(at(0), &acts, 60_000), None);
    }

    #[test]
    fn no_concurrent_activity_resolves_to_nothing() {
        assert_eq!(resolve_terminal_focus(at(0), &[], 60_000), None);
    }

    // ---- artifact references ------------------------------------------

    fn art(owner: Option<&str>, repo: &str, number: &str) -> ArtifactRef {
        ArtifactRef {
            owner: owner.map(ToString::to_string),
            repo: repo.to_string(),
            number: number.to_string(),
        }
    }

    fn mention(artifact: ArtifactRef, stream: &str) -> ArtifactMention {
        ArtifactMention {
            artifact,
            stream_id: stream.to_string(),
        }
    }

    #[test]
    fn a_pull_request_title_names_its_artifact() {
        assert_eq!(
            artifact_in_title(Some(
                "feat(tt): cosmic window/idle watcher by legion-implementer[bot] \
                 · Pull Request #46 · sjawhar/time-tracker - Brave"
            )),
            Some(art(Some("sjawhar"), "time-tracker", "46"))
        );
    }

    #[test]
    fn an_issue_title_names_its_artifact() {
        assert_eq!(
            artifact_in_title(Some(
                "Engineering Priorities and Roadmap · Issue #11280 \
                 · trajectory-labs-pbc/agent-c - Brave"
            )),
            Some(art(Some("trajectory-labs-pbc"), "agent-c", "11280"))
        );
    }

    #[test]
    fn a_new_tab_names_no_artifact() {
        assert_eq!(artifact_in_title(Some("New Tab - Brave")), None);
    }

    #[test]
    fn a_slack_thread_names_no_artifact() {
        assert_eq!(
            artifact_in_title(Some("Threads - Trajectory Labs - 3 new items - Slack")),
            None
        );
    }

    #[test]
    fn a_repo_wide_listing_names_no_artifact() {
        assert_eq!(
            artifact_in_title(Some("Pull requests · sjawhar/time-tracker - Brave")),
            None
        );
        assert_eq!(
            artifact_in_title(Some("Work · Pull requests - Brave")),
            None
        );
    }

    #[test]
    fn resolves_to_the_stream_that_worked_on_the_artifact() {
        let target = art(Some("sjawhar"), "time-tracker", "46");
        let mentions = vec![mention(
            art(Some("sjawhar"), "time-tracker", "46"),
            "tracker",
        )];
        assert_eq!(
            resolve_artifact_focus(&target, &mentions),
            Some("tracker".to_string())
        );
    }

    #[test]
    fn an_artifact_claimed_equally_by_two_streams_resolves_to_nothing() {
        let target = art(Some("sjawhar"), "time-tracker", "46");
        let mentions = vec![
            mention(art(Some("sjawhar"), "time-tracker", "46"), "tracker"),
            mention(art(Some("sjawhar"), "time-tracker", "46"), "tooling"),
        ];
        assert_eq!(resolve_artifact_focus(&target, &mentions), None);
    }

    #[test]
    fn an_unreferenced_artifact_resolves_to_nothing() {
        let target = art(Some("sjawhar"), "time-tracker", "46");
        assert_eq!(resolve_artifact_focus(&target, &[]), None);
    }

    #[test]
    fn a_different_artifact_is_never_adopted() {
        let target = art(Some("sjawhar"), "time-tracker", "46");
        let mentions = vec![
            mention(art(Some("sjawhar"), "time-tracker", "47"), "tracker"),
            mention(art(Some("sjawhar"), "legion", "46"), "legion"),
        ];
        assert_eq!(resolve_artifact_focus(&target, &mentions), None);
    }

    #[test]
    fn a_bare_number_reference_binds_within_its_own_project() {
        let target = art(Some("sjawhar"), "time-tracker", "47");
        let mentions = vec![mention(art(None, "time-tracker", "47"), "tracker")];
        assert_eq!(
            resolve_artifact_focus(&target, &mentions),
            Some("tracker".to_string())
        );
    }

    #[test]
    fn a_known_owner_mismatch_does_not_bind() {
        let target = art(Some("METR"), "hawk", "1090");
        let mentions = vec![mention(
            art(Some("trajectory-labs-pbc"), "hawk", "1090"),
            "fork",
        )];
        assert_eq!(resolve_artifact_focus(&target, &mentions), None);
    }

    #[test]
    fn a_github_url_is_a_reference() {
        let refs = artifact_refs_in_text(
            "opened https://github.com/sjawhar/time-tracker/pull/46 for review",
            None,
        );
        assert_eq!(refs, vec![art(Some("sjawhar"), "time-tracker", "46")]);
    }

    #[test]
    fn an_issues_url_is_a_reference() {
        let refs = artifact_refs_in_text(
            "see github.com/trajectory-labs-pbc/agent-c/issues/11280",
            None,
        );
        assert_eq!(
            refs,
            vec![art(Some("trajectory-labs-pbc"), "agent-c", "11280")]
        );
    }

    #[test]
    fn a_hash_number_is_a_reference_scoped_to_the_project() {
        assert_eq!(
            artifact_refs_in_text("CI is red on #47", Some("time-tracker")),
            vec![art(None, "time-tracker", "47")]
        );
    }

    #[test]
    fn a_hash_number_outside_any_project_is_not_a_reference() {
        assert!(artifact_refs_in_text("CI is red on #47", None).is_empty());
    }

    #[test]
    fn a_plain_number_is_not_a_reference() {
        assert!(artifact_refs_in_text("took 47 minutes", Some("time-tracker")).is_empty());
    }

    #[test]
    fn a_colour_literal_is_not_a_reference() {
        assert!(
            artifact_refs_in_text("set colour #ff0000 please", Some("time-tracker")).is_empty()
        );
    }
}
