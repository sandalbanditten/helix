//! How two texts line up side by side: the line of each text shown on each row, and what changed
//! about the lines.
//!
//! Lines are counted like [`text_lines`](helix_core::diff::text_lines) counts them, so the empty
//! line after a final line break is no line of its own.

use std::{borrow::Cow, ops::Range};

use helix_core::RopeSlice;

/// One of the two texts of a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The text before, shown on the left.
    Old,
    /// The text after, shown on the right.
    New,
}

impl Side {
    fn index(self) -> usize {
        match self {
            Self::Old => 0,
            Self::New => 1,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Old => Self::New,
            Self::New => Self::Old,
        }
    }
}

/// A row of the side-by-side layout: the line of each text shown on it. A side without a line
/// shows a filler on the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub old: Option<u32>,
    pub new: Option<u32>,
}

impl Row {
    pub fn line(self, side: Side) -> Option<u32> {
        match side {
            Side::Old => self.old,
            Side::New => self.new,
        }
    }
}

/// What changed about a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineChange {
    /// All of it: the line is new to its text, or its changes cover all it holds but whitespace.
    Whole,
    /// The byte ranges within the line that changed, sorted and apart, none of them starting or
    /// ending in whitespace.
    Parts(Vec<Range<u32>>),
}

impl LineChange {
    /// The change made of the byte ranges `parts` of `line`, if any of them holds more than
    /// whitespace. The ranges may overlap and come in any order, but must lie on char boundaries.
    pub fn of_parts(line: RopeSlice, parts: impl IntoIterator<Item = Range<u32>>) -> Option<Self> {
        let text = Cow::<str>::from(line);
        let mut parts: Vec<_> = parts.into_iter().collect();
        parts.sort_unstable_by_key(|part| part.start);
        let mut merged: Vec<Range<u32>> = Vec::with_capacity(parts.len());
        for part in parts {
            match merged.last_mut() {
                // Adjacent parts merge too: difftastic reports each token of a line by itself.
                Some(last) if last.end >= part.start => last.end = last.end.max(part.end),
                _ => merged.push(part),
            }
        }
        let trimmed: Vec<_> = merged
            .into_iter()
            .filter_map(|part| trim_whitespace(&text, part))
            .collect();
        if trimmed.is_empty() {
            return None;
        }
        let mut parts = trimmed.iter().peekable();
        let covered = text.char_indices().all(|(i, c)| {
            let i = i as u32;
            while parts.next_if(|part| part.end <= i).is_some() {}
            c.is_whitespace() || parts.peek().is_some_and(|part| part.start <= i)
        });
        Some(if covered {
            Self::Whole
        } else {
            Self::Parts(trimmed)
        })
    }
}

/// `part` of `text` without the whitespace at its ends, unless that is all it holds.
fn trim_whitespace(text: &str, part: Range<u32>) -> Option<Range<u32>> {
    let slice = text.get(part.start as usize..part.end as usize)?;
    let start = part.start + (slice.len() - slice.trim_start().len()) as u32;
    let end = part.end - (slice.len() - slice.trim_end().len()) as u32;
    (start < end).then_some(start..end)
}

/// The filler rows a side shows: above its first line, and after some of its lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fillers {
    pub above: u32,
    /// The lines followed by fillers and how many, sorted by line.
    pub after: Vec<(u32, u32)>,
}

/// The layout of one side.
#[derive(Debug, Default)]
struct Layout {
    /// The row of each line.
    rows: Vec<u32>,
    /// The lines that changed, sorted by line.
    changes: Vec<(u32, LineChange)>,
    fillers: Fillers,
}

/// How two texts line up side by side.
#[derive(Debug, Default)]
pub struct Alignment {
    rows: Vec<Row>,
    sides: [Layout; 2],
    /// The rows of each hunk: a run of rows with a filler or a changed line on a side.
    hunks: Vec<Range<u32>>,
}

impl Alignment {
    /// Lays out `rows`, the rows showing `lines[0]` old and `lines[1]` new lines, with the changes
    /// of the lines of each side, sorted by line. Returns `None` unless the rows show every line
    /// of each side once and in order.
    pub fn new(
        rows: Vec<Row>,
        lines: [u32; 2],
        changes: [Vec<(u32, LineChange)>; 2],
    ) -> Option<Self> {
        let [old_changes, new_changes] = changes;
        let mut sides = [old_changes, new_changes].map(|changes| Layout {
            changes,
            ..Layout::default()
        });
        for side in [Side::Old, Side::New] {
            let layout = &mut sides[side.index()];
            layout.rows = Vec::with_capacity(lines[side.index()] as usize);
            for (index, row) in rows.iter().enumerate() {
                match row.line(side) {
                    Some(line) if line as usize == layout.rows.len() => {
                        layout.rows.push(index as u32);
                    }
                    Some(_) => return None,
                    None => match layout.rows.len().checked_sub(1) {
                        None => layout.fillers.above += 1,
                        Some(last) => match layout.fillers.after.last_mut() {
                            Some((line, count)) if *line as usize == last => *count += 1,
                            _ => layout.fillers.after.push((last as u32, 1)),
                        },
                    },
                }
            }
            if layout.rows.len() != lines[side.index()] as usize
                || !layout.changes.is_sorted_by_key(|(line, _)| *line)
                || layout
                    .changes
                    .last()
                    .is_some_and(|(line, _)| *line >= lines[side.index()])
            {
                return None;
            }
        }
        let mut alignment = Self {
            rows,
            sides,
            hunks: Vec::new(),
        };
        alignment.hunks = alignment.find_hunks();
        Some(alignment)
    }

    fn find_hunks(&self) -> Vec<Range<u32>> {
        let mut changed = vec![false; self.rows.len()];
        for (index, row) in self.rows.iter().enumerate() {
            changed[index] = row.old.is_none() || row.new.is_none();
        }
        for layout in &self.sides {
            for (line, _) in &layout.changes {
                changed[layout.rows[*line as usize] as usize] = true;
            }
        }
        let mut hunks: Vec<Range<u32>> = Vec::new();
        for (index, _) in changed.iter().enumerate().filter(|(_, changed)| **changed) {
            let index = index as u32;
            match hunks.last_mut() {
                Some(hunk) if hunk.end == index => hunk.end += 1,
                _ => hunks.push(index..index + 1),
            }
        }
        hunks
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The runs of rows with a filler or a changed line on a side, in order.
    pub fn hunks(&self) -> &[Range<u32>] {
        &self.hunks
    }

    /// The number of lines of a side.
    pub fn lines(&self, side: Side) -> u32 {
        self.sides[side.index()].rows.len() as u32
    }

    /// The row showing `line` of `side`.
    pub fn row_of_line(&self, side: Side, line: u32) -> Option<u32> {
        self.sides[side.index()].rows.get(line as usize).copied()
    }

    /// The change of `line` of `side`, if it changed.
    pub fn change(&self, side: Side, line: u32) -> Option<&LineChange> {
        let changes = &self.sides[side.index()].changes;
        changes
            .binary_search_by_key(&line, |(line, _)| *line)
            .ok()
            .map(|index| &changes[index].1)
    }

    /// The changed lines of `side` among `lines`, sorted by line.
    pub fn changes(&self, side: Side, lines: Range<u32>) -> &[(u32, LineChange)] {
        let changes = &self.sides[side.index()].changes;
        let start = changes.partition_point(|(line, _)| *line < lines.start);
        let end = changes.partition_point(|(line, _)| *line < lines.end);
        &changes[start..end]
    }

    pub fn fillers(&self, side: Side) -> &Fillers {
        &self.sides[side.index()].fillers
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use helix_core::Rope;

    use super::*;

    fn row(old: Option<u32>, new: Option<u32>) -> Row {
        Row { old, new }
    }

    fn change(line: &str, parts: &[Range<u32>]) -> Option<LineChange> {
        LineChange::of_parts(Rope::from(line).slice(..), parts.iter().cloned())
    }

    #[test]
    fn parts_merge_and_lose_the_whitespace_at_their_ends() {
        let line = "    let x = foo(1, 2);\n";
        assert_eq!(
            change(line, &[(12..15), (15..16), (16..17)]),
            Some(LineChange::Parts(vec![12..17]))
        );
        assert_eq!(
            change(line, &[(0..4), (4..7), (16..18)]),
            Some(LineChange::Parts(vec![4..7, 16..18]))
        );
        assert_eq!(change(line, &[(0..4)]), None, "only indentation changed");
        assert_eq!(change(line, &[]), None);
    }

    #[test]
    fn parts_covering_all_but_whitespace_are_the_whole_line() {
        let line = "  a b\r\n";
        assert_eq!(
            change(line, &[(0..1), (2..3), (4..5)]),
            Some(LineChange::Whole)
        );
        assert_eq!(change(line, &[(2..5)]), Some(LineChange::Whole));
        assert_eq!(
            change("añb", &[(0..1), (3..4)]),
            Some(LineChange::Parts(vec![0..1, 3..4])),
            "byte ranges around a multi-byte char"
        );
    }

    #[test]
    fn rows_lay_out_lines_fillers_and_hunks() {
        // old: a b c d     new: z a c e d f
        let rows = vec![
            row(None, Some(0)),
            row(Some(0), Some(1)),
            row(Some(1), None),
            row(Some(2), Some(2)),
            row(None, Some(3)),
            row(Some(3), Some(4)),
            row(None, Some(5)),
        ];
        let alignment = Alignment::new(
            rows,
            [4, 6],
            [Vec::new(), vec![(2, LineChange::Parts(vec![0..1]))]],
        )
        .unwrap();
        assert_eq!(alignment.hunks(), [0..1, 2..5, 6..7]);
        assert_eq!(alignment.lines(Side::Old), 4);
        assert_eq!(alignment.row_of_line(Side::Old, 2), Some(3));
        assert_eq!(alignment.row_of_line(Side::New, 5), Some(6));
        assert_eq!(alignment.row_of_line(Side::New, 6), None);
        assert_eq!(
            *alignment.fillers(Side::Old),
            Fillers {
                above: 1,
                after: vec![(2, 1), (3, 1)]
            }
        );
        assert_eq!(
            *alignment.fillers(Side::New),
            Fillers {
                above: 0,
                after: vec![(1, 1)]
            }
        );
        assert_eq!(alignment.changes(Side::New, 0..2), []);
        assert_eq!(alignment.changes(Side::New, 2..3).len(), 1);
        assert!(alignment.change(Side::New, 2).is_some());
        assert!(alignment.change(Side::Old, 2).is_none());
    }

    #[test]
    fn rows_must_show_every_line_once_in_order() {
        let none = || [Vec::new(), Vec::new()];
        assert!(Alignment::new(vec![row(Some(0), Some(0))], [1, 1], none()).is_some());
        assert!(Alignment::new(vec![row(Some(1), Some(0))], [2, 1], none()).is_none());
        assert!(Alignment::new(vec![row(Some(0), Some(0))], [2, 1], none()).is_none());
        let late_change = [vec![(1, LineChange::Whole)], Vec::new()];
        assert!(Alignment::new(vec![row(Some(0), Some(0))], [1, 1], late_change).is_none());
        let empty = Alignment::new(Vec::new(), [0, 0], none()).unwrap();
        assert!(empty.hunks().is_empty());
    }
}
