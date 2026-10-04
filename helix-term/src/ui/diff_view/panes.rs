//! The panes of the diff view in the editor: keeping them in step, going through their hunks,
//! opening their file, and drawing what changed.

use std::ops;

use helix_core::{movement::Direction, Position, Range, RopeSlice, Selection};
use helix_view::{
    align_view, current, current_ref,
    diff_view::{LineChange, Pane, Side},
    doc, doc_mut,
    document::Mode,
    editor::Action,
    graphics::{Rect, Style},
    view::ViewPosition,
    Align, Document, DocumentId, Editor, Theme, ViewId,
};

use super::styles::Styles;
use crate::ui::{
    document::{LinePos, TextRenderer},
    text_decorations::Decoration,
};

/// Scrolls the pane `docs[1]` in `views[1]` to the row the focused pane `docs[0]` in `views[0]`
/// shows at its top, and puts its cursor on the row of the focused pane's cursor.
pub(super) fn sync(editor: &mut Editor, views: [ViewId; 2], docs: [DocumentId; 2]) {
    let doc = doc!(editor, &docs[0]);
    let Some(pane) = doc.diff_view.as_ref() else {
        return;
    };
    let text = doc.text().slice(..);
    let offset = doc.view_offset(views[0]);
    let top = top_row(pane, text.char_to_line(offset.anchor), offset);
    let cursor = text.char_to_line(doc.selection(views[0]).primary().cursor(text));
    let cursor_row = pane.row_of_line(cursor);

    let partner = doc_mut!(editor, &docs[1]);
    let Some(pane) = partner.diff_view.as_ref() else {
        return;
    };
    let text = partner.text().slice(..);
    let current = partner.view_offset(views[1]);
    let offset = offset_at_row(pane, text, top, current.horizontal_offset);
    let cursor_line = text.char_to_line(partner.selection(views[1]).primary().cursor(text));
    let cursor = cursor_row
        .map(|row| pane.line_at_or_before(row).unwrap_or(0) as usize)
        .filter(|&line| line != cursor_line)
        .map(|line| text.line_to_char(line));
    if offset != current {
        partner.set_view_offset(views[1], offset);
    }
    if let Some(pos) = cursor {
        partner.set_selection(views[1], Selection::point(pos));
    }
}

/// The row of the alignment at the top of a pane scrolled to `offset`, anchored on `line`.
fn top_row(pane: &Pane, line: usize, offset: ViewPosition) -> u32 {
    // A pane anchored at its start counts from the top, above its first line.
    let anchor_row = if offset.anchor == 0 {
        0
    } else {
        pane.row_of_line(line).unwrap_or(0)
    };
    anchor_row + offset.vertical_offset as u32
}

/// The offset showing `row` of the alignment at the top of `pane`.
fn offset_at_row(pane: &Pane, text: RopeSlice, row: u32, horizontal_offset: usize) -> ViewPosition {
    let (anchor, vertical_offset) = match pane.line_at_or_before(row) {
        Some(line) if line > 0 => {
            let line_row = pane.row_of_line(line as usize).unwrap_or(row);
            (text.line_to_char(line as usize), row - line_row)
        }
        _ => (0, row),
    };
    ViewPosition {
        anchor,
        horizontal_offset,
        vertical_offset: vertical_offset as usize,
    }
}

/// Selects the hunk `count` hunks after or before each selection of the focused pane, like `]g`
/// and `[g` select the changes of the diff gutter. Returns whether there was one.
pub fn goto_hunk(editor: &mut Editor, direction: Direction, count: usize) -> bool {
    let mode = editor.mode;
    let (view, doc) = current!(editor);
    let Some(pane) = doc.diff_view.as_ref() else {
        return false;
    };
    let text = doc.text().slice(..);
    let mut found = false;
    let selection = doc.selection(view.id).clone().transform(|range| {
        let line = range.cursor_line(text);
        let Some(hunk) = hunk_from(pane, line, direction, count) else {
            return range;
        };
        found = true;
        let new_range = hunk_range(pane, text, &pane.alignment.hunks()[hunk]);
        if mode == Mode::Select {
            let head = if new_range.head < range.anchor {
                new_range.anchor
            } else {
                new_range.head
            };
            Range::new(range.anchor, head)
        } else {
            new_range.with_direction(direction)
        }
    });
    if found {
        doc.set_selection(view.id, selection);
    }
    found
}

/// Selects the first hunk of the focused pane, or the last one, like `[G` and `]G`.
pub fn goto_end_hunk(editor: &mut Editor, last: bool) {
    let (view, doc) = current!(editor);
    let Some(pane) = doc.diff_view.as_ref() else {
        return;
    };
    let hunks = pane.alignment.hunks();
    let hunk = if last { hunks.last() } else { hunks.first() };
    if let Some(hunk) = hunk {
        let range = hunk_range(pane, doc.text().slice(..), hunk);
        let jump = (doc.id(), doc.selection(view.id).clone());
        view.push_jump(doc, jump);
        doc.set_selection(view.id, Selection::single(range.anchor, range.head));
    }
}

/// The index of the hunk `count` hunks after or before the row of `line` of `pane`.
fn hunk_from(pane: &Pane, line: usize, direction: Direction, count: usize) -> Option<usize> {
    let row = pane.row_of_line(line)?;
    let hunks = pane.alignment.hunks();
    let count = count.max(1);
    match direction {
        Direction::Forward => {
            let mut next = hunks.partition_point(|hunk| hunk.start <= row);
            // A hunk of only fillers after the last line is selected on the last line, so from
            // there it is behind.
            let last_line = line + 1 == pane.alignment.lines(pane.side) as usize;
            if hunks.get(next).is_some_and(|hunk| {
                last_line
                    && pane
                        .alignment
                        .line_at_or_after(pane.side, hunk.start)
                        .is_none()
            }) {
                next += 1;
            }
            (next < hunks.len()).then(|| (next + count - 1).min(hunks.len() - 1))
        }
        Direction::Backward => {
            let mut before = hunks.partition_point(|hunk| hunk.end <= row);
            // A hunk of only fillers right above the cursor is the one the cursor is on, like a
            // removal of the diff gutter.
            if before > 0 && hunks[before - 1].end == row && !has_lines(pane, &hunks[before - 1]) {
                before -= 1;
            }
            before
                .checked_sub(1)
                .map(|prev| prev.saturating_sub(count - 1))
        }
    }
}

/// Whether the side of `pane` has lines on the rows `rows`.
fn has_lines(pane: &Pane, rows: &ops::Range<u32>) -> bool {
    pane.alignment
        .line_at_or_after(pane.side, rows.start)
        .and_then(|line| pane.row_of_line(line as usize))
        .is_some_and(|row| row < rows.end)
}

/// What selecting the hunk on the rows `rows` of `pane` selects: its lines on the pane's side,
/// or the first char of the line after it where the side has only fillers there.
fn hunk_range(pane: &Pane, text: RopeSlice, rows: &ops::Range<u32>) -> Range {
    let alignment = &pane.alignment;
    let first = alignment.line_at_or_after(pane.side, rows.start);
    let last = alignment.line_at_or_before(pane.side, rows.end.saturating_sub(1));
    match (first, last) {
        (Some(first), Some(last)) if first <= last => Range::new(
            text.line_to_char(first as usize),
            text.line_to_char(last as usize + 1),
        ),
        _ => {
            let line = first.unwrap_or_else(|| alignment.lines(pane.side).saturating_sub(1));
            let anchor = text.line_to_char(line as usize);
            Range::new(anchor, (anchor + 1).min(text.len_chars()))
        }
    }
}

/// Opens the file the focused pane shows a side of in the diff's place, at the line on the
/// cursor's row: the diff closes, and the file opens in the view it was asked for from. Returns
/// whether the focused buffer is a pane.
pub fn open_file(editor: &mut Editor) -> bool {
    let (view, doc) = current_ref!(editor);
    let Some(pane) = doc.diff_view.as_ref() else {
        return false;
    };
    let Some(file) = pane.file.clone() else {
        editor.set_error("The diff shows no file to open");
        return true;
    };
    let text = doc.text().slice(..);
    let line = text.char_to_line(doc.selection(view.id).primary().cursor(text));
    // The file is the new side: from the old side, it opens at the line on the same row.
    let line = match pane.side {
        Side::New => line,
        Side::Old => pane
            .row_of_line(line)
            .and_then(|row| pane.alignment.line_at_or_before(Side::New, row))
            .unwrap_or(0) as usize,
    };
    // Closing one pane closes both, giving the focus back.
    editor.close(view.id);
    let action = if editor.tree.views().next().is_some() {
        Action::Replace
    } else {
        Action::VerticalSplit
    };
    if let Err(err) = editor.open(&file, action) {
        editor.set_error(format!("Cannot open {}: {err}", file.display()));
        return true;
    }
    let (view, doc) = current!(editor);
    let text = doc.text().slice(..);
    let pos = text.line_to_char(line.min(text.len_lines() - 1));
    doc.set_selection(view.id, Selection::point(pos));
    align_view(doc, view, Align::Center);
    true
}

/// The styles of the text that changed on the lines `lines` of the pane `doc`, by char ranges.
pub fn changed_text(
    doc: &Document,
    pane: &Pane,
    lines: ops::Range<usize>,
    theme: &Theme,
) -> Vec<(ops::Range<usize>, Style)> {
    let style = Styles::new(theme).text(pane.side);
    let text = doc.text().slice(..);
    let lines = lines.start as u32..lines.end.min(text.len_lines()) as u32;
    let mut spans = Vec::new();
    for (line, change) in pane.alignment.changes(pane.side, lines) {
        let LineChange::Parts(parts) = change else {
            continue;
        };
        let start = text.line_to_byte(*line as usize);
        for part in parts {
            let range = text.byte_to_char(start + part.start as usize)
                ..text.byte_to_char(start + part.end as usize);
            spans.push((range, style));
        }
    }
    spans
}

/// Paints the rows of a pane: the background of its changed lines and of its fillers.
pub struct Rows<'a> {
    pane: &'a Pane,
    styles: Styles,
    /// The text area of the pane.
    area: Rect,
    /// The first visual row not painted yet.
    next_row: u16,
}

impl<'a> Rows<'a> {
    pub fn new(pane: &'a Pane, theme: &Theme, area: Rect) -> Self {
        Self {
            pane,
            styles: Styles::new(theme),
            area,
            next_row: 0,
        }
    }

    fn paint(&mut self, renderer: &mut TextRenderer, rows: ops::Range<u16>, style: Style) {
        if rows.is_empty() {
            return;
        }
        let area = Rect::new(
            self.area.x,
            rows.start,
            self.area.width,
            rows.end - rows.start,
        );
        renderer.set_style(area, style);
        self.next_row = self.next_row.max(rows.end);
    }
}

impl Decoration for Rows<'_> {
    fn decorate_line(&mut self, renderer: &mut TextRenderer, pos: LinePos) {
        // Rows skipped since the last line are fillers: those above the first line, or those
        // after a line above the view.
        self.paint(renderer, self.next_row..pos.visual_line, self.styles.filler);
        let changed = self
            .pane
            .alignment
            .change(self.pane.side, pos.doc_line as u32);
        let row = pos.visual_line..pos.visual_line + 1;
        if pos.first_visual_line && changed.is_some() {
            self.paint(renderer, row, self.styles.line(self.pane.side));
        } else {
            self.next_row = self.next_row.max(row.end);
        }
    }

    fn render_virt_lines(
        &mut self,
        renderer: &mut TextRenderer,
        pos: LinePos,
        virt_off: Position,
    ) -> Position {
        let fillers = &self.pane.alignment.fillers(self.pane.side).after;
        let count = fillers
            .binary_search_by_key(&(pos.doc_line as u32), |(line, _)| *line)
            .map_or(0, |index| fillers[index].1) as u16;
        let start = pos.visual_line + virt_off.row as u16;
        self.paint(renderer, start..start + count, self.styles.filler);
        Position::new(count as usize, 0)
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use std::sync::Arc;

    use helix_core::Rope;
    use helix_view::diff_view::{builtin, Alignment, Row};

    use super::*;

    fn pane(side: Side, alignment: &Arc<Alignment>) -> Pane {
        Pane {
            side,
            alignment: alignment.clone(),
            name: String::new(),
            partner: DocumentId::default(),
            file: None,
            origin: None,
            wrap: None,
        }
    }

    #[test]
    fn hunks_are_selected_by_the_rows_they_are_on() {
        // A line changed, and one added that the old side shows a filler for.
        let old = Rope::from("a\nb\nc\nd\ne\n");
        let new = Rope::from("a\nB\nc\nd\nnew\ne\n");
        let alignment = Arc::new(builtin::align(old.slice(..), new.slice(..)));
        assert_eq!(alignment.hunks(), [1..2, 4..5]);
        let (old_pane, new_pane) = (pane(Side::Old, &alignment), pane(Side::New, &alignment));
        let (old, new) = (old.slice(..), new.slice(..));

        assert_eq!(hunk_from(&new_pane, 0, Direction::Forward, 1), Some(0));
        assert_eq!(hunk_from(&new_pane, 1, Direction::Forward, 1), Some(1));
        assert_eq!(
            hunk_from(&new_pane, 0, Direction::Forward, 5),
            Some(1),
            "counts stop at the last"
        );
        assert_eq!(hunk_from(&new_pane, 4, Direction::Forward, 1), None);
        assert_eq!(hunk_range(&new_pane, new, &(1..2)), Range::new(2, 4));
        assert_eq!(hunk_range(&new_pane, new, &(4..5)), Range::new(8, 12));

        // The old side has only a filler on the second hunk: it selects the line after it.
        assert_eq!(hunk_range(&old_pane, old, &(4..5)), Range::new(8, 9));
        assert_eq!(hunk_from(&old_pane, 3, Direction::Forward, 1), Some(1));
        // From there `[g` goes past it, as from a removal in the diff gutter.
        assert_eq!(hunk_from(&old_pane, 4, Direction::Backward, 1), Some(0));
        assert_eq!(
            hunk_from(&old_pane, 1, Direction::Backward, 1),
            None,
            "inside the first"
        );
        assert_eq!(hunk_from(&new_pane, 5, Direction::Backward, 1), Some(1));

        // Lines removed at the end: the new side selects its last line for them.
        let old = Rope::from("a\nb\nc\n");
        let new = Rope::from("a\n");
        let alignment = Arc::new(builtin::align(old.slice(..), new.slice(..)));
        assert_eq!(alignment.hunks(), [1..3]);
        let new_pane = pane(Side::New, &alignment);
        assert_eq!(
            hunk_range(&new_pane, new.slice(..), &(1..3)),
            Range::new(0, 1)
        );
        assert_eq!(hunk_from(&new_pane, 0, Direction::Forward, 1), None);
    }

    #[test]
    fn panes_scroll_to_the_same_row() {
        // old: a b c       new: x y a b z c
        let old = Rope::from("a\nb\nc\n");
        let new = Rope::from("x\ny\na\nb\nz\nc\n");
        let alignment = Arc::new(builtin::align(old.slice(..), new.slice(..)));
        assert_eq!(
            alignment.rows()[..3],
            [
                Row {
                    old: None,
                    new: Some(0)
                },
                Row {
                    old: None,
                    new: Some(1)
                },
                Row {
                    old: Some(0),
                    new: Some(2)
                },
            ]
        );
        let (old_pane, new_pane) = (pane(Side::Old, &alignment), pane(Side::New, &alignment));
        let at = |anchor, vertical_offset| ViewPosition {
            anchor,
            horizontal_offset: 0,
            vertical_offset,
        };
        // The new side at its top shows the fillers above the old side's first line.
        assert_eq!(top_row(&new_pane, 0, at(0, 0)), 0);
        assert_eq!(offset_at_row(&old_pane, old.slice(..), 0, 0), at(0, 0));
        assert_eq!(offset_at_row(&old_pane, old.slice(..), 1, 0), at(0, 1));
        assert_eq!(offset_at_row(&old_pane, old.slice(..), 2, 0), at(0, 2));
        // Line 1 of the old side, `b`, is on row 3.
        assert_eq!(offset_at_row(&old_pane, old.slice(..), 3, 0), at(2, 0));
        // Row 4 is a filler after `b` on the old side.
        assert_eq!(
            offset_at_row(&old_pane, old.slice(..), 4, 7),
            ViewPosition {
                anchor: 2,
                horizontal_offset: 7,
                vertical_offset: 1
            }
        );
        assert_eq!(top_row(&old_pane, 1, at(2, 1)), 4);
        assert_eq!(top_row(&new_pane, 4, at(8, 0)), 4);
    }
}
