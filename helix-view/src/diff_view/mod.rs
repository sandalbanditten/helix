//! The diff view: two texts side by side, lined up by rows, with what changed highlighted.
//!
//! [`Alignment`] says which line of each text is shown on each row and what changed about the
//! lines. It comes from difftastic's structural diff ([`difftastic`]) or from Helix's own line
//! diff ([`builtin`]).
//!
//! Each side is shown by a read-only document holding its text as it is, a [`Pane`]. The rows on
//! which a side has no line are virtual lines of the pane ([`FillerLines`]), so both panes are as
//! tall and their rows line up.

pub mod alignment;
pub mod builtin;
pub mod difftastic;

use std::{path::PathBuf, sync::Arc};

use helix_core::{text_annotations::LineAnnotation, Position};

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
}

impl Pane {
    /// The filler rows of the pane, as virtual lines.
    pub fn filler_lines(&self) -> FillerLines<'_> {
        FillerLines(self.alignment.fillers(self.side))
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

/// The filler rows of a pane as virtual lines: above its first line, and after the lines facing
/// lines of the other side that it has none for.
pub struct FillerLines<'a>(&'a Fillers);

impl LineAnnotation for FillerLines<'_> {
    fn virtual_lines_above(&mut self) -> usize {
        self.0.above as usize
    }

    fn insert_virtual_lines(&mut self, _: usize, _: Position, doc_line: usize) -> Position {
        let after = &self.0.after;
        let rows = after
            .binary_search_by_key(&(doc_line as u32), |(line, _)| *line)
            .map_or(0, |index| after[index].1);
        Position::new(rows as usize, 0)
    }
}
