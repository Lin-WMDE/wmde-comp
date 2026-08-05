use smithay::{
    input::{
        Seat, SeatHandler,
        pointer::{
            AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
            GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
            GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
            GrabStartData as PointerGrabStartData, MotionEvent as PointerMotionEvent, PointerGrab,
            PointerInnerHandle, RelativeMotionEvent,
        },
        touch::{
            DownEvent, GrabStartData as TouchGrabStartData, MotionEvent as TouchMotionEvent,
            OrientationEvent, ShapeEvent, TouchGrab, TouchInnerHandle, UpEvent,
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER, Serial},
};

use crate::state::State;

use super::{GrabStartData, SeatMovePendingState};

pub struct DelayGrab<G> {
    grab_factory: Option<Box<dyn FnOnce(&mut State) -> Option<(G, Focus)>>>,
    seat: Seat<State>,
    serial: Option<Serial>,
    start_data: GrabStartData,
}

unsafe impl<G> Send for DelayGrab<G> {}

impl<G> DelayGrab<G> {
    pub fn new(
        factory: impl FnOnce(&mut State) -> Option<(G, Focus)> + 'static,
        seat: Seat<State>,
        serial: Option<Serial>,
        start_data: GrabStartData,
    ) -> Self {
        DelayGrab {
            grab_factory: Some(Box::new(factory)),
            seat,
            serial,
            start_data,
        }
    }

    pub fn is_touch_grab(&self) -> bool {
        match self.start_data {
            GrabStartData::Touch(_) => true,
            GrabStartData::Pointer(_) => false,
        }
    }
}

// WMDE: ends the pending drag `MoveGrab::delayed` began, on every way this grab can go away -
// promoted to a real move grab, released without ever moving, cancelled, or displaced by a
// newer grab. `MoveGrab::new` already ended it by the time a promotion drops this grab; the
// counter saturates at zero, so the double end is harmless, see `SeatMovePendingState`.
impl<G> Drop for DelayGrab<G> {
    fn drop(&mut self) {
        if let Some(pending) = self.seat.user_data().get::<SeatMovePendingState>() {
            pending.end();
        }
    }
}

impl<G: PointerGrab<State>> PointerGrab<State> for DelayGrab<G> {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &PointerMotionEvent,
    ) {
        handle.motion(data, focus, event);

        let distance = self.start_data.distance(event.location);
        if distance >= 1.
            && let Some(factory) = self.grab_factory.take()
        {
            let serial = self.serial.unwrap_or(event.serial);
            let seat = self.seat.clone();
            data.common.event_loop_handle.insert_idle(move |data| {
                // WMDE: an event-loop turn passes between the motion that promoted the drag and
                // this callback, so both of these are checked here. A session locked in
                // between must not get a move grab installed behind the lock screen -
                // `cancel_grabs` runs at lock time and cannot reach a grab that does not exist
                // yet - and the seat can have lost its pointer capability in the meantime, which
                // used to be an `unwrap` on this path. Both are checked before `factory`, which
                // is `Shell::move_request`: skipping it leaves the window mapped where it is
                // instead of unmapping it for a drag that never starts.
                let locked = data.common.shell.read().session_lock.is_some();
                if locked {
                    return;
                }
                let Some(pointer) = seat.get_pointer() else {
                    return;
                };
                if let Some((grab, focus)) = factory(data) {
                    pointer.set_grab(data, grab, serial, focus);
                }
            });
        }
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &PointerGrabStartData<State> {
        match &self.start_data {
            GrabStartData::Pointer(start_data) => start_data,
            _ => unreachable!(),
        }
    }

    fn unset(&mut self, _data: &mut State) {}
}

impl<G: TouchGrab<State>> TouchGrab<State> for DelayGrab<G> {
    fn down(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &DownEvent,
    ) {
        handle.down(data, focus, event);
    }

    fn up(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>, event: &UpEvent) {
        handle.up(data, event);

        if event.slot == TouchGrab::start_data(self).slot {
            handle.unset_grab(self, data);
        }
    }

    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &TouchMotionEvent,
    ) {
        handle.motion(data, focus, event);

        let distance = self.start_data.distance(event.location);
        if distance >= 1.
            && let Some(factory) = self.grab_factory.take()
        {
            let seat = self.seat.clone();
            let serial = self.serial.unwrap_or_else(|| SERIAL_COUNTER.next_serial());
            data.common.event_loop_handle.insert_idle(move |data| {
                // WMDE: same late-callback checks as the pointer promotion above.
                let locked = data.common.shell.read().session_lock.is_some();
                if locked {
                    return;
                }
                let Some(touch) = seat.get_touch() else {
                    return;
                };
                if let Some((grab, _)) = factory(data) {
                    touch.set_grab(data, grab, serial);
                }
            });
        }
    }

    fn frame(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        handle.frame(data)
    }

    fn cancel(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        handle.cancel(data);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &ShapeEvent,
    ) {
        handle.shape(data, event)
    }

    fn orientation(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &OrientationEvent,
    ) {
        handle.orientation(data, event);
    }

    fn start_data(&self) -> &TouchGrabStartData<State> {
        match &self.start_data {
            GrabStartData::Touch(start_data) => start_data,
            _ => unreachable!(),
        }
    }

    fn unset(&mut self, _data: &mut State) {}
}
