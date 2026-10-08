//! Scrollbars: the thumbs of menus and popups, and the rails beside panels and splits, which
//! carry the thumbs and marks of the panes on both their sides.

use std::ops::Range;

use helix_core::movement::Direction;
use helix_view::{
    graphics::{Color, Rect, Style},
    Theme,
};
use tui::buffer::Buffer as Surface;

/// The rows of a scrollbar thumb for `len` rows of content shown `height` rows at a time,
/// scrolled down by `offset` rows. `None` when the content fits.
pub fn scrollbar_thumb(len: usize, height: usize, offset: usize) -> Option<Range<usize>> {
    if len <= height {
        return None;
    }
    thumb(len, offset..offset + height, height)
}

/// The rows of a thumb on a rail `height` rows tall, for the rows `shown` of `len` rows of
/// content. Its size is the share shown, and it ends at the bottom when the last row is shown.
/// `None` when all rows are shown.
pub fn thumb(len: usize, shown: Range<usize>, height: usize) -> Option<Range<usize>> {
    let shown = shown.start.min(len)..shown.end.min(len);
    if height == 0 || shown == (0..len) {
        return None;
    }
    let size = (shown.len() * height).div_ceil(len).clamp(1, height);
    let start = (height - size) * shown.start / (len - shown.len()).max(1);
    Some(start..start + size)
}

/// The offset that puts a thumb `thumb_len` rows long, held `grab` rows below its top, at the
/// row `row` of a rail `height` rows tall, for offsets up to `max_offset`. `None` when the thumb
/// fills the rail.
pub fn dragged_offset(
    row: usize,
    grab: usize,
    thumb_len: usize,
    height: usize,
    max_offset: usize,
) -> Option<usize> {
    let travel = height.checked_sub(thumb_len).filter(|&travel| travel > 0)?;
    let top = row.saturating_sub(grab).min(travel);
    Some((top * max_offset + travel / 2) / travel)
}

/// The way a press on the track at `row` pages: towards it, if it is off the thumb.
pub fn paging_direction(row: u16, thumb: Range<u16>) -> Option<Direction> {
    if row < thumb.start {
        Some(Direction::Backward)
    } else if row >= thumb.end {
        Some(Direction::Forward)
    } else {
        None
    }
}

/// What a pane shows on one row of its half of a rail.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    #[default]
    Track,
    Thumb(Color),
}

/// The thumb and marks a pane shows on its half of a rail, on the rows it is beside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    top: u16,
    cells: Vec<Cell>,
}

impl Bar {
    /// A bar beside the screen rows from `top` on, `height` of them, with a thumb of `color` over
    /// the rows `thumb`.
    pub fn new(top: u16, height: usize, thumb: Option<Range<usize>>, color: Color) -> Self {
        let mut cells = vec![Cell::Track; height];
        for cell in thumb
            .and_then(|thumb| cells.get_mut(thumb))
            .into_iter()
            .flatten()
        {
            *cell = Cell::Thumb(color);
        }
        Self { top, cells }
    }

    fn cell(&self, y: u16) -> Option<Cell> {
        self.cells
            .get(usize::from(y.checked_sub(self.top)?))
            .copied()
    }
}

/// The styles of a rail: the cells under it and its `│` track, and the color of thumbs.
#[derive(Debug, Clone, Copy)]
pub struct RailStyles {
    pub base: Style,
    pub track: Style,
    pub thumb: Color,
}

impl RailStyles {
    /// The rail of a separator (`ui.window`) carrying thumbs like menus (`ui.menu.scroll`), over
    /// `base`.
    pub fn new(theme: &Theme, base: Style) -> Self {
        Self {
            base,
            track: theme.get("ui.window"),
            thumb: theme.get("ui.menu.scroll").fg.unwrap_or(Color::Reset),
        }
    }
}

/// Draws the rail in the column `column` over the screen rows `rows`, with the bars of the
/// panes to its left and to its right on their halves. A thumb is a half block on its pane's
/// half, the other half taking the color of the other pane's thumb; elsewhere the rail is a `│`.
pub fn render_rail(
    surface: &mut Surface,
    column: u16,
    rows: Range<u16>,
    left: &[Bar],
    right: &[Bar],
    styles: RailStyles,
) {
    let cell_at = |bars: &[Bar], y| {
        bars.iter()
            .find_map(|bar| bar.cell(y))
            .unwrap_or(Cell::Track)
    };
    let style = styles.base.patch(styles.track);
    for y in rows {
        let (symbol, style) = match (cell_at(left, y), cell_at(right, y)) {
            (Cell::Thumb(thumb), Cell::Thumb(other)) => ("▌", style.fg(thumb).bg(other)),
            (Cell::Thumb(thumb), Cell::Track) => ("▌", style.fg(thumb)),
            (Cell::Track, Cell::Thumb(thumb)) => ("▐", style.fg(thumb)),
            (Cell::Track, Cell::Track) => ("│", style),
        };
        let cell = &mut surface[(column, y)];
        cell.reset();
        cell.set_symbol(symbol).set_style(style);
    }
}

/// A press on a rail and the drag that may follow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// Pressed the thumb, `grab` rows below its top.
    ThumbPressed {
        column: u16,
        row: u16,
        grab: usize,
    },
    /// Pressed the track, where a release pages.
    TrackPressed {
        column: u16,
        row: u16,
    },
    Scrolling {
        grab: usize,
    },
    Resizing,
    /// Dragging the track up or down, which does nothing.
    Ignoring,
}

impl Gesture {
    /// A press at `column`, `row` on a rail whose thumb covers the screen rows `thumb`. Without
    /// a thumb, the whole rail is track.
    pub fn press(column: u16, row: u16, thumb: Option<Range<u16>>) -> Self {
        match thumb {
            Some(thumb) if thumb.contains(&row) => Self::ThumbPressed {
                column,
                row,
                grab: usize::from(row - thumb.start),
            },
            _ => Self::TrackPressed { column, row },
        }
    }

    /// The gesture once the pointer is at `column`, `row`, `released` or not. The first move
    /// decides what a press becomes: scrolling for the thumb, nothing for the track, or resizing
    /// if it goes sideways and the rail is `resizable`.
    pub fn moved(self, column: u16, row: u16, released: bool, resizable: bool) -> Self {
        let Some((from_column, from_row)) = self.origin().filter(|_| !released) else {
            return self;
        };
        let (dx, dy) = (from_column.abs_diff(column), from_row.abs_diff(row));
        match self {
            _ if dx == dy => self,
            _ if dx > dy && resizable => Self::Resizing,
            Self::ThumbPressed { grab, .. } => Self::Scrolling { grab },
            _ => Self::Ignoring,
        }
    }

    /// Where the rail was pressed, while the drag has not decided what it is yet.
    fn origin(self) -> Option<(u16, u16)> {
        match self {
            Self::ThumbPressed { column, row, .. } | Self::TrackPressed { column, row } => {
                Some((column, row))
            }
            Self::Scrolling { .. } | Self::Resizing | Self::Ignoring => None,
        }
    }
}

/// Whether the screen cell `column`, `row` is in `area`.
pub fn contains(area: Rect, column: u16, row: u16) -> bool {
    (area.left()..area.right()).contains(&column) && (area.top()..area.bottom()).contains(&row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_spans_the_visible_share() {
        assert_eq!(scrollbar_thumb(10, 10, 0), None);
        assert_eq!(scrollbar_thumb(20, 10, 0), Some(0..5));
        assert_eq!(scrollbar_thumb(20, 10, 10), Some(5..10));
        assert_eq!(scrollbar_thumb(1000, 10, 0), Some(0..1));
        assert_eq!(scrollbar_thumb(1000, 10, 990), Some(9..10));
    }

    #[test]
    fn thumbs_follow_the_rows_shown() {
        // All shown, or none to show.
        assert_eq!(thumb(10, 0..10, 5), None);
        assert_eq!(thumb(0, 0..0, 5), None);
        // A third shown, from the start, the middle and the end.
        assert_eq!(thumb(30, 0..10, 9), Some(0..3));
        assert_eq!(thumb(30, 10..20, 9), Some(3..6));
        assert_eq!(thumb(30, 20..30, 9), Some(6..9));
        // Scrolled past the end, the last rows shown end at the bottom.
        assert_eq!(thumb(30, 28..40, 9), Some(8..9));
        // Wrapped lines show fewer rows of content than the rail is tall.
        assert_eq!(thumb(100, 50..53, 10), Some(4..5));
    }

    #[test]
    fn drags_and_presses_map_to_offsets() {
        // A thumb of 2 rows on a rail of 10 travels 8 rows over offsets 0..=80.
        assert_eq!(dragged_offset(0, 0, 2, 10, 80), Some(0));
        assert_eq!(dragged_offset(5, 1, 2, 10, 80), Some(40));
        assert_eq!(dragged_offset(20, 0, 2, 10, 80), Some(80));
        assert_eq!(dragged_offset(3, 0, 10, 10, 80), None);
        assert_eq!(paging_direction(1, 3..5), Some(Direction::Backward));
        assert_eq!(paging_direction(4, 3..5), None);
        assert_eq!(paging_direction(5, 3..5), Some(Direction::Forward));
    }

    #[test]
    fn gestures_decide_on_the_first_move() {
        let thumb = Some(4..6);
        let pressed = Gesture::press(10, 5, thumb.clone());
        assert_eq!(
            pressed,
            Gesture::ThumbPressed {
                column: 10,
                row: 5,
                grab: 1
            }
        );
        assert_eq!(
            pressed.moved(10, 8, false, false),
            Gesture::Scrolling { grab: 1 }
        );
        assert_eq!(pressed.moved(14, 6, false, true), Gesture::Resizing);
        assert_eq!(
            pressed.moved(14, 6, false, false),
            Gesture::Scrolling { grab: 1 }
        );
        let track = Gesture::press(10, 1, thumb);
        assert_eq!(track.moved(10, 3, false, true), Gesture::Ignoring);
        assert_eq!(track.moved(10, 1, true, true), track);
    }

    #[test]
    fn rails_carry_both_sides() {
        let area = Rect::new(0, 0, 1, 6);
        let mut surface = Surface::empty(area);
        let (thumb, other) = (Color::Gray, Color::Blue);
        // The left pane's thumb on rows 0..3, the right one's on rows 2..5.
        let left = Bar::new(0, 6, Some(0..3), thumb);
        let right = Bar::new(0, 6, Some(2..5), other);
        let styles = RailStyles {
            base: Style::default(),
            track: Style::default().fg(Color::White),
            thumb,
        };
        render_rail(&mut surface, 0, 0..6, &[left], &[right], styles);
        let cells: Vec<_> = (0..6)
            .map(|y| {
                let cell = &surface[(0, y)];
                (cell.symbol.as_str(), cell.fg, cell.bg)
            })
            .collect();
        assert_eq!(
            cells,
            [
                ("▌", thumb, Color::Reset),
                ("▌", thumb, Color::Reset),
                ("▌", thumb, other),
                ("▐", other, Color::Reset),
                ("▐", other, Color::Reset),
                ("│", Color::White, Color::Reset),
            ]
        );
    }

    #[test]
    fn rails_cover_what_was_drawn_before() {
        use helix_view::graphics::Modifier;

        let area = Rect::new(0, 0, 4, 3);
        let mut surface = Surface::empty(area);
        let text = Style::default()
            .fg(Color::Red)
            .bg(Color::Blue)
            .add_modifier(Modifier::BOLD | Modifier::ITALIC);
        surface.set_string(0, 0, "text", text);
        let styles = RailStyles {
            base: Style::default().bg(Color::Black),
            track: Style::default().fg(Color::Gray),
            thumb: Color::Gray,
        };
        render_rail(&mut surface, 0, 0..3, &[], &[], styles);
        let cell = &surface[(0, 0)];
        assert_eq!(cell.symbol.as_str(), "│");
        assert_eq!((cell.fg, cell.bg), (Color::Gray, Color::Black));
        assert!(cell.modifier.is_empty());
    }
}
