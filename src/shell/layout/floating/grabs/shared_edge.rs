// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: resizing snapped windows along the edge they share, as Windows 11 does.
//!
//! Dragging an edge of a snapped window moves the whole *line* that edge lies on: every
//! snapped window with an edge on it follows, on both sides. Two halves resize together, and
//! in a 2x2 grid all four quarters do, so the grid stays aligned.
//!
//! Everything here works on plain rectangles, so the rules are testable without a compositor;
//! [`super::ResizeSurfaceGrab`] applies the result to the windows.

use smithay::utils::{Point, Rectangle};

use crate::{shell::grabs::ResizeEdge, utils::prelude::*};

/// A window that moves with the dragged line, and which of its edges lie on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    /// Index into the rectangles passed to [`select`].
    pub idx: usize,
    pub edges: ResizeEdge,
}

/// The windows that move when `edges` of `rects[grabbed]` are dragged.
///
/// `rects` are the snapped windows of one output, the grabbed one included. Each axis is
/// decided on its own: the dragged edge gives a line, and the axis is *shared* when some
/// window has its opposite edge on that line - a window across it. Then every window with an
/// edge on the line is a member. An axis that is not shared moves the grabbed window alone.
///
/// `None` when neither axis is shared: the resize is the ordinary one-window resize.
pub fn select(
    rects: &[Rectangle<i32, Local>],
    grabbed: usize,
    edges: ResizeEdge,
) -> Option<Vec<Member>> {
    let own = *rects.get(grabbed)?;
    let mut member_edges = vec![ResizeEdge::empty(); rects.len()];
    let mut shared = false;

    for horizontal in [true, false] {
        let (start, end) = if horizontal {
            (ResizeEdge::LEFT, ResizeEdge::RIGHT)
        } else {
            (ResizeEdge::TOP, ResizeEdge::BOTTOM)
        };
        let dragged = if edges.contains(start) {
            start
        } else if edges.contains(end) {
            end
        } else {
            continue;
        };
        let line = edge_position(own, dragged);
        let across = if dragged == start { end } else { start };

        if rects.iter().any(|r| edge_position(*r, across) == line) {
            shared = true;
            for (i, rect) in rects.iter().enumerate() {
                for edge in [start, end] {
                    if edge_position(*rect, edge) == line {
                        member_edges[i] |= edge;
                    }
                }
            }
        } else {
            member_edges[grabbed] |= dragged;
        }
    }

    shared.then(|| {
        member_edges
            .into_iter()
            .enumerate()
            .filter(|(_, edges)| !edges.is_empty())
            .map(|(idx, edges)| Member { idx, edges })
            .collect()
    })
}

/// The offsets of the line along one axis that keep `rect` between `min` and `max` along it.
///
/// The range always contains 0: a window that is already outside its limits - a third of a
/// narrow screen below the default minimum - must not make the line jump on the first motion,
/// it only stops the line from going further the wrong way. An axis `edges` has nothing on
/// does not constrain the offset at all.
pub fn delta_range(
    rect: Rectangle<i32, Local>,
    edges: ResizeEdge,
    horizontal: bool,
    min: i32,
    max: i32,
) -> (i32, i32) {
    let (len, start, end) = if horizontal {
        (rect.size.w, ResizeEdge::LEFT, ResizeEdge::RIGHT)
    } else {
        (rect.size.h, ResizeEdge::TOP, ResizeEdge::BOTTOM)
    };
    let (lo, hi) = if edges.contains(start) {
        // The line is this window's start edge: moving it by `d` makes the window `len - d`.
        (len.saturating_sub(max), len.saturating_sub(min))
    } else if edges.contains(end) {
        // The line is this window's end edge: moving it by `d` makes the window `len + d`.
        (min.saturating_sub(len), max.saturating_sub(len))
    } else {
        return (i32::MIN, i32::MAX);
    };
    (lo.min(0), hi.max(0))
}

/// `d` limited to the offsets every range allows.
///
/// Each range from [`delta_range`] contains 0, so their intersection is never empty.
pub fn clamp(d: i32, ranges: impl IntoIterator<Item = (i32, i32)>) -> i32 {
    let (lo, hi) = ranges
        .into_iter()
        .fold((i32::MIN, i32::MAX), |(lo, hi), (a, b)| {
            (lo.max(a), hi.min(b))
        });
    d.clamp(lo, hi)
}

/// `rect` with the line moved by `d`: the edges on the line move, the others stay.
pub fn apply(
    rect: Rectangle<i32, Local>,
    edges: ResizeEdge,
    d: Point<i32, Local>,
) -> Rectangle<i32, Local> {
    let mut rect = rect;
    if edges.contains(ResizeEdge::LEFT) {
        rect.loc.x += d.x;
        rect.size.w -= d.x;
    } else if edges.contains(ResizeEdge::RIGHT) {
        rect.size.w += d.x;
    }
    if edges.contains(ResizeEdge::TOP) {
        rect.loc.y += d.y;
        rect.size.h -= d.y;
    } else if edges.contains(ResizeEdge::BOTTOM) {
        rect.size.h += d.y;
    }
    rect
}

/// The pixel coordinate of one edge of `rect`.
fn edge_position(rect: Rectangle<i32, Local>, edge: ResizeEdge) -> i32 {
    match edge {
        ResizeEdge::LEFT => rect.loc.x,
        ResizeEdge::RIGHT => rect.loc.x + rect.size.w,
        ResizeEdge::TOP => rect.loc.y,
        ResizeEdge::BOTTOM => rect.loc.y + rect.size.h,
        _ => unreachable!("edge_position takes a single edge"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::{Logical, Size};

    use crate::shell::layout::floating::snap::SnapCell;

    const L: ResizeEdge = ResizeEdge::LEFT;
    const R: ResizeEdge = ResizeEdge::RIGHT;
    const T: ResizeEdge = ResizeEdge::TOP;
    const B: ResizeEdge = ResizeEdge::BOTTOM;

    /// An odd height, so a half does not divide evenly.
    fn work_area() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((0, 0)), Size::from((1920, 1053)))
    }

    /// The rectangles a set of snapped cells lands in - the same ones the windows get.
    fn cells(cells: &[(f64, f64, f64, f64)]) -> Vec<Rectangle<i32, Local>> {
        cells
            .iter()
            .map(|&(x, y, w, h)| SnapCell { x, y, w, h }.relative_geometry(work_area(), (0, 8)))
            .collect()
    }

    fn halves() -> Vec<Rectangle<i32, Local>> {
        cells(&[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)])
    }

    /// Top-left, bottom-left, top-right, bottom-right.
    fn quarters() -> Vec<Rectangle<i32, Local>> {
        cells(&[
            (0.0, 0.0, 0.5, 0.5),
            (0.0, 0.5, 0.5, 0.5),
            (0.5, 0.0, 0.5, 0.5),
            (0.5, 0.5, 0.5, 0.5),
        ])
    }

    fn members(list: &[(usize, ResizeEdge)]) -> Option<Vec<Member>> {
        Some(
            list.iter()
                .map(|&(idx, edges)| Member { idx, edges })
                .collect(),
        )
    }

    #[test]
    fn halves_move_together() {
        assert_eq!(select(&halves(), 0, R), members(&[(0, R), (1, L)]));
        // Grabbing the other side of the same line gives the same group.
        assert_eq!(select(&halves(), 1, L), members(&[(0, R), (1, L)]));
    }

    #[test]
    fn an_edge_with_nothing_across_is_a_solo_resize() {
        // The left half's left edge is the screen edge.
        assert_eq!(select(&halves(), 0, L), None);
        // A lone snapped window.
        assert_eq!(select(&halves()[..1], 0, R), None);
    }

    #[test]
    fn thirds_move_only_the_two_on_the_line() {
        let thirds = cells(&[
            (0.0, 0.0, 1.0 / 3.0, 1.0),
            (1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0),
            (2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0),
        ]);
        assert_eq!(select(&thirds, 1, R), members(&[(1, R), (2, L)]));
        assert_eq!(select(&thirds, 1, L), members(&[(0, R), (1, L)]));
    }

    #[test]
    fn the_big_left_window_moves_both_on_the_right() {
        let left_big = cells(&[
            (0.0, 0.0, 0.5, 1.0),
            (0.5, 0.0, 0.5, 0.5),
            (0.5, 0.5, 0.5, 0.5),
        ]);
        assert_eq!(select(&left_big, 0, R), members(&[(0, R), (1, L), (2, L)]));
        // The line between the two on the right does not reach the left window.
        assert_eq!(select(&left_big, 1, B), members(&[(1, B), (2, T)]));
    }

    /// The user's decision: in a grid the whole line moves, not only the pair beside the grab.
    #[test]
    fn a_grid_moves_the_whole_line() {
        assert_eq!(
            select(&quarters(), 0, R),
            members(&[(0, R), (1, R), (2, L), (3, L)])
        );
        assert_eq!(
            select(&quarters(), 0, B),
            members(&[(0, B), (1, T), (2, B), (3, T)])
        );
    }

    #[test]
    fn a_corner_drag_moves_both_lines() {
        assert_eq!(
            select(&quarters(), 0, R | B),
            members(&[(0, R | B), (1, R | T), (2, L | B), (3, L | T)])
        );
        // Only one axis shared: the grabbed window still follows the other one alone.
        assert_eq!(select(&halves(), 0, R | B), members(&[(0, R | B), (1, L)]));
    }

    #[test]
    fn the_range_contains_zero() {
        let narrow = Rectangle::new(Point::from((0, 0)), Size::from((341, 1053)));
        let (lo, hi) = delta_range(narrow, R, true, 360, i32::MAX);
        assert!(lo <= 0 && 0 <= hi, "({lo}, {hi})");
        // It may still grow.
        assert_eq!(hi, i32::MAX - 341);
    }

    #[test]
    fn the_line_stops_at_a_neighbours_minimum() {
        let rects = halves();
        let ranges = [
            delta_range(rects[0], R, true, 360, i32::MAX),
            delta_range(rects[1], L, true, 360, i32::MAX),
        ];
        assert_eq!(clamp(10_000, ranges), 960 - 360);
        assert_eq!(clamp(-10_000, ranges), -(960 - 360));
        assert_eq!(clamp(25, ranges), 25);
    }

    #[test]
    fn an_axis_without_edges_is_unconstrained() {
        assert_eq!(
            delta_range(halves()[0], R, false, 240, 300),
            (i32::MIN, i32::MAX)
        );
    }

    /// Both sides of a moved line land on one pixel - no seam, no overlap - and the far edges
    /// stay where they were.
    #[test]
    fn both_sides_of_the_line_meet() {
        let rects = quarters();
        let group = select(&rects, 0, R | B).unwrap();
        let d = Point::from((137, -61));
        let moved: Vec<_> = group
            .iter()
            .map(|m| apply(rects[m.idx], m.edges, d))
            .collect();
        let (tl, bl, tr, br) = (moved[0], moved[1], moved[2], moved[3]);
        assert_eq!(tl.loc.x + tl.size.w, tr.loc.x);
        assert_eq!(bl.loc.x + bl.size.w, br.loc.x);
        assert_eq!(tl.loc.y + tl.size.h, bl.loc.y);
        assert_eq!(tr.loc.y + tr.size.h, br.loc.y);
        assert_eq!(tl.loc, rects[0].loc);
        assert_eq!(br.loc.x + br.size.w, rects[3].loc.x + rects[3].size.w);
        assert_eq!(br.loc.y + br.size.h, rects[3].loc.y + rects[3].size.h);
    }
}
