//! The diff view: two texts side by side, lined up by rows, with what changed highlighted.
//!
//! [`Alignment`] says which line of each text is shown on each row and what changed about the
//! lines. It comes from difftastic's structural diff ([`difftastic`]) or from Helix's own line
//! diff ([`builtin`]).
//!
//! Each side is shown by a read-only document holding its text as it is, a [`Pane`]. The rows on
//! which a side has no line are virtual lines of the pane ([`FillerLines`]), so both panes are as
//! tall and their rows line up. While the panes wrap lines ([`Wrap`]), a row is as tall as the
//! taller of its lines, the shorter one padded with virtual lines.

pub mod alignment;
pub mod builtin;
pub mod difftastic;

use std::{cell::RefCell, collections::HashMap, path::PathBuf, sync::Arc};

use helix_core::{
    doc_formatter::{DocumentFormatter, TextFormat},
    text_annotations::{LineAnnotation, TextAnnotations},
    Position, Rope, RopeSlice,
};

pub use alignment::{Alignment, Fillers, LineChange, Row, Side};

use crate::{DocumentId, ViewId};

/// What a document of the diff view shows: one side of an alignment.
#[derive(Debug)]
pub struct Pane {
    pub side: Side,
    pub alignment: Arc<Alignment>,
    /// The name the pane goes by, like `src/main.rs (HEAD)`.
    pub name: String,
    /// The pane showing the other side.
    pub partner: DocumentId,
    /// The file `gf` opens to edit what the diff shows, if there is one.
    pub file: Option<PathBuf>,
    /// The view focused before the diff opened, which gets the focus back when it closes.
    pub origin: Option<ViewId>,
    /// How the panes wrap their lines, while either does.
    pub wrap: Option<Wrap>,
}

impl Pane {
    /// The filler rows of the pane showing `text`, and the rows padding its wrapped lines, as
    /// virtual lines.
    pub fn filler_lines<'a>(&'a self, text: RopeSlice<'a>) -> FillerLines<'a> {
        FillerLines { pane: self, text }
    }

    /// The rows `row` of the alignment takes: as many as the taller of its lines wraps to.
    pub fn row_height(&self, row: u32) -> usize {
        self.wrap
            .as_ref()
            .map_or(1, |wrap| wrap.row_height(&self.alignment, row))
    }

    /// The virtual rows above the first line: the fillers facing the other side's first lines.
    pub fn rows_above(&self) -> usize {
        let above = self.alignment.fillers(self.side).above;
        (0..above).map(|row| self.row_height(row)).sum()
    }

    /// The virtual rows after `line`: those padding it to the height of its row, then the fillers
    /// facing lines of the other side it has none for.
    pub fn rows_after(&self, line: u32) -> (usize, usize) {
        let after = &self.alignment.fillers(self.side).after;
        let count = after
            .binary_search_by_key(&line, |(line, _)| *line)
            .map_or(0, |index| after[index].1);
        let (Some(wrap), Some(row)) = (&self.wrap, self.row_of_line(line as usize)) else {
            return (0, count as usize);
        };
        let padding = wrap.row_height(&self.alignment, row) - wrap.line_rows(self.side, line);
        let fillers = (row + 1..row + 1 + count)
            .map(|row| wrap.row_height(&self.alignment, row))
            .sum();
        (padding, fillers)
    }

    /// The row of the alignment the pane shows `line` on.
    pub fn row_of_line(&self, line: usize) -> Option<u32> {
        self.alignment.row_of_line(self.side, line as u32)
    }

    /// The line the pane shows on `row`, or the line before it if `row` is a filler; `None` if no
    /// line comes before it.
    pub fn line_at_or_before(&self, row: u32) -> Option<u32> {
        self.alignment.line_at_or_before(self.side, row)
    }
}

/// How the panes wrap their lines: both texts, and the format each pane lays its text out with.
#[derive(Debug)]
pub struct Wrap {
    texts: [Rope; 2],
    formats: [TextFormat; 2],
    /// The rows of the lines of each side measured so far.
    heights: [RefCell<HashMap<u32, usize>>; 2],
}

impl Wrap {
    /// The wrapping of the old text `texts[0]` and the new one `texts[1]` with `formats`.
    pub fn new(texts: [Rope; 2], formats: [TextFormat; 2]) -> Self {
        Self {
            texts,
            formats,
            heights: Default::default(),
        }
    }

    /// The formats of the old and the new side.
    pub fn formats(&self) -> &[TextFormat; 2] {
        &self.formats
    }

    /// The rows `line` of `side` wraps to.
    fn line_rows(&self, side: Side, line: u32) -> usize {
        let index = match side {
            Side::Old => 0,
            Side::New => 1,
        };
        *self.heights[index]
            .borrow_mut()
            .entry(line)
            .or_insert_with(|| {
                line_rows(
                    self.texts[index].slice(..),
                    line as usize,
                    &self.formats[index],
                )
            })
    }

    fn row_height(&self, alignment: &Alignment, row: u32) -> usize {
        let row = alignment.rows()[row as usize];
        [Side::Old, Side::New]
            .into_iter()
            .filter_map(|side| Some(self.line_rows(side, row.line(side)?)))
            .max()
            .unwrap_or(1)
    }
}

/// The rows `line` of `text` takes laid out with `format`, as the editor lays it out.
fn line_rows(text: RopeSlice, line: usize, format: &TextFormat) -> usize {
    let end = text.line_to_char(line + 1);
    // The end of the text is drawn on the last line, unless a line break ends that.
    let last = line + 1 == text.len_lines();
    let annotations = TextAnnotations::default();
    let formatter = DocumentFormatter::new_at_prev_checkpoint(
        text,
        format,
        &annotations,
        text.line_to_char(line),
    );
    let mut rows = 1;
    for grapheme in formatter {
        if grapheme.char_idx >= end && !last {
            break;
        }
        rows = grapheme.visual_pos.row + 1;
    }
    rows
}

/// The filler rows of a pane as virtual lines: above its first line, and after the lines facing
/// lines of the other side that it has none for. Lines also get the rows padding them to the
/// height of their row.
pub struct FillerLines<'a> {
    pane: &'a Pane,
    text: RopeSlice<'a>,
}

impl LineAnnotation for FillerLines<'_> {
    fn virtual_lines_above(&mut self) -> usize {
        self.pane.rows_above()
    }

    fn insert_virtual_lines(
        &mut self,
        line_end_char_idx: usize,
        _: Position,
        doc_line: usize,
    ) -> Position {
        // A visual line ends where its line wraps too: only the end of the line gets rows.
        if self
            .text
            .char_to_line(line_end_char_idx.min(self.text.len_chars()))
            <= doc_line
        {
            return Position::new(0, 0);
        }
        let (padding, fillers) = self.pane.rows_after(doc_line as u32);
        Position::new(padding + fillers, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "a\nthe quick brown fox jumps over the lazy dog\nc\nd\n";
    const NEW: &str = "an added first line that wraps around\na\nthe quick\nc\n\
                       an added line long enough to wrap a few times over\nd\n";

    fn format(width: u16) -> TextFormat {
        TextFormat {
            soft_wrap: true,
            viewport_width: width,
            ..TextFormat::default()
        }
    }

    /// The texts and panes of the diff of `old` and `new`, wrapped at `widths`.
    fn panes(old: &str, new: &str, widths: [u16; 2]) -> ([Rope; 2], [Pane; 2]) {
        let texts = [Rope::from(old), Rope::from(new)];
        let alignment = Arc::new(builtin::align(texts[0].slice(..), texts[1].slice(..)));
        let pane = |side| Pane {
            side,
            alignment: alignment.clone(),
            name: String::new(),
            partner: DocumentId::default(),
            file: None,
            origin: None,
            wrap: Some(Wrap::new(texts.clone(), widths.map(format))),
        };
        (texts.clone(), [pane(Side::Old), pane(Side::New)])
    }

    /// The visual row each line of `pane`, showing `text`, starts on, laid out with `format`.
    fn line_starts(pane: &Pane, text: &Rope, format: &TextFormat) -> Vec<usize> {
        let text = text.slice(..);
        let mut annotations = TextAnnotations::default();
        annotations.add_line_annotation(Box::new(pane.filler_lines(text)));
        let mut starts = Vec::new();
        for grapheme in DocumentFormatter::new_at_prev_checkpoint(text, format, &annotations, 0) {
            if grapheme.line_idx == starts.len() {
                starts.push(grapheme.visual_pos.row);
            }
        }
        starts.truncate(pane.alignment.lines(pane.side) as usize);
        starts
    }

    /// Asserts that the lines of each row start on the same visual row in both panes.
    fn assert_lined_up([old, new]: &[Rope; 2], [old_pane, new_pane]: &[Pane; 2]) {
        let formats = old_pane.wrap.as_ref().unwrap().formats();
        let old_starts = line_starts(old_pane, old, &formats[0]);
        let new_starts = line_starts(new_pane, new, &formats[1]);
        for row in old_pane.alignment.rows() {
            if let (Some(old_line), Some(new_line)) = (row.old, row.new) {
                assert_eq!(
                    old_starts[old_line as usize], new_starts[new_line as usize],
                    "{row:?} lines up"
                );
            }
        }
    }

    #[test]
    fn wrapped_lines_are_padded_to_the_height_of_their_row() {
        let (texts, panes) = panes(OLD, NEW, [12, 12]);
        let [old_pane, new_pane] = &panes;
        let wrap = old_pane.wrap.as_ref().unwrap();
        let (long, short) = (wrap.line_rows(Side::Old, 1), wrap.line_rows(Side::New, 2));
        assert!(long > short && short == 1, "{long} {short}");
        assert_eq!(
            new_pane.rows_after(2),
            (long - 1, 0),
            "the short line is padded"
        );
        assert_eq!(old_pane.rows_after(1), (0, 0));

        // Fillers are as tall as the lines they face.
        let added = wrap.line_rows(Side::New, 4);
        assert!(added > 1);
        assert_eq!(old_pane.rows_after(2), (0, added));
        let first = wrap.line_rows(Side::New, 0);
        assert!(first > 1);
        assert_eq!(old_pane.rows_above(), first);
        assert_eq!(new_pane.rows_above(), 0);
        assert_eq!(line_starts(old_pane, &texts[0], &format(12))[0], first);
        assert_lined_up(&texts, &panes);
    }

    #[test]
    fn fillers_follow_the_end_of_a_wrapped_line() {
        let line = "a line long enough to wrap a few times";
        let (texts, panes) = panes(
            &format!("{line}\nz\n"),
            &format!("{line}\nadded\nz\n"),
            [12, 12],
        );
        assert!(panes[0].wrap.as_ref().unwrap().line_rows(Side::Old, 0) > 1);
        assert_eq!(panes[0].rows_after(0), (0, 1));
        assert_lined_up(&texts, &panes);
    }

    #[test]
    fn panes_of_different_widths_line_up() {
        let (texts, panes) = panes(OLD, NEW, [12, 20]);
        let wrap = panes[1].wrap.as_ref().unwrap();
        // The same line wraps less in the wider pane.
        assert!(wrap.line_rows(Side::Old, 0) == 1 && wrap.line_rows(Side::New, 0) > 1);
        assert_lined_up(&texts, &panes);
    }

    #[test]
    fn panes_without_wrapping_take_a_row_a_line() {
        let ([old, _], [mut old_pane, _]) = panes(OLD, NEW, [12, 12]);
        old_pane.wrap = None;
        assert_eq!(old_pane.rows_above(), 1);
        assert_eq!(old_pane.rows_after(2), (0, 1));
        assert_eq!(
            line_starts(&old_pane, &old, &TextFormat::default()),
            [1, 2, 3, 5]
        );
    }
}
