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
//! A snapped window sits **flush** against the edge of the work area and against its
//! neighbours: snapped windows meet edge to edge, as they do on Windows 11. The theme's gaps
//! belong to the tiling layout and are not applied here.

use smithay::utils::{Logical, Point, Rectangle, Size};

use super::TiledCorners;

use crate::{shell::grabs::ResizeEdge, utils::prelude::*};

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

    /// Where this cell lands on `work_area`.
    ///
    /// Flush with the work area on every edge it shares with it, and edge to edge with a
    /// neighbouring cell. `_gaps` is not applied: it stays in the signature so the call sites
    /// keep upstream's shape and merges stay cheap.
    pub fn relative_geometry(
        &self,
        work_area: Rectangle<i32, Logical>,
        _gaps: (i32, i32),
    ) -> Rectangle<i32, Local> {
        // Round the far edge from the fraction as well, instead of adding a rounded width to a
        // rounded origin: that keeps two cells sharing an edge from overlapping or leaving a
        // one-pixel seam when the fraction does not divide the area evenly.
        let edge = |frac: f64, len: i32| (frac * len as f64).round() as i32;
        let x0 = work_area.loc.x + edge(self.x, work_area.size.w);
        let y0 = work_area.loc.y + edge(self.y, work_area.size.h);
        let x1 = work_area.loc.x + edge(self.x + self.w, work_area.size.w);
        let y1 = work_area.loc.y + edge(self.y + self.h, work_area.size.h);

        Rectangle::new(
            Point::from((x0, y0)),
            Size::from(((x1 - x0).max(1), (y1 - y0).max(1))),
        )
        .as_local()
    }

    /// This cell after the window was resized to `rect` by dragging `edges`.
    ///
    /// Only the fractions of the moved edges are taken from `rect`; the others are kept as
    /// they were. Rebuilding the whole cell from pixels would turn an exact `0.5` into
    /// `526/1053` on an odd-sized work area, and the cell would stop being a half
    /// ([`Self::as_tiled_corner`]) for nothing but a click on its edge.
    pub fn with_moved_edges(
        self,
        edges: ResizeEdge,
        rect: Rectangle<i32, Local>,
        work_area: Rectangle<i32, Logical>,
    ) -> SnapCell {
        // The fraction of an edge now at `px`; the old one while it still lands on that pixel,
        // so an edge that did not move keeps its exact fraction.
        let refit = |old: f64, px: i32, origin: i32, len: i32| {
            if origin + (old * f64::from(len)).round() as i32 == px {
                old
            } else {
                f64::from(px - origin) / f64::from(len)
            }
        };
        let (ox, oy) = (work_area.loc.x, work_area.loc.y);
        let (w, h) = (work_area.size.w, work_area.size.h);
        let mut cell = self;
        if edges.contains(ResizeEdge::LEFT) {
            let right = cell.x + cell.w;
            cell.x = refit(cell.x, rect.loc.x, ox, w);
            cell.w = right - cell.x;
        } else if edges.contains(ResizeEdge::RIGHT) {
            cell.w = refit(cell.x + cell.w, rect.loc.x + rect.size.w, ox, w) - cell.x;
        }
        if edges.contains(ResizeEdge::TOP) {
            let bottom = cell.y + cell.h;
            cell.y = refit(cell.y, rect.loc.y, oy, h);
            cell.h = bottom - cell.y;
        } else if edges.contains(ResizeEdge::BOTTOM) {
            cell.h = refit(cell.y + cell.h, rect.loc.y + rect.size.h, oy, h) - cell.y;
        }
        cell
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
/// then the next column, columns left to right. Nothing about the geometry depends on the
/// order, but the strip builds its thumbnails as a row of columns by grouping consecutive
/// cells that share an `x` and positions them in list order, so a layout listed row-major, or
/// with its columns out of order, would draw wrong. [`layouts_are_column_major`] asserts it.
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
    // Two rows: the halves turned on their side. Not on the reference, added on request.
    SnapLayout {
        id: "rows",
        cells: &[
            SnapCell::new(0.0, 0.0, 1.0, 0.5),
            SnapCell::new(0.0, 0.5, 1.0, 0.5),
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

    /// A snapped window touches the screen, and it is what lets the corners that touch the
    /// edge be squared.
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

    /// Two snapped windows meet edge to edge whatever gap the theme sets - the gap is the
    /// tiling layout's, not the snapping's.
    #[test]
    fn neighbours_meet_edge_to_edge() {
        for work_area in areas() {
            for inner in GAPS {
                let left =
                    SnapCell::new(0.0, 0.0, 0.5, 1.0).relative_geometry(work_area, (0, inner));
                let right =
                    SnapCell::new(0.5, 0.0, 0.5, 1.0).relative_geometry(work_area, (0, inner));
                assert_eq!(
                    right.loc.x,
                    left.loc.x + left.size.w,
                    "seam between halves at inner={inner}"
                );
                let top =
                    SnapCell::new(0.0, 0.0, 1.0, 0.5).relative_geometry(work_area, (0, inner));
                let bottom =
                    SnapCell::new(0.0, 0.5, 1.0, 0.5).relative_geometry(work_area, (0, inner));
                assert_eq!(
                    bottom.loc.y,
                    top.loc.y + top.size.h,
                    "seam between rows at inner={inner}"
                );
            }
        }
    }

    /// The cells of a layout cover the work area completely. With
    /// [`cells_tile_without_overlap`] holding, equal total area means there is no seam left
    /// anywhere - including between the thirds, where the fractions do not divide evenly.
    #[test]
    fn cells_cover_the_work_area() {
        for work_area in areas() {
            for inner in GAPS {
                for layout in SNAP_LAYOUTS {
                    let covered: i32 = layout
                        .cells
                        .iter()
                        .map(|c| {
                            let geo = c.relative_geometry(work_area, (0, inner));
                            geo.size.w * geo.size.h
                        })
                        .sum();
                    assert_eq!(
                        covered,
                        work_area.size.w * work_area.size.h,
                        "{} leaves a seam at gap {inner}",
                        layout.id
                    );
                }
            }
        }
    }

    /// Moving one edge leaves the other fractions exactly as they were, and the moved edge
    /// lands back on the pixel it was moved to.
    #[test]
    fn moved_edges_keep_the_rest_of_the_cell() {
        for work_area in areas() {
            let left = SnapCell::new(0.0, 0.0, 0.5, 1.0);
            let right = SnapCell::new(0.5, 0.0, 0.5, 1.0);
            let mut geo = left.relative_geometry(work_area, (0, 0));
            geo.size.w += 137;
            let moved = left.with_moved_edges(ResizeEdge::RIGHT, geo, work_area);
            assert_eq!((moved.x, moved.y, moved.h), (0.0, 0.0, 1.0));
            assert_eq!(moved.relative_geometry(work_area, (0, 0)), geo);

            let mut geo = right.relative_geometry(work_area, (0, 0));
            geo.loc.x += 137;
            geo.size.w -= 137;
            let moved = right.with_moved_edges(ResizeEdge::LEFT, geo, work_area);
            assert_eq!((moved.y, moved.h), (0.0, 1.0));
            assert_eq!(
                moved.x + moved.w,
                1.0,
                "the far edge stays on the screen edge"
            );
            assert_eq!(moved.relative_geometry(work_area, (0, 0)), geo);

            // A click without movement changes nothing, on an odd height too - the quarter
            // stays a quarter.
            for corner in [TiledCorners::TopLeft, TiledCorners::BottomRight] {
                for edges in [ResizeEdge::TOP_LEFT, ResizeEdge::BOTTOM_RIGHT] {
                    let cell = corner.as_cell();
                    let geo = cell.relative_geometry(work_area, (0, 0));
                    let same = cell.with_moved_edges(edges, geo, work_area);
                    assert_eq!(same, cell, "{corner:?} {edges:?}");
                    assert_eq!(same.as_tiled_corner(), Some(corner));
                }
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
    ///
    /// The thumbnail also *positions* by list order - columns left to right as they are
    /// listed, cells top to bottom inside one - while the geometry comes from the fractions.
    /// A layout whose columns are listed out of order would snap correctly and draw a
    /// different picture, which is exactly the drift the fractions exist to prevent, so the
    /// list order has to be the canonical one: `x` strictly increasing from column to column,
    /// `y` strictly increasing inside a column.
    #[test]
    fn layouts_are_column_major() {
        for layout in SNAP_LAYOUTS {
            let mut seen: Vec<f64> = Vec::new();
            let mut current = f64::NAN;
            let mut last_y = f64::NAN;
            for cell in layout.cells {
                if cell.x != current {
                    assert!(
                        !seen.contains(&cell.x),
                        "{} returns to column {} after leaving it",
                        layout.id,
                        cell.x
                    );
                    if let Some(previous) = seen.last() {
                        assert!(
                            cell.x > *previous,
                            "{} lists column {} after column {previous}, so the strip would \
                             draw them the other way round",
                            layout.id,
                            cell.x
                        );
                    }
                    seen.push(cell.x);
                    current = cell.x;
                } else {
                    assert!(
                        cell.y > last_y,
                        "{} lists cell y={} after y={last_y} in column {}, so the strip would \
                         stack them the other way round",
                        layout.id,
                        cell.y,
                        cell.x
                    );
                }
                last_y = cell.y;
            }
        }
    }
}
