//! The run a compilation buffer shows.
//!
//! A compilation buffer is a document holding the output of a command, like `cargo build`, as it
//! arrives. The file positions in it, its loci, are diagnostics that open their files.

use std::{ops::Range, path::PathBuf, time::Instant};

use helix_core::{Assoc, ChangeSet};
use helix_stdx::{process::ProcessGroup, pty::Pty};

use crate::{graphics::Style, ViewId};

/// Which command a run is, so that running it again knows what to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The language's `compile-command`.
    Compile,
    /// The language's `test-command`.
    Test,
    /// A command given by hand.
    Any,
}

/// The run a compilation buffer shows.
#[derive(Debug)]
pub struct Compilation {
    pub kind: Kind,
    /// The command line the shell runs.
    pub command: String,
    /// The directory it runs in, which relative loci are looked up from.
    pub dir: PathBuf,
    /// The language whose commands the buffer's `:compile` and `:compile-test` run.
    pub language: Option<String>,
    /// Counts the runs of the buffer, so that output of an earlier run is dropped.
    pub run: u64,
    pub started: Instant,
    /// The running command, stopped when dropped; `None` once it ended.
    pub process: Option<ProcessGroup>,
    /// The terminal the running command writes to, as large as the view showing its output;
    /// `None` on a pipe, or once it ended.
    pub terminal: Option<Pty>,
    /// The split the command was run from, which loci open in.
    pub origin: Option<ViewId>,
    /// Where the locus selected or opened last starts, which `]q` and `[q` go on from while the
    /// buffer is hidden.
    pub visited: Option<usize>,
    /// The colors of the output: the styles of char ranges, in order and apart.
    pub styles: Vec<(Range<usize>, Style)>,
}

impl Compilation {
    /// How the buffer is called, like `[compilation] cargo build`.
    pub fn display_name(&self) -> String {
        format!("[compilation] {}", self.command)
    }

    /// Maps the styles of the output over `changes` to the buffer, dropping those of text
    /// removed. The ones before the first change keep their place.
    pub fn map(&mut self, changes: &ChangeSet) {
        let Some((first, ..)) = changes.changes_iter().next() else {
            return;
        };
        let at = self.styles.partition_point(|(range, _)| range.end <= first);
        let mut moved = self.styles.split_off(at);
        changes.update_positions(moved.iter_mut().flat_map(|(range, _)| {
            let Range { start, end } = range;
            [(start, Assoc::After), (end, Assoc::Before)]
        }));
        moved.retain(|(range, _)| range.start < range.end);
        self.styles.append(&mut moved);
    }
}

#[cfg(test)]
mod tests {
    use helix_core::{Rope, Transaction};

    use super::*;
    use crate::graphics::Color;

    #[test]
    fn styles_follow_edits() {
        let red = Style::default().fg(Color::Indexed(1));
        let mut compilation = Compilation {
            kind: Kind::Any,
            command: String::new(),
            dir: PathBuf::new(),
            language: None,
            run: 0,
            started: Instant::now(),
            process: None,
            terminal: None,
            origin: None,
            visited: None,
            styles: vec![(0..3, red), (4..7, red), (8..11, red)],
        };
        let text = Rope::from("one two six\n");
        // A word inserted before the second, and the third deleted.
        let changes = [(4, 4, Some("new ".into())), (8, 11, None)];
        let transaction = Transaction::change(&text, changes.into_iter());
        compilation.map(transaction.changes());
        assert_eq!(compilation.styles, [(0..3, red), (8..11, red)]);
    }
}
