//! Resolving a focused tmux pane to the agent session running inside it.
//!
//! A `tmux_pane_focus` event carries no window title, no `window_app_id` and no
//! session id, so nothing in the pipeline could ever attribute one. What it does
//! have is a process tree, and a pane hosting an interactive agent holds that
//! agent's session id in that tree persistently — measured at 101 of 101
//! observations, against 0% for every plain shell pane.
//!
//! This is an **identity**, not an inference: the pane being looked at *is*
//! running that session. So nothing here derives a stream. It records only which
//! session the focused pane was running, and the already-trusted session→stream
//! path (`Database::assign_events_by_session_id`, run when the session is
//! classified) does the rest.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::process::Command;

/// The environment variables an agent harness stamps on its own process.
///
/// Exactly two, and the list is closed. An agent process's environment holds
/// credentials, so the walk extracts these values and drops every other byte it
/// read — see [`session_id_in_environ`].
const SESSION_ENV_NAMES: [&[u8]; 2] = [b"OPENCODE_SESSION_ID", b"CLAUDE_CODE_SESSION_ID"];

/// Processes whose environment may be read for one lookup.
const MAX_PROCESSES: usize = 64;

/// Generations walked below the pane's own process.
const MAX_DEPTH: u32 = 8;

/// The process facts the walk needs, injectable so it can be driven without
/// real processes.
pub trait ProcessSource {
    /// Direct children of `pid`. Empty when `pid` has none or cannot be read —
    /// the two are indistinguishable to the walk, and deliberately so.
    fn children(&self, pid: u32) -> Vec<u32>;

    /// `pid`'s raw NUL-separated environment block, or `None` when it cannot be
    /// read (permission denied, or the process exited mid-walk).
    fn environ(&self, pid: u32) -> Option<Vec<u8>>;
}

/// Extracts an agent session id from one process's raw environment block.
///
/// The block is matched **by entry name**, never by substring: an agent's
/// environment legitimately contains `MY_OPENCODE_SESSION_ID`, a note whose
/// *value* spells one of these names, and a great deal that is secret. Only the
/// value of an entry named exactly by [`SESSION_ENV_NAMES`] is returned, and the
/// caller drops `raw` immediately after — nothing else here is copied, logged or
/// stored.
///
/// An exported-but-empty variable is not a session: the harness sets these to a
/// real id, so an empty one means the variable was cleared rather than that a
/// session is running.
fn session_id_in_environ(raw: &[u8]) -> Option<String> {
    raw.split(|byte| *byte == 0).find_map(|entry| {
        let separator = entry.iter().position(|byte| *byte == b'=')?;
        let (name, value) = entry.split_at(separator);
        if !SESSION_ENV_NAMES.contains(&name) {
            return None;
        }
        let value = &value[1..];
        if value.is_empty() {
            return None;
        }
        String::from_utf8(value.to_vec()).ok()
    })
}

/// Walks a pane's process tree breadth-first for the session running in it.
///
/// Breadth-first because the harness sits a fixed few generations below the
/// pane's shell (measured at depth 3 on this machine) while a busy pane's tree is
/// wide, so the nearest process is the likeliest holder. First match wins.
///
/// Bounded twice, because this runs on every pane focus — thousands of times a
/// day: [`MAX_PROCESSES`] environments are read at most, and no generation past
/// [`MAX_DEPTH`] is expanded. Observed trees are 2–8 processes at depth ≤ 5, so
/// both bounds sit well above real work and exist to cap pathology.
///
/// Every failure is silent and total: an unreadable process is skipped rather
/// than aborting the walk, and a pane with no agent returns `None` — which is the
/// correct answer for a plain shell, measured at 0% across every such pane.
pub fn resolve_pane_session<S: ProcessSource>(source: &S, pane_process_id: u32) -> Option<String> {
    let mut queue = VecDeque::from([(pane_process_id, 0_u32)]);
    let mut seen = HashSet::from([pane_process_id]);
    let mut examined = 0_usize;

    while let Some((pid, depth)) = queue.pop_front() {
        if examined >= MAX_PROCESSES {
            break;
        }
        examined += 1;

        if let Some(raw) = source.environ(pid) {
            if let Some(session_id) = session_id_in_environ(&raw) {
                return Some(session_id);
            }
        }

        if depth >= MAX_DEPTH {
            continue;
        }
        for child in source.children(pid) {
            if seen.insert(child) {
                queue.push_back((child, depth + 1));
            }
        }
    }

    None
}

/// The real process tree, read from `/proc`.
///
/// Absent on every non-Linux target and inside a container without `/proc`
/// mounted, where every method returns nothing and the walk degrades to `None`.
pub struct ProcFs;

impl ProcessSource for ProcFs {
    /// Reads `/proc/<pid>/task/<tid>/children` for **every** thread of `pid`.
    ///
    /// Per-thread iteration is load-bearing, not thoroughness: a child is listed
    /// under the thread that forked it, and the harnesses fork from worker
    /// threads. Measured across every live pane on this machine, reading only the
    /// main thread's file found **0** session ids while reading all threads found
    /// the agent — so the main-thread shortcut resolves nothing at all.
    fn children(&self, pid: u32) -> Vec<u32> {
        let Ok(threads) = fs::read_dir(format!("/proc/{pid}/task")) else {
            return Vec::new();
        };
        threads
            .flatten()
            .filter_map(|thread| fs::read_to_string(thread.path().join("children")).ok())
            .flat_map(|listed| {
                listed
                    .split_ascii_whitespace()
                    .filter_map(|child| child.parse::<u32>().ok())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn environ(&self, pid: u32) -> Option<Vec<u8>> {
        fs::read(format!("/proc/{pid}/environ")).ok()
    }
}

/// Asks tmux for a pane's process id.
///
/// The fallback for an install whose `~/.tmux.conf` has not re-sourced
/// `config/tmux-hook.conf` since `--pane-pid` was added, so those keep resolving
/// sessions instead of waiting for a manual step. Any failure — no tmux, a dead
/// server, a pane that has closed — is `None`.
fn pane_pid_from_tmux(pane_id: &str) -> Option<u32> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", pane_id, "#{pane_pid}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// The agent session running in a focused pane, or `None`.
///
/// `None` is a first-class answer, not a failure: a plain shell pane has no
/// session, and every way this lookup can fail — no pane pid, no tmux, no
/// `/proc`, permission denied, a process exiting mid-walk — arrives here as
/// `None` so the focus event is recorded exactly as it was before this existed. A
/// focus event is never lost to this lookup.
/// `pane_process_id` arrives as the text tmux substituted, so it is parsed leniently: an
/// empty or unusable value falls through to asking tmux rather than giving up.
pub fn session_for_pane(pane_id: &str, pane_process_id: Option<&str>) -> Option<String> {
    let pane_process_id = pane_process_id
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .or_else(|| pane_pid_from_tmux(pane_id))?;
    resolve_pane_session(&ProcFs, pane_process_id)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    /// Content an agent process really does carry beside its session id, and
    /// which must never come back out of this module.
    const SECRETS: [&str; 4] = [
        "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIfake",
        "ANTHROPIC_API_KEY=sk-ant-notreal",
        "HOME=/home/sami",
        "PATH=/usr/bin:/bin",
    ];

    /// Builds a NUL-separated environment block in `/proc` layout.
    fn environ_blob(entries: &[&str]) -> Vec<u8> {
        let mut raw = Vec::new();
        for entry in entries {
            raw.extend_from_slice(entry.as_bytes());
            raw.push(0);
        }
        raw
    }

    /// A scripted process tree. A pid absent from `environs` models one whose
    /// environment cannot be read: permission denied, or the process exited
    /// mid-walk.
    struct FakeTree {
        children: HashMap<u32, Vec<u32>>,
        environs: HashMap<u32, Vec<u8>>,
        examined: RefCell<Vec<u32>>,
    }

    impl FakeTree {
        fn new() -> Self {
            Self {
                children: HashMap::new(),
                environs: HashMap::new(),
                examined: RefCell::new(Vec::new()),
            }
        }

        fn with_children(mut self, pid: u32, children: &[u32]) -> Self {
            self.children.insert(pid, children.to_vec());
            self
        }

        fn with_environ(mut self, pid: u32, entries: &[&str]) -> Self {
            self.environs.insert(pid, environ_blob(entries));
            self
        }

        fn examined_count(&self) -> usize {
            self.examined.borrow().len()
        }
    }

    impl ProcessSource for FakeTree {
        fn children(&self, pid: u32) -> Vec<u32> {
            self.children.get(&pid).cloned().unwrap_or_default()
        }

        fn environ(&self, pid: u32) -> Option<Vec<u8>> {
            self.examined.borrow_mut().push(pid);
            self.environs.get(&pid).cloned()
        }
    }

    /// A chain `root -> root+1 -> ... -> root+len`, session id on the deepest.
    fn chain(root: u32, len: u32, session_id: &str) -> FakeTree {
        let mut tree = FakeTree::new();
        for step in 0..len {
            tree = tree
                .with_children(root + step, &[root + step + 1])
                .with_environ(root + step, &SECRETS);
        }
        let deepest = format!("OPENCODE_SESSION_ID={session_id}");
        let mut entries: Vec<&str> = SECRETS.to_vec();
        entries.push(&deepest);
        tree.with_environ(root + len, &entries)
    }

    #[test]
    fn an_opencode_session_id_is_extracted() {
        let raw = environ_blob(&["OPENCODE_SESSION_ID=ses_0210f2ed2ffedhF4"]);
        assert_eq!(
            session_id_in_environ(&raw),
            Some("ses_0210f2ed2ffedhF4".to_string())
        );
    }

    #[test]
    fn a_claude_session_id_is_extracted() {
        let raw = environ_blob(&["CLAUDE_CODE_SESSION_ID=1f7c0e3a-claude"]);
        assert_eq!(
            session_id_in_environ(&raw),
            Some("1f7c0e3a-claude".to_string())
        );
    }

    #[test]
    fn a_session_id_is_found_past_leading_entries() {
        let mut entries: Vec<&str> = SECRETS.to_vec();
        entries.push("OPENCODE_SESSION_ID=ses_late");
        let raw = environ_blob(&entries);
        assert_eq!(session_id_in_environ(&raw), Some("ses_late".to_string()));
    }

    #[test]
    fn only_the_two_session_variables_are_ever_extracted() {
        let mut entries: Vec<&str> = SECRETS.to_vec();
        // Names that contain a session variable's name but are not one.
        entries.push("MY_OPENCODE_SESSION_ID=decoy");
        entries.push("OPENCODE_SESSION_ID_OLD=decoy");
        entries.push("XCLAUDE_CODE_SESSION_ID=decoy");
        entries.push("NOTES=OPENCODE_SESSION_ID=decoy");
        assert_eq!(session_id_in_environ(&environ_blob(&entries)), None);
    }

    #[test]
    fn no_other_environment_content_is_returned() {
        let mut entries: Vec<&str> = SECRETS.to_vec();
        entries.push("OPENCODE_SESSION_ID=ses_real");
        entries.push("GITHUB_TOKEN=ghp_notreal");
        let extracted =
            session_id_in_environ(&environ_blob(&entries)).expect("session id is present");
        assert_eq!(extracted, "ses_real");
        for secret in SECRETS.iter().chain(std::iter::once(&"ghp_notreal")) {
            assert!(
                !extracted.contains(secret),
                "extracted value leaked {secret}"
            );
        }
    }

    #[test]
    fn an_environment_without_a_session_variable_yields_nothing() {
        assert_eq!(session_id_in_environ(&environ_blob(&SECRETS)), None);
        assert_eq!(session_id_in_environ(&[]), None);
    }

    #[test]
    fn an_exported_but_empty_session_variable_is_not_a_session() {
        let raw = environ_blob(&["OPENCODE_SESSION_ID=", "CLAUDE_CODE_SESSION_ID="]);
        assert_eq!(session_id_in_environ(&raw), None);
    }

    #[test]
    fn a_session_id_on_the_pane_process_itself_is_found() {
        let tree = FakeTree::new().with_environ(500, &["OPENCODE_SESSION_ID=ses_root"]);
        assert_eq!(
            resolve_pane_session(&tree, 500),
            Some("ses_root".to_string())
        );
    }

    #[test]
    fn a_session_id_on_a_descendant_is_found() {
        // The measured shape on this machine: the pane's shell at depth 0, the
        // harness three generations below it.
        let tree = chain(64_344, 3, "ses_0210f2ed2ffedhF4");
        assert_eq!(
            resolve_pane_session(&tree, 64_344),
            Some("ses_0210f2ed2ffedhF4".to_string())
        );
    }

    #[test]
    fn a_pane_running_no_agent_yields_no_session() {
        let tree = FakeTree::new()
            .with_children(600, &[601])
            .with_environ(600, &SECRETS)
            .with_environ(601, &SECRETS);
        assert_eq!(resolve_pane_session(&tree, 600), None);
    }

    #[test]
    fn an_unreadable_process_does_not_abort_the_walk() {
        // 701 has no environ entry at all, modelling permission denied or a
        // process that exited mid-walk. The sibling behind it must still be seen.
        let tree = FakeTree::new()
            .with_children(700, &[701, 702])
            .with_environ(700, &SECRETS)
            .with_environ(702, &["OPENCODE_SESSION_ID=ses_sibling"]);
        assert_eq!(
            resolve_pane_session(&tree, 700),
            Some("ses_sibling".to_string())
        );
    }

    #[test]
    fn an_unreadable_pane_process_yields_no_session() {
        let tree = FakeTree::new();
        assert_eq!(resolve_pane_session(&tree, 999), None);
    }

    #[test]
    fn the_walk_examines_no_more_than_max_processes() {
        let children: Vec<u32> = (1..=200).map(|child| 1_000 + child).collect();
        let mut tree = FakeTree::new()
            .with_children(1_000, &children)
            .with_environ(1_000, &SECRETS);
        for child in &children {
            tree = tree.with_environ(*child, &SECRETS);
        }
        // Beyond the budget, so it must not be reached.
        tree = tree.with_environ(1_200, &["OPENCODE_SESSION_ID=ses_beyond_budget"]);

        assert_eq!(resolve_pane_session(&tree, 1_000), None);
        assert_eq!(tree.examined_count(), MAX_PROCESSES);
    }

    #[test]
    fn a_session_id_at_the_depth_bound_is_still_found() {
        let tree = chain(2_000, MAX_DEPTH, "ses_at_bound");
        assert_eq!(
            resolve_pane_session(&tree, 2_000),
            Some("ses_at_bound".to_string())
        );
    }

    #[test]
    fn a_session_id_below_the_depth_bound_is_not_reached() {
        let tree = chain(3_000, MAX_DEPTH + 1, "ses_too_deep");
        assert_eq!(resolve_pane_session(&tree, 3_000), None);
    }

    #[test]
    fn a_cycle_in_the_reported_tree_terminates() {
        let tree = FakeTree::new()
            .with_children(4_000, &[4_001])
            .with_children(4_001, &[4_000, 4_001])
            .with_environ(4_000, &SECRETS)
            .with_environ(4_001, &SECRETS);
        assert_eq!(resolve_pane_session(&tree, 4_000), None);
        assert_eq!(tree.examined_count(), 2);
    }

    /// Spawns a `sleep` carrying `session_id`, returning it and its pid.
    ///
    /// Cargo runs each test on a spawned thread, so this child is forked from a
    /// **non-main thread** — exactly as the agent harnesses fork theirs.
    #[cfg(target_os = "linux")]
    fn spawn_marked_child(session_id: &str) -> (std::process::Child, u32) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .env("OPENCODE_SESSION_ID", session_id)
            .spawn()
            .expect("sleep spawns");
        let pid = child.id();
        (child, pid)
    }

    /// Pins the reason [`ProcFs::children`] iterates `task/*` rather than reading
    /// the main thread's file alone.
    ///
    /// A child is listed under the thread that forked it, and the harnesses fork
    /// from worker threads. Measured across every live pane on this machine, the
    /// main-thread-only read found **0** session ids where the all-threads read
    /// found the agent, so this is a correctness property and not thoroughness.
    ///
    /// Reads no environment at all, so it is immune to whatever session this test
    /// run itself inherited.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_forked_from_a_non_main_thread_is_listed() {
        let (mut child, pid) = spawn_marked_child("ses_listed");
        let listed = ProcFs.children(std::process::id());
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            listed.contains(&pid),
            "child {pid} forked from a non-main thread was not listed in {listed:?}"
        );
    }

    /// Pins [`ProcFs::environ`] against a real process, through the same
    /// extraction the walk uses.
    ///
    /// The retry loop covers the gap between `fork` returning and `exec` replacing
    /// the child's image: until it does, `/proc/<pid>/environ` still shows this
    /// process's own environment.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_process_environment_is_read_through_proc() {
        let (mut child, pid) = spawn_marked_child("ses_real_proc");

        let mut found = None;
        for _ in 0..50 {
            found = ProcFs
                .environ(pid)
                .as_deref()
                .and_then(session_id_in_environ);
            if found.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(found, Some("ses_real_proc".to_string()));
    }
}
