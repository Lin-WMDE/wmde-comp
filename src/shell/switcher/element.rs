// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: what the window switcher draws.
//!
//! A cell's icon, title and selection frame are an `IcedElement`: tiny-skia draws it on the
//! CPU, so the window picture cannot live inside it. The picture is the window's own render
//! elements, fitted into the cell the way the tiling overview fits a tile
//! (`constrain_render_elements` with `Fit`), and stays live because they are the same elements
//! the window draws on screen.

use crate::{
    backend::render::{BackdropShader, element::AsGlowRenderer, wayland::blur_effect::BlurElement},
    shell::{
        CosmicMappedRenderElement, WorkspaceRenderElement,
        element::window::CosmicWindowRenderElement, focus::FocusTarget,
    },
    utils::{
        iced::{IcedElement, IcedRenderElement, Program},
        prelude::*,
    },
};

use super::layout::{
    CAPTION, CAPTION_GAP, HEADER, HEADER_GAP, ICON, PAD, PANEL_BORDER, PANEL_PAD, ROW, ROW_ICON,
    TILE, TILE_GAP,
};

use calloop::LoopHandle;
use cosmic::{
    Apply,
    iced::{
        Alignment, Length,
        core::{
            Border, Color,
            text::{Ellipsize, EllipsizeHeightLimit, Wrapping},
        },
        widget::{column, container, row, space},
    },
    theme,
    widget::{
        icon::{IconFallback, from_name},
        menu, text,
    },
};
use smithay::{
    backend::renderer::{
        ImportAll, ImportMem,
        element::utils::{ConstrainAlign, ConstrainScaleBehavior, constrain_render_elements},
    },
    desktop::space::SpaceElement,
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
};

/// Width of the selection frame.
const FRAME: f32 = 2.0;
/// Between the icon and the title.
const ICON_GAP: f32 = 8.0;
/// The close glyph, centred in a header-high square.
const CLOSE_ICON: u16 = 16;
/// How dark the desktop behind the thumbnails gets, over the blur: white titles have to stay
/// readable on a light wallpaper.
pub const DIM: f32 = 0.3;
/// Blur of the desktop behind the thumbnails, an index into `BLUR_PARAMS`. Its own, and not the
/// theme's `frosted` strength: the theme keeps every frosted flag off (flat stack), and this
/// backdrop is blurred on purpose.
pub const BLUR_STRENGTH: usize = 9;

/// One thing to draw: a window picture or the backdrop shade, cropped to the output like any
/// workspace element, or a compositor-drawn element.
pub enum SwitcherElement<R>
where
    R: AsGlowRenderer,
    R::TextureId: Send + 'static,
{
    Workspace(WorkspaceRenderElement<R>),
    Iced(IcedRenderElement<R>),
}

/// Icon, title and selection frame of one window, sized to its layout frame.
#[derive(Debug, Clone)]
pub struct SwitcherCell {
    elem: IcedElement<CellProgram>,
}

impl SwitcherCell {
    pub fn new(
        evlh: LoopHandle<'static, crate::state::State>,
        mut theme: cosmic::Theme,
        app_id: String,
        title: String,
        selected: bool,
        frame: Size<i32, Logical>,
    ) -> SwitcherCell {
        // The cell is transparent over the thumbnail: no blur of its own under it.
        theme.transparent = false;
        SwitcherCell {
            elem: IcedElement::new(
                CellProgram {
                    app_id,
                    title,
                    selected,
                    size: (frame.w as f32, frame.h as f32),
                },
                frame,
                evlh,
                theme,
            ),
        }
    }

    /// `queue_message` only enqueues - the element drains it from its own input handlers,
    /// and the switcher never routes input through it - so the update is driven by hand and
    /// the cached buffer dropped, as `SnapStrip::set_hovered` does.
    fn send(&self, message: Message) {
        self.elem.queue_message(message);
        self.elem.force_update();
        self.elem.force_redraw();
    }

    pub fn set_selected(&self, selected: bool) {
        if self.elem.with_program(|p| p.selected) != selected {
            self.send(Message::Selected(selected));
        }
    }

    pub fn has_title(&self, title: &str) -> bool {
        self.elem.with_program(|p| p.title == title)
    }

    pub fn set_title(&self, title: &str) {
        if !self.has_title(title) {
            self.send(Message::Title(title.to_string()));
        }
    }

    pub fn output_enter(&self, output: &Output) {
        self.elem
            .output_enter(output, Rectangle::default() /*unused*/);
    }

    pub fn push_render_elements<R>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
        push: &mut dyn FnMut(IcedRenderElement<R>),
    ) where
        R: AsGlowRenderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        self.elem
            .push_render_elements(renderer, location, scale, alpha, [0; 4], push, None);
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Selected(bool),
    Title(String),
}

#[derive(Debug, Clone)]
pub struct CellProgram {
    app_id: String,
    title: String,
    selected: bool,
    size: (f32, f32),
}

impl Program for CellProgram {
    type Message = Message;

    fn update(
        &mut self,
        message: Self::Message,
        _loop_handle: &LoopHandle<'static, crate::state::State>,
        _last_seat: Option<&(
            smithay::input::Seat<crate::state::State>,
            smithay::utils::Serial,
        )>,
    ) -> cosmic::iced::Task<Self::Message> {
        match message {
            Message::Selected(selected) => self.selected = selected,
            Message::Title(title) => self.title = title,
        }
        cosmic::iced::Task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let mut icon = from_name(self.app_id.clone()).size(HEADER as u16);
        icon.fallback = Some(IconFallback::Names(vec!["application-x-executable".into()]));

        // The selected window's close button. Its place is kept on every cell, so the title
        // keeps its width when the selection moves. Hit testing is `layout::close_button`.
        let close: cosmic::Element<'_, Message> = if self.selected {
            from_name("window-close-symbolic")
                .size(CLOSE_ICON)
                .prefer_svg(true)
                .icon()
                .apply(container)
                .center(Length::Fixed(HEADER as f32))
                .class(theme::Container::custom(|_theme| container::Style {
                    icon_color: Some(Color::WHITE),
                    ..Default::default()
                }))
                .into()
        } else {
            space::horizontal().width(HEADER as f32).into()
        };

        let header = row(vec![
            icon.icon().into(),
            space::horizontal().width(ICON_GAP).into(),
            // White over the dimmed desktop in either theme. Set on the text itself: a
            // container's `text_color` does not reach it.
            text::body(self.title.as_str())
                .class(theme::Text::Color(Color::WHITE))
                .wrapping(Wrapping::None)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
                .width(Length::Fill)
                .into(),
            close,
        ])
        .align_y(Alignment::Center)
        .height(Length::Fixed(HEADER as f32));

        let selected = self.selected;
        column(vec![
            header.into(),
            // The thumbnail is drawn by the window itself, under this element.
            space::vertical().height(Length::Fill).into(),
        ])
        .spacing(HEADER_GAP as f32)
        .apply(container)
        .padding(PAD as u16)
        .width(Length::Fixed(self.size.0))
        .height(Length::Fixed(self.size.1))
        .class(theme::Container::custom(move |_theme| container::Style {
            snap: true,
            icon_color: None,
            text_color: None,
            background: None,
            // Square like every selection in the stack, and with no fill: the frame
            // outlines the window, it does not cover it.
            border: if selected {
                Border {
                    radius: 0.0.into(),
                    width: FRAME,
                    color: Color::WHITE,
                }
            } else {
                Border::default()
            },
            shadow: Default::default(),
        }))
        .into()
    }
}

/// What the panel of the Icons and List looks shows of one window.
#[derive(Debug, Clone)]
pub struct PanelEntry {
    pub app_id: String,
    pub title: String,
}

/// How a panel lays out its windows, with the numbers `layout::icons_layout` and
/// `layout::list_layout` worked out, so the drawing and the hit testing agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelLook {
    /// Tiles `columns` to a row, centred over a title line `content_w` wide.
    Icons { columns: usize, content_w: i32 },
    /// `visible` rows, from the first shown.
    List { visible: usize },
}

/// The whole panel of the Icons or List look as one element, framed like the compositor's
/// window menu (`grabs::menu`): a surface of the theme, never translucent.
#[derive(Debug, Clone)]
pub struct SwitcherPanel {
    elem: IcedElement<PanelProgram>,
}

impl SwitcherPanel {
    pub fn new(
        evlh: LoopHandle<'static, crate::state::State>,
        mut theme: cosmic::Theme,
        look: PanelLook,
        entries: Vec<PanelEntry>,
        selected: usize,
        first: usize,
        size: Size<i32, Logical>,
    ) -> SwitcherPanel {
        // Opaque: no blur of its own under it.
        theme.transparent = false;
        SwitcherPanel {
            elem: IcedElement::new(
                PanelProgram {
                    look,
                    entries,
                    selected,
                    first,
                    size: (size.w as f32, size.h as f32),
                },
                size,
                evlh,
                theme,
            ),
        }
    }

    /// As `SwitcherCell::send`: only from the main thread.
    fn send(&self, message: PanelMessage) {
        self.elem.queue_message(message);
        self.elem.force_update();
        self.elem.force_redraw();
    }

    pub fn set_selected(&self, selected: usize) {
        if self.elem.with_program(|p| p.selected) != selected {
            self.send(PanelMessage::Selected(selected));
        }
    }

    pub fn set_first(&self, first: usize) {
        if self.elem.with_program(|p| p.first) != first {
            self.send(PanelMessage::First(first));
        }
    }

    pub fn has_title(&self, index: usize, title: &str) -> bool {
        self.elem
            .with_program(|p| p.entries.get(index).is_none_or(|e| e.title == title))
    }

    pub fn set_title(&self, index: usize, title: &str) {
        if !self.has_title(index, title) {
            self.send(PanelMessage::Title(index, title.to_string()));
        }
    }

    pub fn output_enter(&self, output: &Output) {
        self.elem
            .output_enter(output, Rectangle::default() /*unused*/);
    }

    pub fn push_render_elements<R>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
        push: &mut dyn FnMut(IcedRenderElement<R>),
    ) where
        R: AsGlowRenderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        self.elem
            .push_render_elements(renderer, location, scale, alpha, [0; 4], push, None);
    }
}

#[derive(Debug, Clone)]
pub enum PanelMessage {
    Selected(usize),
    Title(usize, String),
    First(usize),
}

#[derive(Debug, Clone)]
pub struct PanelProgram {
    look: PanelLook,
    entries: Vec<PanelEntry>,
    selected: usize,
    /// The first row the List look shows.
    first: usize,
    size: (f32, f32),
}

fn app_icon(app_id: &str, size: i32) -> cosmic::Element<'static, PanelMessage> {
    let mut icon = from_name(app_id.to_string()).size(size as u16);
    icon.fallback = Some(IconFallback::Names(vec!["application-x-executable".into()]));
    icon.icon().into()
}

fn title_text(title: &str) -> cosmic::widget::Text<'_, cosmic::Theme, cosmic::Renderer> {
    text::body(title)
        .wrapping(Wrapping::None)
        .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
}

impl PanelProgram {
    /// A tile of the Icons look. The selected one is tinted like a selected icon on the desktop
    /// and in Files: the theme's `icon_selection` and the same colour solid as its line.
    fn tile(&self, index: usize, entry: &PanelEntry) -> cosmic::Element<'_, PanelMessage> {
        let selected = index == self.selected;
        app_icon(&entry.app_id, ICON)
            .apply(container)
            .center(Length::Fixed(TILE as f32))
            .class(theme::Container::custom(move |theme| {
                let mut style = container::Style {
                    snap: true,
                    ..Default::default()
                };
                if selected {
                    let fill = Color::from(theme.cosmic().icon_selection);
                    style.background = Some(fill.into());
                    style.border = Border {
                        radius: 0.0.into(),
                        width: 1.0,
                        color: Color { a: 1.0, ..fill },
                    };
                }
                style
            }))
            .into()
    }

    /// A row of the List look: icon, title and, on the selected row, the close glyph in a
    /// row-high square at the right end, where `layout::close_button` hits it. Every row keeps
    /// the square, so the title keeps its width. The selected row is filled like a hovered menu
    /// item, from frame to frame.
    fn row<'a>(&'a self, index: usize, entry: &'a PanelEntry) -> cosmic::Element<'a, PanelMessage> {
        let selected = index == self.selected;
        let close: cosmic::Element<'_, PanelMessage> = if selected {
            from_name("window-close-symbolic")
                .size(CLOSE_ICON)
                .prefer_svg(true)
                .icon()
                .apply(container)
                .center(Length::Fixed(ROW as f32))
                .into()
        } else {
            space::horizontal().width(ROW as f32).into()
        };
        row(vec![
            app_icon(&entry.app_id, ROW_ICON),
            space::horizontal().width(ICON_GAP).into(),
            title_text(&entry.title).width(Length::Fill).into(),
            close,
        ])
        .align_y(Alignment::Center)
        .height(Length::Fixed(ROW as f32))
        .padding([0, 0, 0, menu::ITEM_PADDING])
        .apply(container)
        .width(Length::Fill)
        .class(theme::Container::custom(move |theme| container::Style {
            snap: true,
            background: selected
                .then(|| Color::from(theme.cosmic().background(false).component.hover).into()),
            ..Default::default()
        }))
        .into()
    }
}

impl Program for PanelProgram {
    type Message = PanelMessage;

    fn update(
        &mut self,
        message: Self::Message,
        _loop_handle: &LoopHandle<'static, crate::state::State>,
        _last_seat: Option<&(
            smithay::input::Seat<crate::state::State>,
            smithay::utils::Serial,
        )>,
    ) -> cosmic::iced::Task<Self::Message> {
        match message {
            PanelMessage::Selected(selected) => self.selected = selected,
            PanelMessage::Title(index, title) => {
                if let Some(entry) = self.entries.get_mut(index) {
                    entry.title = title;
                }
            }
            PanelMessage::First(first) => self.first = first,
        }
        cosmic::iced::Task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let content: cosmic::Element<'_, PanelMessage> = match self.look {
            PanelLook::Icons { columns, content_w } => {
                let rows = self
                    .entries
                    .chunks(columns.max(1))
                    .enumerate()
                    .map(|(r, chunk)| {
                        row(chunk
                            .iter()
                            .enumerate()
                            .map(|(c, entry)| self.tile(r * columns + c, entry))
                            .collect::<Vec<_>>())
                        .spacing(TILE_GAP as f32)
                        .into()
                    })
                    .collect::<Vec<_>>();
                let title = self
                    .entries
                    .get(self.selected)
                    .map_or("", |e| e.title.as_str());
                column(vec![
                    column(rows)
                        .spacing(TILE_GAP as f32)
                        .apply(container)
                        .center_x(Length::Fixed(content_w as f32))
                        .into(),
                    title_text(title)
                        .align_x(Alignment::Center)
                        .align_y(Alignment::Center)
                        .width(Length::Fixed(content_w as f32))
                        .height(Length::Fixed(CAPTION as f32))
                        .into(),
                ])
                .spacing(CAPTION_GAP as f32)
                .padding(PANEL_PAD as u16)
                .into()
            }
            PanelLook::List { visible } => column(
                self.entries
                    .iter()
                    .enumerate()
                    .skip(self.first)
                    .take(visible)
                    .map(|(i, entry)| self.row(i, entry))
                    .collect::<Vec<_>>(),
            )
            .padding(PANEL_BORDER as u16)
            .into(),
        };

        content
            .apply(container)
            .width(Length::Fixed(self.size.0))
            .height(Length::Fixed(self.size.1))
            .class(theme::Container::custom(|theme| {
                let component = &theme.cosmic().background(false).component;
                container::Style {
                    snap: true,
                    icon_color: Some(component.on.into()),
                    text_color: Some(component.on.into()),
                    background: Some(Color::from(component.base).into()),
                    border: Border {
                        radius: menu::RADIUS.into(),
                        width: PANEL_BORDER as f32,
                        color: component.divider.into(),
                    },
                    shadow: Default::default(),
                }
            }))
            .into()
    }
}

/// The picture of a cell fitted into `thumb` (output-local), top-first through `push`: each window
/// of `members`, top-first, at its rectangle inside `bounds`, and `bounds` fitted into `thumb`.
/// With the windows' own places it draws exactly where they are, which the animation starts
/// from.
pub fn push_thumbnail<R>(
    renderer: &mut R,
    members: &[(&FocusTarget, Rectangle<i32, Logical>)],
    bounds: Rectangle<i32, Logical>,
    thumb: Rectangle<i32, Logical>,
    scale: f64,
    alpha: f32,
    push: &mut dyn FnMut(SwitcherElement<R>),
) where
    R: AsGlowRenderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
    CosmicMappedRenderElement<R>: smithay::backend::renderer::element::RenderElement<R>,
{
    let target_rect = thumb.to_physical_precise_round(scale);
    let reference = bounds.to_physical_precise_round(scale);
    // Same mapping as the tiling overview, with one origin for all the windows of the cell:
    // every element is scaled about it and constrained against `bounds`, so the windows and
    // their parts keep their places relative to each other.
    let origin = target_rect.loc - reference.loc;
    let fit = |element| match element {
        CosmicMappedRenderElement::Stack(elem) => constrain_render_elements(
            std::iter::once(elem),
            origin,
            target_rect,
            reference,
            ConstrainScaleBehavior::Fit,
            ConstrainAlign::CENTER,
            scale,
        )
        .next()
        .map(CosmicMappedRenderElement::TiledStack),
        CosmicMappedRenderElement::Window(elem) => constrain_render_elements(
            std::iter::once(elem),
            origin,
            target_rect,
            reference,
            ConstrainScaleBehavior::Fit,
            ConstrainAlign::CENTER,
            scale,
        )
        .next()
        .map(CosmicMappedRenderElement::TiledWindow),
        // Focus indicators, overlays and the like belong to the window on screen.
        _ => None,
    };

    // The fit `constrain_render_elements` makes, for the shadows, which are drawn at their size
    // in the thumbnail instead of being scaled down.
    let fit_scale = f64::min(
        target_rect.size.w as f64 / reference.size.w.max(1) as f64,
        target_rect.size.h as f64 / reference.size.h.max(1) as f64,
    );
    let fit_offset = target_rect.loc.to_f64()
        + Point::<f64, Physical>::from((
            (target_rect.size.w as f64 - reference.size.w as f64 * fit_scale) / 2.,
            (target_rect.size.h as f64 - reference.size.h as f64 * fit_scale) / 2.,
        ));

    let bottom = members.len().saturating_sub(1);
    for (i, (target, rect)) in members.iter().enumerate() {
        let place = origin + rect.loc.to_physical_precise_round(scale);
        match target {
            FocusTarget::Window(mapped) => {
                let location = place - mapped.geometry().loc.to_physical_precise_round(scale);
                let mut upper = Vec::new();
                let mut lower = Vec::new();
                mapped.push_render_elements(
                    renderer,
                    location,
                    None,
                    Scale::from(scale),
                    alpha,
                    None,
                    None,
                    &mut |elem| upper.extend(fit(elem)),
                    &mut |elem| lower.extend(fit(elem)),
                );
                // A window over another one of the cell, a dialog over its parent, keeps its
                // shadow, or it melts into the window under it. Between the window and its
                // lower elements, as the floating layer draws it.
                let shadow = (i < bottom)
                    .then(|| {
                        let at = fit_offset
                            + (location - origin - reference.loc)
                                .to_f64()
                                .upscale(fit_scale);
                        mapped.shadow_render_element::<R, CosmicMappedRenderElement<R>>(
                            renderer,
                            at.to_i32_round(),
                            None,
                            Scale::from(scale),
                            fit_scale,
                            alpha,
                        )
                    })
                    .flatten();
                for elem in upper.into_iter().chain(shadow).chain(lower) {
                    push(SwitcherElement::Workspace(WorkspaceRenderElement::Window(
                        elem,
                    )));
                }
            }
            FocusTarget::Fullscreen(surface) => {
                let location = place
                    - SpaceElement::geometry(surface)
                        .loc
                        .to_physical_precise_round(scale);
                surface.push_render_elements(
                    renderer,
                    location,
                    Scale::from(scale),
                    alpha,
                    None,
                    None,
                    false,
                    [0; 4],
                    0,
                    &mut |elem| {
                        let elem: CosmicWindowRenderElement<R> = elem.into();
                        if let Some(elem) = constrain_render_elements(
                            std::iter::once(elem),
                            origin,
                            target_rect,
                            reference,
                            ConstrainScaleBehavior::Fit,
                            ConstrainAlign::CENTER,
                            scale,
                        )
                        .next()
                        {
                            push(SwitcherElement::Workspace(WorkspaceRenderElement::Window(
                                CosmicMappedRenderElement::TiledWindow(elem),
                            )));
                        }
                    },
                    None,
                );
            }
        }
    }
}

/// The shade over the desktop, under the thumbnails, `strength` of it while it fades.
pub fn push_dim<R>(
    renderer: &mut R,
    key: smithay::backend::renderer::element::Id,
    output_size: Size<i32, Logical>,
    strength: f32,
    push: &mut dyn FnMut(SwitcherElement<R>),
) where
    R: AsGlowRenderer,
    R::TextureId: Send + Clone + 'static,
{
    let shade = BackdropShader::element(
        renderer,
        key,
        Rectangle::from_size(output_size).as_local(),
        0.,
        DIM * strength,
        [0.0, 0.0, 0.0],
    );
    push(SwitcherElement::Workspace(WorkspaceRenderElement::Window(
        shade.into(),
    )));
}

/// A blur of whatever is drawn under it, over the whole output. `BlurElement` re-captures only
/// when what lies under it changes, and the desktop under the switcher does not, so the cost is
/// paid once per opening.
pub fn push_blur<R>(
    renderer: &mut R,
    state: &mut crate::backend::render::wayland::blur_effect::BlurState,
    output_size: Size<i32, Logical>,
    scale: f64,
    push: &mut dyn FnMut(SwitcherElement<R>),
) where
    R: AsGlowRenderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    match BlurElement::from_state(
        renderer,
        state,
        Rectangle::from_size(output_size.to_f64()),
        scale,
        [0; 4],
        BLUR_STRENGTH,
    ) {
        Ok(Some(elem)) => push(SwitcherElement::Iced(elem.into())),
        Ok(None) => {}
        Err(err) => tracing::warn!(?err, "Window switcher blur failed"),
    }
}
