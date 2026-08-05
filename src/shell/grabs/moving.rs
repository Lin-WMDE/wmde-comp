// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    backend::render::{
        BackdropShader, IndicatorShader, Key, Usage, cursor::CursorState, element::AsGlowRenderer,
    },
    shell::{
        CosmicMapped, CosmicSurface, Direction, ManagedLayer,
        element::snap_strip::{REVEAL_HEIGHT, SnapStrip},
        element::{CosmicMappedRenderElement, stack_hover::StackHover},
        focus::target::{KeyboardFocusTarget, PointerFocusTarget},
        layout::floating::{TiledCorners, snap::SNAP_LAYOUTS},
    },
    utils::prelude::*,
    wayland::protocols::toplevel_info::{toplevel_enter_output, toplevel_enter_workspace},
};

use calloop::LoopHandle;
use cosmic::theme::CosmicTheme;
use smallvec::SmallVec;
use smithay::{
    backend::{
        drm::DrmNode,
        input::ButtonState,
        renderer::{
            ImportAll, ImportMem,
            element::{RenderElement, utils::RescaleRenderElement},
        },
    },
    desktop::{WindowSurfaceType, layer_map_for_output, space::SpaceElement},
    input::{
        Seat,
        pointer::{
            AxisFrame, ButtonEvent, CursorIcon, GestureHoldBeginEvent, GestureHoldEndEvent,
            GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
            GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
            GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
            RelativeMotionEvent,
        },
        touch::{self, GrabStartData as TouchGrabStartData, TouchGrab, TouchInnerHandle},
    },
    output::Output,
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Scale},
};
use std::{
    collections::HashSet,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
use tracing::warn;

use super::{GrabStartData, ReleaseMode};

pub type SeatMoveGrabState = Mutex<Option<MoveGrabState>>;

// WMDE: a client-initiated move starts as a `DelayGrab`, which only installs the real move
// grab from an idle callback, so `SeatMoveGrabState` is still `None` while the first motion
// events of an ordinary titlebar drag are handled. This marker covers that window, so that
// `pointer_edge_remap_while_dragging` sees the whole drag; the reader is
// `State::pointer_edge_remap_allowed`. Begun in `grabs::MoveGrab::delayed`, ended in
// `MoveGrab::new` and when the `DelayGrab` drops.
//
// A named type rather than an alias for `AtomicBool`: seat user data is keyed by type, and
// `insert_if_missing` keeps whichever value got there first. An alias would share its key with
// any bare `AtomicBool` anyone else puts on the seat, and the loser of that race would read the
// other feature's flag with nothing to warn about it. `ResizeGrabMarker` is wrapped for the
// same reason.
// A counter rather than a flag: a second client move request can arrive while the previous
// `DelayGrab` is still installed, and smithay's grab overwrite drops the old grab AFTER the
// new one has already marked itself pending - a flag would be cleared by its predecessor's
// `Drop`. With a saturating counter the drop order is irrelevant: every `delayed` begins one
// pending drag, every promotion or teardown ends one, and the floor at zero keeps the
// promotion-then-drop pair of the same grab from underflowing.
#[derive(Debug, Default)]
pub struct SeatMovePendingState(AtomicUsize);

impl SeatMovePendingState {
    pub fn get(&self) -> bool {
        self.0.load(Ordering::SeqCst) > 0
    }

    pub fn begin(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    pub fn end(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
    }
}

const RESCALE_ANIMATION_DURATION: f64 = 150.0;

pub struct MoveGrabState {
    window: CosmicMapped,
    window_offset: Point<i32, Logical>,
    indicator_thickness: u8,
    start: Instant,
    previous: ManagedLayer,
    snapping_zone: Option<SnappingZone>,
    // WMDE: the layout strip shown while the pointer is at the top edge, and the cell it is
    // currently over. `snap_pick` takes precedence over `snapping_zone` on release.
    snap_strip: Option<SnapStrip>,
    snap_pick: Option<(usize, usize)>,
    stacking_indicator: Option<(StackHover, Point<i32, Logical>)>,
    location: Point<f64, Logical>,
    cursor_output: Output,
}

impl MoveGrabState {
    #[profiling::function]
    pub fn render<R>(
        &self,
        renderer: &mut R,
        output: &Output,
        theme: &CosmicTheme,
        scanout_node: Option<DrmNode>,
        push: &mut dyn FnMut(CosmicMappedRenderElement<R>),
    ) where
        R: AsGlowRenderer + ImportAll + ImportMem,
        R::TextureId: Send + Clone + 'static,
        CosmicMappedRenderElement<R>: RenderElement<R>,
    {
        let scale = if self.previous == ManagedLayer::Tiling {
            0.6 + ((1.0
                - (Instant::now().duration_since(self.start).as_millis() as f64
                    / RESCALE_ANIMATION_DURATION)
                    .min(1.0))
                * 0.4)
        } else {
            1.0
        };
        let alpha = if &self.cursor_output == output {
            1.0
        } else {
            0.4
        };

        let mut window_geo = self.window.geometry();
        window_geo.loc += self.location.to_i32_round() + self.window_offset;
        if output
            .geometry()
            .as_logical()
            .intersection(window_geo)
            .is_none()
        {
            return;
        }

        let output_scale: Scale<f64> = output.current_scale().fractional_scale().into();
        let scaling_offset =
            self.window_offset - self.window_offset.to_f64().upscale(scale).to_i32_round();
        let render_location = self.location.to_i32_round() - output.geometry().loc.as_logical()
            + self.window_offset
            - scaling_offset;

        // WMDE: the layout strip sits above the dragged window, like the Windows 11 flyout.
        if let Some(strip) = self.snap_strip.as_ref()
            && &self.cursor_output == output
        {
            strip.push_render_elements(
                renderer,
                strip.geometry().loc.to_physical_precise_round(output_scale),
                output_scale,
                1.0,
                &mut |elem| push(elem.into()),
            );
        }

        for (indicator, location) in self.stacking_indicator.iter() {
            indicator.push_render_elements(
                renderer,
                location.to_physical_precise_round(output_scale),
                output_scale,
                1.0,
                &mut |elem| push(elem.into()),
                None,
            );
        }

        self.window.push_popup_render_elements::<R>(
            renderer,
            (render_location - self.window.geometry().loc).to_physical_precise_round(output_scale),
            output_scale,
            alpha,
            scanout_node,
            push,
        );

        let active_window_hint = crate::theme::active_window_hint(theme);
        let radius = self
            .element()
            .corner_radius(window_geo.size, self.indicator_thickness);

        if self.indicator_thickness > 0 {
            push(
                IndicatorShader::focus_element(
                    renderer,
                    Key::Window(Usage::MoveGrabIndicator, self.window.key()),
                    Rectangle::new(
                        render_location,
                        self.window
                            .geometry()
                            .size
                            .to_f64()
                            .upscale(scale)
                            .to_i32_round(),
                    )
                    .as_local(),
                    self.indicator_thickness,
                    radius,
                    alpha,
                    output_scale.x,
                    [
                        active_window_hint.red,
                        active_window_hint.green,
                        active_window_hint.blue,
                    ],
                )
                .into(),
            )
        }

        let map_window_element = |elem| match elem {
            CosmicMappedRenderElement::Stack(stack) => {
                CosmicMappedRenderElement::GrabbedStack(RescaleRenderElement::from_element(
                    stack,
                    render_location
                        .to_physical_precise_round(output.current_scale().fractional_scale()),
                    scale,
                ))
            }
            CosmicMappedRenderElement::Window(window) => {
                CosmicMappedRenderElement::GrabbedWindow(RescaleRenderElement::from_element(
                    window,
                    render_location
                        .to_physical_precise_round(output.current_scale().fractional_scale()),
                    scale,
                ))
            }
            x => x,
        };

        let mut lower_elements = SmallVec::<[CosmicMappedRenderElement<R>; 4]>::new_const();
        self.window.push_render_elements(
            renderer,
            (render_location - self.window.geometry().loc).to_physical_precise_round(output_scale),
            None,
            output_scale,
            alpha,
            Some(false),
            scanout_node,
            &mut |elem| push(map_window_element(elem)),
            &mut |elem| lower_elements.push(map_window_element(elem)),
        );
        if let Some(shadow_element) = self.window.shadow_render_element(
            renderer,
            (render_location - self.window.geometry().loc).to_physical_precise_round(output_scale),
            None,
            output_scale,
            scale,
            alpha,
        ) {
            push(shadow_element);
        }
        for elem in lower_elements.into_iter() {
            push(elem);
        }

        let non_exclusive_geometry = {
            let layers = layer_map_for_output(output);
            layers.non_exclusive_zone()
        };

        let gaps = (theme.gaps.0 as i32, theme.gaps.1 as i32);
        let thickness = self.indicator_thickness.max(1);

        // WMDE: a cell picked from the strip previews as the rectangle it will land in, using
        // the same indicator the edge zones use, so the two read identically.
        let preview_geometry = self
            .snap_pick
            .and_then(|(l, c)| SNAP_LAYOUTS.get(l)?.cells.get(c))
            .map(|cell| cell.relative_geometry(non_exclusive_geometry, gaps))
            .or_else(|| {
                self.snapping_zone
                    .as_ref()
                    .map(|t| t.overlay_geometry(non_exclusive_geometry, gaps))
            });

        if let Some(overlay_geometry) = preview_geometry
            && &self.cursor_output == output
        {
            let base_color = theme.palette.neutral_9;

            push(
                IndicatorShader::element(
                    renderer,
                    Key::Window(Usage::SnappingIndicator, self.window.key()),
                    overlay_geometry,
                    thickness,
                    [
                        theme.radius_s()[0] as u8,
                        theme.radius_s()[1] as u8,
                        theme.radius_s()[2] as u8,
                        theme.radius_s()[3] as u8,
                    ],
                    1.0,
                    output_scale.x,
                    [
                        active_window_hint.red,
                        active_window_hint.green,
                        active_window_hint.blue,
                    ],
                )
                .into(),
            );
            push(
                BackdropShader::element(
                    renderer,
                    Key::Window(Usage::SnappingIndicator, self.window.key()),
                    overlay_geometry,
                    theme.radius_s()[0], // TODO: Fix once shaders support 4 corner radii customization
                    0.4,
                    [base_color.red, base_color.green, base_color.blue],
                )
                .into(),
            )
        }
    }

    pub fn element(&self) -> CosmicMapped {
        self.window.clone()
    }

    pub fn window(&self) -> CosmicSurface {
        self.window.active_window()
    }
}

struct NotSend<T>(pub T);
unsafe impl<T> Send for NotSend<T> {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnappingZone {
    Maximize,
    Top,
    TopLeft,
    Left,
    BottomLeft,
    Bottom,
    BottomRight,
    Right,
    TopRight,
}

const SNAP_RANGE: i32 = 32;
const SNAP_RANGE_MAXIMIZE: i32 = 22;
const SNAP_RANGE_TOP: i32 = 16;

impl SnappingZone {
    pub fn contains(
        &self,
        point: Point<i32, Local>,
        output_geometry: Rectangle<i32, Local>,
    ) -> bool {
        if !output_geometry.contains(point) {
            return false;
        }
        let top_zone_32 = point.y < output_geometry.loc.y + SNAP_RANGE_MAXIMIZE;
        let top_zone_56 = point.y < output_geometry.loc.y + SNAP_RANGE_MAXIMIZE + SNAP_RANGE_TOP;
        let left_zone = point.x < output_geometry.loc.x + SNAP_RANGE;
        let right_zone = point.x > output_geometry.loc.x + output_geometry.size.w - SNAP_RANGE;
        let bottom_zone = point.y > output_geometry.loc.y + output_geometry.size.h - SNAP_RANGE;
        let left_6th = point.x < output_geometry.loc.x + (output_geometry.size.w / 6);
        let right_6th = point.x > output_geometry.loc.x + (output_geometry.size.w * 5 / 6);
        let top_4th = point.y < output_geometry.loc.y + (output_geometry.size.h / 4);
        let bottom_4th = point.y > output_geometry.loc.y + (output_geometry.size.h * 3 / 4);
        match self {
            SnappingZone::Maximize => top_zone_32 && !left_6th && !right_6th,
            SnappingZone::Top => top_zone_56 && !top_zone_32 && !left_6th && !right_6th,
            SnappingZone::TopLeft => (top_zone_56 && left_6th) || (left_zone && top_4th),
            SnappingZone::Left => left_zone && !top_4th && !bottom_4th,
            SnappingZone::BottomLeft => (bottom_zone && left_6th) || (left_zone && bottom_4th),
            SnappingZone::Bottom => bottom_zone && !left_6th && !right_6th,
            SnappingZone::BottomRight => (bottom_zone && right_6th) || (right_zone && bottom_4th),
            SnappingZone::Right => right_zone && !top_4th && !bottom_4th,
            SnappingZone::TopRight => (top_zone_56 && right_6th) || (right_zone && top_4th),
        }
    }
    pub fn overlay_geometry(
        &self,
        non_exclusive_geometry: Rectangle<i32, Logical>,
        gaps: (i32, i32),
    ) -> Rectangle<i32, Local> {
        match self {
            SnappingZone::Maximize => non_exclusive_geometry.as_local(),
            SnappingZone::Top => TiledCorners::Top.relative_geometry(non_exclusive_geometry, gaps),
            SnappingZone::TopLeft => {
                TiledCorners::TopLeft.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::Left => {
                TiledCorners::Left.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::BottomLeft => {
                TiledCorners::BottomLeft.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::Bottom => {
                TiledCorners::Bottom.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::BottomRight => {
                TiledCorners::BottomRight.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::Right => {
                TiledCorners::Right.relative_geometry(non_exclusive_geometry, gaps)
            }
            SnappingZone::TopRight => {
                TiledCorners::TopRight.relative_geometry(non_exclusive_geometry, gaps)
            }
        }
    }
}

pub struct MoveGrab {
    window: CosmicMapped,
    start_data: GrabStartData,
    seat: Seat<State>,
    cursor_output: Output,
    window_outputs: HashSet<Output>,
    previous: ManagedLayer,
    release: ReleaseMode,
    edge_snap_threshold: f64,
    // SAFETY: This is only used on drop which will always be on the main thread
    evlh: NotSend<LoopHandle<'static, State>>,
}

impl MoveGrab {
    fn update_location(&mut self, state: &mut State, location: Point<f64, Logical>) {
        let mut shell = state.common.shell.write();

        let Some(current_output) = shell
            .outputs()
            .find(|output| {
                output
                    .geometry()
                    .as_logical()
                    .overlaps_or_touches(Rectangle::new(location.to_i32_floor(), (0, 0).into()))
            })
            .cloned()
        else {
            return;
        };
        // WMDE: the crossing is remembered, because the layout strip is built for one output and
        // has to be dropped further down, where the grab state is borrowed.
        let output_changed = self.cursor_output != current_output;
        if output_changed {
            // WMDE: the output the drag came from can have been unplugged, the pointer landing
            // on a surviving one - `cleanup_tiling_drag` looks for the placeholder wherever
            // `remove_output` left it, and leaves the workspace the pointer is over now out of
            // its sweep. That one has to keep whatever it holds, because nothing here can put a
            // placeholder back: the drag's own one is made once by
            // `TilingLayout::unmap_as_placeholder` at grab start, and only
            // `Shell::update_pointer_position` ever moves or re-creates it. Of the input paths
            // that drive this grab only `InputEvent::PointerMotion` calls that - right after
            // this handler, in the same event - while `PointerMotionAbsolute` and `TouchMotion`
            // never do, so on a tablet or on an absolute VM pointer a swept workspace would stay
            // swept for the rest of the drag. Logged once per crossing, not per motion event:
            // `cursor_output` is updated right below, so the next crossing finds a live output.
            if cleanup_tiling_drag(&mut shell, &self.cursor_output, Some(&current_output)) {
                warn!(
                    "move grab: output {} is gone, swept the tiling drag it left behind",
                    self.cursor_output.name()
                );
            }
            self.cursor_output = current_output.clone();
        }

        let mut borrow = self
            .seat
            .user_data()
            .get::<SeatMoveGrabState>()
            .map(|s| s.lock().unwrap());
        if let Some(grab_state) = borrow.as_mut().and_then(|s| s.as_mut()) {
            grab_state.location = location;
            grab_state.cursor_output = self.cursor_output.clone();

            // WMDE: a `SnapStrip` freezes both its geometry and its cell hit table from the work
            // area of the output it was built for, so one carried across a crossing would be
            // drawn - and aimed at - in the coordinates of the output the drag came from.
            // Dropping it is enough, exactly as when the pointer leaves the strip below: the
            // element owns its per-output buffers and nothing outside it refers to it. The
            // reveal branch below rebuilds it from the new work area on this very motion, and
            // `snap_pick` indexes into the strip that just went away.
            if output_changed {
                grab_state.snap_strip = None;
                grab_state.snap_pick = None;
            }

            let mut window_geo = self.window.geometry();
            window_geo.loc += location.to_i32_round() + grab_state.window_offset;

            if matches!(self.previous, ManagedLayer::Floating | ManagedLayer::Sticky) {
                let loc = grab_state.window_offset.to_f64() + grab_state.location;
                let size = window_geo.size.to_f64();
                let output_geom = self.cursor_output.geometry().to_f64().as_logical();
                let output_loc = output_geom.loc;
                let output_size = output_geom.size;

                grab_state.location.x = if (loc.x - output_loc.x).abs() < self.edge_snap_threshold {
                    output_loc.x - grab_state.window_offset.x as f64
                } else if ((loc.x + size.w) - (output_loc.x + output_size.w)).abs()
                    < self.edge_snap_threshold
                {
                    output_loc.x + output_size.w - grab_state.window_offset.x as f64 - size.w
                } else {
                    grab_state.location.x
                };
                grab_state.location.y = if (loc.y - output_loc.y).abs() < self.edge_snap_threshold {
                    output_loc.y - grab_state.window_offset.y as f64
                } else if ((loc.y + size.h) - (output_loc.y + output_size.h)).abs()
                    < self.edge_snap_threshold
                {
                    output_loc.y + output_size.h - grab_state.window_offset.y as f64 - size.h
                } else {
                    grab_state.location.y
                };
            }

            for output in shell.outputs() {
                if let Some(overlap) = output.geometry().as_logical().intersection(window_geo) {
                    if self.window_outputs.insert(output.clone()) {
                        self.window.output_enter(output, overlap);
                        if let Some(indicator) =
                            grab_state.stacking_indicator.as_ref().map(|x| &x.0)
                        {
                            indicator.output_enter(output);
                        }
                        if let Some(strip) = grab_state.snap_strip.as_ref() {
                            strip.output_enter(output);
                        }
                    }
                } else if self.window_outputs.remove(output) {
                    self.window.output_leave(output);
                    if let Some(indicator) = grab_state.stacking_indicator.as_ref().map(|x| &x.0) {
                        indicator.output_leave(output);
                    }
                    if let Some(strip) = grab_state.snap_strip.as_ref() {
                        strip.output_leave(output);
                    }
                }
            }

            // WMDE: while a strip cell is aimed at, that cell owns the drop (see `MoveGrab::drop`),
            // so the stack hover must not promise a merge as well - the strip hangs over the top
            // edge, which is exactly where a window snapped to the top keeps its tab row.
            // Suppressed here rather than by clearing `grab_state.stacking_indicator` afterwards,
            // because the hover it mirrors is not the grab's: `hovered_stack` is set only by
            // `FloatingLayout::update_pointer_position`, which of the input paths that drive this
            // grab only `InputEvent::PointerMotion` reaches, through
            // `Shell::update_pointer_position` right after this handler in the same event. So a
            // cleared indicator would meet an unchanged hover on the next relative motion and be
            // rebuilt - element, buffers and all - on every one of them, while on
            // `PointerMotionAbsolute` and `TouchMotion` nothing recomputes the hover at all and
            // whatever it holds would be left standing. Suppressing the source is right on both.
            // This reads the pick of the previous motion, one event of lag that no one can see,
            // and the drop itself never depends on it.
            let indicator_location = if grab_state.snap_pick.is_some() {
                None
            } else {
                shell.stacking_indicator(&current_output, self.previous)
            };
            if indicator_location.is_some() != grab_state.stacking_indicator.is_some() {
                grab_state.stacking_indicator = indicator_location.map(|geo| {
                    let size = geo.size.as_logical();
                    let element = StackHover::new(
                        state.common.event_loop_handle.clone(),
                        size,
                        state.common.theme.clone(),
                    );
                    for output in &self.window_outputs {
                        element.output_enter(output);
                    }
                    (element, geo.loc.as_logical())
                });
            }

            // WMDE: the layout strip. It drops down once the pointer reaches the top edge and
            // stays for as long as the pointer is over it, so it can be aimed at; leaving both
            // the reveal band and the strip itself puts the plain edge zones back in charge.
            if grab_state.previous == ManagedLayer::Floating {
                let local = location
                    .as_global()
                    .to_local(&current_output)
                    .to_i32_floor();
                let work_area = {
                    let layers = layer_map_for_output(&current_output);
                    layers.non_exclusive_zone()
                };
                let at_top = local.y < work_area.loc.y + REVEAL_HEIGHT;
                let over_strip = grab_state
                    .snap_strip
                    .as_ref()
                    .is_some_and(|s| s.geometry().contains(local.as_logical()));

                if at_top || over_strip {
                    if grab_state.snap_strip.is_none() {
                        let strip = SnapStrip::new(
                            state.common.event_loop_handle.clone(),
                            work_area,
                            state.common.theme.clone(),
                        );
                        for output in &self.window_outputs {
                            strip.output_enter(output);
                        }
                        grab_state.snap_strip = Some(strip);
                    }
                } else {
                    grab_state.snap_strip = None;
                }

                grab_state.snap_pick = grab_state
                    .snap_strip
                    .as_ref()
                    .and_then(|s| s.cell_at(local.as_logical()));
                if let Some(strip) = grab_state.snap_strip.as_ref() {
                    strip.set_hovered(grab_state.snap_pick);
                }
            } else {
                grab_state.snap_strip = None;
                grab_state.snap_pick = None;
            }

            // Check for overlapping with zones
            if grab_state.previous == ManagedLayer::Floating {
                let output_geometry = current_output.geometry().to_local(&current_output);
                grab_state.snapping_zone = [
                    SnappingZone::Maximize,
                    SnappingZone::Top,
                    SnappingZone::TopLeft,
                    SnappingZone::Left,
                    SnappingZone::BottomLeft,
                    SnappingZone::Bottom,
                    SnappingZone::BottomRight,
                    SnappingZone::Right,
                    SnappingZone::TopRight,
                ]
                .iter()
                .find(|&x| {
                    x.contains(
                        location
                            .as_global()
                            .to_local(&current_output)
                            .to_i32_floor(),
                        output_geometry,
                    )
                })
                .cloned();

                // While a cell is aimed at, the edge zones must not also claim the pointer -
                // the top of the screen is exactly where they overlap the strip.
                if grab_state.snap_pick.is_some() {
                    grab_state.snapping_zone = None;
                }
            }
        }
        drop(borrow);
    }
}

// WMDE: drop what a tiling drag left in the tree - the placeholder gap and the pill indicator -
// for the drag that was running on `output`. `dragging_on` is the output the drag goes on with,
// whose active workspace is kept out of the sweep below; `None` when the drag is over and there
// is nothing left to keep.
//
// The placeholder only ever sits in the active workspace of the output the pointer is on:
// `TilingLayout::unmap_as_placeholder` puts it where the drag started and
// `Shell::update_pointer_position` hands the location to that one workspace and `None` to all
// others. So keying by output is right as long as the output is still there. It can be unplugged
// mid-drag, and `Workspaces::remove_output` then does one of two things with its set: it parks
// the whole set in `backup_set` when it was the last output, which `Workspaces::active_mut`
// still answers from, or it moves the workspaces into the surviving output's set, where they
// keep the placeholder while no output key reaches them any more. The sweep covers that second
// case, and only it: in the first one `active_mut` answers from `backup_set` and the branch
// above cleans the parked set directly - which is just as well, because `Workspaces::spaces_mut`
// walks `sets` and never `backup_set`. `cleanup_drag` walks the tree and only pushes a new one
// where it actually removed something. Returns whether it had to sweep, so the caller can report
// the vanished output once instead of once per motion event.
fn cleanup_tiling_drag(shell: &mut Shell, output: &Output, dragging_on: Option<&Output>) -> bool {
    if let Some(workspace) = shell.workspaces.active_mut(output) {
        workspace.tiling_layer.cleanup_drag();
        false
    } else {
        let keep = dragging_on
            .and_then(|output| shell.active_space(output))
            .map(|workspace| workspace.handle);
        for workspace in shell.workspaces.spaces_mut() {
            if Some(workspace.handle) != keep {
                workspace.tiling_layer.cleanup_drag();
            }
        }
        true
    }
}

impl PointerGrab<State> for MoveGrab {
    fn motion(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        self.update_location(state, event.location);

        // While the grab is active, no client has pointer focus
        handle.motion(state, None, event);
        if !self.window.alive() {
            handle.unset_grab(self, state, event.serial, event.time, true);
        }
    }

    fn relative_motion(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.relative_motion(state, None, event);
    }

    fn button(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(state, event);
        match self.release {
            ReleaseMode::NoMouseButtons => {
                if handle.current_pressed().is_empty() {
                    handle.unset_grab(self, state, event.serial, event.time, true);
                }
            }
            ReleaseMode::Click => {
                if event.state == ButtonState::Pressed {
                    handle.unset_grab(self, state, event.serial, event.time, true);
                }
            }
        }
    }

    fn axis(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(state, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data)
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event)
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event)
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event)
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event)
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event)
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event)
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event)
    }

    fn start_data(&self) -> &PointerGrabStartData<State> {
        match &self.start_data {
            GrabStartData::Pointer(start_data) => start_data,
            _ => unreachable!(),
        }
    }

    fn unset(&mut self, _data: &mut State) {}
}

impl TouchGrab<State> for MoveGrab {
    fn down(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &touch::DownEvent,
    ) {
        handle.down(data, None, event)
    }

    fn up(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &touch::UpEvent,
    ) {
        if event.slot == <Self as TouchGrab<State>>::start_data(self).slot {
            handle.unset_grab(self, data);
        }

        handle.up(data, event);
    }

    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &touch::MotionEvent,
    ) {
        if event.slot == <Self as TouchGrab<State>>::start_data(self).slot {
            self.update_location(data, event.location);
        }

        handle.motion(data, None, event);
    }

    fn frame(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        handle.frame(data)
    }

    fn cancel(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &touch::ShapeEvent,
    ) {
        handle.shape(data, event)
    }

    fn orientation(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &touch::OrientationEvent,
    ) {
        handle.orientation(data, event)
    }

    fn start_data(&self) -> &TouchGrabStartData<State> {
        match &self.start_data {
            GrabStartData::Touch(start_data) => start_data,
            _ => unreachable!(),
        }
    }

    fn unset(&mut self, _data: &mut State) {}
}

impl MoveGrab {
    pub fn new(
        start_data: GrabStartData,
        window: CosmicMapped,
        seat: &Seat<State>,
        initial_window_location: Point<i32, Global>,
        cursor_output: Output,
        indicator_thickness: u8,
        edge_snap_threshold: f64,
        previous_layer: ManagedLayer,
        release: ReleaseMode,
        evlh: LoopHandle<'static, State>,
    ) -> MoveGrab {
        // false-positive: `Output`s hash is based on it's inner ptr
        #[allow(clippy::mutable_key_type)]
        let mut outputs = HashSet::new();
        outputs.insert(cursor_output.clone());
        window.output_enter(&cursor_output, window.geometry()); // not accurate but...
        window.moved_since_mapped.store(true, Ordering::SeqCst);

        let grab_state = MoveGrabState {
            window: window.clone(),
            window_offset: (initial_window_location
                - start_data.location().as_global().to_i32_round())
            .as_logical(),
            indicator_thickness,
            start: Instant::now(),
            snap_strip: None,
            snap_pick: None,
            stacking_indicator: None,
            snapping_zone: None,
            previous: previous_layer,
            location: start_data.location(),
            cursor_output: cursor_output.clone(),
        };

        *seat
            .user_data()
            .get::<SeatMoveGrabState>()
            .unwrap()
            .lock()
            .unwrap() = Some(grab_state);

        // WMDE: the delayed phase is over, `SeatMoveGrabState` answers for the drag from here on.
        if let Some(pending) = seat.user_data().get::<SeatMovePendingState>() {
            pending.end();
        }

        {
            let cursor_state = seat.user_data().get::<CursorState>().unwrap();
            cursor_state.lock().unwrap().set_shape(CursorIcon::Grabbing);
        }

        MoveGrab {
            window,
            start_data,
            seat: seat.clone(),
            cursor_output,
            window_outputs: outputs,
            previous: previous_layer,
            release,
            edge_snap_threshold,
            evlh: NotSend(evlh),
        }
    }

    pub fn is_tiling_grab(&self) -> bool {
        self.previous == ManagedLayer::Tiling
    }

    pub fn is_touch_grab(&self) -> bool {
        match self.start_data {
            GrabStartData::Touch(_) => true,
            GrabStartData::Pointer(_) => false,
        }
    }
}

impl Drop for MoveGrab {
    fn drop(&mut self) {
        // No more buttons are pressed, release the grab.
        let output = self.cursor_output.clone();
        let seat = self.seat.clone();
        // false-positive: `Output`s hash is based on it's inner ptr
        #[allow(clippy::mutable_key_type)]
        let window_outputs = self.window_outputs.drain().collect::<HashSet<_>>();
        let previous = self.previous;
        let window = self.window.clone();
        let is_touch_grab = matches!(self.start_data, GrabStartData::Touch(_));
        let cursor_output = self.cursor_output.clone();

        let _ = self.evlh.0.insert_idle(move |state| {
            let position: Option<(CosmicMapped, Point<i32, Global>)> = if let Some(grab_state) =
                seat.user_data()
                    .get::<SeatMoveGrabState>()
                    .and_then(|s| s.lock().unwrap().take())
            {
                let mut shell = state.common.shell.write();

                // WMDE: this runs from an idle callback, so the output the drag ended on can
                // already be unplugged: `Workspaces::remove_output` took its workspace set with
                // it and every `active_space` below would have `None` to unwrap. Fall back to
                // the output the seat was moved to, and if there is no workspace to be had there
                // either, leave the window unplaced - losing where it landed beats taking the
                // session down. Every `active_space` keyed by `output` below rests on this check.
                let (output, relocated) = if shell.active_space(&output).is_some() {
                    (Some(output), false)
                } else {
                    let fallback = seat.active_output();
                    warn!(
                        "move grab ended on output {}, which is gone; falling back to {}",
                        output.name(),
                        fallback.name()
                    );
                    (
                        shell.active_space(&fallback).is_some().then_some(fallback),
                        true,
                    )
                };
                let workspace_handle = output
                    .as_ref()
                    .and_then(|output| Some(shell.active_space(output)?.handle));

                if let Some(output) = output
                    && let Some(workspace_handle) = workspace_handle
                    && grab_state.window.alive()
                {
                    let mut window_location =
                        (grab_state.location.to_i32_round() + grab_state.window_offset).as_global();

                    // WMDE: `location` was tracked in the global coordinates of the output that
                    // is gone, a region the fallback output does not cover, and an explicit
                    // position is taken as given - `FloatingLayout::map_internal` only computes
                    // one when none is passed. Pull the window into the work area of the output
                    // it is actually landing on, or the drop puts it fully off-screen.
                    if relocated {
                        let work_area = {
                            let layers = layer_map_for_output(&output);
                            layers.non_exclusive_zone()
                        }
                        .as_local()
                        .to_global(&output);
                        let size = grab_state.window.geometry().size.as_global();
                        // The upper bound is floored at the lower one: a window larger than the
                        // work area pins to its top left corner instead of tripping `clamp`.
                        window_location.x = window_location.x.clamp(
                            work_area.loc.x,
                            (work_area.loc.x + work_area.size.w - size.w).max(work_area.loc.x),
                        );
                        window_location.y = window_location.y.clamp(
                            work_area.loc.y,
                            (work_area.loc.y + work_area.size.h - size.h).max(work_area.loc.y),
                        );
                    }

                    for old_output in window_outputs.iter().filter(|o| *o != &output) {
                        grab_state.window.output_leave(old_output);
                    }

                    for (window, _) in grab_state.window.windows() {
                        toplevel_enter_output(&window, &output);
                        if previous != ManagedLayer::Sticky {
                            toplevel_enter_workspace(&window, &workspace_handle);
                        }
                    }

                    match previous {
                        ManagedLayer::Sticky => {
                            grab_state.window.set_geometry(Rectangle::new(
                                window_location,
                                grab_state.window.geometry().size.as_global(),
                            ));
                            // WMDE: the sticky layer belongs to the output's own set, and the
                            // check above does not prove that set is still in `sets`: once the
                            // last output is gone `Workspaces::remove_output` parks the whole
                            // set - sticky layer, windows and all - in `backup_set`, and that is
                            // where `active_space` just answered from. So look the set up the
                            // way `Workspaces::active` does, `sets` first and `backup_set`
                            // after, and the window is dropped into a real sticky layer either
                            // way. `Shell::remap_unfullscreened_window` reaches it through the
                            // same fallback.
                            let workspaces = &mut shell.workspaces;
                            if let Some(set) = workspaces
                                .sets
                                .get_mut(&output)
                                .or(workspaces.backup_set.as_mut())
                            {
                                let (window, location) = set.sticky_layer.drop_window(
                                    grab_state.window,
                                    window_location.to_local(&output),
                                );

                                Some((window, location.to_global(&output)))
                            } else {
                                // Unreachable: the check above consulted the same two places in
                                // the same order and one of them answered.
                                None
                            }
                        }
                        ManagedLayer::Tiling
                            if shell.active_space(&output).unwrap().tiling_enabled =>
                        {
                            let (window, location) = shell
                                .active_space_mut(&output)
                                .unwrap()
                                .tiling_layer
                                .drop_window(grab_state.window);
                            Some((window, location.to_global(&output)))
                        }
                        _ => {
                            grab_state.window.set_geometry(Rectangle::new(
                                window_location,
                                grab_state.window.geometry().size.as_global(),
                            ));
                            let theme = shell.theme.clone();
                            let workspace = shell.active_space_mut(&output).unwrap();

                            // WMDE: a picked cell wins over a hovered stack. The strip hangs over
                            // the top edge, where a window snapped to the top keeps its tab row,
                            // so the hover would have `drop_window` merge the window into that
                            // stack and return the stack - and the snap below would then apply to
                            // the whole stack instead of the window being dragged. With the hover
                            // cleared `drop_window` maps and returns the dragged window itself.
                            if grab_state.snap_pick.is_some() {
                                workspace.floating_layer.update_pointer_position(None);
                            }

                            let (window, location) = workspace.floating_layer.drop_window(
                                grab_state.window,
                                window_location.to_local(&workspace.output),
                            );

                            if matches!(previous, ManagedLayer::Floating)
                                && let Some(cell) = grab_state
                                    .snap_pick
                                    .and_then(|(l, c)| SNAP_LAYOUTS.get(l)?.cells.get(c))
                            {
                                // `last_geometry` holds the pre-drag geometry (set in
                                // FloatingLayout::unmap); snap_to_cell must not lose it, or
                                // restore-to-floating forgets where the window was.
                                let pre_drag_geometry = *window.last_geometry.lock().unwrap();
                                workspace.floating_layer.snap_to_cell(&window, cell);
                                if let Some(geo) = pre_drag_geometry {
                                    *window.last_geometry.lock().unwrap() = Some(geo);
                                }
                            } else if matches!(previous, ManagedLayer::Floating)
                                && let Some(sz) = grab_state.snapping_zone
                            {
                                // `last_geometry` was set to the pre-drag geometry(in FloatingLayout::unmap).
                                // Snapshot it here and restore it after so "restore-to-floating" goes back to where the user had the window.
                                let pre_drag_geometry = *window.last_geometry.lock().unwrap();

                                if sz == SnappingZone::Maximize {
                                    shell.maximize_toggle(
                                        &window,
                                        &seat,
                                        &state.common.event_loop_handle,
                                    );
                                    if let Some(geo) = pre_drag_geometry
                                        && let Some(state) =
                                            window.maximized_state.lock().unwrap().as_mut()
                                    {
                                        state.original_geometry = geo;
                                    }
                                } else {
                                    let directions = match sz {
                                        SnappingZone::Maximize => vec![],
                                        SnappingZone::Top => vec![Direction::Up],
                                        SnappingZone::TopLeft => {
                                            vec![Direction::Up, Direction::Left]
                                        }
                                        SnappingZone::Left => vec![Direction::Left],
                                        SnappingZone::BottomLeft => {
                                            vec![Direction::Down, Direction::Left]
                                        }
                                        SnappingZone::Bottom => vec![Direction::Down],
                                        SnappingZone::BottomRight => {
                                            vec![Direction::Down, Direction::Right]
                                        }
                                        SnappingZone::Right => vec![Direction::Right],
                                        SnappingZone::TopRight => {
                                            vec![Direction::Up, Direction::Right]
                                        }
                                    };
                                    for direction in directions {
                                        workspace.floating_layer.move_element(
                                            direction,
                                            &seat,
                                            ManagedLayer::Floating,
                                            &theme,
                                            &window,
                                        );
                                    }
                                    if let Some(geo) = pre_drag_geometry {
                                        *window.last_geometry.lock().unwrap() = Some(geo);
                                    }
                                }
                            }
                            Some((window, location.to_global(&output)))
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let mut shell = state.common.shell.write();
            // WMDE: the output can have been unplugged during the drag, in which case the
            // workspace this cleans up lives somewhere else now - `cleanup_tiling_drag` finds
            // it. Nothing is kept back this time (`None`): the drag is over, the window has been
            // placed above, and `TilingLayout::drop_window` already dropped the placeholders of
            // the workspace it went into. Silent on purpose: whenever there was a window to
            // place, the fallback above has already reported the vanished output once.
            cleanup_tiling_drag(&mut shell, &cursor_output, None);
            shell.set_overview_mode(None, state.common.event_loop_handle.clone());
            // WMDE: read while the guard is here, because the focus calls below take the shell
            // lock themselves.
            let locked = shell.session_lock.is_some();
            drop(shell);

            {
                let cursor_state = seat.user_data().get::<CursorState>().unwrap();
                cursor_state.lock().unwrap().unset_shape();
            }

            // WMDE: the placement above always runs, the focus below only on an unlocked
            // session. `cancel_grabs` drops a running move grab as the session locks, and this
            // idle is what that drop queues - so without the check a drag that was cut short by
            // the lock would still hand the dragged window pointer focus (the `pointer.motion`
            // with a client target) and keyboard focus (`Shell::set_focus`, which has no lock
            // check of its own), behind the lock screen. Skipping it leaves the window mapped
            // where the drag left it and simply unfocused: focus stays where the lock put it,
            // and `Common::refresh_focus` keeps it there - `focus_target_is_valid` accepts
            // nothing but a lock surface while `session_lock` is set.
            if !locked && let Some((mapped, position)) = position {
                let serial = SERIAL_COUNTER.next_serial();
                if !is_touch_grab {
                    let pointer = seat.get_pointer().unwrap();
                    let current_location = pointer.current_location();

                    if let Some((target, offset)) = mapped.focus_under(
                        current_location - position.as_logical().to_f64(),
                        WindowSurfaceType::ALL,
                        &seat,
                    ) {
                        pointer.motion(
                            state,
                            Some((
                                target,
                                position.as_logical().to_f64() - window.geometry().loc.to_f64()
                                    + offset,
                            )),
                            &MotionEvent {
                                location: pointer.current_location(),
                                serial,
                                time: state.common.clock.now().as_millis(),
                            },
                        );
                    }
                }
                Shell::set_focus(
                    state,
                    Some(&KeyboardFocusTarget::from(mapped)),
                    &seat,
                    Some(serial),
                    false,
                )
            }
        });
    }
}
