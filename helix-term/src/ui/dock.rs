//! What the panels docked beside the editor share: their width and the rail beside them.

use helix_view::{editor::FileTreeSide, graphics::Rect};
use tui::buffer::Buffer as Surface;

use crate::ui::scrollbar::{self, Bar, RailStyles};

/// The narrowest and widest a panel gets, its rail included.
pub const MIN_WIDTH: u16 = 16;
pub const MAX_WIDTH: u16 = 64;
/// The columns the panels always leave to the editor; with fewer they yield.
pub const MIN_EDITOR_WIDTH: u16 = 20;

/// The side of the editor a panel docks on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl From<FileTreeSide> for Side {
    fn from(side: FileTreeSide) -> Self {
        match side {
            FileTreeSide::Left => Self::Left,
            FileTreeSide::Right => Self::Right,
        }
    }
}

/// The widest a panel may get beside the editor in `main`.
pub fn max_width(main: Rect) -> u16 {
    main.width.saturating_sub(MIN_EDITOR_WIDTH).min(MAX_WIDTH)
}

/// `width` within the limits of a panel at most `max_width` wide.
pub fn clamp_width(width: u16, max_width: u16) -> u16 {
    width.min(max_width).max(MIN_WIDTH)
}

/// The width `|` switches a panel `width` wide to: `max_width`, and from there the narrowest.
pub fn toggled_width(width: u16, max_width: u16) -> u16 {
    let max_width = if max_width == 0 { MAX_WIDTH } else { max_width };
    if width < max_width {
        clamp_width(max_width, max_width)
    } else {
        MIN_WIDTH
    }
}

/// The width that shows rows `widest` columns wide whole, with the rail, within the limits.
pub fn fitted_width(widest: usize, max_width: u16) -> u16 {
    let width = u16::try_from(widest + 1).unwrap_or(u16::MAX);
    clamp_width(width, max_width)
}

/// The area of a panel docked on `side` in `area` that its rows take, and the column of its
/// rail, which is on the editor's side.
pub fn split(area: Rect, side: Side) -> (Rect, u16) {
    match side {
        Side::Left => (area.clip_right(1), area.right() - 1),
        Side::Right => (area.clip_left(1), area.left()),
    }
}

/// Draws the rail of a panel docked on `side` in `area`: the panel's `bar` on its half, and the
/// bars of the splits beside it, `neighbours`, on the other.
pub fn render_rail(
    surface: &mut Surface,
    area: Rect,
    side: Side,
    bar: Bar,
    neighbours: &[Bar],
    styles: RailStyles,
) {
    let (_, column) = split(area, side);
    let bar = [bar];
    let (left, right) = match side {
        Side::Left => (&bar[..], neighbours),
        Side::Right => (neighbours, &bar[..]),
    };
    let rows = area.top()..area.bottom();
    scrollbar::render_rail(surface, column, rows, left, right, styles);
}

/// The last row of a view's `area`, which panels end above.
pub fn statusline_area(area: Rect, docks: &[Rect]) -> Rect {
    let mut row = area.clip_top(area.height.saturating_sub(1));
    loop {
        let wider = docks.iter().find_map(|dock| {
            if dock.bottom() != row.top() {
                None
            } else if dock.right() == row.left() {
                Some(Rect::new(dock.left(), row.y, dock.width + row.width, 1))
            } else if row.right() == dock.left() {
                Some(Rect::new(row.x, row.y, row.width + dock.width, 1))
            } else {
                None
            }
        });
        match wider {
            Some(wider) => row = wider,
            None => return row,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuslines_below_panels_take_their_columns() {
        // A 100x30 screen: a panel takes 20 columns and ends above the bottom statusline row.
        let left = Rect::new(0, 0, 20, 28);
        let right = Rect::new(80, 0, 20, 28);
        // One view beside the panel.
        assert_eq!(
            statusline_area(Rect::new(20, 0, 80, 29), &[left]),
            Rect::new(0, 28, 100, 1)
        );
        assert_eq!(
            statusline_area(Rect::new(0, 0, 80, 29), &[right]),
            Rect::new(0, 28, 100, 1)
        );
        // Stacked views: only the bottom one reaches below the panel.
        assert_eq!(
            statusline_area(Rect::new(20, 0, 80, 14), &[left]),
            Rect::new(20, 13, 80, 1)
        );
        assert_eq!(
            statusline_area(Rect::new(20, 14, 80, 15), &[left]),
            Rect::new(0, 28, 100, 1)
        );
        // Side by side: only the view next to the panel.
        assert_eq!(
            statusline_area(Rect::new(61, 0, 39, 29), &[left]),
            Rect::new(61, 28, 39, 1)
        );
        assert_eq!(
            statusline_area(Rect::new(20, 0, 80, 29), &[]),
            Rect::new(20, 28, 80, 1)
        );
        // Panels on both sides, and two on one side.
        assert_eq!(
            statusline_area(Rect::new(20, 0, 60, 29), &[left, right]),
            Rect::new(0, 28, 100, 1)
        );
        let outer = Rect::new(80, 0, 20, 28);
        let inner = Rect::new(60, 0, 20, 28);
        assert_eq!(
            statusline_area(Rect::new(0, 0, 60, 29), &[outer, inner]),
            Rect::new(0, 28, 100, 1)
        );
    }

    #[test]
    fn widths_stay_within_the_limits() {
        let max = max_width(Rect::new(0, 0, 100, 30));
        assert_eq!(max, MAX_WIDTH);
        assert_eq!(max_width(Rect::new(0, 0, 50, 30)), 30);
        assert_eq!(fitted_width(3, max), MIN_WIDTH);
        assert_eq!(fitted_width(40, max), 41);
        assert_eq!(fitted_width(400, max), MAX_WIDTH);
        assert_eq!(clamp_width(70, 30), 30);
        assert_eq!(toggled_width(20, 30), 30);
        assert_eq!(toggled_width(30, 30), MIN_WIDTH);
        assert_eq!(toggled_width(MIN_WIDTH, 30), 30);
        assert_eq!(toggled_width(MIN_WIDTH, 0), MAX_WIDTH);
    }
}
