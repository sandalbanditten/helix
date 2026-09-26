//! Which rows fit into the panel.
//!
//! The panel shows the rows from `start` on. Above them it pins the rows that the row at `start`
//! hangs from (its directories, up to the root row) so the current place in the hierarchy stays
//! legible, unless they would not all fit.

use std::ops::Range;

use super::rows::Rows;
use crate::ui::scrollbar_thumb;

/// The rows pinned above the ordinary rows when those start at `start`, outermost first.
pub fn pinned(rows: &Rows, start: usize, height: usize) -> Vec<usize> {
    if start >= rows.len() {
        return Vec::new();
    }
    let ancestors = rows.ancestors(start);
    if ancestors.len() < height {
        ancestors
    } else {
        Vec::new()
    }
}

/// The number of ordinary rows that fit below the pinned ones.
pub fn capacity(rows: &Rows, start: usize, height: usize) -> usize {
    height - pinned(rows, start, height).len()
}

/// The largest useful `start`: the first one that shows the last row with nothing below it.
pub fn max_start(rows: &Rows, height: usize) -> usize {
    let len = rows.len();
    let mut start = len.saturating_sub(height);
    while start + 1 < len && pinned(rows, start, height).len() + (len - start) > height {
        start += 1;
    }
    start
}

pub fn clamp(rows: &Rows, start: usize, height: usize) -> usize {
    start.min(max_start(rows, height))
}

/// Whether row `index` is among the ordinary rows.
pub fn is_visible(rows: &Rows, start: usize, height: usize, index: usize) -> bool {
    (start..start + capacity(rows, start, height)).contains(&index)
}

/// The `start` closest to `start` that shows row `target` with `scrolloff` rows around it.
pub fn reveal(rows: &Rows, start: usize, height: usize, target: usize, scrolloff: usize) -> usize {
    let margin = |start| scrolloff.min(capacity(rows, start, height).saturating_sub(1) / 2);
    let start = clamp(rows, start, height);
    if target < start + margin(start) {
        return clamp(rows, target.saturating_sub(margin(start)), height);
    }
    let max = max_start(rows, height);
    // No more than `height` rows fit, so there is no need to try starts before that.
    let mut start = start.max((target + 1).saturating_sub(height));
    while start < max && target + margin(start) >= start + capacity(rows, start, height) {
        start += 1;
    }
    start
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Top,
    Center,
    Bottom,
}

/// The `start` that puts row `target` at the top, center or bottom of the ordinary rows.
pub fn align(rows: &Rows, height: usize, target: usize, align: Align) -> usize {
    let start = match align {
        Align::Top => target,
        Align::Center => (target.saturating_sub(height)..=target)
            .find(|&start| target - start <= capacity(rows, start, height).saturating_sub(1) / 2)
            .unwrap_or(target),
        Align::Bottom => (target.saturating_sub(height)..=target)
            .find(|&start| target < start + capacity(rows, start, height))
            .unwrap_or(target),
    };
    clamp(rows, start, height)
}

/// The rows of the rail that its thumb covers, if the rows do not fit.
pub fn thumb(rows: &Rows, start: usize, height: usize) -> Option<Range<usize>> {
    let len = rows.len();
    let max = max_start(rows, height);
    // Pinned rows let `start` go past `len - height`; scale it so the thumb ends at the bottom.
    let offset = if max == 0 {
        0
    } else {
        start.min(max) * len.saturating_sub(height) / max
    };
    scrollbar_thumb(len, height, offset)
}

#[cfg(test)]
mod tests {
    use super::super::tree::tests::{dir, file, tree_with};
    use super::super::tree::Tree;
    use super::*;

    /// root, `alpha` (expanded, 2 files), `beta`, then `tail-00`..`tail-{tails}`.
    fn fixture(tails: usize) -> Rows {
        let mut entries = vec![dir("alpha"), dir("beta")];
        entries.extend((0..tails).map(|i| file(&format!("tail-{i:02}"))));
        let mut tree: Tree = tree_with(entries);
        let alpha = tree.find("alpha".as_ref()).unwrap();
        tree.expand(alpha);
        tree.apply_listing(alpha, Some(vec![file("a-0"), file("a-1")]));
        Rows::build(&tree, true, None)
    }

    #[test]
    fn ancestors_are_pinned_when_they_fit() {
        let rows = fixture(4);
        // 0 root, 1 alpha, 2 a-0, 3 a-1, 4 beta, 5.. tails
        assert_eq!(pinned(&rows, 0, 5), Vec::<usize>::new());
        assert_eq!(pinned(&rows, 2, 5), [0, 1]);
        assert_eq!(pinned(&rows, 4, 5), [0]);
        assert_eq!(pinned(&rows, 2, 2), Vec::<usize>::new());
        assert_eq!(capacity(&rows, 2, 5), 3);
    }

    #[test]
    fn the_last_row_ends_at_the_bottom() {
        let rows = fixture(4); // 9 rows
        assert_eq!(max_start(&rows, 5), 5);
        assert_eq!(pinned(&rows, 5, 5), [0]);
        assert_eq!(max_start(&rows, 20), 0);
        assert_eq!(clamp(&rows, 8, 5), 5);
    }

    #[test]
    fn revealing_moves_as_little_as_possible() {
        let rows = fixture(10); // 15 rows
        assert_eq!(reveal(&rows, 0, 6, 3, 0), 0);
        // Row 7 is below the six visible rows. Starting at `a-1` (3) pins root and `alpha`,
        // leaving room for rows 3..7 only; starting at `beta` (4) pins just the root.
        assert_eq!(reveal(&rows, 0, 6, 7, 0), 4);
        assert!(!is_visible(&rows, 3, 6, 7));
        assert!(is_visible(&rows, 4, 6, 7));
        assert_eq!(reveal(&rows, 8, 6, 5, 0), 5);
        // With scrolloff, one row stays visible past the target.
        assert_eq!(reveal(&rows, 0, 6, 7, 1), 4);
        assert_eq!(reveal(&rows, 8, 6, 8, 1), 7);
    }

    #[test]
    fn aligning_puts_the_row_in_place() {
        let rows = fixture(10);
        assert_eq!(align(&rows, 6, 8, Align::Top), 8);
        assert_eq!(align(&rows, 6, 8, Align::Bottom), 4);
        assert_eq!(align(&rows, 6, 8, Align::Center), 6);
        assert_eq!(align(&rows, 6, 14, Align::Top), max_start(&rows, 6));
    }

    #[test]
    fn thumb_spans_the_rail() {
        let rows = fixture(10);
        let max = max_start(&rows, 6);
        assert_eq!(thumb(&rows, 0, 6), Some(0..3));
        assert_eq!(thumb(&rows, max, 6).map(|thumb| thumb.end), Some(6));
        assert_eq!(thumb(&rows, 0, 20), None);
    }
}
