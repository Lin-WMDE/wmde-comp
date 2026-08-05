use std::{collections::HashMap, os::fd::OwnedFd};

use anyhow::{Context, Result};
use calloop::{InsertError, LoopHandle, stream::StreamSource};
use logind_zbus::{
    manager::{InhibitType::HandleLidSwitch, ManagerProxy},
    session::SessionProxy,
};

use crate::{
    shell::SessionLock,
    state::{Common, State},
    wayland::handlers::session_lock::cancel_grabs,
};

pub fn inhibit_lid(common: &Common) -> Result<OwnedFd> {
    let fd = futures_executor::block_on(async {
        let conn = common.dbus_state.system_conn().await?;
        let manager = ManagerProxy::new(conn).await?;
        manager
            .inhibit(
                HandleLidSwitch,
                "wmde-comp",
                "External output connected",
                "block",
            )
            .await
    })?;

    Ok(fd.into())
}

pub fn lid_closed(common: &Common) -> Result<bool> {
    futures_executor::block_on(async {
        let conn = common.dbus_state.system_conn().await?;
        let manager = ManagerProxy::new(conn).await?;
        manager
            .lid_closed()
            .await
            .context("Failed to talk to logind")
    })
}

/// WMDE: last-resort screen lock.
///
/// Locking is normally a client's job: `loginctl lock-session` makes logind emit Lock, the
/// wmde-greeter locker hears it and drives ext-session-lock. If that client is missing or
/// dead, nothing at all used to happen - the user asked for a lock, got no error and no lock,
/// and their screen stayed open. That silence is the thing this closes.
///
/// So the compositor listens for the same signal and, only when no lock is held, locks the
/// session itself with no client attached. Everything needed for that already exists, and all
/// of it keys off `shell.session_lock`: `render_input_order` then yields `Stage::SessionLock`
/// and nothing else, so the output renders empty (`session_lock_elements` has no surface to
/// draw) and `State::surface_under`, which walks the same order, has nothing to hand pointer
/// input to; `update_focus_target` gives keyboard focus to a lock surface only, here to nothing
/// at all, and `focus_target_is_valid` rejects any other target `Common::refresh_focus` finds
/// still set; and `cancel_grabs` below drops the seat grabs, which sit in front of all of that.
///
/// It is a blank screen and nothing more - there is no client, so there is nothing to type a
/// password into. Getting back in means `loginctl unlock-session` from a VT. That is a poor
/// experience and it is meant to be: it only ever happens on a broken install, and a screen
/// that stays shut is the safer of the two failures.
pub async fn session_lock_task(
    conn: zbus::Connection,
    evlh: LoopHandle<'static, State>,
) -> Result<()> {
    let manager = ManagerProxy::new(&conn).await?;
    let path = manager
        .get_session_by_PID(std::process::id())
        .await
        .context("Failed to find our own logind session")?;
    let session = SessionProxy::builder(&conn)
        .path(path)?
        .build()
        .await
        .context("Failed to build a logind session proxy")?;

    let lock = session
        .receive_lock()
        .await
        .context("Failed to subscribe to the logind Lock signal")?;
    evlh.insert_source(StreamSource::new(lock).unwrap(), |_, _, state| {
        let mut shell = state.common.shell.write();
        // A lock client got there first (usually the same signal reaching both of us) - it
        // owns the screen, and it can put a password prompt on it. Nothing to do.
        if shell.session_lock.is_some() {
            return;
        }
        tracing::warn!(
            "logind asked to lock the session and no lock client answered - blanking the \
             outputs instead. There is no password prompt on a blank lock: use `loginctl \
             unlock-session` from a VT. Check that wmde-greeter is installed and running."
        );
        shell.session_lock = Some(SessionLock {
            ext_session_lock: None,
            surfaces: HashMap::new(),
        });
        for output in shell.outputs() {
            state.backend.schedule_render(output);
        }
        // The blank lock hides the windows but not the seats: a grab set before it would keep
        // acting on input from behind the blank screen. Same treatment as the real lock.
        std::mem::drop(shell);
        cancel_grabs(state);
    })
    .map_err(|InsertError { error, .. }| error)
    .context("Failed to add the logind Lock signal to the event loop")?;

    let unlock = session
        .receive_unlock()
        .await
        .context("Failed to subscribe to the logind Unlock signal")?;
    evlh.insert_source(StreamSource::new(unlock).unwrap(), |_, _, state| {
        let mut shell = state.common.shell.write();
        // Only ever release our OWN blank. A client's lock is the client's to unlock, after
        // it has authenticated somebody - releasing that here would turn `loginctl
        // unlock-session` into a way past the password prompt.
        if !shell
            .session_lock
            .as_ref()
            .is_some_and(|lock| lock.ext_session_lock.is_none())
        {
            return;
        }
        tracing::info!("logind asked to unlock the session - releasing the blank screen");
        shell.session_lock = None;
        for output in shell.outputs() {
            state.backend.schedule_render(output);
        }
    })
    .map_err(|InsertError { error, .. }| error)
    .context("Failed to add the logind Unlock signal to the event loop")?;

    Ok(())
}
