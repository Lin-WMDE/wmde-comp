// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: the Alt+Tab window switcher, drawn by the compositor.
//!
//! The first NextWindow / PrevWindow press opens a session: the windows of the active workspace
//! on the output under the pointer, most recently used first and minimized ones last (where
//! minimizing puts a window in Windows), with the selection on the next or the previous one.
//! While the binding's modifiers stay held, its key and Shift with it move the selection and
//! nothing changes focus; letting go of them switches to the selected window, restoring it if
//! it was minimized. A binding without modifiers opens a session that stays until Enter, a
//! click or Esc.
//!
//! Delete, or the close button the selected thumbnail carries, closes that window and leaves the
//! switcher open.
//!
//! A window with a modal dialog gets no cell of its own: the dialog's cell stands for both, since
//! focusing the window would land on the dialog anyway (`Shell::set_focus`). The cell shows the
//! window with the dialog over it and carries the window's icon and title; picking and closing
//! act on the dialog. When the dialog goes away while the switcher is open, the window takes its
//! cell.
//!
//! The switcher shows only after [`SHOW_DELAY`]: a quick Alt+Tab switches to the previous
//! window without drawing anything. On screen it is a grid of live thumbnails over the blurred
//! and dimmed desktop - wallpaper and desktop icons, without windows, panels or overlays (see
//! `Stage::Switcher` in `shell::focus::order`).
//!
//! With `animate`, opening moves every window from its place on screen into its thumbnail
//! (minimized ones fade in), and closing moves them back, the picked one on top. Picking a
//! minimized window skips the closing half: the window has its own restore animation.
//!
//! The `None` style keeps the switcher off the screen: each press focuses the next window right
//! away (`Shell::cycle_window`).

mod element;
mod grab;
pub mod layout;

use std::{
    fmt,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use calloop::{
    LoopHandle, RegistrationToken,
    timer::{TimeoutAction, Timer},
};
use cosmic_comp_config::WindowSwitcherStyle;
use cosmic_settings_config::shortcuts;
use keyframe::{ease, functions::EaseInOutCubic};
use smithay::{
    backend::{
        input::{InputTime, KeyState},
        renderer::{
            ImportAll, ImportMem,
            element::{Id, RenderElement},
        },
    },
    desktop::space::SpaceElement,
    input::{
        Seat,
        keyboard::{Keysym, ModifiersState},
        pointer::{Focus, GrabStartData},
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::seat::WaylandFocus,
};

use crate::{
    backend::render::{element::AsGlowRenderer, wayland::blur_effect::BlurState},
    shell::{
        CosmicMappedRenderElement, CosmicSurface, SeatExt, Shell,
        focus::{FocusTarget, target::KeyboardFocusTarget},
        workspace::MinimizedWindow,
    },
    state::State,
    utils::{prelude::*, tween::EaseRectangle},
    wayland::protocols::toplevel_management::ToplevelManagementHandler,
};

pub use self::element::SwitcherElement;
use self::{
    element::SwitcherCell,
    grab::SwitcherGrab,
    layout::{Dir, FamilyShape, GridLayout, GridParams},
};

/// How long the opening press has to be held before the switcher shows. A quicker Alt+Tab
/// switches to the previous window with nothing flashing on screen, as in Windows.
const SHOW_DELAY: Duration = Duration::from_millis(100);
/// Windows into their thumbnails, and back.
const OPEN_DURATION: Duration = Duration::from_millis(200);
const CLOSE_DURATION: Duration = Duration::from_millis(150);

/// Tells sessions apart in callbacks that outlive one: the show timer, the pointer grab.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitcherOp {
    Step {
        forward: bool,
    },
    Move(Dir),
    Hover(usize),
    Commit(Option<usize>),
    Cancel,
    /// Close the window of an entry, the selected one with `None`; the switcher stays open.
    Close(Option<usize>),
}

/// What an open switcher does with a key, decided before the global shortcuts see it.
pub enum SwitcherKey {
    /// A modifier: the client sees it as usual.
    Forward,
    /// The held modifiers were let go: switch. The release still reaches the client.
    Release(SwitcherOp),
    /// Any other key: it never reaches the client, and `None` does nothing with it.
    Press(Option<SwitcherOp>),
}

pub struct Session {
    generation: u64,
    seat: Seat<State>,
    output: Output,
    entries: Vec<FocusTarget>,
    /// The windows behind each entry, whose modal dialog it is, top-first: the one right under
    /// the dialog first, the root last. The cell draws them under the dialog and is named after
    /// the root; they take the entry's place when the dialog goes away.
    families: Vec<Vec<FocusTarget>>,
    selected: usize,
    /// What has to stay held, `None` for a session that stays open until Enter or Esc.
    hold: Option<shortcuts::Modifiers>,
    visible: bool,
    timer: Option<RegistrationToken>,
    blur_background: bool,
    animate: bool,
    /// Built when the switcher shows, rebuilt when a listed window goes away.
    view: Option<View>,
    /// Where the windows of each entry lie, output-local, taken when the switcher shows. The
    /// animation starts and ends at the bounds of one that is on screen.
    shapes: Vec<FamilyShape>,
    opened: Option<Instant>,
    /// Set once the session ended and only its closing animation runs, with the picked entry.
    closing: Option<(Instant, Option<usize>)>,
    blur: Mutex<BlurState>,
    dim: Id,
}

struct View {
    layout: GridLayout,
    cells: Vec<SwitcherCell>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("generation", &self.generation)
            .field("output", &self.output.name())
            .field("entries", &self.entries.len())
            .field("selected", &self.selected)
            .field("hold", &self.hold)
            .field("visible", &self.visible)
            .finish_non_exhaustive()
    }
}

/// The window a target stands for: the active tab of a stack.
fn surface_of(target: &FocusTarget) -> CosmicSurface {
    match target {
        FocusTarget::Window(mapped) => mapped.active_window(),
        FocusTarget::Fullscreen(surface) => surface.clone(),
    }
}

fn size_of(target: &FocusTarget) -> Size<i32, Logical> {
    match target {
        FocusTarget::Window(mapped) => mapped.geometry().size,
        FocusTarget::Fullscreen(surface) => SpaceElement::geometry(surface).size,
    }
}

/// Where the window of `target` is on `output`, output-local, if it is on screen at all.
fn origin_of(
    shell: &Shell,
    output: &Output,
    target: &FocusTarget,
) -> Option<Rectangle<i32, Logical>> {
    if target.is_minimized() {
        return None;
    }
    let set = shell.workspaces.sets.get(output)?;
    match target {
        FocusTarget::Fullscreen(_) => {
            Some(Rectangle::from_size(output.geometry().size.as_logical()))
        }
        FocusTarget::Window(mapped) => set
            .sticky_layer
            .element_geometry(mapped)
            .or_else(|| set.workspaces[set.active].element_geometry(mapped))
            .map(|geo| geo.as_logical()),
    }
}

/// `rect` scaled by `factor` about its centre.
fn shrunk(rect: Rectangle<i32, Logical>, factor: f64) -> Rectangle<i32, Logical> {
    let size = rect.size.to_f64().upscale(factor).to_i32_round();
    Rectangle::new(
        rect.loc + Point::from(((rect.size.w - size.w) / 2, (rect.size.h - size.h) / 2)),
        size,
    )
}

/// The modal dialog that takes the focus of `target` (`Shell::set_focus`), and its cell.
pub(super) fn dialog_of(shell: &Shell, target: &FocusTarget) -> Option<FocusTarget> {
    shell
        .resolve_modal_redirect(&target.clone().into())
        .map(FocusTarget::Window)
}

/// `targets` one family to a cell (`layout::modal_families`), each family top-first.
fn group(shell: &Shell, targets: Vec<FocusTarget>) -> Vec<(FocusTarget, Vec<FocusTarget>)> {
    layout::modal_families(targets, |t| dialog_of(shell, t))
        .into_iter()
        .map(|(head, behind)| {
            let behind = layout::modal_chain(&head, behind, |p, c| {
                surface_of(p).is_parent_of(&surface_of(c))
            });
            (head, behind)
        })
        .collect()
}

/// The window a cell is named after: the root of its family, or the entry itself.
fn root_of<'a>(target: &'a FocusTarget, family: &'a [FocusTarget]) -> &'a FocusTarget {
    family.last().unwrap_or(target)
}

/// Where `target` and the windows behind it lie on `output`.
fn shape_of(
    shell: &Shell,
    output: &Output,
    target: &FocusTarget,
    family: &[FocusTarget],
) -> FamilyShape {
    let placed = std::iter::once(target)
        .chain(family)
        .map(|t| (origin_of(shell, output, t), size_of(t)))
        .collect::<Vec<_>>();
    layout::family_shape(&placed)
}

fn add(entries: &mut Vec<FocusTarget>, target: FocusTarget) {
    // By the window and not the target: a window that went fullscreen is listed once.
    let surface = surface_of(&target);
    if target.alive() && !entries.iter().any(|t| surface_of(t) == surface) {
        entries.push(target);
    }
}

/// The windows of the active workspace of `output` and its sticky windows: most recently used
/// first, then the ones never focused, then the minimized ones. Each comes with the windows its
/// cell stands for, those whose modal dialog it is.
fn collect(
    shell: &Shell,
    seat: &Seat<State>,
    output: &Output,
    include_minimized: bool,
) -> Vec<(FocusTarget, Vec<FocusTarget>)> {
    let Some(set) = shell.workspaces.sets.get(output) else {
        return Vec::new();
    };
    let workspace = &set.workspaces[set.active];

    let mut entries = Vec::new();
    for target in workspace.focus_stack.get(seat).iter_with_minimized() {
        add(&mut entries, target.clone());
    }
    for fullscreen in workspace.get_fullscreen_surfaces() {
        add(
            &mut entries,
            FocusTarget::Fullscreen(fullscreen.surface.clone()),
        );
    }
    for mapped in set.sticky_layer.mapped().chain(workspace.mapped()) {
        add(&mut entries, FocusTarget::Window(mapped.clone()));
    }
    for minimized in workspace
        .minimized_windows
        .iter()
        .chain(set.minimized_windows.iter())
    {
        let target = match minimized {
            MinimizedWindow::Fullscreen { surface, .. } => FocusTarget::Fullscreen(surface.clone()),
            _ => match minimized.mapped() {
                Some(mapped) => FocusTarget::Window(mapped.clone()),
                None => continue,
            },
        };
        add(&mut entries, target);
    }

    let (mut open, minimized): (Vec<_>, Vec<_>) =
        entries.into_iter().partition(|t| !t.is_minimized());
    if include_minimized {
        open.extend(minimized);
    }
    group(shell, open)
}

impl Session {
    /// The title each cell should carry now, by index: its family root's, while the root lives.
    fn titles(&self) -> impl Iterator<Item = (usize, String)> + '_ {
        self.entries
            .iter()
            .zip(&self.families)
            .enumerate()
            .filter_map(|(i, (target, family))| {
                let root = root_of(target, family);
                root.alive().then(|| (i, surface_of(root).title()))
            })
    }

    /// Every window the switcher shows: the entries and the windows behind them.
    fn shown(&self) -> impl Iterator<Item = &FocusTarget> {
        self.entries.iter().chain(self.families.iter().flatten())
    }

    fn is_visible_on(&self, output: &Output) -> bool {
        self.visible && &self.output == output
    }

    /// How open the switcher looks, 0 to 1: rising while it opens, falling while it closes.
    fn openness(&self) -> f64 {
        if let Some((closed, _)) = self.closing {
            return 1.0 - (closed.elapsed().as_secs_f64() / CLOSE_DURATION.as_secs_f64()).min(1.0);
        }
        match self.opened {
            Some(opened) if self.animate => {
                (opened.elapsed().as_secs_f64() / OPEN_DURATION.as_secs_f64()).min(1.0)
            }
            _ => 1.0,
        }
    }

    pub fn is_animating(&self) -> bool {
        self.closing.is_some()
            || (self.animate && self.opened.is_some_and(|o| o.elapsed() < OPEN_DURATION))
    }

    fn build_view(&mut self, evlh: LoopHandle<'static, State>, theme: cosmic::Theme) {
        let area = self.output.geometry().size.as_logical();
        let aspects = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let size = self
                    .shapes
                    .get(i)
                    .map_or_else(|| size_of(t), |s| s.bounds.size);
                size.w as f64 / size.h.max(1) as f64
            })
            .collect::<Vec<_>>();
        let layout = layout::grid_layout(area, &aspects, &GridParams::for_output(area));
        let cells = layout
            .cells
            .iter()
            .zip(self.entries.iter().zip(&self.families))
            .enumerate()
            .map(|(i, (cell, (target, family)))| {
                let surface = surface_of(root_of(target, family));
                let elem = SwitcherCell::new(
                    evlh.clone(),
                    theme.clone(),
                    surface.app_id(),
                    surface.title(),
                    i == self.selected,
                    cell.frame.size,
                );
                elem.output_enter(&self.output);
                elem
            })
            .collect();
        self.view = Some(View { layout, cells });
    }

    fn select(&mut self, index: usize) {
        if index >= self.entries.len() || index == self.selected {
            return;
        }
        self.selected = index;
        if let Some(view) = &self.view {
            for (i, cell) in view.cells.iter().enumerate() {
                cell.set_selected(i == index);
            }
        }
    }

    /// Top-first, like every list `workspace_elements` builds: icons, titles and the frame,
    /// the thumbnails, the shade, the blur. The desktop layers under it are the stage's.
    pub fn render<R>(&self, renderer: &mut R, scale: f64, push: &mut dyn FnMut(SwitcherElement<R>))
    where
        R: AsGlowRenderer + ImportAll + ImportMem,
        R::TextureId: Send + Clone + 'static,
        CosmicMappedRenderElement<R>: RenderElement<R>,
    {
        let Some(view) = self.view.as_ref() else {
            return;
        };
        let area = self.output.geometry().size.as_logical();
        let open = self.openness();
        let eased: f64 = ease(EaseInOutCubic, 0.0, 1.0, open);
        // Titles and frames come in over the second half and go over the first.
        let chrome = ((open - 0.5) * 2.0).clamp(0.0, 1.0) as f32;

        if chrome > 0.0 {
            // Titles are not set from here: this runs on the render thread (see
            // `State::switcher_sync_titles`).
            for ((cell, elem), target) in
                view.layout.cells.iter().zip(&view.cells).zip(&self.entries)
            {
                if !target.alive() {
                    continue;
                }
                elem.push_render_elements(
                    renderer,
                    cell.frame.loc.to_physical_precise_round(scale),
                    scale.into(),
                    chrome,
                    &mut |e| push(SwitcherElement::Iced(e)),
                );
            }
        }

        // Top-first: the most recent window on top, as on screen; closing, the picked one.
        let mut order = (0..self.entries.len()).collect::<Vec<_>>();
        if let Some((_, Some(pick))) = self.closing
            && pick < order.len()
        {
            order.remove(pick);
            order.insert(0, pick);
        }
        for i in order {
            let (Some(cell), Some(target), Some(family), Some(shape)) = (
                view.layout.cells.get(i),
                self.entries.get(i),
                self.families.get(i),
                self.shapes.get(i),
            ) else {
                continue;
            };
            if !target.alive() {
                continue;
            }
            let members = std::iter::once(target)
                .chain(family)
                .zip(shape.members.iter().copied())
                .filter(|(t, _)| t.alive())
                .collect::<Vec<_>>();
            let (rect, alpha) = match shape.on_screen.then_some(shape.bounds) {
                Some(origin) if open < 1.0 => (
                    ease(
                        EaseInOutCubic,
                        EaseRectangle(origin),
                        EaseRectangle(cell.thumb),
                        open,
                    )
                    .unwrap(),
                    1.0,
                ),
                // Not on screen: grows out of its cell while it fades in.
                None if open < 1.0 => (shrunk(cell.thumb, 0.8 + 0.2 * eased), eased as f32),
                _ => (cell.thumb, 1.0),
            };
            element::push_thumbnail(renderer, &members, shape.bounds, rect, scale, alpha, push);
        }
        element::push_dim(renderer, self.dim.clone(), area, eased as f32, push);
        if self.blur_background {
            element::push_blur(renderer, &mut self.blur.lock().unwrap(), area, scale, push);
        }
    }
}

impl Shell {
    /// The switcher to draw on `output` instead of its windows, if one shows there: the open
    /// one, or one whose closing animation still runs.
    pub fn switcher_on(&self, output: &Output) -> Option<&Session> {
        self.switcher
            .as_ref()
            .filter(|s| s.is_visible_on(output))
            .or_else(|| {
                self.switcher_closing
                    .as_ref()
                    .filter(|s| &s.output == output)
            })
    }

    /// Whether a switcher animation needs frames.
    pub fn switcher_animating(&self) -> bool {
        self.switcher.as_ref().is_some_and(Session::is_animating) || self.switcher_closing.is_some()
    }

    /// The output to redraw when `surface` commits: the switcher's, while it shows `surface`.
    /// A minimized window is visible nowhere else, so nothing else would redraw its thumbnail.
    pub fn switcher_output_for_surface(&self, surface: &WlSurface) -> Option<&Output> {
        let session = self
            .switcher
            .as_ref()
            .filter(|s| s.visible)
            .or(self.switcher_closing.as_ref())?;
        session
            .shown()
            .any(|t| match t {
                FocusTarget::Window(mapped) => mapped
                    .windows()
                    .any(|(w, _)| w.wl_surface().is_some_and(|s| *s == *surface)),
                FocusTarget::Fullscreen(s) => s.wl_surface().is_some_and(|s| *s == *surface),
            })
            .then_some(&session.output)
    }

    /// Minimized windows shown on `output`: they get frame callbacks at the output's rate while
    /// their thumbnails are on screen, instead of the throttled one.
    pub fn switcher_minimized_on(&self, output: &Output) -> Vec<CosmicSurface> {
        self.switcher_on(output)
            .map(|s| {
                s.shown()
                    .filter(|t| t.is_minimized())
                    .map(surface_of)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether a window named on a cell of the shown switcher changed its title, and the session
    /// needs [`State::switcher_sync_titles`].
    pub fn switcher_titles_stale(&self) -> bool {
        self.switcher
            .as_ref()
            .filter(|s| s.visible)
            .and_then(|s| Some((s, s.view.as_ref()?)))
            .is_some_and(|(s, view)| {
                s.titles()
                    .any(|(i, title)| view.cells.get(i).is_some_and(|c| !c.has_title(&title)))
            })
    }

    /// Whether a shown window went away, listed or behind a dialog, and the session needs
    /// [`State::switcher_prune`].
    pub fn switcher_needs_prune(&self) -> bool {
        self.switcher
            .as_ref()
            .is_some_and(|s| s.shown().any(|t| !t.alive()))
    }

    /// The session on `output`, if any, for `Common::remove_output` to cancel.
    pub fn switcher_generation_on(&self, output: &Output) -> Option<u64> {
        self.switcher
            .as_ref()
            .filter(|s| &s.output == output)
            .map(|s| s.generation)
    }
}

impl State {
    /// NextWindow / PrevWindow from a binding. Opens the switcher; while it is open the keys
    /// come through [`filter_key`] instead.
    pub fn switcher_next(
        &mut self,
        forward: bool,
        seat: &Seat<State>,
        pattern: &shortcuts::Binding,
    ) {
        let config = self.common.config.cosmic_conf.window_switcher;
        if config.style == WindowSwitcherStyle::None {
            let target = self.common.shell.write().cycle_window(forward, seat);
            if let Some(target) = target {
                Shell::set_focus(self, Some(&target), seat, None, true);
            }
            return;
        }

        if self.common.shell.read().switcher.is_some() {
            self.switcher_op(SwitcherOp::Step { forward }, seat);
            return;
        }
        // A closing animation still running gives way to the new session at once.
        if let Some(closing) = self.common.shell.write().switcher_closing.take() {
            self.backend.schedule_render(&closing.output);
        }

        let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        let hold = layout::hold_modifiers(&pattern.modifiers);
        {
            let mut shell = self.common.shell.write();
            if shell.session_lock.is_some() {
                return;
            }
            let output = seat.active_output();
            let (entries, families): (Vec<_>, Vec<_>) =
                collect(&shell, seat, &output, config.include_minimized)
                    .into_iter()
                    .unzip();
            if entries.is_empty() {
                return;
            }

            let focused = seat.get_keyboard().and_then(|k| k.current_focus());
            let first_focused = match (&focused, entries.first()) {
                (Some(KeyboardFocusTarget::Element(f)), Some(FocusTarget::Window(e))) => f == e,
                (Some(KeyboardFocusTarget::Fullscreen(f)), Some(FocusTarget::Fullscreen(e))) => {
                    f == e
                }
                _ => false,
            };
            let selected = match (forward, first_focused) {
                (true, true) if entries.len() > 1 => 1,
                (true, _) => 0,
                (false, _) => entries.len() - 1,
            };

            let timer = hold.as_ref().and_then(|_| {
                self.common
                    .event_loop_handle
                    .insert_source(Timer::from_duration(SHOW_DELAY), move |_, _, state| {
                        state.switcher_show(generation);
                        TimeoutAction::Drop
                    })
                    .ok()
            });

            shell.switcher = Some(Session {
                generation,
                seat: seat.clone(),
                output,
                entries,
                families,
                selected,
                hold: hold.clone(),
                visible: false,
                timer,
                blur_background: config.blur_background,
                animate: config.animate,
                view: None,
                shapes: Vec::new(),
                opened: None,
                closing: None,
                blur: Mutex::new(BlurState::default()),
                dim: Id::new(),
            });
        }

        if hold.is_none() {
            self.switcher_show(generation);
        }

        // Clicks pick a thumbnail and never reach a window. A pointer already grabbed - a
        // window being dragged - keeps its grab, and the switcher works from the keyboard.
        // The start data names no surface: smithay unsets a grab whose start focus dies, and
        // the window under the pointer may be the very one closed from the switcher.
        let pointer = seat.get_pointer().unwrap();
        if !pointer.is_grabbed() {
            let start_data = GrabStartData {
                focus: None,
                button: 0x110,
                location: pointer.current_location(),
            };
            pointer.set_grab(
                self,
                SwitcherGrab::new(start_data, generation),
                SERIAL_COUNTER.next_serial(),
                Focus::Clear,
            );
        }
    }

    fn switcher_show(&mut self, generation: u64) {
        let evlh = self.common.event_loop_handle.clone();
        let theme = self.common.theme.clone();
        let mut shell = self.common.shell.write();
        let shapes = match shell.switcher.as_ref() {
            Some(s) if s.generation == generation && !s.visible => s
                .entries
                .iter()
                .zip(&s.families)
                .map(|(t, family)| shape_of(&shell, &s.output, t, family))
                .collect::<Vec<_>>(),
            _ => return,
        };
        let Some(session) = shell.switcher.as_mut() else {
            return;
        };
        session.timer = None;
        session.visible = true;
        session.shapes = shapes;
        session.opened = Some(Instant::now());
        session.build_view(evlh, theme);
        let output = session.output.clone();
        std::mem::drop(shell);
        self.backend.schedule_render(&output);
    }

    pub fn switcher_op(&mut self, op: SwitcherOp, _seat: &Seat<State>) {
        let output = {
            let mut shell = self.common.shell.write();
            let Some(session) = shell.switcher.as_mut() else {
                return;
            };
            let len = session.entries.len();
            let index = match op {
                SwitcherOp::Step { forward } => layout::step(len, session.selected, forward),
                SwitcherOp::Move(dir) => match &session.view {
                    Some(view) => layout::neighbour(&view.layout, session.selected, dir),
                    None => match dir {
                        Dir::Left => layout::step(len, session.selected, false),
                        Dir::Right => layout::step(len, session.selected, true),
                        _ => session.selected,
                    },
                },
                SwitcherOp::Hover(index) => index,
                SwitcherOp::Commit(pick) => {
                    let pick = pick.unwrap_or(session.selected);
                    std::mem::drop(shell);
                    self.switcher_finish(Some(pick));
                    return;
                }
                SwitcherOp::Cancel => {
                    std::mem::drop(shell);
                    self.switcher_finish(None);
                    return;
                }
                // The entry goes once the window is gone (`switcher_prune`); a window that asks
                // first - unsaved changes - stays, and its dialog shows after the switcher.
                SwitcherOp::Close(index) => {
                    if let Some(target) = session.entries.get(index.unwrap_or(session.selected)) {
                        surface_of(target).close();
                    }
                    return;
                }
            };
            session.select(index);
            session.visible.then(|| session.output.clone())
        };
        if let Some(output) = output {
            self.backend.schedule_render(&output);
        }
    }

    /// Closes the switcher and switches to entry `pick`, or to nothing.
    fn switcher_finish(&mut self, pick: Option<usize>) {
        let Some(mut session) = self.common.shell.write().switcher.take() else {
            return;
        };
        if let Some(timer) = session.timer {
            self.common.event_loop_handle.remove(timer);
        }
        // Not under the shell lock: the grab's `unset` runs from here.
        let pointer = session.seat.get_pointer().unwrap();
        if pointer.with_grab(|_, grab| grab.is::<SwitcherGrab>()) == Some(true) {
            pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), InputTime::now());
        }
        if session.visible {
            self.backend.schedule_render(&session.output);
        }

        // Checked before the switch: a picked minimized window is restored by it and plays its
        // own animation from the panel, which the closing one would hide.
        let picked_minimized = pick
            .and_then(|i| session.entries.get(i))
            .is_some_and(|t| t.is_minimized());
        if session.visible && session.animate && !picked_minimized {
            let generation = session.generation;
            session.closing = Some((Instant::now(), pick));
            let _ = self.common.event_loop_handle.insert_source(
                Timer::from_duration(CLOSE_DURATION + Duration::from_millis(16)),
                move |_, _, state| {
                    state.switcher_closed(generation);
                    TimeoutAction::Drop
                },
            );
            let output = session.output.clone();
            let seat = session.seat.clone();
            let target = pick.and_then(|i| session.entries.get(i)).cloned();
            self.common.shell.write().switcher_closing = Some(session);
            self.backend.schedule_render(&output);
            self.switcher_activate(target.as_ref(), Some(seat));
            return;
        }

        let seat = session.seat.clone();
        self.switcher_activate(pick.and_then(|i| session.entries.get(i)), Some(seat));
    }

    /// Ends a closing animation, so the windows draw themselves again.
    fn switcher_closed(&mut self, generation: u64) {
        let mut shell = self.common.shell.write();
        if shell
            .switcher_closing
            .as_ref()
            .is_some_and(|s| s.generation == generation)
            && let Some(session) = shell.switcher_closing.take()
        {
            std::mem::drop(shell);
            self.backend.schedule_render(&session.output);
        }
    }

    fn switcher_activate(&mut self, target: Option<&FocusTarget>, seat: Option<Seat<State>>) {
        if let Some(target) = target
            && target.alive()
        {
            // The same path as a click on the panel: restores a minimized window, switches
            // its workspace if need be, raises and focuses it.
            let dh = self.common.display_handle.clone();
            ToplevelManagementHandler::activate(self, &dh, &surface_of(target), seat);
        }
    }

    /// Closes the switcher without switching. With `generation`, only if that session is the
    /// one still open: callbacks of a closed session must not close a newer one.
    pub fn switcher_cancel(&mut self, generation: Option<u64>) {
        let open = self
            .common
            .shell
            .read()
            .switcher
            .as_ref()
            .map(|s| s.generation);
        if open.is_some() && (generation.is_none() || generation == open) {
            self.switcher_finish(None);
        }
    }

    /// Commits a pick made by the pointer grab, which cannot finish the session itself.
    fn switcher_pick(&mut self, generation: u64, pick: Option<usize>) {
        let open = self
            .common
            .shell
            .read()
            .switcher
            .as_ref()
            .map(|s| s.generation);
        if open == Some(generation) {
            self.switcher_finish(pick);
        }
    }

    /// The cell whose close button is under `location` (global), for the pointer grab.
    fn switcher_close_at(&self, generation: u64, location: Point<f64, Logical>) -> Option<usize> {
        let shell = self.common.shell.read();
        let session = shell
            .switcher
            .as_ref()
            .filter(|s| s.generation == generation && s.visible)?;
        let local = location - session.output.geometry().loc.as_logical().to_f64();
        layout::close_at(&session.view.as_ref()?.layout, local)
    }

    /// The cell under `location` (global), for the pointer grab.
    fn switcher_cell_at(&self, generation: u64, location: Point<f64, Logical>) -> Option<usize> {
        let shell = self.common.shell.read();
        let session = shell
            .switcher
            .as_ref()
            .filter(|s| s.generation == generation && s.visible)?;
        let local = location - session.output.geometry().loc.as_logical().to_f64();
        layout::cell_at(&session.view.as_ref()?.layout, local)
    }

    /// Gives the cells the titles their windows carry now. Only from the main thread, never from
    /// the render: updating a cell queues its task on the executor of the calling thread, and
    /// dropping the cell on the main thread then aborts the compositor.
    pub fn switcher_sync_titles(&mut self) {
        let output = {
            let shell = self.common.shell.read();
            let Some(session) = shell.switcher.as_ref().filter(|s| s.visible) else {
                return;
            };
            let Some(view) = session.view.as_ref() else {
                return;
            };
            for (i, title) in session.titles() {
                if let Some(cell) = view.cells.get(i) {
                    cell.set_title(&title);
                }
            }
            session.output.clone()
        };
        self.backend.schedule_render(&output);
    }

    /// Drops shown windows that went away, rebuilding the grid, and closes the switcher when
    /// none is left. The windows a dialog that went away stood for take its place, grouped
    /// anew: one of them may still have a dialog of its own. A window gone from behind a dialog
    /// that stays leaves its cell, reshaped and named after what is left.
    pub fn switcher_prune(&mut self) {
        let evlh = self.common.event_loop_handle.clone();
        let theme = self.common.theme.clone();
        let output = {
            let mut shell = self.common.shell.write();
            let Some(old) = shell.switcher.as_ref() else {
                return;
            };
            if old.shown().all(|t| t.alive()) {
                return;
            }
            let selected = old.entries.get(old.selected).cloned();
            // Shapes are taken when the switcher shows, and stay empty until then.
            let shown = !old.shapes.is_empty();
            let mut entries = Vec::new();
            let mut families = Vec::new();
            let mut shapes = Vec::new();
            for (i, (target, family)) in old.entries.iter().zip(&old.families).enumerate() {
                if target.alive() {
                    let lost = family.iter().any(|t| !t.alive());
                    let family = family
                        .iter()
                        .filter(|t| t.alive())
                        .cloned()
                        .collect::<Vec<_>>();
                    if shown && lost {
                        shapes.push(shape_of(&shell, &old.output, target, &family));
                    } else {
                        shapes.extend(old.shapes.get(i).cloned());
                    }
                    entries.push(target.clone());
                    families.push(family);
                    continue;
                }
                let behind = family.iter().filter(|t| t.alive()).cloned().collect();
                for (target, family) in group(&shell, behind) {
                    if shown {
                        shapes.push(shape_of(&shell, &old.output, &target, &family));
                    }
                    entries.push(target);
                    families.push(family);
                }
            }
            let Some(session) = shell.switcher.as_mut() else {
                return;
            };
            session.entries = entries;
            session.families = families;
            session.shapes = shapes;
            if session.entries.is_empty() {
                std::mem::drop(shell);
                self.switcher_finish(None);
                return;
            }
            session.selected = selected
                .and_then(|s| session.entries.iter().position(|t| *t == s))
                .unwrap_or(session.selected.min(session.entries.len() - 1));
            if session.view.is_some() {
                session.build_view(evlh, theme);
            }
            session.visible.then(|| session.output.clone())
        };
        if let Some(output) = output {
            self.backend.schedule_render(&output);
        }
    }
}

/// Decides a key while a switcher is open on `seat`; `None` when there is no switcher or the
/// key is a release it leaves alone. `key_matches` is the layout-agnostic match
/// `filter_keyboard_input` uses for bindings.
pub fn filter_key(
    state: &State,
    seat: &Seat<State>,
    modifiers: &ModifiersState,
    sym: Keysym,
    key_state: KeyState,
    key_matches: &dyn Fn(Keysym) -> bool,
) -> Option<SwitcherKey> {
    let hold = {
        let shell = state.common.shell.read();
        let session = shell.switcher.as_ref().filter(|s| &s.seat == seat)?;
        session.hold.clone()
    };
    let now = shortcuts::Modifiers {
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
        logo: modifiers.logo,
    };

    if sym.is_modifier_key() {
        if key_state == KeyState::Released
            && hold
                .as_ref()
                .is_some_and(|hold| layout::hold_released(hold, &now))
        {
            return Some(SwitcherKey::Release(SwitcherOp::Commit(None)));
        }
        return Some(SwitcherKey::Forward);
    }
    if key_state == KeyState::Released {
        return None;
    }

    let op = match sym {
        Keysym::Escape => SwitcherOp::Cancel,
        Keysym::Return | Keysym::KP_Enter => SwitcherOp::Commit(None),
        Keysym::Delete | Keysym::KP_Delete => SwitcherOp::Close(None),
        Keysym::Left | Keysym::KP_Left => SwitcherOp::Move(Dir::Left),
        Keysym::Right | Keysym::KP_Right => SwitcherOp::Move(Dir::Right),
        Keysym::Up | Keysym::KP_Up => SwitcherOp::Move(Dir::Up),
        Keysym::Down | Keysym::KP_Down => SwitcherOp::Move(Dir::Down),
        _ => match switcher_binding(state, &now, key_matches) {
            Some(forward) => SwitcherOp::Step { forward },
            // A session without held modifiers has no binding key to repeat: Tab walks it.
            None if hold.is_none() && matches!(sym, Keysym::Tab | Keysym::ISO_Left_Tab) => {
                SwitcherOp::Step {
                    forward: !now.shift,
                }
            }
            None => return Some(SwitcherKey::Press(None)),
        },
    };
    Some(SwitcherKey::Press(Some(op)))
}

/// Whether the key is a NextWindow / PrevWindow binding's, and which way it goes: the binding
/// with exactly these modifiers decides, otherwise the key of either with Shift reversing it.
fn switcher_binding(
    state: &State,
    now: &shortcuts::Modifiers,
    key_matches: &dyn Fn(Keysym) -> bool,
) -> Option<bool> {
    let mut loose = None;
    for (binding, action) in state.common.config.shortcuts.iter() {
        let forward = match action {
            shortcuts::Action::NextWindow => true,
            shortcuts::Action::PrevWindow => false,
            _ => continue,
        };
        if !binding.key.is_some_and(key_matches) {
            continue;
        }
        if binding.modifiers == *now {
            return Some(forward);
        }
        loose.get_or_insert(forward != (now.shift && !binding.modifiers.shift));
    }
    loose
}
