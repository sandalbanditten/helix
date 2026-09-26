//! The mouse in the file tree: clicking rows, the wheel, and the rail between tree and editor.
//! Dragging the rail's thumb scrolls, pressing its track pages, and dragging it sideways resizes
//! the tree. The first move of a drag decides which of the two it is.

use helix_view::{
    editor::{Action as OpenAction, FileTreeSide},
    graphics::Rect,
    input::{MouseButton, MouseEvent, MouseEventKind},
    Editor,
};

use super::{ops, tree::Kind, viewport, FileTree, Workspace, MAX_WIDTH, MIN_WIDTH};
use crate::compositor::EventResult;

/// A press on the rail and the drag that may follow it.
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

impl FileTree {
    /// Handles a mouse event over the tree, or one of a drag that started on its rail. `None`
    /// leaves the event to the editor.
    pub fn handle_mouse(&mut self, event: &MouseEvent, editor: &mut Editor) -> Option<EventResult> {
        let area = self.area?;
        let MouseEvent {
            kind, column, row, ..
        } = *event;
        if let Some(gesture) = self.gesture.take() {
            if let MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) =
                kind
            {
                self.continue_gesture(gesture, event, editor);
                return Some(EventResult::Consumed(None));
            }
        }
        if !contains(area, column, row) {
            return None;
        }
        let side = editor.config().file_tree.side;
        let rail = match side {
            FileTreeSide::Left => area.right() - 1,
            FileTreeSide::Right => area.left(),
        };
        match kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let lines = editor.config().scroll_lines.unsigned_abs();
                if let Some(workspace) = &mut self.workspace {
                    let start = workspace.start;
                    workspace.start = match kind {
                        MouseEventKind::ScrollDown => start + lines,
                        _ => start.saturating_sub(lines),
                    };
                    workspace.clamp_start();
                }
            }
            MouseEventKind::Down(MouseButton::Left) if column == rail => {
                // Without a thumb, the whole rail is track, which still resizes.
                let thumb = self.workspace.as_ref().and_then(Workspace::thumb);
                self.gesture = Some(match thumb {
                    Some(thumb) if thumb.contains(&row) => Gesture::ThumbPressed {
                        column,
                        row,
                        grab: usize::from(row - thumb.start),
                    },
                    _ => Gesture::TrackPressed { column, row },
                });
            }
            MouseEventKind::Down(MouseButton::Left) => self.click(row, editor),
            _ => {}
        }
        Some(EventResult::Consumed(None))
    }

    fn continue_gesture(&mut self, gesture: Gesture, event: &MouseEvent, editor: &Editor) {
        let released = matches!(event.kind, MouseEventKind::Up(_));
        let (column, row) = (event.column, event.row);
        // The first move decides between scrolling and resizing.
        let gesture = match gesture.origin().filter(|_| !released) {
            Some((from_column, from_row)) => {
                let (dx, dy) = (from_column.abs_diff(column), from_row.abs_diff(row));
                match gesture {
                    _ if dx == dy => gesture,
                    _ if dx > dy => Gesture::Resizing,
                    Gesture::ThumbPressed { grab, .. } => Gesture::Scrolling { grab },
                    _ => Gesture::Ignoring,
                }
            }
            None => gesture,
        };
        match gesture {
            Gesture::Scrolling { grab } => {
                if let Some(workspace) = &mut self.workspace {
                    workspace.drag_thumb(row, grab);
                }
            }
            Gesture::Resizing => self.resize_to(column, editor),
            Gesture::TrackPressed { row, .. } if released => {
                if let Some(workspace) = &mut self.workspace {
                    workspace.page_towards(row);
                }
            }
            _ => {}
        }
        if !released {
            self.gesture = Some(gesture);
        }
    }

    /// Widens or narrows the tree so that its rail is at `column`.
    fn resize_to(&mut self, column: u16, editor: &Editor) {
        let Some(area) = self.area else {
            return;
        };
        let width = match editor.config().file_tree.side {
            FileTreeSide::Left => (column + 1).saturating_sub(area.left()),
            FileTreeSide::Right => area.right().saturating_sub(column),
        };
        self.width = Some(width.min(self.max_width.min(MAX_WIDTH)).max(MIN_WIDTH));
    }

    /// Opens the file on the screen row `row`, or expands or collapses the directory on it.
    fn click(&mut self, row: u16, editor: &mut Editor) {
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let Some(index) = workspace.row_at(row) else {
            return;
        };
        let node = workspace.rows[index].node;
        let kind = workspace.tree.node(node).kind;
        if kind == Kind::Directory && index != 0 {
            workspace.toggle_row(index);
            // The rows stay where they are under the pointer.
            self.update_rows(editor);
        } else if kind.is_file() {
            let path = workspace.root.join(&workspace.rows[index].path);
            match ops::open(editor, &path, OpenAction::Replace) {
                // Like opening with Enter.
                Ok(()) => self.unfocus(),
                Err(err) => editor.set_error(err.to_string()),
            }
        }
    }
}

impl Workspace {
    /// The ordinary row drawn on the screen row `row`. Pinned rows and blank space have none.
    fn row_at(&self, row: u16) -> Option<usize> {
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
    fn thumb(&self) -> Option<std::ops::Range<u16>> {
        let area = self.rows_area;
        let thumb = viewport::thumb(&self.rows, self.drawn_start, area.height as usize)?;
        Some(area.top() + thumb.start as u16..area.top() + thumb.end as u16)
    }

    /// Scrolls so that the thumb, held `grab` rows below its top, is at the screen row `row`.
    fn drag_thumb(&mut self, row: u16, grab: usize) {
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
    fn page_towards(&mut self, row: u16) {
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

fn contains(area: Rect, column: u16, row: u16) -> bool {
    (area.left()..area.right()).contains(&column) && (area.top()..area.bottom()).contains(&row)
}

#[cfg(test)]
mod tests {
    use super::super::{tests::workspace, tree::tests::file};
    use super::*;

    /// The test workspace with ten more files, drawn four rows high.
    fn scrollable() -> Workspace {
        let mut workspace = workspace();
        let root = workspace.tree.root();
        let mut entries: Vec<_> = (0..10).map(|i| file(&format!("f{i}"))).collect();
        entries.extend(["a", "b"].map(file));
        workspace.tree.apply_listing(root, Some(entries));
        workspace.rebuild_rows();
        workspace.rows_area = Rect::new(0, 5, 20, 4);
        workspace
    }

    #[test]
    fn rows_are_found_below_the_pinned_ones() {
        let mut workspace = scrollable();
        assert_eq!(workspace.row_at(5), Some(0));
        assert_eq!(workspace.row_at(9), None);
        // Scrolled down, the root row is pinned and inert.
        workspace.drawn_start = 3;
        assert_eq!(workspace.row_at(5), None);
        assert_eq!(workspace.row_at(6), Some(3));
    }

    #[test]
    fn the_thumb_drags_and_the_track_pages() {
        let mut workspace = scrollable();
        let rows = workspace.rows.len();
        assert_eq!(workspace.thumb(), Some(5..7));
        // Dragging the thumb to the bottom shows the last rows.
        workspace.drag_thumb(8, 0);
        assert_eq!(workspace.start, viewport::max_start(&workspace.rows, 4));
        workspace.drawn_start = workspace.start;
        assert_eq!(workspace.thumb().map(|thumb| thumb.end), Some(9));
        // Pressing the track above the thumb goes a page up.
        let before = workspace.start;
        workspace.page_towards(5);
        assert!(workspace.start < before);
        assert!(rows > 4);
    }
}
