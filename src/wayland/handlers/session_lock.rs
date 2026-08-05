// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    shell::{SessionLock, focus::target::KeyboardFocusTarget},
    state::State,
    utils::prelude::*,
};
use smithay::{
    input::pointer::MotionEvent,
    output::Output,
    reexports::wayland_server::{Resource, protocol::wl_output::WlOutput},
    utils::{SERIAL_COUNTER, Size},
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};
use std::collections::HashMap;

/// WMDE: drop every seat grab as the session locks and recompute focus against the lock.
///
/// A grab sits in front of focus: while one is set, the seat routes pointer, touch and keyboard
/// events straight into the grab handler and the lock never gets a say. `focus_target_is_valid`
/// only constrains keyboard *focus*, and the filter in `backend::render::workspace_elements`
/// only stops the grab from being drawn - so without this an open window menu would still run
/// the item under the click, and a move grab would still drag and snap the window, both
/// invisibly, on lock-screen input.
///
/// Unsetting is the whole cleanup a grab needs. `unset_grab` calls `PointerGrab::unset` (where
/// `ResizeForkGrab` commits its tree) and then drops the grab, whose `Drop` finishes the job:
/// `MoveGrab::drop` queues an idle that drops the dragged window back into its layout, so the
/// window stays mapped where the drag left it instead of being stranded. That idle would also
/// hand the window pointer and keyboard focus, which is exactly what a locked session must not
/// get - it checks `shell.session_lock` itself and does the re-map only.
///
/// Two ordering rules this rests on. The shell lock must be released first - `ResizeForkGrab`
/// takes `shell.write()` from its `unset`, and unsetting under a guard would deadlock the
/// compositor. And `shell.session_lock` must already be set, because unsetting a pointer grab
/// restores the focus the pointer would have had without the grab - the one the last motion
/// computed, which predates the lock - so the recomputation below has to see the lock to send
/// the pointer anywhere but back onto that window.
pub fn cancel_grabs(state: &mut State) {
    let seats = state
        .common
        .shell
        .read()
        .seats
        .iter()
        .cloned()
        .collect::<Vec<_>>();

    for seat in seats {
        let serial = SERIAL_COUNTER.next_serial();
        let time = state.common.clock.now().as_millis();

        if let Some(keyboard) = seat.get_keyboard()
            && keyboard.is_grabbed()
        {
            keyboard.unset_grab(state);
        }
        if let Some(touch) = seat.get_touch()
            && touch.is_grabbed()
        {
            touch.unset_grab(state);
        }
        let Some(pointer) = seat.get_pointer() else {
            continue;
        };
        if pointer.is_grabbed() {
            pointer.unset_grab(state, serial, time);
        }

        // Either way - grab unset just now, or none to begin with - the pointer is left on
        // whatever the last motion put under it, which predates the lock. Recompute it against
        // the lock: the same thing `update_pointer_focus` does from `Common::refresh_focus`,
        // only without waiting for its next tick, so a click arriving before that tick cannot
        // reach the window underneath.
        let output = seat.active_output();
        let under = {
            let shell = state.common.shell.read();
            State::surface_under(pointer.current_location().as_global(), &output, &shell)
                .map(|(target, pos)| (target, pos.as_logical()))
        };
        if pointer.current_focus().as_ref() != under.as_ref().map(|(target, _)| target) {
            pointer.motion(
                state,
                under,
                &MotionEvent {
                    location: pointer.current_location(),
                    serial,
                    time,
                },
            );
        }

        // Keyboard focus needs the same and gets it from nowhere else here: unsetting a grab
        // does not touch it, so the pre-lock window keeps every keystroke until
        // `Common::refresh_focus` rejects it through `focus_target_is_valid` - a pass `refresh`
        // (src/lib.rs) runs at most once per 150 ms. Take it away now instead.
        //
        // The target is always `None` at this point: `lock` and the blank lock in
        // `dbus::logind` both install `SessionLock` with an empty `surfaces` map. Handing the
        // locker its focus is `new_surface`'s job - `refresh_focus` cannot do it, because with
        // no focus and an empty focus stack it treats the state as valid and returns before it
        // would call `update_focus_target`.
        //
        // `Shell::set_focus` takes `shell.write()` itself, so no guard may be held here. It
        // appends to the focus stack for window targets only, so clearing focus leaves the
        // stack untouched and unlock still refills from it. `update_cursor` stays false so
        // `cursor_follows_focus` cannot warp the pointer that was just recomputed above.
        Shell::set_focus(state, None, &seat, None, false);
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.common.session_lock_manager_state
    }

    fn lock(&mut self, locker: SessionLocker) {
        let mut shell = self.common.shell.write();

        // Reject lock if sesion lock exists and is still valid
        if let Some(session_lock) = shell.session_lock.as_ref()
            && let Some(ext_session_lock) = session_lock.ext_session_lock.as_ref()
            && self
                .common
                .display_handle
                .get_client(ext_session_lock.id())
                .is_ok()
        {
            return;
        }
        // A lock with no client is the compositor's own blank screen (see
        // dbus::logind::session_lock_task). A real lock client is allowed to take over from
        // it - that is the whole point of the blank being a stand-in.

        let ext_session_lock = locker.ext_session_lock().clone();
        locker.lock();
        shell.session_lock = Some(SessionLock {
            ext_session_lock: Some(ext_session_lock),
            surfaces: HashMap::new(),
        });

        for output in shell.outputs() {
            self.backend.schedule_render(output);
        }

        // WMDE: the lock is set; drop the guard and take the seats away from whatever was
        // holding them. See `cancel_grabs` for why that order is the only one that works.
        std::mem::drop(shell);
        cancel_grabs(self);
    }

    fn unlock(&mut self) {
        let mut shell = self.common.shell.write();
        shell.session_lock = None;

        for output in shell.outputs() {
            self.backend.schedule_render(output);
        }
    }

    fn new_surface(&mut self, lock_surface: LockSurface, wl_output: WlOutput) {
        let mut shell = self.common.shell.write();
        let mut seats = Vec::new();
        if let Some(session_lock) = &mut shell.session_lock
            && let Some(output) = Output::from_resource(&wl_output)
        {
            lock_surface.with_pending_state(|states| {
                let size = output.geometry().size;
                states.size = Some(Size::from((size.w as u32, size.h as u32)));
            });
            lock_surface.send_configure();
            session_lock
                .surfaces
                .insert(output.clone(), lock_surface.clone());

            // WMDE: hand this surface the keyboard, or the locker never receives a keystroke.
            // `cancel_grabs` cleared focus when the lock engaged, and `Common::refresh_focus`
            // does not recover from that: with no focus and an empty focus stack it treats the
            // state as valid and returns before it would ask `update_focus_target` for the
            // lock surface. A password field that swallows every key looks like a hung locker.
            seats = shell
                .seats
                .iter()
                .filter(|seat| seat.focused_or_active_output() == output)
                .cloned()
                .collect();
        }

        // `Shell::set_focus` takes `shell.write()` itself.
        std::mem::drop(shell);
        let target = KeyboardFocusTarget::from(lock_surface);
        for seat in seats {
            Shell::set_focus(self, Some(&target), &seat, None, false);
        }
    }
}
