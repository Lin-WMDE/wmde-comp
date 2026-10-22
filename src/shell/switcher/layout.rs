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

/// The Icons and List looks draw a panel of the theme, framed like the compositor's menu: this
/// frame, `menu::RADIUS` corners.
pub const PANEL_BORDER: i32 = 1;
/// Icons look: the icon, the tile around it (the selection fills the tile) and the gap between
/// tiles, at most `ICON_COLUMNS` tiles to a row as in the classic Windows switcher.
pub const ICON: i32 = 48;
pub const TILE: i32 = 64;
pub const TILE_GAP: i32 = 8;
pub const ICON_COLUMNS: usize = 7;
/// Between the Icons panel's edge and its tiles.
pub const PANEL_PAD: i32 = 16;
/// The title line under the tiles, the gap above it, and the narrowest it gets, so that one
/// or two windows still leave room for a title.
pub const CAPTION: i32 = 28;
pub const CAPTION_GAP: i32 = 8;
pub const MIN_CAPTION_WIDTH: i32 = 320;
/// List look: the panel width, a row and its icon.
pub const LIST_WIDTH: i32 = 480;
pub const ROW: i32 = 40;
pub const ROW_ICON: i32 = 32;

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
    /// The panel of the Icons and List looks; the grid has none.
    pub panel: Option<Rectangle<i32, Logical>>,
    /// The line of the Icons look that carries the selected window's title.
    pub caption: Option<Rectangle<i32, Logical>>,
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
        ..GridLayout::default()
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

/// The close button of a cell: a square as tall as the header, at its right end, so the
/// title is never drawn under it.
pub fn close_button(cell: &Cell) -> Rectangle<i32, Logical> {
    let side = cell.header.size.h.min(cell.header.size.w);
    Rectangle::new(
        Point::from((
            cell.header.loc.x + cell.header.size.w - side,
            cell.header.loc.y,
        )),
        Size::from((side, side)),
    )
}

/// The cell whose close button holds `point`, output-local, half-open like [`cell_at`].
pub fn close_at(layout: &GridLayout, point: Point<f64, Logical>) -> Option<usize> {
    layout.cells.iter().position(|c| {
        let b = close_button(c).to_f64();
        point.x >= b.loc.x
            && point.y >= b.loc.y
            && point.x < b.loc.x + b.size.w
            && point.y < b.loc.y + b.size.h
    })
}

/// Whether `point` (output-local) is on the panel of the Icons or List look.
pub fn in_panel(layout: &GridLayout, point: Point<f64, Logical>) -> bool {
    layout.panel.is_some_and(|p| {
        let p = p.to_f64();
        point.x >= p.loc.x
            && point.y >= p.loc.y
            && point.x < p.loc.x + p.size.w
            && point.y < p.loc.y + p.size.h
    })
}

/// The Icons look for `n` windows on an output of `area`: tiles in rows of at most
/// `ICON_COLUMNS` and no wider than the grid's widest row, filled from the left, with the title
/// line under them; the block of tiles is centred over the title line, the panel on the output.
/// A tile is the cell's frame and picture; it has no header, so no close button either.
pub fn icons_layout(area: Size<i32, Logical>, n: usize) -> GridLayout {
    if n == 0 {
        return GridLayout::default();
    }
    let max_w = (area.w as f64 * MAX_WIDTH_SHARE).round() as i32;
    let fit = ((max_w - 2 * PANEL_PAD + TILE_GAP) / (TILE + TILE_GAP)).max(1) as usize;
    let columns = n.min(ICON_COLUMNS).min(fit);
    let rows = n.div_ceil(columns);
    let tiles = Size::<i32, Logical>::from((
        columns as i32 * (TILE + TILE_GAP) - TILE_GAP,
        rows as i32 * (TILE + TILE_GAP) - TILE_GAP,
    ));
    // Tiles are an even width, so the block centres on whole pixels.
    let content_w = tiles.w.max(MIN_CAPTION_WIDTH);
    let size = Size::from((
        content_w + 2 * PANEL_PAD,
        tiles.h + CAPTION_GAP + CAPTION + 2 * PANEL_PAD,
    ));
    let panel = Rectangle::new(
        Point::from(((area.w - size.w) / 2, (area.h - size.h) / 2)),
        size,
    );
    let origin = panel.loc + Point::from((PANEL_PAD + (content_w - tiles.w) / 2, PANEL_PAD));
    let cells = (0..n)
        .map(|i| {
            let (row, column) = (i / columns, i % columns);
            let tile = Rectangle::new(
                origin
                    + Point::from((
                        column as i32 * (TILE + TILE_GAP),
                        row as i32 * (TILE + TILE_GAP),
                    )),
                Size::from((TILE, TILE)),
            );
            Cell {
                frame: tile,
                header: Rectangle::default(),
                thumb: tile,
                row,
            }
        })
        .collect();
    let caption = Rectangle::new(
        panel.loc + Point::from((PANEL_PAD, PANEL_PAD + tiles.h + CAPTION_GAP)),
        Size::from((content_w, CAPTION)),
    );
    GridLayout {
        cells,
        rows,
        panel: Some(panel),
        caption: Some(caption),
    }
}

/// How many rows of the List look fit an output of `area`, out of `n`.
pub fn list_visible(area: Size<i32, Logical>, n: usize) -> usize {
    let max_h = (area.h as f64 * MAX_HEIGHT_SHARE).round() as i32;
    (((max_h - 2 * PANEL_BORDER) / ROW).max(1) as usize).min(n)
}

/// The first row shown so that `selected` is in view, moving the window of `visible` rows as
/// little as it takes.
pub fn list_first(n: usize, visible: usize, first: usize, selected: usize) -> usize {
    let first = if selected < first {
        selected
    } else if selected >= first + visible {
        selected + 1 - visible
    } else {
        first
    };
    first.min(n.saturating_sub(visible))
}

/// The List look for `n` windows on an output of `area`, rows `first..` in view: rows end to end
/// inside the panel frame, each its own grid row, so the arrows go up and down the list. A row
/// out of view has an empty frame and is neither drawn nor hit. The header is the whole row, and
/// the close button the square at its right end.
pub fn list_layout(area: Size<i32, Logical>, n: usize, first: usize) -> GridLayout {
    if n == 0 {
        return GridLayout::default();
    }
    let visible = list_visible(area, n);
    let first = first.min(n - visible);
    let width = LIST_WIDTH.min((area.w as f64 * MAX_WIDTH_SHARE).round() as i32);
    let size = Size::from((width, visible as i32 * ROW + 2 * PANEL_BORDER));
    let panel = Rectangle::new(
        Point::from(((area.w - size.w) / 2, (area.h - size.h) / 2)),
        size,
    );
    let cells = (0..n)
        .map(|i| {
            let frame = if (first..first + visible).contains(&i) {
                Rectangle::new(
                    panel.loc
                        + Point::from((PANEL_BORDER, PANEL_BORDER + (i - first) as i32 * ROW)),
                    Size::from((width - 2 * PANEL_BORDER, ROW)),
                )
            } else {
                Rectangle::default()
            };
            Cell {
                frame,
                header: frame,
                thumb: frame,
                row: i,
            }
        })
        .collect();
    GridLayout {
        cells,
        rows: n,
        panel: Some(panel),
        caption: None,
    }
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

/// One cell per window family: each entry that keeps a cell, with the entries behind it.
/// `dialog_of` is the modal dialog that takes an entry's focus (`Shell::resolve_modal_redirect`);
/// the entry goes behind that dialog when the dialog is listed and passes the focus on no
/// further. A cycle of X11 transients, where every window has a dialog, keeps all its cells.
pub fn modal_families<T: Clone + PartialEq>(
    entries: Vec<T>,
    dialog_of: impl Fn(&T) -> Option<T>,
) -> Vec<(T, Vec<T>)> {
    let heads = entries
        .iter()
        .map(|t| dialog_of(t).filter(|d| entries.contains(d) && dialog_of(d).is_none()))
        .collect::<Vec<_>>();
    let mut families = entries
        .iter()
        .zip(&heads)
        .filter(|(_, head)| head.is_none())
        .map(|(t, _)| (t.clone(), Vec::new()))
        .collect::<Vec<_>>();
    for (t, head) in entries.into_iter().zip(heads) {
        if let Some(head) = head
            && let Some((_, behind)) = families.iter_mut().find(|(h, _)| *h == head)
        {
            behind.push(t);
        }
    }
    families
}

/// The windows behind a cell's dialog `head`, top-first: the one right under it, on down to the
/// root. They form a chain of modal parents; one off the chain goes to the bottom.
pub fn modal_chain<T>(
    head: &T,
    mut behind: Vec<T>,
    is_parent_of: impl Fn(&T, &T) -> bool,
) -> Vec<T> {
    let mut chain = Vec::with_capacity(behind.len());
    while let Some(i) = behind
        .iter()
        .position(|p| is_parent_of(p, chain.last().unwrap_or(head)))
    {
        chain.push(behind.remove(i));
    }
    chain.extend(behind);
    chain
}

/// Where the windows of one cell lie, top-first, and the box around them all, which the
/// thumbnail fits into its cell. `on_screen` when these are the windows' own places on the
/// output, where the animation starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyShape {
    pub bounds: Rectangle<i32, Logical>,
    pub members: Vec<Rectangle<i32, Logical>>,
    pub on_screen: bool,
}

/// The shape of a cell from each window's place on screen, if it has one, and its size,
/// top-first. With every window on screen they stay where they are. Otherwise they are laid out
/// anew: the bottom one at the origin, each one above centred over the one under it, the way a
/// modal dialog is placed over its parent.
pub fn family_shape(
    placed: &[(Option<Rectangle<i32, Logical>>, Size<i32, Logical>)],
) -> FamilyShape {
    let on_screen = placed.iter().all(|(rect, _)| rect.is_some());
    let members = if on_screen {
        placed
            .iter()
            .filter_map(|(rect, _)| *rect)
            .collect::<Vec<_>>()
    } else {
        let mut members = Vec::with_capacity(placed.len());
        let mut under: Option<Rectangle<i32, Logical>> = None;
        for (_, size) in placed.iter().rev() {
            let loc = under.map_or(Point::default(), |u| {
                u.loc + Point::from(((u.size.w - size.w) / 2, (u.size.h - size.h) / 2))
            });
            let rect = Rectangle::new(loc, *size);
            members.push(rect);
            under = Some(rect);
        }
        members.reverse();
        members
    };
    let bounds = members
        .iter()
        .copied()
        .reduce(Rectangle::merge)
        .unwrap_or_default();
    FamilyShape {
        bounds,
        members,
        on_screen,
    }
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
    fn close_button_sits_at_the_right_of_the_header() {
        let (_, l) = layout(3);
        for (i, cell) in l.cells.iter().enumerate() {
            let b = close_button(cell);
            assert_eq!(b.size.h, cell.header.size.h);
            assert_eq!(b.loc.x + b.size.w, cell.header.loc.x + cell.header.size.w);
            assert!(cell.frame.contains_rect(b));
            assert_eq!(close_at(&l, b.loc.to_f64()), Some(i));
            // The thumbnail under the header is not the button.
            assert_eq!(close_at(&l, cell.thumb.loc.to_f64()), None);
        }
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

    /// `modal_families` over window names, with the dialog each one's focus goes to.
    fn families(
        entries: &[&'static str],
        dialogs: &[(&str, &'static str)],
    ) -> Vec<(&'static str, Vec<&'static str>)> {
        modal_families(entries.to_vec(), |t| {
            dialogs.iter().find(|(w, _)| w == t).map(|(_, d)| *d)
        })
    }

    #[test]
    fn windows_without_dialogs_keep_their_cells() {
        assert_eq!(
            families(&["a", "b"], &[]),
            vec![("a", vec![]), ("b", vec![])]
        );
    }

    #[test]
    fn a_listed_modal_dialog_stands_for_its_parent() {
        // The dialog was focused last, its parent before it.
        assert_eq!(
            families(&["dialog", "parent", "other"], &[("parent", "dialog")]),
            vec![("dialog", vec!["parent"]), ("other", vec![])]
        );
    }

    #[test]
    fn a_chain_of_dialogs_is_one_cell() {
        // The focus of the parent and of the first dialog goes to the deepest one.
        assert_eq!(
            families(
                &["parent", "first", "second"],
                &[("parent", "second"), ("first", "second")]
            ),
            vec![("second", vec!["parent", "first"])]
        );
    }

    #[test]
    fn a_parent_keeps_its_cell_when_its_dialog_is_not_listed() {
        // A minimized dialog left out, or one moved to another workspace.
        assert_eq!(
            families(&["parent", "other"], &[("parent", "dialog")]),
            vec![("parent", vec![]), ("other", vec![])]
        );
    }

    #[test]
    fn a_cycle_of_transients_keeps_every_cell() {
        assert_eq!(
            families(&["a", "b"], &[("a", "b"), ("b", "a")]),
            vec![("a", vec![]), ("b", vec![])]
        );
    }

    /// `modal_chain` over window names, with `(parent, child)` pairs.
    fn chain(
        head: &'static str,
        behind: &[&'static str],
        parents: &[(&str, &str)],
    ) -> Vec<&'static str> {
        modal_chain(&head, behind.to_vec(), |p, c| parents.contains(&(*p, *c)))
    }

    #[test]
    fn the_chain_runs_from_the_dialog_down_to_the_root() {
        assert_eq!(
            chain("dialog", &["parent"], &[("parent", "dialog")]),
            vec!["parent"]
        );
        assert_eq!(
            chain(
                "second",
                &["parent", "first"],
                &[("parent", "first"), ("first", "second")]
            ),
            vec!["first", "parent"]
        );
    }

    #[test]
    fn a_window_off_the_chain_goes_to_the_bottom() {
        assert_eq!(
            chain("dialog", &["stranger", "parent"], &[("parent", "dialog")]),
            vec!["parent", "stranger"]
        );
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn a_lone_window_keeps_its_place() {
        let r = rect(100, 50, 800, 600);
        assert_eq!(
            family_shape(&[(Some(r), r.size)]),
            FamilyShape {
                bounds: r,
                members: vec![r],
                on_screen: true
            }
        );
        let shape = family_shape(&[(None, r.size)]);
        assert_eq!(shape.bounds, rect(0, 0, 800, 600));
        assert!(!shape.on_screen);
    }

    #[test]
    fn a_dialog_inside_its_parent_keeps_the_parent_box() {
        let parent = rect(100, 50, 800, 600);
        let dialog = rect(340, 250, 320, 160);
        let shape = family_shape(&[(Some(dialog), dialog.size), (Some(parent), parent.size)]);
        assert_eq!(shape.bounds, parent);
        assert_eq!(shape.members, vec![dialog, parent]);
        assert!(shape.on_screen);
    }

    #[test]
    fn a_dialog_wider_than_its_parent_widens_the_box() {
        let parent = rect(100, 50, 800, 600);
        let dialog = rect(50, 200, 900, 200);
        let shape = family_shape(&[(Some(dialog), dialog.size), (Some(parent), parent.size)]);
        assert_eq!(shape.bounds, rect(50, 50, 900, 600));
    }

    #[test]
    fn off_screen_windows_are_centred_over_the_one_under() {
        let parent = rect(100, 50, 800, 600);
        let shape = family_shape(&[(None, Size::from((320, 160))), (Some(parent), parent.size)]);
        assert_eq!(
            shape.members,
            vec![rect(240, 220, 320, 160), rect(0, 0, 800, 600)]
        );
        assert_eq!(shape.bounds, rect(0, 0, 800, 600));
        assert!(!shape.on_screen);

        let shape = family_shape(&[
            (Some(rect(0, 0, 200, 100)), Size::from((200, 100))),
            (None, Size::from((400, 300))),
            (Some(parent), parent.size),
        ]);
        assert_eq!(
            shape.members,
            vec![
                rect(300, 250, 200, 100),
                rect(200, 150, 400, 300),
                rect(0, 0, 800, 600)
            ]
        );
    }

    fn overlaps(a: Rectangle<i32, Logical>, b: Rectangle<i32, Logical>) -> bool {
        a.loc.x < b.loc.x + b.size.w
            && b.loc.x < a.loc.x + a.size.w
            && a.loc.y < b.loc.y + b.size.h
            && b.loc.y < a.loc.y + a.size.h
    }

    #[test]
    fn icons_sit_on_a_centred_panel_with_the_title_under_them() {
        let l = icons_layout(fhd(), 3);
        let panel = l.panel.unwrap();
        // Three tiles are narrower than the title line, which sets the width.
        assert_eq!(panel.size.w, MIN_CAPTION_WIDTH + 2 * PANEL_PAD);
        assert_eq!(panel.loc.x, (1920 - panel.size.w) / 2);
        assert_eq!(panel.loc.y, (1080 - panel.size.h) / 2);
        assert_eq!(l.rows, 1);
        // The block of tiles is centred over the title line.
        let tiles_w = 3 * TILE + 2 * TILE_GAP;
        assert_eq!(
            l.cells[0].frame.loc.x,
            panel.loc.x + PANEL_PAD + (MIN_CAPTION_WIDTH - tiles_w) / 2
        );
        let caption = l.caption.unwrap();
        assert_eq!(caption.loc.y, l.cells[0].frame.loc.y + TILE + CAPTION_GAP);
        assert_eq!(
            caption.loc.y + CAPTION + PANEL_PAD,
            panel.loc.y + panel.size.h
        );
        for c in &l.cells {
            assert_eq!(c.frame.size, Size::from((TILE, TILE)));
            // No header, so no close button to hit.
            assert_eq!(close_button(c).size, Size::default());
        }
    }

    #[test]
    fn icons_wrap_after_seven_and_never_overlap() {
        let l = icons_layout(fhd(), 10);
        assert_eq!(l.rows, 2);
        assert_eq!(l.cells[6].row, 0);
        assert_eq!(l.cells[7].row, 1);
        // The second row starts under the first tile, filled from the left.
        assert_eq!(l.cells[7].frame.loc.x, l.cells[0].frame.loc.x);
        assert_eq!(
            l.cells[7].frame.loc.y,
            l.cells[0].frame.loc.y + TILE + TILE_GAP
        );
        for (i, a) in l.cells.iter().enumerate() {
            assert!(l.panel.unwrap().contains_rect(a.frame));
            for b in &l.cells[i + 1..] {
                assert!(!overlaps(a.frame, b.frame));
            }
        }
        // Down from the second tile lands on the tile under it.
        assert_eq!(neighbour(&l, 1, Dir::Down), 8);
        assert_eq!(neighbour(&l, 8, Dir::Up), 1);
    }

    #[test]
    fn icons_rows_stay_within_a_narrow_output() {
        let narrow = Size::from((400, 800));
        let l = icons_layout(narrow, 7);
        // 0.9 * 400 = 360 takes four tiles and the panel's padding.
        assert_eq!(l.cells.iter().filter(|c| c.row == 0).count(), 4);
        assert!(l.panel.unwrap().size.w <= 400);
    }

    #[test]
    fn list_rows_run_end_to_end_with_the_close_button_at_the_right() {
        let l = list_layout(fhd(), 5, 0);
        let panel = l.panel.unwrap();
        assert_eq!(
            panel.size,
            Size::from((LIST_WIDTH, 5 * ROW + 2 * PANEL_BORDER))
        );
        assert_eq!(panel.loc.x, (1920 - LIST_WIDTH) / 2);
        for (i, c) in l.cells.iter().enumerate() {
            assert_eq!(c.row, i);
            assert_eq!(c.frame.loc.y, panel.loc.y + PANEL_BORDER + i as i32 * ROW);
            assert_eq!(
                c.frame.size,
                Size::from((LIST_WIDTH - 2 * PANEL_BORDER, ROW))
            );
            let close = close_button(c);
            assert_eq!(close.size, Size::from((ROW, ROW)));
            assert_eq!(close.loc.x + ROW, c.frame.loc.x + c.frame.size.w);
        }
        assert_eq!(neighbour(&l, 3, Dir::Up), 2);
        assert_eq!(neighbour(&l, 3, Dir::Down), 4);
        assert_eq!(neighbour(&l, 4, Dir::Down), 4);
        assert_eq!(neighbour(&l, 0, Dir::Up), 0);
    }

    #[test]
    fn a_long_list_shows_a_window_of_rows_around_the_selection() {
        let visible = list_visible(fhd(), 30);
        // 0.7 * 1080 = 756, less the frame, in rows of 40.
        assert_eq!(visible, 18);
        assert_eq!(list_first(30, visible, 0, 5), 0);
        assert_eq!(list_first(30, visible, 0, 25), 8);
        assert_eq!(list_first(30, visible, 8, 3), 3);
        assert_eq!(list_first(30, visible, 3, 29), 12);

        let l = list_layout(fhd(), 30, 12);
        let shown = l
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.frame.size.w > 0)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        assert_eq!(shown, (12..30).collect::<Vec<_>>());
        // A row out of view is neither hit nor closed.
        assert_eq!(cell_at(&l, Point::from((0.0, 0.0))), None);
        let top = l.cells[12].frame;
        assert_eq!(top.loc.y, l.panel.unwrap().loc.y + PANEL_BORDER);
        assert_eq!(cell_at(&l, top.loc.to_f64()), Some(12));
        // Up from the top row in view goes to the row above it, which scrolls.
        assert_eq!(neighbour(&l, 12, Dir::Up), 11);
    }

    #[test]
    fn a_click_on_the_panel_is_not_a_click_outside() {
        let l = icons_layout(fhd(), 2);
        let panel = l.panel.unwrap();
        assert!(in_panel(&l, (panel.loc + Point::from((2, 2))).to_f64()));
        assert!(!in_panel(&l, Point::from((0.0, 0.0))));
        assert!(!in_panel(&layout(3).1, Point::from((960.0, 540.0))));
    }
}
