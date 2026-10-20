// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: where the window switcher puts its cells. Pure geometry with no compositor state, so
//! the arithmetic is tested on its own.
//!
//! First look: ref/alt-tab_large.png (Windows 10 TP 9841). Every thumbnail has one height and a
//! width from its window's aspect; rows are filled greedily and centred, the last one too, and
//! the grid is centred on the output. The height starts at a share of the output and shrinks
//! until the grid fits the width and height limits.

use std::ops::Range;

use cosmic_settings_config::shortcuts::Modifiers;
use smithay::utils::{Logical, Point, Rectangle, Size};

/// Thumbnail height as a share of the output height (144 of 750 on the reference).
const THUMB_SHARE: f64 = 0.19;
/// Gap between cells, across and down, as a share of the output width (15 of 1200).
const GAP_SHARE: f64 = 0.0125;
/// The widest row and the whole grid, as shares of the output.
const MAX_WIDTH_SHARE: f64 = 0.9;
const MAX_HEIGHT_SHARE: f64 = 0.7;
/// Window aspects outside this range are letterboxed into it, so a sliver of a window still
/// gets a cell that can be aimed at and a panorama does not take a whole row.
const MIN_ASPECT: f64 = 0.5;
const MAX_ASPECT: f64 = 2.5;

/// Height of the icon and title line above a thumbnail; the icon is this size.
pub const HEADER: i32 = 24;
/// Between the header and the thumbnail.
pub const HEADER_GAP: i32 = 4;
/// Between the selection frame and what it outlines.
pub const PAD: i32 = 4;
/// The grid stops shrinking here and overflows instead.
pub const MIN_THUMB: i32 = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridParams {
    /// Starting thumbnail height.
    pub thumb: i32,
    pub min_thumb: i32,
    pub gap: i32,
    pub max_width: i32,
    pub max_height: i32,
    pub header: i32,
    pub header_gap: i32,
    pub pad: i32,
}

impl GridParams {
    pub fn for_output(size: Size<i32, Logical>) -> GridParams {
        GridParams {
            thumb: ((size.h as f64 * THUMB_SHARE).round() as i32).max(MIN_THUMB),
            min_thumb: MIN_THUMB,
            // Neighbouring frames must not touch: each grows by `PAD` into the gap.
            gap: ((size.w as f64 * GAP_SHARE).round() as i32).max(2 * PAD + 4),
            max_width: (size.w as f64 * MAX_WIDTH_SHARE).round() as i32,
            max_height: (size.h as f64 * MAX_HEIGHT_SHARE).round() as i32,
            header: HEADER,
            header_gap: HEADER_GAP,
            pad: PAD,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// What the selection frame outlines: the header and the thumbnail, `pad` all round.
    pub frame: Rectangle<i32, Logical>,
    /// Icon and title, above the thumbnail and as wide as it.
    pub header: Rectangle<i32, Logical>,
    /// The window picture is fitted into this with its aspect kept.
    pub thumb: Rectangle<i32, Logical>,
    pub row: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridLayout {
    pub cells: Vec<Cell>,
    pub rows: usize,
}

/// Cells for windows of the given aspects (width / height), in that order, on an output of
/// `area`. Coordinates are output-local.
pub fn grid_layout(area: Size<i32, Logical>, aspects: &[f64], p: &GridParams) -> GridLayout {
    if aspects.is_empty() {
        return GridLayout::default();
    }

    let mut h = p.thumb.max(p.min_thumb);
    loop {
        let widths = widths(aspects, h);
        match fill_rows(&widths, p.gap, p.max_width) {
            Some(rows) => {
                if grid_height(rows.len(), h, p) <= p.max_height || h <= p.min_thumb {
                    return place(area, &widths, &rows, h, p);
                }
            }
            // A single cell wider than a row even at the floor: one cell per row.
            None if h <= p.min_thumb => {
                let rows = (0..widths.len()).map(|i| i..i + 1).collect::<Vec<_>>();
                return place(area, &widths, &rows, h, p);
            }
            None => {}
        }
        h = (h - (h / 20).max(1)).max(p.min_thumb);
    }
}

fn widths(aspects: &[f64], h: i32) -> Vec<i32> {
    aspects
        .iter()
        .map(|a| {
            let a = if a.is_finite() && *a > 0.0 { *a } else { 1.0 };
            ((a.clamp(MIN_ASPECT, MAX_ASPECT) * h as f64).round() as i32).max(1)
        })
        .collect()
}

/// Greedy rows, or `None` when one cell alone is wider than a row.
fn fill_rows(widths: &[i32], gap: i32, max_width: i32) -> Option<Vec<Range<usize>>> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut row_w = 0;
    for (i, &w) in widths.iter().enumerate() {
        if w > max_width {
            return None;
        }
        if i == start {
            row_w = w;
        } else if row_w + gap + w > max_width {
            rows.push(start..i);
            start = i;
            row_w = w;
        } else {
            row_w += gap + w;
        }
    }
    rows.push(start..widths.len());
    Some(rows)
}

fn grid_height(rows: usize, h: i32, p: &GridParams) -> i32 {
    let rows = rows as i32;
    rows * (p.header + p.header_gap + h) + (rows - 1) * p.gap
}

fn place(
    area: Size<i32, Logical>,
    widths: &[i32],
    rows: &[Range<usize>],
    h: i32,
    p: &GridParams,
) -> GridLayout {
    let pitch = p.header + p.header_gap + h + p.gap;
    let y0 = (area.h - grid_height(rows.len(), h, p)) / 2;
    let mut cells = Vec::with_capacity(widths.len());
    for (r, range) in rows.iter().enumerate() {
        let row = &widths[range.clone()];
        let row_w = row.iter().sum::<i32>() + p.gap * (row.len() as i32 - 1);
        let mut x = (area.w - row_w) / 2;
        let y = y0 + r as i32 * pitch;
        for &w in row {
            cells.push(Cell {
                frame: Rectangle::new(
                    Point::from((x - p.pad, y - p.pad)),
                    Size::from((w + 2 * p.pad, p.header + p.header_gap + h + 2 * p.pad)),
                ),
                header: Rectangle::new(Point::from((x, y)), Size::from((w, p.header))),
                thumb: Rectangle::new(
                    Point::from((x, y + p.header + p.header_gap)),
                    Size::from((w, h)),
                ),
                row: r,
            });
            x += w + p.gap;
        }
    }
    GridLayout {
        cells,
        rows: rows.len(),
    }
}

/// The cell after `from`, or before it, wrapping round.
pub fn step(len: usize, from: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (from + 1) % len
    } else {
        (from + len - 1) % len
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The cell an arrow key moves to: along the order with wrap-around, or to the cell of the next
/// row whose centre is nearest, staying put at the top and bottom rows.
pub fn neighbour(layout: &GridLayout, from: usize, dir: Dir) -> usize {
    let len = layout.cells.len();
    let Some(cell) = layout.cells.get(from) else {
        return from;
    };
    let row = match dir {
        Dir::Left => return step(len, from, false),
        Dir::Right => return step(len, from, true),
        Dir::Up if cell.row == 0 => return from,
        Dir::Up => cell.row - 1,
        Dir::Down => cell.row + 1,
    };
    let centre = cell.thumb.loc.x * 2 + cell.thumb.size.w;
    layout
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.row == row)
        .min_by_key(|(_, c)| ((c.thumb.loc.x * 2 + c.thumb.size.w) - centre).abs())
        .map(|(i, _)| i)
        .unwrap_or(from)
}

/// The cell whose frame holds `point`, output-local. Frames do not overlap, and the bounds are
/// half-open so a shared pixel cannot belong to two of them.
pub fn cell_at(layout: &GridLayout, point: Point<f64, Logical>) -> Option<usize> {
    layout.cells.iter().position(|c| {
        let f = c.frame.to_f64();
        point.x >= f.loc.x
            && point.y >= f.loc.y
            && point.x < f.loc.x + f.size.w
            && point.y < f.loc.y + f.size.h
    })
}

/// What has to stay held for the switcher to stay open: the binding's modifiers less Shift,
/// which only reverses the direction. A Shift-only binding holds Shift; a binding without
/// modifiers holds nothing, and the switcher stays open until Enter, a click or Esc.
pub fn hold_modifiers(binding: &Modifiers) -> Option<Modifiers> {
    let hold = Modifiers {
        shift: false,
        ..binding.clone()
    };
    if hold.ctrl || hold.alt || hold.logo {
        Some(hold)
    } else if binding.shift {
        Some(Modifiers {
            shift: true,
            ..Modifiers::default()
        })
    } else {
        None
    }
}

/// Whether the hold is over: none of its modifiers is down any more.
pub fn hold_released(hold: &Modifiers, now: &Modifiers) -> bool {
    !((hold.ctrl && now.ctrl)
        || (hold.alt && now.alt)
        || (hold.logo && now.logo)
        || (hold.shift && now.shift))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fhd() -> Size<i32, Logical> {
        Size::from((1920, 1080))
    }

    fn aspects(n: usize) -> Vec<f64> {
        // Deterministic spread of window shapes: portrait, 4:3, 16:10, 16:9, wide.
        const SHAPES: [f64; 5] = [0.7, 1.33, 1.6, 1.78, 2.2];
        (0..n).map(|i| SHAPES[(i * 3 + 1) % SHAPES.len()]).collect()
    }

    fn layout(n: usize) -> (GridParams, GridLayout) {
        let p = GridParams::for_output(fhd());
        (p, grid_layout(fhd(), &aspects(n), &p))
    }

    fn row_span(l: &GridLayout, row: usize) -> (i32, i32) {
        let cells = l.cells.iter().filter(|c| c.row == row);
        let left = cells.clone().map(|c| c.thumb.loc.x).min().unwrap();
        let right = cells.map(|c| c.thumb.loc.x + c.thumb.size.w).max().unwrap();
        (left, right)
    }

    #[test]
    fn no_windows_no_cells() {
        let (_, l) = layout(0);
        assert!(l.cells.is_empty());
        assert_eq!(l.rows, 0);
    }

    #[test]
    fn one_window_is_centred_at_the_reference_height() {
        let p = GridParams::for_output(fhd());
        let l = grid_layout(fhd(), &[1.6], &p);
        let c = l.cells[0];
        assert_eq!(c.thumb.size.h, (1080.0 * THUMB_SHARE).round() as i32);
        let block_top = c.header.loc.y;
        let block_bottom = c.thumb.loc.y + c.thumb.size.h;
        assert!((block_top - (fhd().h - block_bottom)).abs() <= 1);
        assert!((c.thumb.loc.x - (fhd().w - c.thumb.loc.x - c.thumb.size.w)).abs() <= 1);
    }

    #[test]
    fn neighbours_are_one_gap_apart() {
        let (p, l) = layout(4);
        for pair in l.cells.windows(2) {
            if pair[0].row == pair[1].row {
                assert_eq!(
                    pair[1].thumb.loc.x - (pair[0].thumb.loc.x + pair[0].thumb.size.w),
                    p.gap
                );
            }
        }
    }

    #[test]
    fn grid_stays_inside_the_limits() {
        for n in 1..=60 {
            let (p, l) = layout(n);
            assert_eq!(l.cells.len(), n);
            let h = l.cells[0].thumb.size.h;
            for row in 0..l.rows {
                let (left, right) = row_span(&l, row);
                assert!(right - left <= p.max_width, "n={n} row={row}");
            }
            let top = l.cells.iter().map(|c| c.header.loc.y).min().unwrap();
            let bottom = l
                .cells
                .iter()
                .map(|c| c.thumb.loc.y + c.thumb.size.h)
                .max()
                .unwrap();
            if h > p.min_thumb {
                assert!(bottom - top <= p.max_height, "n={n}");
            }
        }
    }

    #[test]
    fn every_row_is_centred() {
        for n in 1..=30 {
            let (_, l) = layout(n);
            for row in 0..l.rows {
                let (left, right) = row_span(&l, row);
                assert!((left - (fhd().w - right)).abs() <= 1, "n={n} row={row}");
            }
        }
    }

    #[test]
    fn thumbnails_never_grow_with_more_windows() {
        let mut last = i32::MAX;
        for n in 1..=60 {
            let (_, l) = layout(n);
            let h = l.cells[0].thumb.size.h;
            assert!(h <= last, "n={n}");
            assert!(l.cells.iter().all(|c| c.thumb.size.h == h));
            last = h;
        }
    }

    #[test]
    fn frames_do_not_overlap() {
        for n in [2, 7, 13, 40] {
            let (_, l) = layout(n);
            for (i, a) in l.cells.iter().enumerate() {
                for b in &l.cells[i + 1..] {
                    assert!(!a.frame.overlaps(b.frame), "n={n}");
                }
            }
        }
    }

    #[test]
    fn extreme_aspects_are_clamped() {
        let p = GridParams::for_output(fhd());
        let l = grid_layout(fhd(), &[0.1, 9.0, f64::NAN], &p);
        let h = l.cells[0].thumb.size.h;
        assert_eq!(
            l.cells[0].thumb.size.w,
            (h as f64 * MIN_ASPECT).round() as i32
        );
        assert_eq!(
            l.cells[1].thumb.size.w,
            (h as f64 * MAX_ASPECT).round() as i32
        );
        assert_eq!(l.cells[2].thumb.size.w, h);
    }

    #[test]
    fn steps_wrap_round() {
        assert_eq!(step(3, 2, true), 0);
        assert_eq!(step(3, 0, false), 2);
        assert_eq!(step(1, 0, true), 0);
        assert_eq!(step(0, 0, true), 0);
    }

    #[test]
    fn arrows_follow_the_grid() {
        let (_, l) = layout(13);
        assert!(l.rows >= 2);
        let first_of_row1 = l.cells.iter().position(|c| c.row == 1).unwrap();
        // Up from the top row and down from the bottom row stay put.
        assert_eq!(neighbour(&l, 0, Dir::Up), 0);
        let last = l.cells.len() - 1;
        assert_eq!(neighbour(&l, last, Dir::Down), last);
        // Down lands in the next row, on the cell nearest below.
        let down = neighbour(&l, 0, Dir::Down);
        assert_eq!(l.cells[down].row, 1);
        assert_eq!(neighbour(&l, down, Dir::Up), 0);
        // Left from the first cell wraps to the last one.
        assert_eq!(neighbour(&l, 0, Dir::Left), last);
        assert_eq!(neighbour(&l, first_of_row1 - 1, Dir::Right), first_of_row1);
    }

    #[test]
    fn hit_test_uses_half_open_frames() {
        let (_, l) = layout(3);
        let f = l.cells[1].frame;
        assert_eq!(cell_at(&l, f.loc.to_f64()), Some(1));
        let outside = Point::from(((f.loc.x + f.size.w) as f64, f.loc.y as f64));
        assert_ne!(cell_at(&l, outside), Some(1));
        assert_eq!(cell_at(&l, Point::from((0.0, 0.0))), None);
    }

    #[test]
    fn hold_drops_shift_unless_it_is_all_there_is() {
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let alt_shift = Modifiers {
            shift: true,
            ..alt.clone()
        };
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(hold_modifiers(&alt), Some(alt.clone()));
        assert_eq!(hold_modifiers(&alt_shift), Some(alt.clone()));
        assert_eq!(hold_modifiers(&shift), Some(shift.clone()));
        assert_eq!(hold_modifiers(&Modifiers::default()), None);
    }

    #[test]
    fn hold_ends_when_none_of_it_is_down() {
        let ctrl_alt = Modifiers {
            ctrl: true,
            alt: true,
            ..Modifiers::default()
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let alt_shift = Modifiers {
            shift: true,
            ..alt.clone()
        };
        assert!(!hold_released(&alt, &alt));
        assert!(!hold_released(&alt, &alt_shift));
        assert!(hold_released(&alt, &Modifiers::default()));
        // Letting go of one of two held modifiers does not close it yet.
        assert!(!hold_released(&ctrl_alt, &alt));
        assert!(hold_released(&ctrl_alt, &Modifiers::default()));
    }
}
