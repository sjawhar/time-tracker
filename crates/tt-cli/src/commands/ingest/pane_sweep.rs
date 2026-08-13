//! Periodic pane→session identity sweep.
//!
//! Capture walks a pane's process tree only at focus time — exactly when no tool
//! call is usually in flight in the pane just switched to — so most focus events
//! carry no identity: measured over ordinary use, 17 of 174 (~10%). The sweep walks
//! every live tmux pane on a timer instead, recording the same environment-variable
//! identity into `pane_session_bindings`, where the existing import fallback and
//! backfill already consume it. Identity, never inference: nothing here reads a
//! title or a cwd, and nothing here writes a stream.

use anyhow::Result;
use chrono::Utc;
use tt_db::Database;

use super::pane_session::{ProcFs, ProcessSource, resolve_pane_session};

/// What one sweep observed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PaneSweepOutcome {
    /// Panes enumerated from the tmux server.
    pub panes: usize,
    /// Panes whose process tree carried an agent session id.
    pub identified: usize,
    /// Bindings newly recorded (a repeat observation in the same second is ignored).
    pub recorded: u64,
}

/// Resolves the session identity of every listed pane.
///
/// The pure half, so tests can drive it with a fake process tree. A pane whose walk
/// finds nothing is skipped silently: a plain shell has no session, and per the
/// capture doctrine every failure leaves the world exactly as it was.
fn resolve_panes<S: ProcessSource>(source: &S, panes: &[(String, u32)]) -> Vec<(String, String)> {
    panes
        .iter()
        .filter_map(|(pane_id, pid)| {
            resolve_pane_session(source, *pid).map(|session| (pane_id.clone(), session))
        })
        .collect()
}

/// Lists the unix socket paths of every live tmux server on this machine.
///
/// The daemon runs under systemd without the shell's `TMUX_TMPDIR`, and this user's
/// tmux serves a non-default socket (`~/.tmux/sockets/...`), so a pathless `tmux`
/// connects to `/tmp/tmux-<uid>/default` and finds nothing — the sweep observed 0
/// of 8 live panes until it asked `/proc/net/unix`, which names every live socket
/// regardless of who exported what. A socket the sweep cannot use fails its own
/// probe below and is skipped silently.
fn tmux_sockets() -> Vec<String> {
    let Ok(net_unix) = std::fs::read_to_string("/proc/net/unix") else {
        return Vec::new();
    };
    let mut sockets: Vec<String> = net_unix
        .lines()
        .filter_map(|line| {
            let path = line.split_whitespace().last()?;
            (path.starts_with('/') && path.contains("tmux-")).then(|| path.to_string())
        })
        .collect();
    sockets.sort();
    sockets.dedup();
    sockets
}

/// Lists every pane on every live tmux server as `(pane_id, pane_pid)`.
///
/// Absent tmux, an unreachable server, and an unparseable line all degrade to an
/// empty list: the sweep is an enrichment, and a headless machine must not log an
/// error every thirty seconds for not running tmux. Pane ids are per-server, so two
/// servers can both hold a `%1` — the same ambiguity focus capture already has,
/// since the hook records `pane_id` with no server discriminator either.
fn list_tmux_panes() -> Vec<(String, u32)> {
    let sockets = tmux_sockets();
    let invocations: Vec<Vec<String>> = if sockets.is_empty() {
        // No socket discovered (or no /proc): let tmux resolve its default path.
        vec![vec![]]
    } else {
        sockets
            .into_iter()
            .map(|socket| vec!["-S".to_string(), socket])
            .collect()
    };
    let mut panes = Vec::new();
    for socket_args in invocations {
        let Ok(output) = std::process::Command::new("tmux")
            .args(&socket_args)
            .args(["list-panes", "-a", "-F", "#{pane_id} #{pane_pid}"])
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        panes.extend(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let (pane, pid) = line.trim().split_once(' ')?;
                    Some((pane.to_string(), pid.parse().ok()?))
                }),
        );
    }
    panes
}

/// Runs one sweep: enumerate live panes, walk each process tree, record what was
/// observed.
pub fn sweep_pane_sessions(db: &Database) -> Result<PaneSweepOutcome> {
    let identity = crate::machine::require_machine_identity()?;
    let panes = list_tmux_panes();
    let observations = resolve_panes(&ProcFs, &panes);
    let recorded =
        db.record_pane_session_bindings(&identity.machine_id, &observations, Utc::now())?;
    Ok(PaneSweepOutcome {
        panes: panes.len(),
        identified: observations.len(),
        recorded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A scripted process tree: children and environments by pid.
    struct FakeTree {
        children: HashMap<u32, Vec<u32>>,
        environs: HashMap<u32, Vec<u8>>,
    }

    impl FakeTree {
        fn new() -> Self {
            Self {
                children: HashMap::new(),
                environs: HashMap::new(),
            }
        }

        fn with_agent(mut self, pane_pid: u32, child: u32, session_id: &str) -> Self {
            self.children.insert(pane_pid, vec![child]);
            self.environs.insert(
                child,
                format!("HOME=/home/x\0OPENCODE_SESSION_ID={session_id}\0").into_bytes(),
            );
            self
        }

        fn with_shell(mut self, pane_pid: u32) -> Self {
            self.children.insert(pane_pid, Vec::new());
            self.environs
                .insert(pane_pid, b"HOME=/home/x\0SHELL=/bin/bash\0".to_vec());
            self
        }
    }

    impl ProcessSource for FakeTree {
        fn children(&self, pid: u32) -> Vec<u32> {
            self.children.get(&pid).cloned().unwrap_or_default()
        }

        fn environ(&self, pid: u32) -> Option<Vec<u8>> {
            self.environs.get(&pid).cloned()
        }
    }

    #[test]
    fn a_sweep_identifies_agent_panes_and_skips_plain_shells() {
        // Given: two panes running agents and one plain shell.
        let tree = FakeTree::new()
            .with_agent(100, 101, "ses_alpha")
            .with_agent(200, 201, "ses_beta")
            .with_shell(300);
        let panes = vec![
            ("%1".to_string(), 100),
            ("%2".to_string(), 200),
            ("%3".to_string(), 300),
        ];

        // When
        let observations = resolve_panes(&tree, &panes);

        // Then: the shells contribute nothing and the agents are paired to their panes.
        assert_eq!(
            observations,
            vec![
                ("%1".to_string(), "ses_alpha".to_string()),
                ("%2".to_string(), "ses_beta".to_string()),
            ]
        );
    }

    #[test]
    fn recorded_observations_stamp_a_later_sessionless_focus_event() {
        // Given: a sweep observed an agent in pane %5 a minute ago.
        let db = Database::open_in_memory().unwrap();
        let observed_at = Utc::now() - chrono::Duration::minutes(1);
        let recorded = db
            .record_pane_session_bindings(
                "m1",
                &[("%5".to_string(), "ses_gamma".to_string())],
                observed_at,
            )
            .unwrap();
        assert_eq!(recorded, 1);

        // When: a sessionless pane focus event arrives afterwards.
        let event = tt_db::StoredEvent {
            data: serde_json::Value::Null,
            id: "evt-focus".to_string(),
            timestamp: Utc::now(),
            event_type: tt_core::EventType::TmuxPaneFocus,
            source: "remote.tmux".to_string(),
            machine_id: Some("m1".to_string()),
            schema_version: 1,
            pane_id: Some("%5".to_string()),
            tmux_session: Some("dev".to_string()),
            window_index: None,
            cwd: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            action: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
            window_app_id: None,
            window_title: None,
        };
        db.insert_event(&event).unwrap();

        // Then: the import fallback hands it the swept identity.
        let events = db.get_events(None, None).unwrap();
        let stamped = events
            .iter()
            .find(|e| e.id == "evt-focus")
            .and_then(|e| e.session_id.as_deref());
        assert_eq!(stamped, Some("ses_gamma"));
    }
}
