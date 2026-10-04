//! Browsing a tree in a panel: the rows it shows, the cursor and the scroll position.

use std::path::Path;

use helix_view::{graphics::Rect, smooth_scroll::SmoothOffset, Editor};

use super::{
    rows::{InputRow, Rows},
    tree::{NodeId, Tree},
    viewport::{self, Align},
};

/// A move of the cursor, or of the rows around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Down,
    Up,
    Expand,
    Collapse,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    First,
    Last,
    AlignCenter,
    AlignTop,
    AlignBottom,
}

pub struct Browser {
    pub tree: Tree,
    pub rows: Rows,
    /// Whether a run of single-child directories shows as one row.
    pub flatten_dirs: bool,
    /// Whether `rows` needs rebuilding.
    pub dirty: bool,
    pub cursor: NodeId,
    /// The first ordinary row.
    pub start: usize,
    pub smooth_scroll: SmoothOffset,
    /// A node to scroll into view once the panel height is known.
    pub scroll_to: Option<NodeId>,
    /// The number of rows the panel showed last.
    pub height: usize,
    /// Where the rows were drawn last, and the first ordinary row drawn, for the mouse.
    pub rows_area: Rect,
    pub drawn_start: usize,
}

impl Browser {
    /// Browses `tree` from its root.
    pub fn new(tree: Tree, flatten_dirs: bool) -> Self {
        let rows = Rows::build(&tree, flatten_dirs, None);
        Self {
            cursor: tree.root(),
            tree,
            rows,
            flatten_dirs,
            dirty: false,
            start: 0,
            smooth_scroll: SmoothOffset::default(),
            scroll_to: None,
            height: 0,
            rows_area: Rect::default(),
            drawn_start: 0,
        }
    }

    /// Moves the cursor by `motion`, or expands or collapses the directory under it.
    pub fn navigate(&mut self, motion: Motion) {
        let Some(cursor) = self.rows.index_of(self.cursor) else {
            self.cursor = self.tree.root();
            return;
        };
        let last = self.rows.len() - 1;
        let height = self.height.max(1);
        let start = viewport::clamp(&self.rows, self.start, height);
        let page = viewport::capacity(&self.rows, start, height).max(1);
        let target = match motion {
            Motion::Down if cursor == last => 0,
            Motion::Down => cursor + 1,
            Motion::Up if cursor == 0 => last,
            Motion::Up => cursor - 1,
            Motion::HalfPageDown => (cursor + page / 2).min(last),
            Motion::HalfPageUp => cursor.saturating_sub(page / 2),
            Motion::PageDown => (cursor + page).min(last),
            Motion::PageUp => cursor.saturating_sub(page),
            Motion::First => 0,
            Motion::Last => last,
            Motion::Expand => {
                self.expand_row(cursor);
                cursor
            }
            Motion::Collapse => {
                let row = &self.rows[cursor];
                if cursor != 0 && self.tree.node(row.node).expanded {
                    // Collapsing the first directory of a run collapses the rest of it.
                    self.tree.collapse(row.head);
                    self.dirty = true;
                }
                cursor
            }
            Motion::AlignCenter | Motion::AlignTop | Motion::AlignBottom => {
                let align = match motion {
                    Motion::AlignCenter => Align::Center,
                    Motion::AlignTop => Align::Top,
                    _ => Align::Bottom,
                };
                self.start = viewport::align(&self.rows, height, cursor, align);
                cursor
            }
        };
        self.cursor = self.rows[target].node;
    }

    /// Expands or collapses the directory of row `index`.
    pub fn toggle_row(&mut self, index: usize) {
        let row = &self.rows[index];
        if self.tree.node(row.node).expanded {
            // Collapsing the first directory of a run collapses the rest of it.
            self.tree.collapse(row.head);
            self.dirty = true;
        } else {
            self.expand_row(index);
        }
    }

    /// Expands every directory of the run that row `index` stands for.
    pub fn expand_row(&mut self, index: usize) {
        let row = &self.rows[index];
        let (head, mut node) = (row.head, Some(row.node));
        while let Some(id) = node {
            self.tree.expand(id);
            node = (id != head).then(|| self.tree.node(id).parent).flatten();
        }
        self.dirty = true;
    }

    /// Rebuilds `rows` with the input row `input`, keeping the cursor and the scroll position on the
    /// same entries.
    pub fn rebuild_rows(&mut self, input: Option<InputRow>) {
        let path_of = |node| {
            self.rows
                .index_of(node)
                .map(|index| self.rows[index].path.clone())
        };
        let anchor = self
            .rows
            .get(self.start)
            .map(|row| (row.node, row.path.clone()));
        let cursor_path = path_of(self.cursor);
        self.rows = Rows::build(&self.tree, self.flatten_dirs, input);
        self.start = anchor
            .and_then(|(node, path)| self.shown_row(node, Some(&path)))
            .unwrap_or(self.start.min(self.rows.len() - 1));
        self.cursor = self
            .shown_row(self.cursor, cursor_path.as_deref())
            .map_or(self.tree.root(), |index| self.rows[index].node);
    }

    /// The row of `node`, last seen at `path`, or of its closest ancestor that has one.
    pub fn shown_row(&self, node: NodeId, path: Option<&Path>) -> Option<usize> {
        let mut current = if self.tree.contains(node) {
            Some(node)
        } else {
            // The entry is gone; start from its closest ancestor that is still there.
            path?
                .ancestors()
                .find_map(|ancestor| self.tree.find(ancestor))
        };
        while let Some(node) = current {
            if let Some(index) = self.rows.index_of(node) {
                return Some(index);
            }
            current = self.tree.node(node).parent;
        }
        None
    }

    /// Scrolls just enough to show row `index` with `scrolloff` rows around it.
    pub fn reveal_row(&mut self, index: usize, scrolloff: usize) {
        let height = self.height.max(1);
        self.start = viewport::reveal(&self.rows, self.start, height, index, scrolloff);
    }

    /// Lays out the rows for drawing in `area`. Returns the first ordinary row to draw.
    pub fn frame(&mut self, area: Rect, editor: &mut Editor) -> usize {
        let height = area.height as usize;
        self.height = height;
        if let Some(target) = self.scroll_to.take() {
            if let Some(index) = self.rows.index_of(target) {
                if !viewport::is_visible(&self.rows, self.start, height, index) {
                    self.start = viewport::align(&self.rows, height, index, Align::Center);
                }
            }
        }
        let start = viewport::clamp(&self.rows, self.start, height);
        let start = self.smooth_scroll.frame(start, area.height, editor);
        (self.rows_area, self.drawn_start) = (area, start);
        start
    }

    /// Scrolls `lines` rows down, or up, as the mouse wheel does.
    pub fn scroll(&mut self, lines: usize, down: bool) {
        self.start = if down {
            self.start + lines
        } else {
            self.start.saturating_sub(lines)
        };
        self.clamp_start();
    }

    /// The ordinary row drawn on the screen row `row`. Pinned rows and blank space have none.
    pub fn row_at(&self, row: u16) -> Option<usize> {
        let area = self.rows_area;
        if !(area.top()..area.bottom()).contains(&row) {
            return None;
        }
        let offset = usize::from(row - area.top());
        let pinned = viewport::pinned(&self.rows, self.drawn_start, area.height as usize).len();
        let index = self.drawn_start + offset.checked_sub(pinned)?;
        (index < self.rows.len()).then_some(index)
    }

    /// The screen rows of the rail's thumb, if the rows do not fit.
    pub fn thumb(&self) -> Option<std::ops::Range<u16>> {
        let area = self.rows_area;
        let thumb = viewport::thumb(&self.rows, self.drawn_start, area.height as usize)?;
        Some(area.top() + thumb.start as u16..area.top() + thumb.end as u16)
    }

    /// Scrolls so that the thumb, held `grab` rows below its top, is at the screen row `row`.
    pub fn drag_thumb(&mut self, row: u16, grab: usize) {
        let Some(thumb) = self.thumb() else {
            return;
        };
        let height = self.rows_area.height as usize;
        let travel = height - thumb.len();
        if travel == 0 {
            return;
        }
        let offset = usize::from(row.saturating_sub(self.rows_area.top()))
            .saturating_sub(grab)
            .min(travel);
        let max_start = viewport::max_start(&self.rows, height);
        self.start = (offset * max_start + travel / 2) / travel;
    }

    /// Scrolls a page towards the screen row `row` of the rail's track.
    pub fn page_towards(&mut self, row: u16) {
        let Some(thumb) = self.thumb() else {
            return;
        };
        let height = self.rows_area.height as usize;
        let page = viewport::capacity(&self.rows, self.start, height).max(1);
        if row < thumb.start {
            self.start = self.start.saturating_sub(page);
        } else if row >= thumb.end {
            self.start += page;
        }
        self.clamp_start();
    }

    fn clamp_start(&mut self) {
        self.start = viewport::clamp(&self.rows, self.start, self.rows_area.height as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{tree::tests::file, workspace::tests::workspace};
    use super::*;

    /// The test workspace's browser with ten more files, drawn four rows high.
    fn scrollable() -> Browser {
        let mut browser = workspace().browser;
        let root = browser.tree.root();
        let mut entries: Vec<_> = (0..10).map(|i| file(&format!("f{i}"))).collect();
        entries.extend(["a", "b"].map(file));
        browser.tree.apply_listing(root, Some(entries));
        browser.rebuild_rows(None);
        browser.rows_area = Rect::new(0, 5, 20, 4);
        browser
    }

    fn cursor_label(browser: &Browser) -> &str {
        let index = browser.rows.index_of(browser.cursor).unwrap();
        &browser.rows[index].label
    }

    #[test]
    fn the_cursor_wraps_around() {
        let mut browser = workspace().browser;
        browser.navigate(Motion::Up);
        assert_eq!(cursor_label(&browser), "b");
        browser.navigate(Motion::Down);
        assert_eq!(cursor_label(&browser), "root");
        browser.navigate(Motion::PageDown);
        assert_eq!(cursor_label(&browser), "b");
        browser.navigate(Motion::HalfPageUp);
        assert_eq!(cursor_label(&browser), "root");
    }

    #[test]
    fn rows_are_found_below_the_pinned_ones() {
        let mut browser = scrollable();
        assert_eq!(browser.row_at(5), Some(0));
        assert_eq!(browser.row_at(9), None);
        // Scrolled down, the root row is pinned and inert.
        browser.drawn_start = 3;
        assert_eq!(browser.row_at(5), None);
        assert_eq!(browser.row_at(6), Some(3));
    }

    #[test]
    fn the_thumb_drags_and_the_track_pages() {
        let mut browser = scrollable();
        let rows = browser.rows.len();
        assert_eq!(browser.thumb(), Some(5..7));
        // Dragging the thumb to the bottom shows the last rows.
        browser.drag_thumb(8, 0);
        assert_eq!(browser.start, viewport::max_start(&browser.rows, 4));
        browser.drawn_start = browser.start;
        assert_eq!(browser.thumb().map(|thumb| thumb.end), Some(9));
        // Pressing the track above the thumb goes a page up.
        let before = browser.start;
        browser.page_towards(5);
        assert!(browser.start < before);
        assert!(rows > 4);
    }
}
