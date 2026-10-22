// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: the pointer while the window switcher is open. Hovering a thumbnail moves the
//! selection to it, a press on one switches to it, a press on its close button closes that
//! window, a press anywhere else closes the switcher.
//! No client gets pointer focus meanwhile.
//!
//! Pointer only, unlike the grabs `check_grab_preconditions` places by serial: the switcher is
//! opened from the keyboard, with no press or touch point to start a touch or tablet grab from.

use smithay::{
    backend::input::ButtonState,
    input::pointer::{
        AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
        GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab,
        PointerInnerHandle, RelativeMotionEvent,
    },
    utils::{Logical, Point},
};

use crate::{shell::focus::target::PointerFocusTarget, state::State};

use super::SwitcherOp;

pub struct SwitcherGrab {
    start_data: GrabStartData<State>,
    generation: u64,
    /// Set when the grab itself picked: its own `unset` must not cancel that pick.
    finishing: bool,
}

impl SwitcherGrab {
    pub fn new(start_data: GrabStartData<State>, generation: u64) -> SwitcherGrab {
        SwitcherGrab {
            start_data,
            generation,
            finishing: false,
        }
    }
}

impl PointerGrab<State> for SwitcherGrab {
    fn motion(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(state, None, event);
        if let Some(index) = state.switcher_cell_at(self.generation, event.location) {
            let seat = state.common.shell.read().seats.last_active().clone();
            state.switcher_op(SwitcherOp::Hover(index), &seat);
        }
    }

    fn relative_motion(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(state, None, event);
    }

    fn button(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        if event.state != ButtonState::Pressed {
            handle.button(state, event);
            return;
        }
        // The close button closes that window and keeps the switcher open.
        if let Some(index) = state.switcher_close_at(self.generation, handle.current_location()) {
            let seat = state.common.shell.read().seats.last_active().clone();
            state.switcher_op(SwitcherOp::Close(Some(index)), &seat);
            return;
        }
        let pick = state.switcher_cell_at(self.generation, handle.current_location());
        // On the panel of the Icons and List styles, a click between the cells does nothing.
        if pick.is_none() && state.switcher_in_panel(self.generation, handle.current_location()) {
            return;
        }
        // The session cannot be finished from in here - that unsets this very grab through
        // the outer handle, which is locked - so the pick runs once the event is through.
        self.finishing = true;
        let generation = self.generation;
        state
            .common
            .event_loop_handle
            .insert_idle(move |state| state.switcher_pick(generation, pick));
        handle.unset_grab(self, state, event.serial, event.time, true);
    }

    fn axis(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(state, details);
    }

    fn frame(&mut self, state: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(state)
    }

    fn gesture_swipe_begin(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(state, event)
    }

    fn gesture_swipe_update(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(state, event)
    }

    fn gesture_swipe_end(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(state, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(state, event)
    }

    fn gesture_pinch_update(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(state, event)
    }

    fn gesture_pinch_end(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(state, event)
    }

    fn gesture_hold_begin(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(state, event)
    }

    fn gesture_hold_end(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(state, event)
    }

    fn start_data(&self) -> &GrabStartData<State> {
        &self.start_data
    }

    /// Unset by someone else - a session lock, another grab - the switcher closes with it.
    /// From an idle: `unset` can run under the shell lock.
    fn unset(&mut self, state: &mut State) {
        if !self.finishing {
            let generation = self.generation;
            state
                .common
                .event_loop_handle
                .insert_idle(move |state| state.switcher_cancel(Some(generation)));
        }
    }
}
