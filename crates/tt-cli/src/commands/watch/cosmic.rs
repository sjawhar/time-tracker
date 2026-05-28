use anyhow::{Context, Result, bail};
use cctk::cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;
use cctk::sctk::{
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
};
use cctk::toplevel_info::{ToplevelInfoHandler, ToplevelInfoState};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, backend::ObjectId,
    globals::registry_queue_init, protocol::wl_output, protocol::wl_seat,
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1,
    idle_notify::v1::client::{ext_idle_notification_v1, ext_idle_notifier_v1},
};

use super::backend::{ActiveWindow, IdleState, Snapshot, WindowBackend};

const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 180;

pub struct CosmicBackend {
    event_queue: EventQueue<AppData>,
    app_data: AppData,
}

impl CosmicBackend {
    pub fn new(idle_timeout_secs: Option<u64>) -> Result<Self> {
        let timeout_secs = idle_timeout_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);
        let timeout_ms = timeout_secs
            .checked_mul(1_000)
            .and_then(|value| u32::try_from(value).ok())
            .context("--idle-timeout is too large to fit the Wayland idle protocol timeout")?;

        let conn = Connection::connect_to_env()
            .context("failed to connect to Wayland display from WAYLAND_DISPLAY")?;
        let (globals, mut event_queue) =
            registry_queue_init(&conn).context("failed to initialize Wayland registry")?;
        let qh = event_queue.handle();
        let registry_state = RegistryState::new(&globals);
        let output_state = OutputState::new(&globals, &qh);

        let toplevel_info_state = ToplevelInfoState::try_new(&registry_state, &qh).context(
            "COSMIC watcher requires ext-foreign-toplevel-list-v1; compositor did not advertise it",
        )?;
        if toplevel_info_state.cosmic_toplevel_info.is_none() {
            bail!(
                "COSMIC watcher requires zcosmic_toplevel_info_v1; this does not look like a COSMIC Wayland session"
            );
        }

        let idle_notifier = globals
            .bind::<ext_idle_notifier_v1::ExtIdleNotifierV1, _, _>(&qh, 1..=2, cctk::GlobalData)
            .context(
                "COSMIC watcher requires ext-idle-notify-v1; compositor did not advertise it",
            )?;
        let seat = globals
            .bind::<wl_seat::WlSeat, _, _>(&qh, 1..=8, cctk::GlobalData)
            .context("COSMIC watcher requires wl_seat; compositor did not advertise it")?;
        let idle_notification =
            idle_notifier.get_input_idle_notification(timeout_ms, &seat, &qh, IdleNotificationData);

        let mut app_data = AppData {
            registry_state,
            output_state,
            toplevel_info_state,
            sticky_active_toplevel: None,
            has_seen_activation: false,
            idle: false,
            idle_timeout_ms: i64::from(timeout_ms),
            _idle_notifier: idle_notifier,
            _seat: seat,
            _idle_notification: idle_notification,
        };

        for _ in 0..3 {
            event_queue
                .roundtrip(&mut app_data)
                .context("failed to read initial COSMIC Wayland state")?;
        }

        Ok(Self {
            event_queue,
            app_data,
        })
    }
}

impl WindowBackend for CosmicBackend {
    fn poll(&mut self) -> Result<Snapshot> {
        self.event_queue
            .roundtrip(&mut self.app_data)
            .context("failed to poll COSMIC Wayland state")?;

        let candidates = self
            .app_data
            .toplevel_info_state
            .toplevels()
            .map(|info| ActiveWindowCandidate {
                id: info.foreign_toplevel.id(),
                app_id: info.app_id.as_str(),
                title: info.title.as_str(),
                #[cfg(test)]
                is_activated: info
                    .state
                    .contains(&zcosmic_toplevel_handle_v1::State::Activated),
            })
            .collect::<Vec<_>>();
        let active = active_window_from_candidates(
            &candidates,
            self.app_data.sticky_active_toplevel.as_ref(),
            !self.app_data.has_seen_activation,
        );
        if matches!(
            active,
            Some(ActiveWindowSelection {
                reason: ActiveWindowSelectionReason::BootstrapFallback,
                ..
            })
        ) {
            tracing::debug!(
                "using first-listed COSMIC toplevel as bootstrap active-window fallback before any Activated state has been observed"
            );
        }
        let active = active.map(|selection| selection.window);
        let idle = if self.app_data.idle {
            IdleState::Idle {
                since_ms: self.app_data.idle_timeout_ms,
            }
        } else {
            IdleState::Active
        };

        Ok(Snapshot { active, idle })
    }
}

struct AppData {
    registry_state: RegistryState,
    output_state: OutputState,
    toplevel_info_state: ToplevelInfoState,
    sticky_active_toplevel: Option<ObjectId>,
    has_seen_activation: bool,
    idle: bool,
    idle_timeout_ms: i64,
    _idle_notifier: ext_idle_notifier_v1::ExtIdleNotifierV1,
    _seat: wl_seat::WlSeat,
    _idle_notification: ext_idle_notification_v1::ExtIdleNotificationV1,
}

impl AppData {
    fn remember_activated_toplevel(
        &mut self,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        let Some(info) = self.toplevel_info_state.info(toplevel) else {
            return;
        };
        if info
            .state
            .contains(&zcosmic_toplevel_handle_v1::State::Activated)
        {
            self.sticky_active_toplevel = sticky_active_after_event(
                self.sticky_active_toplevel.as_ref(),
                Some(&toplevel.id()),
            );
            self.has_seen_activation = true;
        }
    }

    fn clear_sticky_active_toplevel(
        &mut self,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        if self.sticky_active_toplevel.as_ref() == Some(&toplevel.id()) {
            self.sticky_active_toplevel = None;
        }
    }
}

struct ActiveWindowCandidate<'a, Id> {
    id: Id,
    app_id: &'a str,
    title: &'a str,
    #[cfg(test)]
    is_activated: bool,
}

struct ActiveWindowSelection {
    window: ActiveWindow,
    reason: ActiveWindowSelectionReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveWindowSelectionReason {
    Sticky,
    BootstrapFallback,
}

fn active_window_from_candidates<Id: PartialEq>(
    candidates: &[ActiveWindowCandidate<'_, Id>],
    sticky_active: Option<&Id>,
    allow_bootstrap_fallback: bool,
) -> Option<ActiveWindowSelection> {
    let sticky_candidate = sticky_active
        .and_then(|sticky| candidates.iter().find(|candidate| &candidate.id == sticky));
    let (candidate, reason) = sticky_candidate
        .map(|candidate| (candidate, ActiveWindowSelectionReason::Sticky))
        .or_else(|| {
            allow_bootstrap_fallback
                .then(|| candidates.first())
                .flatten()
                .map(|candidate| (candidate, ActiveWindowSelectionReason::BootstrapFallback))
        })?;

    Some(ActiveWindowSelection {
        window: ActiveWindow {
            app_id: candidate.app_id.to_string(),
            title: candidate.title.to_string(),
        },
        reason,
    })
}

fn sticky_active_after_event<Id: Clone>(
    current: Option<&Id>,
    activated: Option<&Id>,
) -> Option<Id> {
    activated.or(current).cloned()
}

#[cfg(test)]
fn activated_toplevel_from_candidates<Id: Clone>(
    candidates: &[ActiveWindowCandidate<'_, Id>],
) -> Option<Id> {
    candidates
        .iter()
        .find(|candidate| candidate.is_activated)
        .map(|candidate| candidate.id.clone())
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    cctk::sctk::registry_handlers!(OutputState);
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.remember_activated_toplevel(toplevel);
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.remember_activated_toplevel(toplevel);
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.clear_sticky_active_toplevel(toplevel);
    }
}

struct IdleNotificationData;

impl Dispatch<ext_idle_notifier_v1::ExtIdleNotifierV1, cctk::GlobalData> for AppData {
    fn event(
        _state: &mut Self,
        _proxy: &ext_idle_notifier_v1::ExtIdleNotifierV1,
        _event: ext_idle_notifier_v1::Event,
        _data: &cctk::GlobalData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, cctk::GlobalData> for AppData {
    fn event(
        _state: &mut Self,
        _proxy: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &cctk::GlobalData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, IdleNotificationData> for AppData {
    fn event(
        state: &mut Self,
        _proxy: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _data: &IdleNotificationData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.idle = true,
            ext_idle_notification_v1::Event::Resumed => state.idle = false,
            _ => unreachable!(),
        }
    }
}

cctk::sctk::delegate_output!(AppData);
cctk::sctk::delegate_registry!(AppData);
cctk::delegate_toplevel_info!(AppData);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_window_prefers_activated_candidate() {
        let candidates = [
            ActiveWindowCandidate {
                id: "first",
                app_id: "first",
                title: "First",
                is_activated: false,
            },
            ActiveWindowCandidate {
                id: "focused",
                app_id: "focused",
                title: "Focused",
                is_activated: true,
            },
        ];

        let sticky = activated_toplevel_from_candidates(&candidates);
        let active = active_window_from_candidates(&candidates, sticky.as_ref(), true)
            .expect("active window");

        assert_eq!(active.window.app_id, "focused");
        assert_eq!(active.window.title, "Focused");
        assert_eq!(active.reason, ActiveWindowSelectionReason::Sticky);
    }

    #[test]
    fn active_window_uses_first_candidate_only_before_activation_is_seen() {
        let candidates = [
            ActiveWindowCandidate {
                id: "first",
                app_id: "first",
                title: "First",
                is_activated: false,
            },
            ActiveWindowCandidate {
                id: "second",
                app_id: "second",
                title: "Second",
                is_activated: false,
            },
        ];

        let active = active_window_from_candidates(&candidates, None, true).expect("active window");

        assert_eq!(active.window.app_id, "first");
        assert_eq!(active.window.title, "First");
        assert_eq!(
            active.reason,
            ActiveWindowSelectionReason::BootstrapFallback
        );
    }

    #[test]
    fn active_window_retains_sticky_when_cosmic_omits_activation() {
        let candidates = [
            ActiveWindowCandidate {
                id: "first",
                app_id: "first",
                title: "First",
                is_activated: false,
            },
            ActiveWindowCandidate {
                id: "second",
                app_id: "second",
                title: "Second",
                is_activated: false,
            },
        ];

        let active = active_window_from_candidates(&candidates, Some(&"second"), false)
            .expect("active window");

        assert_eq!(active.window.app_id, "second");
        assert_eq!(active.window.title, "Second");
        assert_eq!(active.reason, ActiveWindowSelectionReason::Sticky);
    }

    #[test]
    fn active_window_never_uses_first_candidate_after_activation_was_seen() {
        let candidates = [ActiveWindowCandidate {
            id: "first",
            app_id: "first",
            title: "First",
            is_activated: false,
        }];

        let active = active_window_from_candidates(&candidates, None, false);

        assert!(active.is_none());
    }

    #[test]
    fn sticky_active_is_retained_until_different_toplevel_activates() {
        let candidates = [
            ActiveWindowCandidate {
                id: "first",
                app_id: "first",
                title: "First",
                is_activated: false,
            },
            ActiveWindowCandidate {
                id: "second",
                app_id: "second",
                title: "Second",
                is_activated: false,
            },
        ];

        let sticky = sticky_active_after_event(
            Some(&"second"),
            activated_toplevel_from_candidates(&candidates).as_ref(),
        );

        assert_eq!(sticky, Some("second"));
    }

    #[test]
    fn different_newly_activated_toplevel_replaces_sticky_active() {
        let candidates = [
            ActiveWindowCandidate {
                id: "first",
                app_id: "first",
                title: "First",
                is_activated: false,
            },
            ActiveWindowCandidate {
                id: "second",
                app_id: "second",
                title: "Second",
                is_activated: true,
            },
        ];

        let sticky = sticky_active_after_event(
            Some(&"first"),
            activated_toplevel_from_candidates(&candidates).as_ref(),
        );

        assert_eq!(sticky, Some("second"));
    }
}
