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

use super::layout::{HEADER, HEADER_GAP, PAD};

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
        text,
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

    pub fn set_title(&self, title: &str) {
        if self.elem.with_program(|p| p.title != title) {
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
        push: &mut dyn FnMut(IcedRenderElement<R>),
    ) where
        R: AsGlowRenderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        self.elem
            .push_render_elements(renderer, location, scale, 1.0, [0; 4], push, None);
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

/// The picture of `target` fitted into `thumb` (output-local), top-first through `push`.
pub fn push_thumbnail<R>(
    renderer: &mut R,
    target: &FocusTarget,
    thumb: Rectangle<i32, Logical>,
    scale: f64,
    push: &mut dyn FnMut(SwitcherElement<R>),
) where
    R: AsGlowRenderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
    CosmicMappedRenderElement<R>: smithay::backend::renderer::element::RenderElement<R>,
{
    let target_rect = thumb.to_physical_precise_round(scale);
    match target {
        FocusTarget::Window(mapped) => {
            let elem_geometry = mapped.geometry().to_physical_precise_round(scale);
            let location = target_rect.loc - elem_geometry.loc;
            // Same mapping as the tiling overview: each element is constrained against the
            // window's geometry, so its parts keep their places relative to each other.
            let fit = |element| match element {
                CosmicMappedRenderElement::Stack(elem) => constrain_render_elements(
                    std::iter::once(elem),
                    location,
                    target_rect,
                    elem_geometry,
                    ConstrainScaleBehavior::Fit,
                    ConstrainAlign::CENTER,
                    scale,
                )
                .next()
                .map(CosmicMappedRenderElement::TiledStack),
                CosmicMappedRenderElement::Window(elem) => constrain_render_elements(
                    std::iter::once(elem),
                    location,
                    target_rect,
                    elem_geometry,
                    ConstrainScaleBehavior::Fit,
                    ConstrainAlign::CENTER,
                    scale,
                )
                .next()
                .map(CosmicMappedRenderElement::TiledWindow),
                // Focus indicators, overlays and the like belong to the window on screen.
                _ => None,
            };

            let mut upper = Vec::new();
            let mut lower = Vec::new();
            mapped.push_render_elements(
                renderer,
                location,
                None,
                Scale::from(scale),
                1.0,
                None,
                None,
                &mut |elem| upper.extend(fit(elem)),
                &mut |elem| lower.extend(fit(elem)),
            );
            for elem in upper.into_iter().chain(lower) {
                push(SwitcherElement::Workspace(WorkspaceRenderElement::Window(
                    elem,
                )));
            }
        }
        FocusTarget::Fullscreen(surface) => {
            let elem_geometry = SpaceElement::geometry(surface).to_physical_precise_round(scale);
            let location = target_rect.loc - elem_geometry.loc;
            surface.push_render_elements(
                renderer,
                location,
                Scale::from(scale),
                1.0,
                None,
                None,
                false,
                [0; 4],
                0,
                &mut |elem| {
                    let elem: CosmicWindowRenderElement<R> = elem.into();
                    if let Some(elem) = constrain_render_elements(
                        std::iter::once(elem),
                        location,
                        target_rect,
                        elem_geometry,
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

/// The shade over the desktop, under the thumbnails.
pub fn push_dim<R>(
    renderer: &mut R,
    key: smithay::backend::renderer::element::Id,
    output_size: Size<i32, Logical>,
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
        DIM,
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
