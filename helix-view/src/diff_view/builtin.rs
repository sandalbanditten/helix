//! Helix's own diff: a line diff whose changed lines show the words that changed.
//!
//! The lines of a hunk are lined up in order, the extra lines of the longer side facing fillers.
//! Within a hunk the words of both sides are diffed as one text, so a word moving to another line
//! of the hunk still matches; this follows the intra-line diff of helix PR #15631.

use std::ops::Range;

use helix_core::{
    diff::{line_hunks, text_lines},
    RopeSlice,
};
use imara_diff::{Algorithm, Diff, InternedInput, TokenSource};

use super::alignment::{Alignment, LineChange, Row};

/// The largest hunk whose words are diffed, in lines of both sides and in bytes; the lines of a
/// larger one change as a whole.
const MAX_WORD_DIFF_LINES: usize = 200;
const MAX_WORD_DIFF_BYTES: usize = 64 * 1024;

/// Lines added and removed, as `git diff --numstat` counts them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub added: u32,
    pub removed: u32,
}

impl std::ops::AddAssign for Stats {
    fn add_assign(&mut self, other: Self) {
        self.added += other.added;
        self.removed += other.removed;
    }
}

/// The lines added and removed from `old` to `new`.
pub fn stats(old: RopeSlice, new: RopeSlice) -> Stats {
    line_hunks(old, new)
        .iter()
        .fold(Stats::default(), |stats, hunk| Stats {
            added: stats.added + hunk.after.len() as u32,
            removed: stats.removed + hunk.before.len() as u32,
        })
}

/// Lines up `old` and `new` by a line diff.
pub fn align(old: RopeSlice, new: RopeSlice) -> Alignment {
    let lines = [text_lines(old) as u32, text_lines(new) as u32];
    let mut rows = Vec::with_capacity(lines[0].max(lines[1]) as usize);
    let mut changes = [Vec::new(), Vec::new()];
    let (mut old_line, mut new_line) = (0, 0);
    for hunk in line_hunks(old, new) {
        push_unchanged(&mut rows, old_line..hunk.before.start, new_line);
        let paired = hunk.before.len().min(hunk.after.len()) as u32;
        let words = word_changes(old, new, &hunk.before, &hunk.after);
        for i in 0..paired {
            rows.push(Row {
                old: Some(hunk.before.start + i),
                new: Some(hunk.after.start + i),
            });
        }
        for line in hunk.before.start + paired..hunk.before.end {
            rows.push(Row {
                old: Some(line),
                new: None,
            });
        }
        for line in hunk.after.start + paired..hunk.after.end {
            rows.push(Row {
                old: None,
                new: Some(line),
            });
        }
        for (side, (text, range, words)) in [
            (old, &hunk.before, words.as_ref().map(|words| &words[0])),
            (new, &hunk.after, words.as_ref().map(|words| &words[1])),
        ]
        .into_iter()
        .enumerate()
        {
            for line in range.clone() {
                let change = match words {
                    // Lines facing fillers are new to their side.
                    Some(words) if line < range.start + paired => {
                        let parts = words
                            .iter()
                            .filter(|(word_line, _)| *word_line == line)
                            .map(|(_, part)| part.clone());
                        LineChange::of_parts(text.line(line as usize), parts)
                    }
                    _ => Some(LineChange::Whole),
                };
                if let Some(change) = change {
                    changes[side].push((line, change));
                }
            }
        }
        (old_line, new_line) = (hunk.before.end, hunk.after.end);
    }
    push_unchanged(&mut rows, old_line..lines[0], new_line);
    Alignment::new(rows, lines, changes).expect("a line diff shows each line once")
}

/// Pushes the rows of the unchanged old lines `old_lines`, the first facing new line `new_line`.
fn push_unchanged(rows: &mut Vec<Row>, old_lines: Range<u32>, new_line: u32) {
    rows.extend(old_lines.clone().map(|line| Row {
        old: Some(line),
        new: Some(new_line + line - old_lines.start),
    }));
}

/// Byte ranges of lines: the line and the range within it.
type LineParts = Vec<(u32, Range<u32>)>;

/// The words that changed between the lines `old_lines` of `old` and `new_lines` of `new`, as
/// byte ranges of lines of each side, or `None` if the hunk is too large to diff its words.
fn word_changes(
    old: RopeSlice,
    new: RopeSlice,
    old_lines: &Range<u32>,
    new_lines: &Range<u32>,
) -> Option<[LineParts; 2]> {
    if old_lines.is_empty()
        || new_lines.is_empty()
        || old_lines.len() + new_lines.len() > MAX_WORD_DIFF_LINES
    {
        return None;
    }
    let hunk_text = |text: RopeSlice, lines: &Range<u32>| -> Option<String> {
        let start = text.line_to_byte(lines.start as usize);
        let end = text.line_to_byte(lines.end as usize);
        (end - start <= MAX_WORD_DIFF_BYTES).then(|| text.byte_slice(start..end).to_string())
    };
    let texts = [hunk_text(old, old_lines)?, hunk_text(new, new_lines)?];
    let starts = texts.each_ref().map(|text| word_starts(text));
    let line_starts = texts.each_ref().map(|text| line_starts(text));
    let input = InternedInput::new(Words(&texts[0]), Words(&texts[1]));
    let diff = Diff::compute(Algorithm::Myers, &input);

    let mut changes = [Vec::new(), Vec::new()];
    for hunk in diff.hunks() {
        for (side, words, first_line) in [
            (0, hunk.before, old_lines.start),
            (1, hunk.after, new_lines.start),
        ] {
            if words.is_empty() {
                continue;
            }
            let text = &texts[side];
            let byte = |word: u32| {
                starts[side]
                    .get(word as usize)
                    .map_or(text.len(), |&start| start)
            };
            split_lines(
                text,
                &line_starts[side],
                byte(words.start)..byte(words.end),
                first_line,
                &mut changes[side],
            );
        }
    }
    Some(changes)
}

/// Splits the byte range `range` of `text` at line breaks into byte ranges of its lines, given
/// the byte where each line starts, the first being line `first_line` of its document.
fn split_lines(
    text: &str,
    line_starts: &[usize],
    range: Range<usize>,
    first_line: u32,
    out: &mut LineParts,
) {
    let mut line = line_starts.partition_point(|&start| start <= range.start) - 1;
    let mut start = range.start;
    while start < range.end {
        let line_end = line_starts.get(line + 1).copied().unwrap_or(text.len());
        let end = range.end.min(line_end);
        let offset = line_starts[line];
        out.push((
            first_line + line as u32,
            (start - offset) as u32..(end - offset) as u32,
        ));
        start = end;
        line += 1;
    }
}

/// The byte where each line of `text` starts.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .filter(|&start| start < text.len() || start == 0)
        .collect()
}

/// The words of a text: runs of alphanumeric chars and underscores, and every other char alone.
struct Words<'a>(&'a str);

impl<'a> TokenSource for Words<'a> {
    type Token = &'a str;
    type Tokenizer = WordIter<'a>;

    fn tokenize(&self) -> Self::Tokenizer {
        WordIter(self.0)
    }

    fn estimate_tokens(&self) -> u32 {
        (self.0.len() / 4).max(1) as u32
    }
}

struct WordIter<'a>(&'a str);

impl<'a> Iterator for WordIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let first = self.0.chars().next()?;
        let len = if is_word_char(first) {
            self.0
                .find(|c: char| !is_word_char(c))
                .unwrap_or(self.0.len())
        } else {
            first.len_utf8()
        };
        let (word, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(word)
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The byte where each word of `text` starts.
fn word_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut start = 0;
    for word in WordIter(text) {
        starts.push(start);
        start += word.len();
    }
    starts
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use helix_core::Rope;

    use super::*;
    use crate::diff_view::alignment::Side;

    fn align_texts(old: &str, new: &str) -> Alignment {
        align(Rope::from(old).slice(..), Rope::from(new).slice(..))
    }

    fn rows(alignment: &Alignment) -> Vec<(Option<u32>, Option<u32>)> {
        alignment
            .rows()
            .iter()
            .map(|row| (row.old, row.new))
            .collect()
    }

    #[test]
    fn hunks_pair_their_lines_and_fill_the_rest() {
        let alignment = align_texts("a\nb\nc\nd\n", "a\nB\nx\ny\nc\nd\n");
        assert_eq!(
            rows(&alignment),
            [
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (None, Some(2)),
                (None, Some(3)),
                (Some(2), Some(4)),
                (Some(3), Some(5)),
            ]
        );
        assert_eq!(alignment.hunks(), [1..4]);
        assert_eq!(alignment.change(Side::Old, 1), Some(&LineChange::Whole));
        assert_eq!(alignment.change(Side::New, 2), Some(&LineChange::Whole));
        assert_eq!(alignment.fillers(Side::Old).after, [(1, 2)]);
    }

    #[test]
    fn insertions_at_the_top_fill_above_the_first_line() {
        let alignment = align_texts("fn main() {}\n", "use x;\nuse y;\nfn main() {}\n");
        assert_eq!(alignment.fillers(Side::Old).above, 2);
        assert_eq!(alignment.row_of_line(Side::Old, 0), Some(2));
        assert_eq!(alignment.hunks(), [0..2]);
    }

    #[test]
    fn paired_lines_show_the_words_that_changed() {
        let alignment = align_texts(
            "fn main() {\n    let x = foo(1);\n}\n",
            "fn main() {\n    let x = bar(1);\n}\n",
        );
        assert_eq!(
            alignment.change(Side::Old, 1),
            Some(&LineChange::Parts(vec![12..15]))
        );
        assert_eq!(
            alignment.change(Side::New, 1),
            Some(&LineChange::Parts(vec![12..15]))
        );
        assert_eq!(alignment.change(Side::New, 0), None);
    }

    #[test]
    fn whitespace_changes_alone_highlight_nothing() {
        let alignment = align_texts("a\nb", "a\nb\n");
        assert_eq!(rows(&alignment), [(Some(0), Some(0)), (Some(1), Some(1))]);
        assert!(alignment.hunks().is_empty());
        let reindented = align_texts("if x {\ny\n}\n", "if x {\n    y\n}\n");
        assert!(reindented.hunks().is_empty());
    }

    #[test]
    fn words_moving_between_lines_of_a_hunk_still_match() {
        let alignment = align_texts(
            "let x = Self { a, b };\n",
            "let x = Self {\n    a,\n    b,\n};\n",
        );
        assert_eq!(alignment.lines(Side::Old), 1);
        assert_eq!(alignment.lines(Side::New), 4);
        // The extra lines face fillers and are new to their side.
        assert_eq!(alignment.change(Side::New, 1), Some(&LineChange::Whole));
        let Some(LineChange::Parts(parts)) = alignment.change(Side::Old, 0) else {
            panic!("only part of the old line changed");
        };
        assert!(
            !parts.iter().any(|part| part.start < 13),
            "`let x = Self {{` stays"
        );
    }

    #[test]
    fn stats_count_lines_like_git() {
        let stats =
            |old: &str, new: &str| stats(Rope::from(old).slice(..), Rope::from(new).slice(..));
        assert_eq!(
            stats("a\nb\n", "a\nB\nc\n"),
            Stats {
                added: 2,
                removed: 1
            }
        );
        assert_eq!(
            stats("a\nb", "a\nb\n"),
            Stats {
                added: 1,
                removed: 1
            }
        );
        assert_eq!(stats("", ""), Stats::default());
    }

    #[test]
    fn empty_texts_line_up() {
        assert!(rows(&align_texts("", "")).is_empty());
        let created = align_texts("", "a\nb\n");
        assert_eq!(rows(&created), [(None, Some(0)), (None, Some(1))]);
        assert_eq!(created.fillers(Side::Old).above, 2);
    }
}
