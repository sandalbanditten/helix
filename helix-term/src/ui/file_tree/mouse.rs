//! The mouse in the file tree: clicking rows, the wheel, and the rail between tree and editor.

use helix_view::{
    editor::{Action as OpenAction, FileTreeSide},
    graphics::Rect,
    input::{MouseButton, MouseEvent, MouseEventKind},
    Editor,
};

use super::{ops, tree::Kind, FileTree};
use crate::compositor::EventResult;
use crate::ui::dock;

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
    /// Handles a mouse event over the tree. `None` leaves the event to the editor.
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
                    let down = kind == MouseEventKind::ScrollDown;
                    workspace.browser.scroll(lines, down);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if column == rail => {
                // Without a thumb, the whole rail is track, which still resizes.
                let thumb = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspace.browser.thumb());
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
                    workspace.browser.drag_thumb(row, grab);
                }
            }
            Gesture::Resizing => self.resize_to(column, editor),
            Gesture::TrackPressed { row, .. } if released => {
                if let Some(workspace) = &mut self.workspace {
                    workspace.browser.page_towards(row);
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
        self.width = Some(dock::clamp_width(
            width,
            self.max_width.min(dock::MAX_WIDTH),
        ));
    }

    /// Opens the file on the screen row `row`, or expands or collapses the directory on it.
    fn click(&mut self, row: u16, editor: &mut Editor) {
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let browser = &mut workspace.browser;
        let Some(index) = browser.row_at(row) else {
            return;
        };
        let node = browser.rows[index].node;
        let kind = browser.tree.node(node).kind;
        if kind == Kind::Directory && index != 0 {
            browser.toggle_row(index);
            // The rows stay where they are under the pointer.
            self.update_rows(editor);
        } else if kind.is_file() {
            let path = workspace.root.join(&workspace.browser.rows[index].path);
            match ops::open(editor, &path, OpenAction::Replace) {
                // Like opening with Enter.
                Ok(()) => self.unfocus(),
                Err(err) => editor.set_error(err.to_string()),
            }
        }
    }
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    (area.left()..area.right()).contains(&column) && (area.top()..area.bottom()).contains(&row)
}
