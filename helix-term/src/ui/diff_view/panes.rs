//! The panes of the diff view in the editor: keeping them in step, going through their hunks,
//! opening their file, and drawing what changed.

use std::ops;

use helix_core::{
    anchor_at_visual_offset,
    doc_formatter::{FormattedGrapheme, TextFormat},
    line_ending::line_end_char_index,
    movement::Direction,
    text_annotations::TextAnnotations,
    unicode::width::UnicodeWidthChar,
    visual_offset_from_anchor, Position, Range, RopeSlice, Selection,
};
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
    let offset = doc!(editor, &docs[0]).view_offset(views[0]);
    let Some(offset) = partner_offset(editor, views, docs, offset) else {
        return;
    };
    let doc = doc!(editor, &docs[0]);
    let text = doc.text().slice(..);
    let cursor = text.char_to_line(doc.selection(views[0]).primary().cursor(text));
    let cursor_row = doc
        .diff_view
        .as_ref()
        .and_then(|pane| pane.row_of_line(cursor));

    let partner = doc!(editor, &docs[1]);
    let Some(pane) = partner.diff_view.as_ref() else {
        return;
    };
    let text = partner.text().slice(..);
    let cursor_line = text.char_to_line(partner.selection(views[1]).primary().cursor(text));
    let cursor = cursor_row
        .map(|row| pane.line_at_or_before(row).unwrap_or(0) as usize)
        .filter(|&line| line != cursor_line)
        .map(|line| text.line_to_char(line));

    let partner = doc_mut!(editor, &docs[1]);
    if offset != partner.view_offset(views[1]) {
        partner.set_view_offset(views[1], offset);
    }
    if let Some(pos) = cursor {
        partner.set_selection(views[1], Selection::point(pos));
    }
}

/// Draws the pane `docs[1]` in `views[1]` at the row the frame of the focused pane `docs[0]` in
/// `views[0]` shows at its top, while the focused pane glides, so that their rows line up.
pub(super) fn follow_frame(editor: &mut Editor, views: [ViewId; 2], docs: [DocumentId; 2]) {
    let (view, doc) = (editor.tree.get(views[0]), doc!(editor, &docs[0]));
    let frame = view.render_offset(doc);
    if frame == doc.view_offset(views[0]) {
        return;
    }
    let Some(offset) = partner_offset(editor, views, docs, frame) else {
        return;
    };
    let partner = &editor.documents[&docs[1]];
    editor.tree.get_mut(views[1]).follow_frame(partner, offset);
}

/// The offset of the pane `docs[1]` in `views[1]` that shows the row the focused pane `docs[0]`
/// in `views[0]` shows at its top when scrolled to `offset`.
fn partner_offset(
    editor: &Editor,
    views: [ViewId; 2],
    docs: [DocumentId; 2],
    offset: ViewPosition,
) -> Option<ViewPosition> {
    let (view, doc) = (editor.tree.get(views[0]), doc!(editor, &docs[0]));
    let pane = doc.diff_view.as_ref()?;
    let format = doc.text_format(view.inner_width(doc), None);
    let annotations = view.text_annotations(doc, None);
    let top = top(pane, doc.text().slice(..), offset, &format, &annotations);

    let (view, partner) = (editor.tree.get(views[1]), doc!(editor, &docs[1]));
    let pane = partner.diff_view.as_ref()?;
    let format = partner.text_format(view.inner_width(partner), None);
    let annotations = view.text_annotations(partner, None);
    Some(offset_at(
        pane,
        partner.text().slice(..),
        top,
        partner.view_offset(views[1]).horizontal_offset,
        &format,
        &annotations,
    ))
}

/// Where the top of a pane is among the rows both panes show: on a row of the alignment, below
/// `rows` of the visual rows it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Top {
    row: u32,
    rows: usize,
}

/// The top of `pane`, showing `text` laid out with `format` and `annotations`, scrolled to
/// `offset`.
fn top(
    pane: &Pane,
    text: RopeSlice,
    offset: ViewPosition,
    format: &TextFormat,
    annotations: &TextAnnotations,
) -> Top {
    let line = text.char_to_line(offset.anchor.min(text.len_chars()));
    // A pane anchored on its first line counts from the top, above it.
    let (row, from) = match pane.row_of_line(line) {
        Some(row) if line > 0 => (row, text.line_to_char(line)),
        _ => (0, 0),
    };
    // The rows of a wrapping line above the anchor.
    let above = if offset.anchor == from {
        0
    } else {
        visual_offset_from_anchor(text, from, offset.anchor, format, annotations, usize::MAX)
            .map_or(0, |(pos, _)| pos.row)
    };
    let mut top = Top {
        row,
        rows: above + offset.vertical_offset,
    };
    // Rows past those of the row are those of the rows after it.
    let last = pane.alignment.rows().len().saturating_sub(1) as u32;
    while top.row < last {
        let height = pane.row_height(top.row);
        if top.rows < height {
            break;
        }
        top.rows -= height;
        top.row += 1;
    }
    top
}

/// The offset showing `top` at the top of `pane`, showing `text` laid out with `format` and
/// `annotations`.
fn offset_at(
    pane: &Pane,
    text: RopeSlice,
    top: Top,
    horizontal_offset: usize,
    format: &TextFormat,
    annotations: &TextAnnotations,
) -> ViewPosition {
    // From the pane's line on the row or the last one before it, else from the top.
    let (from, from_row) = match pane.line_at_or_before(top.row) {
        Some(line) if line > 0 => (
            text.line_to_char(line as usize),
            pane.row_of_line(line as usize).unwrap_or(top.row),
        ),
        _ => (0, 0),
    };
    let rows = (from_row..top.row)
        .map(|row| pane.row_height(row))
        .sum::<usize>()
        + top.rows;
    let (anchor, vertical_offset) =
        anchor_at_visual_offset(text, from, rows as isize, format, annotations);
    ViewPosition {
        anchor,
        horizontal_offset,
        vertical_offset,
    }
}

/// Selects the hunk `count` hunks after or before each selection of the focused pane. Returns
/// whether there was one.
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

/// What selecting the hunk on the rows `rows` of `pane` selects.
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

/// Opens the file the focused pane shows a side of in the diff's place, at the cursor's row.
/// Returns whether the focused buffer is a pane.
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

/// Paints the rows of a pane: its changed lines and their padding, and its fillers.
pub struct Rows<'a> {
    pane: &'a Pane,
    text: RopeSlice<'a>,
    styles: Styles,
    /// The character the fillers are drawn with and the columns it takes, if any.
    filler: Option<(String, u16)>,
    /// The text area of the pane.
    area: Rect,
    /// The first visual row not painted yet.
    next_row: u16,
    /// While the pane wraps lines: whether the visual line drawn last ends its line, and the
    /// line whose end comes next.
    line_ends: bool,
    next_line: usize,
}

impl<'a> Rows<'a> {
    pub fn new(
        pane: &'a Pane,
        text: RopeSlice<'a>,
        theme: &Theme,
        filler: Option<char>,
        area: Rect,
    ) -> Self {
        Self {
            pane,
            text,
            styles: Styles::new(theme),
            filler: filler.map(|filler| (filler.to_string(), filler.width().unwrap_or(0) as u16)),
            area,
            next_row: 0,
            line_ends: false,
            next_line: 0,
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

    /// Paints `rows` as fillers: gray, and drawn with the filler character.
    fn paint_fillers(&mut self, renderer: &mut TextRenderer, rows: ops::Range<u16>) {
        self.paint(renderer, rows.clone(), self.styles.filler);
        let Some((filler, width)) = &self.filler else {
            return;
        };
        // Only the rows shown: those after a line can be thousands.
        let top = renderer.offset.row as u16;
        let shown = top..top + renderer.viewport.height;
        let (area, style) = (self.area, self.styles.filler_character);
        for row in rows.start.max(shown.start)..rows.end.min(shown.end) {
            renderer.fill_row(area.x, row, area.width, filler, *width, style);
        }
    }

    /// Paints `rows` as rows of `line`: in the color of a changed line.
    fn paint_line(&mut self, renderer: &mut TextRenderer, rows: ops::Range<u16>, line: usize) {
        if self
            .pane
            .alignment
            .change(self.pane.side, line as u32)
            .is_some()
        {
            self.paint(renderer, rows, self.styles.line(self.pane.side));
        } else {
            self.next_row = self.next_row.max(rows.end);
        }
    }

    /// Paints `rows`, the last of the virtual rows after `line`, or above the first line without one.
    fn paint_virtual(
        &mut self,
        renderer: &mut TextRenderer,
        rows: ops::Range<u16>,
        line: Option<usize>,
    ) {
        if rows.is_empty() {
            return;
        }
        let fillers = line.map_or(usize::MAX, |line| self.pane.rows_after(line as u32).1);
        let padding = rows.len().saturating_sub(fillers) as u16;
        let fillers_start = rows.start + padding;
        if let Some(line) = line {
            self.paint_line(renderer, rows.start..fillers_start, line);
        }
        self.paint_fillers(renderer, fillers_start..rows.end);
    }

    /// Asks for the grapheme where `line` ends, while the pane wraps lines.
    fn hook_line_end(&mut self, line: usize) -> usize {
        self.next_line = line;
        if self.pane.wrap.is_none() || line >= self.text.len_lines() {
            usize::MAX
        } else if line + 1 < self.text.len_lines() {
            line_end_char_index(&self.text, line)
        } else {
            self.text.len_chars()
        }
    }
}

impl Decoration for Rows<'_> {
    fn reset_pos(&mut self, pos: usize) -> usize {
        self.line_ends = false;
        if self.pane.wrap.is_none() {
            return usize::MAX;
        }
        self.hook_line_end(self.text.char_to_line(pos.min(self.text.len_chars())))
    }

    fn decorate_grapheme(&mut self, _: &mut TextRenderer, _: &FormattedGrapheme) -> usize {
        self.line_ends = true;
        self.hook_line_end(self.next_line + 1)
    }

    fn decorate_line(&mut self, renderer: &mut TextRenderer, pos: LinePos) {
        // Rows skipped since the last line are virtual rows of the line before, whose end is
        // above the view, or those above the first line.
        let skipped = self.next_row..pos.visual_line;
        self.paint_virtual(renderer, skipped, pos.doc_line.checked_sub(1));
        self.paint_line(renderer, pos.visual_line..pos.visual_line + 1, pos.doc_line);
    }

    fn render_virt_lines_above(&mut self, renderer: &mut TextRenderer, first_line: LinePos) {
        self.paint_virtual(renderer, self.next_row..first_line.visual_line, None);
    }

    fn render_virt_lines(
        &mut self,
        renderer: &mut TextRenderer,
        pos: LinePos,
        virt_off: Position,
    ) -> Position {
        // Where a line wraps, its virtual rows come after its last visual line.
        let line_ends = std::mem::take(&mut self.line_ends) || self.pane.wrap.is_none();
        let (padding, fillers) = self.pane.rows_after(pos.doc_line as u32);
        if !line_ends || padding + fillers == 0 {
            return Position::new(0, 0);
        }
        let start = pos.visual_line + virt_off.row as u16;
        let fillers_start = start + padding as u16;
        self.paint_line(renderer, start..fillers_start, pos.doc_line);
        self.paint_fillers(renderer, fillers_start..fillers_start + fillers as u16);
        Position::new(padding + fillers, 0)
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use std::sync::Arc;

    use helix_core::Rope;
    use helix_view::diff_view::{builtin, Alignment, Row, Wrap};

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

    /// The top of `pane` showing `text` at `offset`, laid out with `format`.
    fn top_of(pane: &Pane, text: &Rope, offset: ViewPosition, format: &TextFormat) -> Top {
        let text = text.slice(..);
        let mut annotations = TextAnnotations::default();
        annotations.add_line_annotation(Box::new(pane.filler_lines(text)));
        top(pane, text, offset, format, &annotations)
    }

    /// The offset showing `top` at the top of `pane` showing `text`, laid out with `format`.
    fn offset_of(pane: &Pane, text: &Rope, top: Top, format: &TextFormat) -> ViewPosition {
        let text = text.slice(..);
        let mut annotations = TextAnnotations::default();
        annotations.add_line_annotation(Box::new(pane.filler_lines(text)));
        offset_at(pane, text, top, 0, format, &annotations)
    }

    fn at(anchor: usize, vertical_offset: usize) -> ViewPosition {
        ViewPosition {
            anchor,
            horizontal_offset: 0,
            vertical_offset,
        }
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
        let format = TextFormat::default();
        let row = |row, rows| Top { row, rows };
        // The new side at its top shows the fillers above the old side's first line.
        assert_eq!(top_of(&new_pane, &new, at(0, 0), &format), row(0, 0));
        assert_eq!(offset_of(&old_pane, &old, row(0, 0), &format), at(0, 0));
        assert_eq!(offset_of(&old_pane, &old, row(1, 0), &format), at(0, 1));
        assert_eq!(offset_of(&old_pane, &old, row(2, 0), &format), at(0, 2));
        // Line 1 of the old side, `b`, is on row 3.
        assert_eq!(offset_of(&old_pane, &old, row(3, 0), &format), at(2, 0));
        // Row 4 is a filler after `b` on the old side: below the end of `b`, as Helix anchors
        // views on virtual lines.
        assert_eq!(offset_of(&old_pane, &old, row(4, 0), &format), at(3, 1));
        assert_eq!(top_of(&old_pane, &old, at(3, 1), &format), row(4, 0));
        assert_eq!(top_of(&old_pane, &old, at(2, 1), &format), row(4, 0));
        assert_eq!(top_of(&new_pane, &new, at(8, 0), &format), row(4, 0));
    }

    #[test]
    fn wrapped_panes_scroll_to_the_same_row() {
        // The old side's second line wraps to three rows, which the new one's pads.
        let long = "the quick brown fox jumps";
        let old = Rope::from(format!("a\n{long}\nc\n"));
        let new = Rope::from("a\nthe quick\nc\n");
        let alignment = Arc::new(builtin::align(old.slice(..), new.slice(..)));
        let format = TextFormat {
            soft_wrap: true,
            viewport_width: 12,
            ..TextFormat::default()
        };
        let wrap = || {
            Some(Wrap::new(
                [old.clone(), new.clone()],
                [format.clone(), format.clone()],
            ))
        };
        let (mut old_pane, mut new_pane) =
            (pane(Side::Old, &alignment), pane(Side::New, &alignment));
        (old_pane.wrap, new_pane.wrap) = (wrap(), wrap());
        let rows = old_pane.row_height(1);
        assert!(rows > 2, "{rows}");

        // The old side's top on the second row of `long`.
        let second =
            anchor_at_visual_offset(old.slice(..), 2, 1, &format, &TextAnnotations::default());
        assert!(second.0 > 2 && second.1 == 0, "{second:?}");
        let top = top_of(&old_pane, &old, at(second.0, 0), &format);
        assert_eq!(top, Top { row: 1, rows: 1 });
        // The new side shows its padding there, below the end of its line, and comes back to
        // the same row.
        let end = new.line_to_char(2) - 1;
        assert_eq!(offset_of(&new_pane, &new, top, &format), at(end, 1));
        assert_eq!(top_of(&new_pane, &new, at(end, 1), &format), top);
        assert_eq!(offset_of(&old_pane, &old, top, &format), at(second.0, 0));
        // Past the padding, both are at `c`.
        let after = Top { row: 2, rows: 0 };
        assert_eq!(top_of(&new_pane, &new, at(2, rows), &format), after);
        let c = old.line_to_char(2);
        assert_eq!(offset_of(&old_pane, &old, after, &format), at(c, 0));
    }
}
