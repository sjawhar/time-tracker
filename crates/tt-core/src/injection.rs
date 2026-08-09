//! Detection of harness-injected text in agent "user" messages.
//!
//! Coding agents record every message on the user turn as a user message,
//! including text the harness injected on its own initiative: background-task
//! notifications, process-exit notices, continuation directives and nudges,
//! tool caveats, compaction banners.
//! Those carry no human attention. Treating them as human input manufactures
//! direct time — it filled an entire night with attention windows on a day the
//! user was asleep.
//!
//! # Denylist, never allowlist
//!
//! Injections are a closed set: this repo's own harness emits them, so the list
//! is enumerable and changes only when the harness changes. Human prefixes are
//! open-ended — every new skill, mode, or command adds one (`<skill-instruction>`,
//! `[analyze-mode]`, `[CONTEXT]`, …). A shape-based rule such as "starts with
//! `<tag>`" would silently discard real intent every time a skill is added, and
//! the loss is invisible: under-reported attention looks like a quiet day.
//! So: match known injections, keep everything else.
//!
//! # Leading-token matching, not substring matching
//!
//! A marker counts only when it *opens* the message (after leading whitespace
//! and Markdown horizontal rules). Substring matching was measured against real
//! sessions and rejected: a human message that quotes or discusses a marker —
//! a bug report about this very code, a pasted transcript — contains the marker
//! mid-body and would be erased. Leading-token matching classified 733/744
//! `<system-reminder>` messages and all `---`-prefixed continuation directives
//! in a 150-session sample, while leaving quoting messages intact.
//!
//! The residue is messages where the harness prepends a mode banner
//! (`[analyze-mode]`, `[search-mode]`) to an injected payload. Those keep their
//! timestamp, because the banner is also what wraps genuine human input and the
//! two are indistinguishable from the banner alone. Over-counting a handful of
//! messages is the safe direction: the alternative deletes real attention.

/// Text fragments that, when they open a message, identify it as harness-injected.
///
/// Public because classification consumes the same list: a proposal built from
/// injected text describes the harness, not the work.
///
/// Every entry is verified present in real session data. Ordering is by
/// frequency so the common cases short-circuit first.
pub const INJECTION_MARKERS: &[&str] = &[
    // Wrapper the agent harness uses for all out-of-band notices.
    "<system-reminder>",
    // Preamble the harness writes onto the user turn when the user runs a tool
    // directly instead of typing. Byte-identical across every occurrence, which is
    // what identifies it as machine-authored: 2,066 of 27,653 persisted prompts open
    // with it and not one contains it mid-body. Each opened a 5-minute attention
    // window on a turn where the human said nothing.
    "The following tool was executed by the user",
    // Continuation / execution-protocol directives injected between turns.
    "[SYSTEM DIRECTIVE: OH-MY-OPENCODE",
    // Agent-to-agent messages delivered onto the user turn.
    "[NOTIFICATION from",
    // Caveat appended when a slash command expands to a local shell command.
    "<local-command-caveat>",
    // Banner opening a session resumed from a compacted transcript.
    "This session is being continued from a previous",
    // Exit notice the PTY tool runtime posts when a background process ends.
    "<pty_exited>",
    // Nudge the harness sends when an agent stops without a clear next step.
    "Continue if you have next steps, or stop and ask for clarification",
];

/// Returns `true` when `message` was injected by the agent harness rather than
/// written by a human.
///
/// See the module docs for why this is a leading-token denylist.
#[must_use]
pub fn is_injected(message: &str) -> bool {
    let body = strip_leading_rules(message);
    INJECTION_MARKERS
        .iter()
        .any(|marker| body.starts_with(marker))
}

/// Returns the message when it is human-authored, `None` when it is injected.
///
/// Shaped for `filter_map` so extractors skip injected text in one step.
#[must_use]
pub fn human_message(message: &str) -> Option<&str> {
    (!is_injected(message)).then_some(message)
}

/// Strips leading whitespace and any Markdown horizontal-rule lines.
///
/// Injected payloads are frequently separated from what precedes them by a
/// `---` line, so the rule itself is not part of the message's opening token.
fn strip_leading_rules(message: &str) -> &str {
    let mut rest = message.trim_start();
    loop {
        let line_end = rest.find('\n').unwrap_or(rest.len());
        let line = rest[..line_end].trim_end();
        if line.len() < 3 || !line.bytes().all(|b| b == b'-') {
            return rest;
        }
        rest = rest[line_end..].trim_start();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_reminder_opening_a_message_is_injected() {
        assert!(is_injected(
            "<system-reminder>\n[BACKGROUND TASK COMPLETED]\n**ID:** `bg_f60bcb1c`"
        ));
    }

    #[test]
    fn boulder_continuation_directive_is_injected() {
        assert!(is_injected(
            "[SYSTEM DIRECTIVE: OH-MY-OPENCODE - BOULDER CONTINUATION]\n\nContinue the plan."
        ));
    }

    #[test]
    fn agent_notification_is_injected() {
        assert!(is_injected(
            "[NOTIFICATION from agent (reply-to: ses_1e17671e3ffeZB1JjTuRD0mqDR)]\nChecking in:"
        ));
    }

    #[test]
    fn local_command_caveat_is_injected() {
        assert!(is_injected("<local-command-caveat>Caveat: the messages"));
    }

    #[test]
    fn compaction_banner_is_injected() {
        assert!(is_injected(
            "This session is being continued from a previous conversation that ran out of context."
        ));
    }

    #[test]
    fn pty_exit_notice_is_injected() {
        assert!(is_injected(
            "<pty_exited>\nID: pty_8e357f94\nDescription: Run lint typecheck and test suite\n\
             Exit Code: 1\n</pty_exited>\n\nProcess failed."
        ));
    }

    #[test]
    fn continuation_nudge_is_injected() {
        assert!(is_injected(
            "Continue if you have next steps, or stop and ask for clarification if you are \
             unsure how to proceed."
        ));
    }

    #[test]
    fn a_human_asking_to_continue_keeps_their_message() {
        // The nudge is matched as a leading token by its full opening clause, so
        // ordinary continuation requests are untouched.
        assert!(!is_injected("continue"));
        assert!(!is_injected("Continue with the next phase."));
        assert!(!is_injected(
            "Continue if you have next steps for the migration — I am heading out."
        ));
    }

    #[test]
    fn horizontal_rule_before_a_directive_does_not_hide_it() {
        // The single largest real-data case: the harness separates the payload
        // from the preceding turn with a Markdown rule.
        assert!(is_injected(
            "---\n\n[SYSTEM DIRECTIVE: OH-MY-OPENCODE - PROMETHEUS READ-ONLY]\n\nYou are being invoked"
        ));
        assert!(is_injected("-----\n[NOTIFICATION from agent]\nstatus"));
    }

    #[test]
    fn leading_whitespace_does_not_hide_a_marker() {
        assert!(is_injected(
            "\n\n   <system-reminder>\nbackground task done"
        ));
    }

    #[test]
    fn skill_instruction_is_real_intent() {
        assert!(!is_injected(
            "<skill-instruction>\nUse the using-jj skill for version control."
        ));
    }

    #[test]
    fn human_markers_that_a_shape_rule_would_destroy_all_survive() {
        // Each of these opens real human messages in the corpus. A rule like
        // `<[a-zA-Z-_]+>` or `\[[A-Z-]+\]` would erase every one of them.
        for opener in [
            "<skill-instruction>",
            "<command-instruction>",
            "<command-message>",
            "<command_instructions>",
            "<Work_Context>",
            "<review_type>",
            "<ultrawork-mode>",
            "<hyperplan-mode>",
            "<bash-input>",
            "<teammate-message from=\"lead\">",
            "[analyze-mode]",
            "[CONTEXT]",
            "[search-mode]",
            "[TASK]",
            "[GOAL]",
        ] {
            let message = format!("{opener}\nreal work here");
            assert!(
                !is_injected(&message),
                "{opener} is human intent and must not be treated as injected"
            );
        }
    }

    #[test]
    fn mode_banners_survive() {
        assert!(!is_injected(
            "[analyze-mode]\nANALYSIS MODE. Gather context"
        ));
        assert!(!is_injected("[CONTEXT]\nWe are mid-refactor."));
        assert!(!is_injected("[search-mode]\nMAXIMIZE SEARCH EFFORT."));
    }

    #[test]
    fn a_human_quoting_a_marker_keeps_its_message() {
        // Substring matching would delete this. It is a person describing the
        // bug this module fixes — exactly the message we must not lose.
        assert!(!is_injected(
            "The bug: agent-injected text like <system-reminder> is recorded as a \
             user_message and opens a 5-minute attention window."
        ));
        assert!(!is_injected(
            "Grep the corpus for [SYSTEM DIRECTIVE: OH-MY-OPENCODE and tell me the count."
        ));
    }

    #[test]
    fn empty_and_whitespace_only_messages_are_not_injected() {
        assert!(!is_injected(""));
        assert!(!is_injected("   \n\t "));
    }

    #[test]
    fn a_message_of_only_horizontal_rules_terminates_and_is_not_injected() {
        assert!(!is_injected("---"));
        assert!(!is_injected("---\n---\n---\n"));
    }

    #[test]
    fn harness_tool_execution_preamble_is_injected() {
        // Byte-identical across all 2,066 occurrences in the live corpus, which is what
        // marks it machine-authored: the human ran a tool and typed nothing, yet the
        // turn opened a 5-minute attention window.
        assert!(is_injected("The following tool was executed by the user"));
    }

    #[test]
    fn a_human_quoting_the_tool_preamble_mid_body_is_not_injected() {
        // The leading-token rule is what protects this. A bug report about the very
        // marker above is real intent and must survive; 0 of 27,653 persisted prompts
        // contain it mid-body today, but a human writing one tomorrow must not vanish.
        assert!(!is_injected(
            "Why does tt count 'The following tool was executed by the user' as attention?"
        ));
    }

    #[test]
    fn human_message_passes_through_real_intent_and_drops_injections() {
        assert_eq!(human_message("[TASK] ship it"), Some("[TASK] ship it"));
        assert_eq!(human_message("<system-reminder>\nnotice"), None);
    }
}
