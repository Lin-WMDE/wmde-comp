// SPDX-License-Identifier: GPL-3.0-only

//! `fun.wmde.Comp.Windows` - the open windows together with the pid that owns each one.
//!
//! A Wayland client cannot learn another client's pid, and the toplevel-info protocol carries
//! no pid field: the `Window` trait in `crate::wayland::protocols::toplevel_info` hands out
//! `title()` and `app_id()` and nothing else. Extending the protocol would mean forking
//! `pop-os/cosmic-protocols`, which is pulled unpatched, so the pid is published here instead.
//!
//! The zbus interface runs off the compositor's thread, so it never reaches into `Shell`: the
//! main loop writes a snapshot into the shared vector from `Common::refresh`, and the
//! interface only clones it.

use std::sync::{Arc, Mutex};

use smithay::{
    reexports::wayland_server::{DisplayHandle, Resource},
    utils::user_data::UserDataMap,
    wayland::seat::WaylandFocus,
};
use zbus::names::{UniqueName, WellKnownName};
use zbus::object_server::SignalEmitter;

use super::name_owners::NameOwners;
use crate::shell::CosmicSurface;
use crate::state::State;
use crate::wayland::protocols::toplevel_info::ToplevelInfoState;

const PATH: &str = "/fun/wmde/Comp/Windows";

/// Window titles name the documents, addresses and conversations the user has open, and the
/// pid is what a caller needs to kill a process. Both stay behind the same owner check the
/// other interfaces use.
static ALLOWED_NAMES: &[WellKnownName] = &[WellKnownName::from_static_str_unchecked(
    "fun.wmde.TaskManager",
)];

/// One row of the published list: pid, app id, title, and whether the window holds focus.
pub type WindowRow = (u32, String, String, bool);

type WindowList = Arc<Mutex<Vec<WindowRow>>>;

/// The pid of a window's owner, cached in the window's own user data.
///
/// `Common::refresh` runs every turn of the main loop, so the lookup must not repeat: for a
/// Wayland window it is a `SO_PEERCRED` call, for an X11 one a round trip to Xwayland. Only a
/// successful lookup is stored, so a window that has not yet published a pid is retried
/// rather than remembered as unknown.
struct CachedPid(u32);

#[derive(Debug)]
pub struct WindowsState {
    executor: calloop::futures::Scheduler<()>,
    conn: zbus::Connection,
    windows: WindowList,
}

impl WindowsState {
    pub async fn new(
        conn: &zbus::Connection,
        name_owners: &NameOwners,
        executor: &calloop::futures::Scheduler<()>,
    ) -> zbus::Result<Self> {
        let windows: WindowList = Arc::new(Mutex::new(Vec::new()));

        let iface = Windows {
            windows: windows.clone(),
            name_owners: name_owners.clone(),
        };
        conn.object_server().at(PATH, iface).await?;
        // `fun.wmde.Comp` is already owned by `ei`; asking again is harmless.
        conn.request_name("fun.wmde.Comp").await?;

        Ok(Self {
            executor: executor.clone(),
            conn: conn.clone(),
            windows,
        })
    }

    /// Take a fresh snapshot of the open windows; signal only when it actually differs.
    pub fn refresh(&self, dh: &DisplayHandle, toplevels: &ToplevelInfoState<State, CosmicSurface>) {
        let next = toplevels
            .registered_toplevels()
            .filter_map(|window| {
                // `is_activated(false)` is the committed state; `true` would report what has
                // been sent but not yet acknowledged.
                pid_of(dh, window).map(|pid| {
                    (
                        pid,
                        window.app_id(),
                        window.title(),
                        window.is_activated(false),
                    )
                })
            })
            .collect::<Vec<_>>();

        {
            let mut current = self.windows.lock().unwrap();
            if *current == next {
                return;
            }
            *current = next;
        }

        let Ok(emitter) = SignalEmitter::new(&self.conn, PATH) else {
            return;
        };
        let future = Windows::changed(emitter);
        let _ = self.executor.schedule(async move {
            let _ = future.await;
        });
    }
}

/// The pid behind a window, cached on first success.
fn pid_of(dh: &DisplayHandle, window: &CosmicSurface) -> Option<u32> {
    if let Some(CachedPid(pid)) = window.user_data().get::<CachedPid>() {
        return Some(*pid);
    }

    let pid = if let Some(x11) = window.x11_surface() {
        // The RES extension answers with the pid the X server itself knows for the window's
        // client. `_NET_WM_PID` is the fallback because the client sets it by hand and may set
        // it wrong or not at all. `wl_surface().client()` is not an option here at all: every
        // X11 window shares one Wayland client, Xwayland, so it would name Xwayland's pid for
        // all of them.
        x11.get_client_pid().ok().or_else(|| x11.pid())
    } else {
        window
            .wl_surface()
            .and_then(|surface| surface.client())
            .and_then(|client| client.get_credentials(dh).ok())
            .and_then(|creds| u32::try_from(creds.pid).ok())
    };

    if let Some(pid) = pid {
        UserDataMap::insert_if_missing_threadsafe(window.user_data(), || CachedPid(pid));
    }
    pid
}

struct Windows {
    windows: WindowList,
    name_owners: NameOwners,
}

#[zbus::interface(name = "fun.wmde.Comp.Windows")]
impl Windows {
    /// Every open window: pid, app id, title, focused.
    async fn list(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<Vec<WindowRow>> {
        if let Some(sender) = header.sender() {
            self.check_sender_allowed(sender).await?;
        }
        Ok(self.windows.lock().unwrap().clone())
    }

    /// The list changed. Carries nothing: the caller re-reads it.
    #[zbus(signal)]
    async fn changed(ctx: SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Windows {
    async fn check_sender_allowed(&self, sender: &UniqueName<'_>) -> zbus::fdo::Result<()> {
        if self.name_owners.check_owner(sender, ALLOWED_NAMES).await {
            Ok(())
        } else {
            Err(zbus::fdo::Error::AccessDenied("Access denied".to_string()))
        }
    }
}
