// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: the layout strip that drops down from the top edge while a window is being dragged.
//!
//! Modelled on ref/w11/win_place.png: a row of layout thumbnails, each thumbnail split into the
//! cells of that layout. Hovering a cell previews it (the move grab draws the full-size zone
//! outline it already draws for edge snapping) and releasing there snaps the window into it.
//!
//! The thumbnails are drawn from [`SNAP_LAYOUTS`], the same data that produces the real
//! geometry, so a thumbnail cannot show a split the compositor would not actually apply.
//!
//! Hit testing deliberately does NOT go through iced. The grab owns the pointer during a move,
//! so it computes cell rectangles itself with [`SnapStrip::cell_at`] and merely tells this
//! element what to paint as hovered. That keeps picking in code the grab controls and leaves
//! iced doing only what it is good at here - drawing.

use crate::{
    shell::layout::floating::snap::{SNAP_LAYOUTS, SnapCell},
    utils::iced::{IcedElement, IcedRenderElement, Program},
};

use calloop::LoopHandle;
use cosmic::{
    Apply,
    iced::{
        Length,
        core::{Background, Border, Color},
        widget::{column, container, row, space},
    },
    theme,
};
use smithay::{
    backend::renderer::ImportMem,
    desktop::space::SpaceElement,
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
};

/// Size of one cell in a thumbnail, before the layout's aspect is applied.
const THUMB_H: f32 = 44.0;
/// Thumbnails are drawn at the screen's aspect ratio; this is the width at [`THUMB_H`] for a
/// 16:9 output, and is adjusted per output in [`SnapStrip::new`].
const THUMB_W: f32 = THUMB_H * 16.0 / 9.0;
/// Gap between the cells inside one thumbnail.
const CELL_GAP: f32 = 3.0;
/// Gap between thumbnails, and the padding around the whole strip.
const PAD: f32 = 12.0;

/// How close to the top edge the pointer has to come for the strip to drop down.
pub const REVEAL_HEIGHT: i32 = 8;

pub struct SnapStrip {
    elem: IcedElement<SnapStripInternal>,
    /// Where the strip sits, in output-local coordinates.
    geometry: Rectangle<i32, Logical>,
    /// Cell rectangles in the same space as `geometry`, parallel to `SNAP_LAYOUTS`.
    cells: Vec<Vec<Rectangle<i32, Logical>>>,
}

impl SnapStrip {
    /// Builds the strip for `output_size` and centres it against the top edge of `work_area`.
    pub fn new(
        evlh: LoopHandle<'static, crate::state::State>,
        work_area: Rectangle<i32, Logical>,
        mut theme: cosmic::Theme,
    ) -> SnapStrip {
        theme.transparent = theme.cosmic().frosted_system_interface;

        // Thumbnails mirror the screen's shape, so a layout reads the way it will land.
        let aspect = work_area.size.w as f32 / work_area.size.h.max(1) as f32;
        let thumb_h = THUMB_H;
        let thumb_w = (thumb_h * aspect).clamp(THUMB_H, THUMB_W * 1.5);

        let width = PAD + SNAP_LAYOUTS.iter().map(|_| thumb_w + PAD).sum::<f32>();
        let height = thumb_h + PAD * 2.0;
        let size = Size::<i32, Logical>::from((width.round() as i32, height.round() as i32));

        let elem = IcedElement::new(
            SnapStripInternal {
                hovered: None,
                thumb: (thumb_w, thumb_h),
            },
            size,
            evlh,
            theme,
        );

        let geometry = Rectangle::new(
            Point::from((
                work_area.loc.x + (work_area.size.w - size.w) / 2,
                work_area.loc.y,
            )),
            size,
        );

        // Mirror the widget's own arithmetic so hit testing lands on what is drawn. Kept next
        // to the view code on purpose - if one changes, the other has to change with it.
        let mut cells = Vec::with_capacity(SNAP_LAYOUTS.len());
        for (i, layout) in SNAP_LAYOUTS.iter().enumerate() {
            let origin_x = geometry.loc.x as f32 + PAD + i as f32 * (thumb_w + PAD);
            let origin_y = geometry.loc.y as f32 + PAD;
            cells.push(
                layout
                    .cells
                    .iter()
                    .map(|c| thumb_cell_rect(*c, origin_x, origin_y, thumb_w, thumb_h))
                    .collect(),
            );
        }

        SnapStrip {
            elem,
            geometry,
            cells,
        }
    }

    pub fn geometry(&self) -> Rectangle<i32, Logical> {
        self.geometry
    }

    /// The layout and cell under `point`, if any. `point` is output-local.
    pub fn cell_at(&self, point: Point<i32, Logical>) -> Option<(usize, usize)> {
        self.cells.iter().enumerate().find_map(|(l, cells)| {
            cells
                .iter()
                .position(|rect| rect.contains(point))
                .map(|c| (l, c))
        })
    }

    /// Paint `hovered` as the highlighted cell. Cheap when unchanged - the element only
    /// redraws when the value actually differs.
    pub fn set_hovered(&self, hovered: Option<(usize, usize)>) {
        if self.elem.with_program(|p| p.hovered) != hovered {
            self.elem.queue_message(Message::Hover(hovered));
        }
    }

    pub fn push_render_elements<R>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
        push_above: &mut dyn FnMut(IcedRenderElement<R>),
    ) where
        R: crate::backend::render::element::AsGlowRenderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        self.elem.push_render_elements(
            renderer,
            location,
            scale,
            alpha,
            self.elem
                .with_theme(|theme| theme.cosmic().radius_s())
                .map(|x| x.round() as u8),
            push_above,
            None,
        );
    }

    pub fn output_enter(&self, output: &Output) {
        self.elem
            .output_enter(output, Rectangle::default() /*unused*/);
    }

    pub fn output_leave(&self, output: &Output) {
        self.elem.output_leave(output);
    }
}

/// One cell's rectangle inside a thumbnail whose top-left is at `(ox, oy)`.
///
/// Shared by the hit-test table and the view below, so the two cannot disagree: the view lays
/// a cell out as a fixed-size box padded by half the gap, which is exactly this rectangle.
fn thumb_cell_rect(
    cell: SnapCell,
    ox: f32,
    oy: f32,
    thumb_w: f32,
    thumb_h: f32,
) -> Rectangle<i32, Logical> {
    let half = CELL_GAP / 2.0;
    let x0 = ox + (cell.x as f32 * thumb_w) + half;
    let y0 = oy + (cell.y as f32 * thumb_h) + half;
    let x1 = ox + ((cell.x + cell.w) as f32 * thumb_w) - half;
    let y1 = oy + ((cell.y + cell.h) as f32 * thumb_h) - half;
    Rectangle::new(
        Point::from((x0.round() as i32, y0.round() as i32)),
        Size::from(((x1 - x0).round() as i32, (y1 - y0).round() as i32)),
    )
}

#[derive(Debug, Clone, Copy)]
pub enum Message {
    Hover(Option<(usize, usize)>),
}

pub struct SnapStripInternal {
    hovered: Option<(usize, usize)>,
    thumb: (f32, f32),
}

impl Program for SnapStripInternal {
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
            Message::Hover(hovered) => self.hovered = hovered,
        }
        cosmic::iced::Task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let (thumb_w, thumb_h) = self.thumb;

        let thumbnails = SNAP_LAYOUTS
            .iter()
            .enumerate()
            .map(|(l, layout)| {
                // Cells are column-major (asserted by a test next to the data), so consecutive
                // cells sharing an x form one column and the thumbnail is a row of columns.
                let mut columns: Vec<cosmic::Element<'_, Message>> = Vec::new();
                let mut current: Option<(f64, Vec<cosmic::Element<'_, Message>>)> = None;

                for (c, cell) in layout.cells.iter().enumerate() {
                    let boxed = cell_box(*cell, thumb_w, thumb_h, self.hovered == Some((l, c)));
                    match &mut current {
                        Some((x, items)) if *x == cell.x => items.push(boxed),
                        _ => {
                            if let Some((_, items)) = current.take() {
                                columns.push(column(items).into());
                            }
                            current = Some((cell.x, vec![boxed]));
                        }
                    }
                }
                if let Some((_, items)) = current.take() {
                    columns.push(column(items).into());
                }

                row(columns)
                    .apply(container)
                    .width(Length::Fixed(thumb_w))
                    .height(Length::Fixed(thumb_h))
                    .into()
            })
            .collect::<Vec<cosmic::Element<'_, Message>>>();

        row(thumbnails)
            .spacing(PAD)
            .apply(container)
            .padding(PAD as u16)
            .class(theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                let mut background = cosmic.bg_color();
                if theme.transparent {
                    background.alpha = cosmic.alpha_map.blurred_alpha(cosmic.frosted);
                }
                container::Style {
                    snap: true,
                    icon_color: None,
                    text_color: None,
                    background: Some(Background::Color(background.into())),
                    border: Border {
                        radius: cosmic.radius_s().into(),
                        width: 1.0,
                        color: Color::from(cosmic.bg_divider()),
                    },
                    shadow: Default::default(),
                }
            }))
            .width(Length::Shrink)
            .height(Length::Shrink)
            .into()
    }
}

/// One cell of a thumbnail: a fixed-size box padded by half the gap, with the styled fill
/// inside it. The padding is what produces the gap, and it is what makes the drawn rectangle
/// identical to [`thumb_cell_rect`].
fn cell_box(
    cell: SnapCell,
    thumb_w: f32,
    thumb_h: f32,
    hovered: bool,
) -> cosmic::Element<'static, Message> {
    space::horizontal()
        .apply(container)
        .width(Length::Fill)
        .height(Length::Fill)
        .class(cell_style(hovered))
        .apply(container)
        .width(Length::Fixed(cell.w as f32 * thumb_w))
        .height(Length::Fixed(cell.h as f32 * thumb_h))
        .padding(cosmic::iced::Padding::from(CELL_GAP / 2.0))
        .into()
}

/// Neutral normally, accent-filled while hovered.
fn cell_style(hovered: bool) -> theme::Container<'static> {
    theme::Container::custom(move |theme| {
        let cosmic = theme.cosmic();
        let background = if hovered {
            cosmic.accent_color()
        } else {
            cosmic.bg_component_color()
        };
        container::Style {
            snap: true,
            icon_color: None,
            text_color: None,
            background: Some(Background::Color(background.into())),
            border: Border {
                radius: 2.0.into(),
                width: 1.0,
                color: Color::from(cosmic.bg_divider()),
            },
            shadow: Default::default(),
        }
    })
}
