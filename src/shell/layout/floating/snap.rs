// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: snap layout templates for the drag-to-top layout strip.
//!
//! A layout is a list of cells expressed in *fractions* of the work area rather than as enum
//! variants. That is deliberate: the same numbers drive both the geometry a window is snapped
//! to and the thumbnail drawn for it in the strip, so the picture the user clicks and the
//! place the window lands cannot drift apart.
//!
//! [`TiledCorners`] - the snapped states that already existed - is expressed as cells too and
//! computes its geometry through here, so drag-to-edge snapping and the strip cannot place the
//! same half of the screen in two different rectangles.
//!
//! A snapped window sits **flush** against the edge of the work area; the theme's gap goes
//! between neighbouring windows only. There is nothing between a window and the screen for a
//! gap to separate, and it is what lets the corners that touch the edge be squared.

use smithay::utils::{Logical, Point, Rectangle, Size};

use super::TiledCorners;

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
    /// Flush with the work area on any edge the cell shares with it, and separated from a
    /// neighbouring cell by the theme's `inner` gap.
    pub fn relative_geometry(
        &self,
        work_area: Rectangle<i32, Logical>,
        gaps: (i32, i32),
    ) -> Rectangle<i32, Local> {
        let (_, inner) = gaps;
        let half = inner / 2;

        // Round the far edge from the fraction as well, instead of adding a rounded width to a
        // rounded origin: that keeps two cells sharing an edge from overlapping or leaving a
        // one-pixel seam when the fraction does not divide the area evenly.
        let edge = |frac: f64, len: i32| (frac * len as f64).round() as i32;
        let x0 = work_area.loc.x + edge(self.x, work_area.size.w);
        let y0 = work_area.loc.y + edge(self.y, work_area.size.h);
        let x1 = work_area.loc.x + edge(self.x + self.w, work_area.size.w);
        let y1 = work_area.loc.y + edge(self.y + self.h, work_area.size.h);

        // The gap goes between windows, never between a window and the screen: a snapped
        // window sits flush against the edge of the work area. Each internal edge gives up
        // half a gap and its neighbour gives up the other half, so the gap between two windows
        // comes out as `inner`.
        let at_start = |frac: f64| frac.abs() < f64::EPSILON;
        let at_end = |frac: f64| (frac - 1.0).abs() < f64::EPSILON;
        let left = if at_start(self.x) { 0 } else { half };
        let top = if at_start(self.y) { 0 } else { half };
        let right = if at_end(self.x + self.w) { 0 } else { half };
        let bottom = if at_end(self.y + self.h) { 0 } else { half };

        Rectangle::new(
            Point::from((x0 + left, y0 + top)),
            Size::from((
                (x1 - x0 - left - right).max(1),
                (y1 - y0 - top - bottom).max(1),
            )),
        )
        .as_local()
    }

    /// The [`TiledCorners`] this cell is the same rectangle as, if there is one.
    ///
    /// Halves and quarters already exist as snapped states, so dropping a window on one of
    /// those cells goes through the same path as dragging it to that edge - same recorded
    /// state, same restore behaviour. Thirds and the composite layouts have no equivalent.
    ///
    /// Derived from [`TiledCorners::as_cell`] rather than written out again: a new snapped
    /// state only has to be described in one place.
    pub fn as_tiled_corner(&self) -> Option<TiledCorners> {
        const ALL: [TiledCorners; 8] = [
            TiledCorners::Top,
            TiledCorners::Bottom,
            TiledCorners::Left,
            TiledCorners::Right,
            TiledCorners::TopLeft,
            TiledCorners::TopRight,
            TiledCorners::BottomLeft,
            TiledCorners::BottomRight,
        ];
        ALL.into_iter().find(|corner| corner.as_cell() == *self)
    }
}

impl TiledCorners {
    /// The cell this snapped state is, so both paths compute one geometry.
    pub const fn as_cell(self) -> SnapCell {
        match self {
            TiledCorners::Top => SnapCell::new(0.0, 0.0, 1.0, 0.5),
            TiledCorners::Bottom => SnapCell::new(0.0, 0.5, 1.0, 0.5),
            TiledCorners::Left => SnapCell::new(0.0, 0.0, 0.5, 1.0),
            TiledCorners::Right => SnapCell::new(0.5, 0.0, 0.5, 1.0),
            TiledCorners::TopLeft => SnapCell::new(0.0, 0.0, 0.5, 0.5),
            TiledCorners::TopRight => SnapCell::new(0.5, 0.0, 0.5, 0.5),
            TiledCorners::BottomLeft => SnapCell::new(0.0, 0.5, 0.5, 0.5),
            TiledCorners::BottomRight => SnapCell::new(0.5, 0.5, 0.5, 0.5),
        }
    }

    /// Where this snapped state lands on `work_area`.
    ///
    /// Delegates to [`SnapCell::relative_geometry`] so drag-to-edge snapping and the layout
    /// strip cannot place the same half of the screen in two different rectangles.
    pub fn relative_geometry(
        self,
        work_area: Rectangle<i32, Logical>,
        gaps: (i32, i32),
    ) -> Rectangle<i32, Local> {
        self.as_cell().relative_geometry(work_area, gaps)
    }
}

/// A layout offered in the strip: the cells a window can be dropped into.
///
/// Cells are listed **column-major** - all the cells of the leftmost column top to bottom,
/// then the next column. Nothing about the geometry depends on the order, but the strip builds
/// its thumbnails as a row of columns by grouping consecutive cells that share an `x`, so a
/// layout listed row-major would draw wrong. [`layouts_are_column_major`] asserts it.
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
            SnapCell::new(0.0, 0.5, 0.5, 0.5),
            SnapCell::new(0.5, 0.0, 0.5, 0.5),
            SnapCell::new(0.5, 0.5, 0.5, 0.5),
        ],
    },
    // Two columns, the left one twice the width of the right. Measured off the reference:
    // 63px against 31px in a 98px group.
    SnapLayout {
        id: "two-thirds",
        cells: &[
            SnapCell::new(0.0, 0.0, 2.0 / 3.0, 1.0),
            SnapCell::new(2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0),
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

    /// Offset so that an origin dropped somewhere in the arithmetic shows up instead of
    /// cancelling out; one even and one odd size, because the fractions divide differently.
    const AREAS: [(i32, i32, i32, i32); 2] = [(36, 20, 1920, 1080), (37, 21, 1920, 1053)];
    const GAPS: [i32; 5] = [0, 2, 4, 8, 16];

    fn areas() -> impl Iterator<Item = Rectangle<i32, Logical>> {
        AREAS
            .into_iter()
            .map(|(x, y, w, h)| Rectangle::new(Point::from((x, y)), Size::from((w, h))))
    }

    /// A snapped window touches the screen. The gap is for the space between two windows -
    /// there is nothing between a window and the edge of the work area for it to separate,
    /// and it is what lets the corners that touch the edge be squared.
    #[test]
    fn cells_are_flush_with_the_work_area() {
        for work_area in areas() {
            for inner in GAPS {
                for layout in SNAP_LAYOUTS {
                    for cell in layout.cells {
                        let geo = cell.relative_geometry(work_area, (0, inner));
                        let area = work_area.as_local();
                        if cell.x == 0.0 {
                            assert_eq!(geo.loc.x, area.loc.x, "{} left edge", layout.id);
                        }
                        if cell.y == 0.0 {
                            assert_eq!(geo.loc.y, area.loc.y, "{} top edge", layout.id);
                        }
                        if cell.x + cell.w == 1.0 {
                            assert_eq!(
                                geo.loc.x + geo.size.w,
                                area.loc.x + area.size.w,
                                "{} right edge",
                                layout.id
                            );
                        }
                        if cell.y + cell.h == 1.0 {
                            assert_eq!(
                                geo.loc.y + geo.size.h,
                                area.loc.y + area.size.h,
                                "{} bottom edge",
                                layout.id
                            );
                        }
                    }
                }
            }
        }
    }

    /// Cells of one layout must not overlap and must stay inside the work area - including the
    /// thirds, where the fractions do not divide evenly.
    #[test]
    fn cells_tile_without_overlap() {
        for work_area in areas() {
            for inner in GAPS {
                for layout in SNAP_LAYOUTS {
                    let rects: Vec<_> = layout
                        .cells
                        .iter()
                        .map(|c| c.relative_geometry(work_area, (0, inner)))
                        .collect();
                    let area = work_area.as_local();

                    for (i, a) in rects.iter().enumerate() {
                        assert!(
                            a.loc.x >= area.loc.x
                                && a.loc.y >= area.loc.y
                                && a.loc.x + a.size.w <= area.loc.x + area.size.w
                                && a.loc.y + a.size.h <= area.loc.y + area.size.h,
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

    /// Two windows sharing an internal edge are separated by the whole gap - each gives up
    /// half of it.
    #[test]
    fn neighbours_are_separated_by_the_gap() {
        for work_area in areas() {
            for inner in GAPS {
                let left =
                    SnapCell::new(0.0, 0.0, 0.5, 1.0).relative_geometry(work_area, (0, inner));
                let right =
                    SnapCell::new(0.5, 0.0, 0.5, 1.0).relative_geometry(work_area, (0, inner));
                assert_eq!(
                    right.loc.x - (left.loc.x + left.size.w),
                    inner - inner % 2,
                    "gap between halves at inner={inner}"
                );
            }
        }
    }

    /// Every snapped state that already existed still has exactly one cell describing it, and
    /// a cell that is one reports itself as such - dropping on the strip must leave a window in
    /// the same internal state a drag to that edge does.
    #[test]
    fn tiled_corners_round_trip() {
        for corner in [
            TiledCorners::Top,
            TiledCorners::Bottom,
            TiledCorners::Left,
            TiledCorners::Right,
            TiledCorners::TopLeft,
            TiledCorners::TopRight,
            TiledCorners::BottomLeft,
            TiledCorners::BottomRight,
        ] {
            assert_eq!(corner.as_cell().as_tiled_corner(), Some(corner));
        }
        // A third is not any of them.
        assert_eq!(
            SnapCell::new(1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0).as_tiled_corner(),
            None
        );
    }

    /// The strip draws a thumbnail as a row of columns, grouping consecutive cells that share
    /// an `x`. A layout listed row-major would still snap correctly but would draw wrong, and
    /// that is the sort of thing nobody notices until it is on screen.
    #[test]
    fn layouts_are_column_major() {
        for layout in SNAP_LAYOUTS {
            let mut seen: Vec<f64> = Vec::new();
            let mut current = f64::NAN;
            for cell in layout.cells {
                if cell.x != current {
                    assert!(
                        !seen.contains(&cell.x),
                        "{} returns to column {} after leaving it",
                        layout.id,
                        cell.x
                    );
                    seen.push(cell.x);
                    current = cell.x;
                }
            }
        }
    }
}
