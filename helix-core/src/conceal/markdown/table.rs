//! Markdown's pipe tables, shown with their columns aligned and their borders drawn with
//! box-drawing characters.
//!
//! A column is as wide as its widest cell as it is drawn, after the conceals in it, like those of
//! links, so its rows line up. The borders around a table go over the blank lines next to it, or
//! else on rows of their own.

use std::borrow::Cow;
use std::iter;
use std::ops::Range;

use helix_stdx::rope::RopeSliceExt;
use tree_house::tree_sitter::Node;
use unicode_width::UnicodeWidthStr;

use super::{bytes_of, Renderer};
use crate::conceal::VirtualRow;
use crate::graphemes::grapheme_width;
use crate::text_annotations::{Conceal, ConcealStyle, Replacement};

/// How the cells of a column are aligned, from its delimiter cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    /// `---`
    None,
    /// `:--`
    Left,
    /// `:-:`
    Center,
    /// `--:`
    Right,
}

impl Align {
    /// The padding before and after a cell's content that leaves `space` columns free.
    fn padding(self, space: usize) -> (usize, usize) {
        match self {
            Self::None | Self::Left => (0, space),
            Self::Center => (space / 2, space - space / 2),
            Self::Right => (space, 0),
        }
    }
}

/// The corners and crossings of a border row: left, middle and right.
const TOP: [char; 3] = ['┌', '┬', '┐'];
const DELIMITER: [char; 3] = ['├', '┼', '┤'];
const BOTTOM: [char; 3] = ['└', '┴', '┘'];

impl Renderer<'_, '_> {
    /// Shows the rows of the pipe table `table` with their columns aligned and borders around
    /// them.
    pub(in crate::conceal) fn table(&mut self, table: &Node) {
        let text = self.text;
        let rows: Vec<_> = table
            .children()
            .filter(|row| {
                matches!(
                    row.kind(),
                    "pipe_table_header" | "pipe_table_delimiter_row" | "pipe_table_row"
                )
            })
            .collect();
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
            return;
        };
        let first_line = text.byte_to_line(first.start_byte() as usize);
        let last_line = text.byte_to_line(last.start_byte() as usize);
        let around = first_line.saturating_sub(1)..(last_line + 2).min(text.len_lines());
        self.rendered.table_lines.push(around);

        // the conceals of the cells, which are drawn as their replacements
        let table_chars = text.byte_to_char(first.start_byte() as usize)
            ..text.byte_to_char(last.end_byte() as usize);
        let mut inline: Vec<Conceal> = self
            .rendered
            .conceals
            .iter()
            .map(|cached| cached.conceal.clone())
            .filter(|conceal| table_chars.start <= conceal.start && conceal.end <= table_chars.end)
            .collect();
        inline.sort_by_key(|conceal| (conceal.start, conceal.end));

        let mut aligns = Vec::new();
        let mut widths: Vec<usize> = Vec::new();
        let mut row_cells = Vec::with_capacity(rows.len());
        for row in &rows {
            if row.kind() == "pipe_table_delimiter_row" {
                aligns = row
                    .children()
                    .filter(|cell| cell.kind() == "pipe_table_delimiter_cell")
                    .map(|cell| {
                        let kinds: Vec<_> = cell.children().map(|mark| mark.kind()).collect();
                        let left = kinds.contains(&"pipe_table_align_left");
                        let right = kinds.contains(&"pipe_table_align_right");
                        match (left, right) {
                            (true, true) => Align::Center,
                            (true, false) => Align::Left,
                            (false, true) => Align::Right,
                            (false, false) => Align::None,
                        }
                    })
                    .collect();
                row_cells.push(Vec::new());
                continue;
            }
            let cells: Vec<_> = row
                .children()
                .filter(|cell| cell.kind() == "pipe_table_cell")
                .map(|cell| {
                    let content = self.content(&cell);
                    let width = self.shown_width(content.clone(), &inline);
                    (content, width)
                })
                .collect();
            for (column, (_, width)) in cells.iter().enumerate() {
                match widths.get_mut(column) {
                    Some(widest) => *widest = (*widest).max(*width),
                    None => widths.push(*width),
                }
            }
            row_cells.push(cells);
        }
        widths.resize(widths.len().max(aligns.len()), 0);
        // a column is at least as wide as its delimiter's dashes need
        for width in &mut widths {
            *width = (*width).max(1);
        }
        let align = |column: usize| aligns.get(column).copied().unwrap_or(Align::None);

        let prefix = self.prefix(first);
        for (row, cells) in rows.iter().zip(&row_cells) {
            let bytes = bytes_of(row);
            if row.kind() == "pipe_table_delimiter_row" {
                let border = border(&widths, DELIMITER, &aligns);
                self.conceal(bytes, Replacement::from(border), ConcealStyle::Guide);
                continue;
            }
            // everything but the cells' contents is concealed, from one content to the next, and
            // is revealed with the cells on both sides of it
            let row_start = text.byte_to_char(bytes.start);
            let mut start = row_start;
            let mut previous_start = row_start;
            let mut shown = String::from("│ ");
            let mut cells = cells.iter();
            for (column, &width) in widths.iter().enumerate() {
                if column > 0 {
                    shown.push_str(" │ ");
                }
                match cells.next().filter(|(content, _)| !content.is_empty()) {
                    Some((content, content_width)) => {
                        let (before, after) = align(column).padding(width - content_width);
                        shown.extend(iter::repeat_n(' ', before));
                        let replacement = Replacement::from(std::mem::take(&mut shown));
                        self.element = Some(previous_start..content.end);
                        self.conceal_chars(start..content.start, replacement);
                        shown.extend(iter::repeat_n(' ', after));
                        (start, previous_start) = (content.end, content.start);
                    }
                    None => shown.extend(iter::repeat_n(' ', width)),
                }
            }
            shown.push_str(" │");
            let end = text.byte_to_char(bytes.end);
            self.element = Some(previous_start..end);
            self.conceal_chars(start..end, Replacement::from(shown));
            self.element = None;
        }

        // the borders above and below, over the blank lines there or on rows of their own
        let top = format!("{prefix}{}", border(&widths, TOP, &[]));
        let follows_table = table
            .prev_sibling()
            .is_some_and(|before| before.kind() == "pipe_table");
        if first_line > 0 && !follows_table && self.is_blank(first_line - 1) {
            self.border_over(first_line - 1, top);
        } else if first_line > 0 {
            self.row(first_line - 1, false, top);
        } else {
            self.row(0, true, top);
        }
        let bottom = format!("{prefix}{}", border(&widths, BOTTOM, &[]));
        if last_line + 1 < text.len_lines() && self.is_blank(last_line + 1) {
            self.border_over(last_line + 1, bottom);
        } else {
            self.row(last_line, false, bottom);
        }
    }

    /// The chars of the content of a cell, without the whitespace around it.
    fn content(&self, cell: &Node) -> Range<usize> {
        let text = self.text;
        let bytes = bytes_of(cell);
        let written = self.slice(bytes.clone());
        let start = bytes.start + (written.len() - written.trim_start().len());
        let end = bytes.start + written.trim_end().len();
        text.byte_to_char(start)..text.byte_to_char(end.max(start))
    }

    /// The columns the chars `range` take as they are drawn, with the conceals in `inline`.
    fn shown_width(&self, range: Range<usize>, inline: &[Conceal]) -> usize {
        let text = self.text;
        let first = inline.partition_point(|conceal| conceal.start < range.start);
        let mut pos = range.start;
        let mut width = 0;
        for conceal in inline[first..]
            .iter()
            .take_while(|conceal| conceal.end <= range.end)
        {
            if conceal.start < pos {
                continue;
            }
            width += graphemes_width(text.slice(pos..conceal.start));
            width += match &conceal.replacement {
                Replacement::Text(shown) => shown.width(),
                Replacement::Fill(_) | Replacement::Hidden => 0,
            };
            pos = conceal.end;
        }
        width + graphemes_width(text.slice(pos..range.end))
    }

    /// What comes before the table's rows on the line of `row`, like the indentation of a list
    /// item or the `>` of a block quote, as it is drawn.
    fn prefix(&self, row: &Node) -> String {
        let text = self.text;
        let start = text.byte_to_char(row.start_byte() as usize);
        let line_start = text.line_to_char(text.char_to_line(start));
        let written: Cow<str> = text.slice(line_start..start).into();
        written
            .chars()
            .map(|c| match c {
                '>' => super::QUOTE_BAR,
                '\t' => "    ",
                _ => " ",
            })
            .collect()
    }

    fn is_blank(&self, line: usize) -> bool {
        self.text.line(line).chars().all(char::is_whitespace)
    }

    fn conceal_chars(&mut self, chars: Range<usize>, replacement: Replacement) {
        let text = self.text;
        let bytes = text.char_to_byte(chars.start)..text.char_to_byte(chars.end);
        if chars.is_empty() {
            self.insert(bytes.start, replacement, ConcealStyle::Guide);
        } else {
            self.conceal(bytes, replacement, ConcealStyle::Guide);
        }
    }

    /// Draws `border` over the blank line `line`.
    fn border_over(&mut self, line: usize, border: String) {
        let text = self.text;
        let start = text.line_to_char(line);
        let end = crate::line_ending::line_end_char_index(&text, line);
        self.conceal_chars(start..end, Replacement::from(border));
    }

    /// Draws `border` on a row of its own below `line`, or above it if `above`.
    fn row(&mut self, line: usize, above: bool, border: String) {
        let row = VirtualRow {
            parts: vec![(border, ConcealStyle::Guide)],
        };
        self.rendered.rows.push((line, above, row));
    }
}

/// A border row across columns `widths` wide, made of the left, middle and right `corners`, with
/// the alignment marks of `aligns`.
fn border(widths: &[usize], [left, middle, right]: [char; 3], aligns: &[Align]) -> String {
    let mut border = String::from(left);
    for (column, &width) in widths.iter().enumerate() {
        if column > 0 {
            border.push(middle);
        }
        let (start, end) = match aligns.get(column) {
            Some(Align::Left) => ('━', '─'),
            Some(Align::Center) => ('━', '━'),
            Some(Align::Right) => ('─', '━'),
            Some(Align::None) | None => ('─', '─'),
        };
        border.push(start);
        border.extend(iter::repeat_n('─', width));
        border.push(end);
    }
    border.push(right);
    border
}

fn graphemes_width(text: crate::RopeSlice) -> usize {
    text.graphemes()
        .map(|grapheme| grapheme_width(&Cow::from(grapheme)))
        .sum()
}
