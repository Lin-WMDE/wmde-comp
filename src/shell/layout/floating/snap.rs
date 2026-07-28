// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: snap layout templates for the drag-to-top layout strip.
//!
//! A layout is a list of cells expressed in *fractions* of the work area rather than as enum
//! variants. That is deliberate: the same numbers drive both the geometry a window is snapped
//! to and the thumbnail drawn for it in the strip, so the picture the user clicks and the
//! place the window lands cannot drift apart.
//!
//! [`SnapCell::relative_geometry`] reproduces [`super::TiledCorners::relative_geometry`]
//! exactly for the halves and quarters that already existed - there are tests for it below.
//! Adding the strip therefore does not quietly change where the existing edge snapping puts
//! windows.

use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::utils::prelude::*;

/// One cell of a layout, as a fraction of the work area.
///
/// `x`/`y` are the top-left corner and `w`/`h` the size, all in `0.0..=1.0`. Fractions rather
/// than pixels so a layout is independent of resolution and of the panel's exclusive zone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapCell {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl SnapCell {
    const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Where this cell lands on `work_area`, with the theme's gaps applied.
    ///
    /// The gap convention is the one [`super::TiledCorners`] uses: `inner` at the outer edges
    /// of the work area and `inner` between neighbouring cells. That falls out of insetting
    /// the work area by half a gap, placing the cell inside it by fraction, and then insetting
    /// the cell by half a gap again - two halves meet at every shared edge.
    pub fn relative_geometry(
        &self,
        work_area: Rectangle<i32, Logical>,
        gaps: (i32, i32),
    ) -> Rectangle<i32, Local> {
        let (_, inner) = gaps;
        let half = inner / 2;

        // The box the fractions are measured against.
        let area_x = work_area.loc.x + half;
        let area_y = work_area.loc.y + half;
        let area_w = work_area.size.w - inner;
        let area_h = work_area.size.h - inner;

        // Round the far edge from the fraction as well, instead of adding a rounded width to a
        // rounded origin: that keeps two cells sharing an edge from overlapping or leaving a
        // one-pixel seam when the fraction does not divide the area evenly.
        let x0 = area_x + (self.x * area_w as f64).round() as i32;
        let y0 = area_y + (self.y * area_h as f64).round() as i32;
        let x1 = area_x + ((self.x + self.w) * area_w as f64).round() as i32;
        let y1 = area_y + ((self.y + self.h) * area_h as f64).round() as i32;

        Rectangle::new(
            Point::from((x0 + half, y0 + half)),
            Size::from(((x1 - x0 - inner).max(1), (y1 - y0 - inner).max(1))),
        )
        .as_local()
    }
}

/// A layout offered in the strip: the cells a window can be dropped into.
#[derive(Debug, Clone, Copy)]
pub struct SnapLayout {
    /// Identifier used for the render element key, so each thumbnail gets a stable one.
    pub id: &'static str,
    pub cells: &'static [SnapCell],
}

/// The layouts the strip offers, left to right.
///
/// The first three are what the existing edge snapping already produces (halves and quarters);
/// the thirds and the composite are new, and are the ones ref/w11/win_place.png shows.
pub const SNAP_LAYOUTS: &[SnapLayout] = &[
    // Two columns.
    SnapLayout {
        id: "halves",
        cells: &[
            SnapCell::new(0.0, 0.0, 0.5, 1.0),
            SnapCell::new(0.5, 0.0, 0.5, 1.0),
        ],
    },
    // Four quarters.
    SnapLayout {
        id: "quarters",
        cells: &[
            SnapCell::new(0.0, 0.0, 0.5, 0.5),
            SnapCell::new(0.5, 0.0, 0.5, 0.5),
            SnapCell::new(0.0, 0.5, 0.5, 0.5),
            SnapCell::new(0.5, 0.5, 0.5, 0.5),
        ],
    },
    // One large left, two stacked right.
    SnapLayout {
        id: "left-big",
        cells: &[
            SnapCell::new(0.0, 0.0, 0.5, 1.0),
            SnapCell::new(0.5, 0.0, 0.5, 0.5),
            SnapCell::new(0.5, 0.5, 0.5, 0.5),
        ],
    },
    // Three columns.
    SnapLayout {
        id: "thirds",
        cells: &[
            SnapCell::new(0.0, 0.0, 1.0 / 3.0, 1.0),
            SnapCell::new(1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0),
            SnapCell::new(2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0),
        ],
    },
    // Wide middle between two narrow columns.
    SnapLayout {
        id: "wide-middle",
        cells: &[
            SnapCell::new(0.0, 0.0, 0.25, 1.0),
            SnapCell::new(0.25, 0.0, 0.5, 1.0),
            SnapCell::new(0.75, 0.0, 0.25, 1.0),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::layout::floating::TiledCorners;

    /// Offset so that an origin dropped somewhere in the arithmetic shows up instead of
    /// cancelling out, and evenly sized - see [`odd_area`].
    fn area() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((36, 20)), Size::from((1920, 1080)))
    }

    /// Odd height, where this module and `TiledCorners` legitimately disagree by a pixel.
    fn odd_area() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((37, 21)), Size::from((1920, 1053)))
    }

    const CORNER_CASES: &[(SnapCell, TiledCorners)] = &[
        (SnapCell::new(0.0, 0.0, 1.0, 0.5), TiledCorners::Top),
        (SnapCell::new(0.0, 0.5, 1.0, 0.5), TiledCorners::Bottom),
        (SnapCell::new(0.0, 0.0, 0.5, 1.0), TiledCorners::Left),
        (SnapCell::new(0.5, 0.0, 0.5, 1.0), TiledCorners::Right),
        (SnapCell::new(0.0, 0.0, 0.5, 0.5), TiledCorners::TopLeft),
        (SnapCell::new(0.5, 0.0, 0.5, 0.5), TiledCorners::TopRight),
        (SnapCell::new(0.0, 0.5, 0.5, 0.5), TiledCorners::BottomLeft),
        (SnapCell::new(0.5, 0.5, 0.5, 0.5), TiledCorners::BottomRight),
    ];

    /// The whole point of the fraction form is that it does not move windows that the existing
    /// edge snapping already places. If this fails, drag-to-edge behaviour changed.
    ///
    /// Even gaps only: `TiledCorners` mixes `inner / 2` and `inner * 3 / 2` in integer
    /// arithmetic, so an odd gap rounds differently there than anywhere else. The theme ships
    /// even values, and reproducing that bug-for-bug is not worth it.
    #[test]
    fn matches_tiled_corners() {
        for inner in [0, 2, 4, 8, 16] {
            for (cell, corner) in CORNER_CASES {
                assert_eq!(
                    cell.relative_geometry(area(), (0, inner)),
                    corner.relative_geometry(area(), (0, inner)),
                    "cell {cell:?} vs {corner:?} at gap {inner}"
                );
            }
        }
    }

    /// On an odd-sized work area the two disagree by exactly one pixel, and the disagreement
    /// is ours to keep: `TiledCorners` derives a half from `h / 2`, which truncates and leaves
    /// the last row unused, while a cell is measured from its far edge so the halves meet and
    /// the area is filled. Measuring from the far edge is what keeps the thirds - whose
    /// fractions never divide evenly - from overlapping or leaving a seam.
    #[test]
    fn differs_from_tiled_corners_by_at_most_a_pixel_when_odd() {
        for inner in [0, 2, 4, 8, 16] {
            for (cell, corner) in CORNER_CASES {
                let ours = cell.relative_geometry(odd_area(), (0, inner));
                let theirs = corner.relative_geometry(odd_area(), (0, inner));
                for (a, b, what) in [
                    (ours.loc.x, theirs.loc.x, "x"),
                    (ours.loc.y, theirs.loc.y, "y"),
                    (ours.size.w, theirs.size.w, "width"),
                    (ours.size.h, theirs.size.h, "height"),
                ] {
                    assert!(
                        (a - b).abs() <= 1,
                        "{what} of {cell:?} vs {corner:?} at gap {inner}: {a} vs {b}"
                    );
                }
            }
        }
    }

    /// Cells of one layout must not overlap, must stay inside the work area, and must leave
    /// the gap between them - including the thirds, where the fractions do not divide evenly.
    #[test]
    fn cells_tile_without_overlap() {
        for work_area in [area(), odd_area()] {
            for inner in [0, 2, 4, 8, 16] {
                for layout in SNAP_LAYOUTS {
                    let rects: Vec<_> = layout
                        .cells
                        .iter()
                        .map(|c| c.relative_geometry(work_area, (0, inner)))
                        .collect();

                    for (i, a) in rects.iter().enumerate() {
                        assert!(
                            a.loc.x >= work_area.loc.x + inner
                                && a.loc.y >= work_area.loc.y + inner
                                && a.loc.x + a.size.w <= work_area.loc.x + work_area.size.w - inner
                                && a.loc.y + a.size.h <= work_area.loc.y + work_area.size.h - inner,
                            "{} cell {i} escapes the work area at gap {inner}: {a:?}",
                            layout.id
                        );
                        for (j, b) in rects.iter().enumerate().skip(i + 1) {
                            assert!(
                                a.intersection(*b).is_none(),
                                "{} cells {i} and {j} overlap at gap {inner}: {a:?} {b:?}",
                                layout.id
                            );
                        }
                    }
                }
            }
        }
    }
}
