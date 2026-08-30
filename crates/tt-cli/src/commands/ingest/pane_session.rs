//! Resolving a focused tmux pane to the agent session running inside it.
//!
//! A `tmux_pane_focus` event carries no title, no `window_app_id` and no session
//! id; the pane's process tree identifies the session through two channels:
//!
//! - **An environment variable.** `OpenCode` and Claude Code stamp their session
//!   id on their own process.
//! - **An open transcript.** omp stamps no variable; it holds its current
//!   session's transcript open, and the filename carries the session id
//!   (`<sessions_dir>/<cwd-slug>/<ISO-8601-timestamp>_<uuid>.jsonl`, the naming
//!   `tt_core::omp` derives ids from). `OMP_SESSION_ID` is deliberately not
//!   read: a runtime `process.env` mutation is visible only in child processes,
//!   where a long-lived child keeps a stale copy across a session switch.
//!
//! Either way this is an **identity**, not an inference: nothing here derives a
//! stream. It records which session the focused pane was running; the trusted
//! session→stream path does the rest. See root `AGENTS.md`, "A pane's process
//! tree is an identity" and "An identity channel is per-harness".

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variables an agent harness stamps on its own process.
/// A closed list, matched by entry name — see [`session_id_in_environ`].
const SESSION_ENV_NAMES: [&[u8]; 2] = [b"OPENCODE_SESSION_ID", b"CLAUDE_CODE_SESSION_ID"];

/// Processes whose environment may be read for one lookup.
const MAX_PROCESSES: usize = 64;

/// Generations walked below the pane's own process.
const MAX_DEPTH: u32 = 8;

/// File descriptors examined per process (a live omp process held 85).
const MAX_FDS: usize = 256;

/// The process facts the walk needs, injectable so it can be driven without
/// real processes.
pub trait ProcessSource {
    /// Direct children of `pid`. Empty when `pid` has none or cannot be read —
    /// the two are indistinguishable to the walk, and deliberately so.
    fn children(&self, pid: u32) -> Vec<u32>;

    /// `pid`'s raw NUL-separated environment block, or `None` when it cannot be
    /// read (permission denied, or the process exited mid-walk).
    fn environ(&self, pid: u32) -> Option<Vec<u8>>;

    /// Paths of `pid`'s open descriptors targeting a `.jsonl` file **held open
    /// for writing**. Writable is the contract: the harness writing its
    /// transcript is running that session; a reader (`less`, backup) is not.
    fn open_file_paths(&self, _pid: u32) -> Vec<PathBuf> {
        Vec::new()
    }
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

/// Extracts an omp session id from one process's open transcripts.
///
/// Only a `.jsonl` exactly two components below `sessions_dir` qualifies
/// (deeper files are subagent transcripts), and the uuid check mirrors
/// `tt_core::omp::fallback_session_id` (36 chars, four dashes), which also
/// rejects `/proc`'s ` (deleted)` suffix. Two different qualifying sessions in
/// one process are refused as ambiguous. Paths are dropped after matching.
fn omp_session_in_open_files(paths: &[PathBuf], omp_sessions_dir: &Path) -> Option<String> {
    let mut found: Option<&str> = None;
    for path in paths {
        let Ok(relative) = path.strip_prefix(omp_sessions_dir) else {
            continue;
        };
        if relative.components().count() != 2 {
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some((_, uuid)) = stem.rsplit_once('_') else {
            continue;
        };
        if uuid.len() != 36 || uuid.matches('-').count() != 4 {
            continue;
        }
        match found {
            Some(existing) if existing != uuid => return None,
            _ => found = Some(uuid),
        }
    }
    found.map(str::to_string)
}

/// Walks a pane's process tree breadth-first for the session running in it.
///
/// The whole bounded tree is visited: two *different* identities under one pane
/// refuse as ambiguous rather than resolve by visit order. Within one process
/// the environment outranks open files. The sessions dir is canonicalized
/// because `/proc/<pid>/fd` reports resolved targets. Bounded by
/// [`MAX_PROCESSES`], [`MAX_DEPTH`] and [`MAX_FDS`]; every failure is silent,
/// and a pane with no agent returns `None`.
pub fn resolve_pane_session<S: ProcessSource>(
    source: &S,
    pane_process_id: u32,
    omp_sessions_dir: &Path,
) -> Option<String> {
    let omp_sessions_dir =
        fs::canonicalize(omp_sessions_dir).unwrap_or_else(|_| omp_sessions_dir.to_path_buf());
    let mut queue = VecDeque::from([(pane_process_id, 0_u32)]);
    let mut seen = HashSet::from([pane_process_id]);
    let mut examined = 0_usize;
    let mut found: Option<String> = None;

    while let Some((pid, depth)) = queue.pop_front() {
        if examined >= MAX_PROCESSES {
            break;
        }
        examined += 1;

        let candidate = source
            .environ(pid)
            .as_deref()
            .and_then(session_id_in_environ)
            .or_else(|| omp_session_in_open_files(&source.open_file_paths(pid), &omp_sessions_dir));
        if let Some(candidate) = candidate {
            match &found {
                Some(existing) if *existing != candidate => return None,
                _ => found = Some(candidate),
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

    found
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

    /// Readlinks `/proc/<pid>/fd/*` (capped at [`MAX_FDS`]), keeping `.jsonl`
    /// targets whose descriptor is open for writing per `fdinfo` — read only
    /// for targets that pass the extension check, and failing closed when
    /// unreadable, so a transcript *reader* never becomes a pane identity.
    fn open_file_paths(&self, pid: u32) -> Vec<PathBuf> {
        let Ok(descriptors) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            return Vec::new();
        };
        descriptors
            .flatten()
            .take(MAX_FDS)
            .filter_map(|descriptor| {
                let target = fs::read_link(descriptor.path()).ok()?;
                if target.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                    return None;
                }
                let fd_name = descriptor.file_name();
                let fdinfo =
                    fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", fd_name.to_string_lossy()))
                        .ok()?;
                fdinfo_is_writable(&fdinfo).then_some(target)
            })
            .collect()
    }
}

/// Whether an `fdinfo` block says its descriptor is open for writing.
///
/// The `flags:` line carries the open flags in octal; `O_ACCMODE` (3) masks the
/// access mode, where `O_RDONLY` is 0. Unparseable input is not writable — the
/// caller uses this to exclude transcript readers, so it fails closed.
fn fdinfo_is_writable(fdinfo: &str) -> bool {
    fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("flags:"))
        .and_then(|value| u32::from_str_radix(value.trim(), 8).ok())
        .is_some_and(|flags| flags & 0o3 != 0)
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
    resolve_pane_session(&ProcFs, pane_process_id, &super::get_omp_sessions_dir())
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

    /// The sessions directory every walk test resolves against.
    fn sessions_dir() -> &'static Path {
        Path::new("/home/user/.omp/agent/sessions")
    }

    /// A top-level omp transcript path for `uuid` inside [`sessions_dir`].
    fn transcript(uuid: &str) -> PathBuf {
        sessions_dir().join(format!(
            "-Code-project/2026-08-30T16-55-24-400Z_{uuid}.jsonl"
        ))
    }

    /// A scripted process tree. A pid absent from `environs` models one whose
    /// environment cannot be read: permission denied, or the process exited
    /// mid-walk.
    struct FakeTree {
        children: HashMap<u32, Vec<u32>>,
        environs: HashMap<u32, Vec<u8>>,
        open_files: HashMap<u32, Vec<PathBuf>>,
        examined: RefCell<Vec<u32>>,
    }

    impl FakeTree {
        fn new() -> Self {
            Self {
                children: HashMap::new(),
                environs: HashMap::new(),
                open_files: HashMap::new(),
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

        fn with_open_files(mut self, pid: u32, paths: &[PathBuf]) -> Self {
            self.open_files.insert(pid, paths.to_vec());
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

        fn open_file_paths(&self, pid: u32) -> Vec<PathBuf> {
            self.open_files.get(&pid).cloned().unwrap_or_default()
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
            resolve_pane_session(&tree, 500, sessions_dir()),
            Some("ses_root".to_string())
        );
    }

    #[test]
    fn a_session_id_on_a_descendant_is_found() {
        // The measured shape on this machine: the pane's shell at depth 0, the
        // harness three generations below it.
        let tree = chain(64_344, 3, "ses_0210f2ed2ffedhF4");
        assert_eq!(
            resolve_pane_session(&tree, 64_344, sessions_dir()),
            Some("ses_0210f2ed2ffedhF4".to_string())
        );
    }

    #[test]
    fn a_pane_running_no_agent_yields_no_session() {
        let tree = FakeTree::new()
            .with_children(600, &[601])
            .with_environ(600, &SECRETS)
            .with_environ(601, &SECRETS);
        assert_eq!(resolve_pane_session(&tree, 600, sessions_dir()), None);
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
            resolve_pane_session(&tree, 700, sessions_dir()),
            Some("ses_sibling".to_string())
        );
    }

    #[test]
    fn an_unreadable_pane_process_yields_no_session() {
        let tree = FakeTree::new();
        assert_eq!(resolve_pane_session(&tree, 999, sessions_dir()), None);
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

        assert_eq!(resolve_pane_session(&tree, 1_000, sessions_dir()), None);
        assert_eq!(tree.examined_count(), MAX_PROCESSES);
    }

    #[test]
    fn a_session_id_at_the_depth_bound_is_still_found() {
        let tree = chain(2_000, MAX_DEPTH, "ses_at_bound");
        assert_eq!(
            resolve_pane_session(&tree, 2_000, sessions_dir()),
            Some("ses_at_bound".to_string())
        );
    }

    #[test]
    fn a_session_id_below_the_depth_bound_is_not_reached() {
        let tree = chain(3_000, MAX_DEPTH + 1, "ses_too_deep");
        assert_eq!(resolve_pane_session(&tree, 3_000, sessions_dir()), None);
    }

    #[test]
    fn a_cycle_in_the_reported_tree_terminates() {
        let tree = FakeTree::new()
            .with_children(4_000, &[4_001])
            .with_children(4_001, &[4_000, 4_001])
            .with_environ(4_000, &SECRETS)
            .with_environ(4_001, &SECRETS);
        assert_eq!(resolve_pane_session(&tree, 4_000, sessions_dir()), None);
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

    const OMP_UUID: &str = "01a05398-e5f0-7000-a23d-362dc282c9df";
    const OTHER_UUID: &str = "01a0448c-641d-7000-ad02-75ed95a69f0a";

    #[test]
    fn an_open_omp_transcript_identifies_the_session() {
        let tree = FakeTree::new()
            .with_environ(500, &SECRETS)
            .with_open_files(500, &[transcript(OMP_UUID)]);
        assert_eq!(
            resolve_pane_session(&tree, 500, sessions_dir()),
            Some(OMP_UUID.to_string())
        );
    }

    #[test]
    fn an_open_transcript_on_a_descendant_is_found() {
        // The measured shape: the pane's shell at depth 0, omp one generation below.
        let tree = FakeTree::new()
            .with_children(600, &[601])
            .with_environ(600, &SECRETS)
            .with_environ(601, &SECRETS)
            .with_open_files(601, &[transcript(OMP_UUID)]);
        assert_eq!(
            resolve_pane_session(&tree, 600, sessions_dir()),
            Some(OMP_UUID.to_string())
        );
    }

    #[test]
    fn a_stamped_variable_wins_over_an_open_transcript() {
        // Within one process the environment is the harness's own claim about
        // itself; an open file is one step more inferred, so it must not shadow.
        let tree = FakeTree::new()
            .with_environ(700, &["OPENCODE_SESSION_ID=ses_env"])
            .with_open_files(700, &[transcript(OMP_UUID)]);
        assert_eq!(
            resolve_pane_session(&tree, 700, sessions_dir()),
            Some("ses_env".to_string())
        );
    }

    #[test]
    fn a_subagent_transcript_is_not_an_identity() {
        // Three components below the sessions dir: a subagent transcript. Its stem
        // carries no session id, and the parent's own transcript is the identity.
        let subagent = sessions_dir().join("-Code-project/2026-08-30T16-55-24-400Z_x/Scout.jsonl");
        let tree = FakeTree::new()
            .with_environ(800, &SECRETS)
            .with_open_files(800, &[subagent]);
        assert_eq!(resolve_pane_session(&tree, 800, sessions_dir()), None);
    }

    #[test]
    fn files_that_are_not_top_level_transcripts_are_ignored() {
        let noise = [
            // Outside the sessions dir entirely, uuid-shaped name or not.
            PathBuf::from(format!(
                "/home/user/notes/2026-08-30T00-00-00-000Z_{OMP_UUID}.jsonl"
            )),
            PathBuf::from("/home/user/.omp/agent/agent.db"),
            // Right depth, wrong extension: the .log and .md omp writes beside
            // every transcript, and /proc's rendering of an unlinked transcript.
            sessions_dir().join(format!(
                "-Code-project/2026-08-30T00-00-00-000Z_{OMP_UUID}.log"
            )),
            sessions_dir().join(format!(
                "-Code-project/2026-08-30T00-00-00-000Z_{OMP_UUID}.jsonl (deleted)"
            )),
            // Right place and extension, stem naming no session.
            sessions_dir().join("-Code-project/notes.jsonl"),
            sessions_dir().join("-Code-project/2026-08-30T00-00-00-000Z_not-a-uuid.jsonl"),
        ];
        let tree = FakeTree::new()
            .with_environ(900, &SECRETS)
            .with_open_files(900, &noise);
        assert_eq!(resolve_pane_session(&tree, 900, sessions_dir()), None);
    }

    #[test]
    fn two_different_open_sessions_are_refused_as_ambiguous() {
        let tree = FakeTree::new()
            .with_environ(1_000, &SECRETS)
            .with_open_files(1_000, &[transcript(OMP_UUID), transcript(OTHER_UUID)]);
        assert_eq!(resolve_pane_session(&tree, 1_000, sessions_dir()), None);
    }

    #[test]
    fn the_same_transcript_open_twice_still_resolves() {
        let tree = FakeTree::new()
            .with_environ(1_100, &SECRETS)
            .with_open_files(1_100, &[transcript(OMP_UUID), transcript(OMP_UUID)]);
        assert_eq!(
            resolve_pane_session(&tree, 1_100, sessions_dir()),
            Some(OMP_UUID.to_string())
        );
    }

    #[test]
    fn two_agents_under_one_pane_are_refused_as_ambiguous() {
        // A backgrounded harness beside a foreground one: env var in one process,
        // an omp transcript in a sibling. BFS visit order is not an identity
        // signal, so neither candidate may win.
        let tree = FakeTree::new()
            .with_children(1_200, &[1_201, 1_202])
            .with_environ(1_200, &SECRETS)
            .with_environ(1_201, &["OPENCODE_SESSION_ID=ses_background"])
            .with_environ(1_202, &SECRETS)
            .with_open_files(1_202, &[transcript(OMP_UUID)]);
        assert_eq!(resolve_pane_session(&tree, 1_200, sessions_dir()), None);
    }

    #[test]
    fn the_same_session_seen_in_two_processes_still_resolves() {
        // A harness and its worker both carry the one identity: agreement is not
        // ambiguity.
        let tree = FakeTree::new()
            .with_children(1_300, &[1_301, 1_302])
            .with_environ(1_300, &SECRETS)
            .with_open_files(1_301, &[transcript(OMP_UUID)])
            .with_open_files(1_302, &[transcript(OMP_UUID)]);
        assert_eq!(
            resolve_pane_session(&tree, 1_300, sessions_dir()),
            Some(OMP_UUID.to_string())
        );
    }

    /// Pins [`ProcFs::open_file_paths`] against a real process: a transcript-shaped
    /// file this test holds open **for writing** appears among its own readlinked
    /// descriptors, and the same file held open read-only does not — a `less` or a
    /// backup job reading a transcript is not the harness writing it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_open_file_is_listed_through_proc_only_when_writable() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir
            .path()
            .join(format!("2026-08-30T16-55-24-400Z_{OMP_UUID}.jsonl"));
        let held_writable = std::fs::File::create(&path).expect("create transcript");

        let listed = ProcFs.open_file_paths(std::process::id());
        assert!(
            listed.contains(&path),
            "writable transcript {path:?} not among {listed:?}"
        );

        drop(held_writable);
        let _held_readonly = std::fs::File::open(&path).expect("reopen read-only");
        let listed = ProcFs.open_file_paths(std::process::id());
        assert!(
            !listed.contains(&path),
            "read-only transcript {path:?} must not be listed, got {listed:?}"
        );
    }

    #[test]
    fn fdinfo_flags_decide_writability() {
        assert!(fdinfo_is_writable(
            "pos:\t0\nflags:\t0100001\nmnt_id:\t29\n"
        )); // O_WRONLY
        assert!(fdinfo_is_writable("pos:\t0\nflags:\t02100002\n")); // O_RDWR | O_APPEND
        assert!(!fdinfo_is_writable("pos:\t0\nflags:\t0100000\n")); // O_RDONLY
        assert!(!fdinfo_is_writable("")); // unparseable fails closed
    }
}
