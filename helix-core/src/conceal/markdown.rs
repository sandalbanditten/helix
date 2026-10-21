//! Markdown's pipe tables, shown with their columns aligned and their borders drawn with
//! box-drawing characters.

use std::borrow::Cow;
use std::iter;
use std::ops;

use tree_house::tree_sitter::Node;
use unicode_segmentation::UnicodeSegmentation;

use crate::graphemes::grapheme_width;
use crate::line_ending::line_end_char_index;
use crate::text_annotations::{Conceal, Replacement};
use crate::RopeSlice;

/// How a column's cells are aligned, from its delimiter cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    /// The padding before and after a cell's contents that leaves `space` columns free.
    fn padding(self, space: usize) -> (usize, usize) {
        match self {
            Self::Left => (0, space),
            Self::Center => (space / 2, space - space / 2),
            Self::Right => (space, 0),
        }
    }
}

/// Appends the conceals of the rows of `table`, a `pipe_table` node, that lie in the chars
/// `range`, and those of the blank lines around it, which show its top and bottom borders.
pub(super) fn table_conceals(
    text: RopeSlice,
    table: &Node,
    range: ops::Range<usize>,
    conceals: &mut Vec<Conceal>,
) {
    // every line of a table is a row, after what comes before the first row on its line, like a
    // block quote's `>`. Reading them as text is much faster than walking their nodes.
    let Some(header) = table.named_child(0) else {
        return;
    };
    let table_start = header.start_byte() as usize;
    let first_line = text.byte_to_line(table_start);
    let last_line = text.byte_to_line((table.end_byte() as usize - 1).max(table_start));
    let prefix = table_start - text.line_to_byte(first_line);
    let rows = || {
        let lines = text.lines_at(first_line).take(last_line + 1 - first_line);
        (first_line..).zip(lines).map(|(line, text)| {
            let row = Cow::<str>::from(text);
            (line, row, line == first_line + 1)
        })
    };

    let mut widths = Vec::new();
    let mut aligns = Vec::new();
    let mut row_cells = Vec::new();
    for (_, line, is_delimiter) in rows() {
        let (_, row) = row_text(&line, prefix);
        cells(row, &mut row_cells);
        if is_delimiter {
            aligns.extend(row_cells.iter().map(|cell| {
                let cell = &row.as_bytes()[cell.clone()];
                match (
                    cell.len() > 1 && cell[0] == b':',
                    cell.last() == Some(&b':'),
                ) {
                    (true, true) => Align::Center,
                    (false, true) => Align::Right,
                    _ => Align::Left,
                }
            }));
            continue;
        }
        for (column, cell) in row_cells.iter().enumerate() {
            let cell_width = width(&row[cell.clone()]);
            match widths.get_mut(column) {
                Some(widest) => *widest = cell_width.max(*widest),
                None => widths.push(cell_width),
            }
        }
    }
    let align = |column: usize| aligns.get(column).copied().unwrap_or(Align::Left);

    let range_lines = text.char_to_line(range.start)..match range.end {
        end if end == text.len_chars() => usize::MAX,
        end => text.char_to_line(end),
    };
    for (line, line_text, is_delimiter) in rows() {
        if !range_lines.contains(&line) {
            continue;
        }
        let (row_offset, row) = row_text(&line_text, prefix);
        let row_start = text.line_to_char(line) + line_text[..row_offset].chars().count();
        let row_end = row_start + row.chars().count();
        if is_delimiter {
            let rule = Replacement::Rule(border(&widths, ['├', '┼', '┤']).into());
            conceals.push(Conceal::new(row_start, row_end, rule));
            continue;
        }

        // everything but the cells' contents is concealed, from one content to the next
        let char_at = |byte: usize| row_start + row[..byte].chars().count();
        let mut start = row_start;
        let mut shown = String::from("│ ");
        cells(row, &mut row_cells);
        let mut cells = row_cells.iter();
        for (column, &column_width) in widths.iter().enumerate() {
            if column > 0 {
                shown.push_str(" │ ");
            }
            match cells.next().filter(|cell| !cell.is_empty()) {
                Some(cell) => {
                    let space = column_width - width(&row[cell.clone()]);
                    let (before, after) = align(column).padding(space);
                    shown.extend(iter::repeat_n(' ', before));
                    let concealed = Replacement::Rule(std::mem::take(&mut shown).into());
                    conceals.push(Conceal::new(start, char_at(cell.start), concealed));
                    shown.extend(iter::repeat_n(' ', after));
                    start = char_at(cell.end);
                }
                None => shown.extend(iter::repeat_n(' ', column_width)),
            }
        }
        shown.push_str(" │");
        conceals.push(Conceal::new(
            start,
            row_end,
            Replacement::Rule(shown.into()),
        ));
    }

    // the borders are indented like the rows, which may be in a list item, but are left out of a
    // block quote, whose blank lines are outside it
    let prefix = Cow::from(text.byte_slice(table_start - prefix..table_start));
    if !prefix.chars().all(char::is_whitespace) {
        return;
    }
    let indent = width(&prefix);
    if first_line > 0 {
        let top = border(&widths, ['┌', '┬', '┐']);
        blank_line_border(text, first_line - 1, indent, &top, conceals);
    }
    // the last line has no line break to show a border before
    if last_line + 2 < text.len_lines() {
        let bottom = border(&widths, ['└', '┴', '┘']);
        blank_line_border(text, last_line + 1, indent, &bottom, conceals);
    }
}

/// The row on the table line `line`, without its line break and the `prefix` bytes before it,
/// and the byte it starts at.
fn row_text(line: &str, prefix: usize) -> (usize, &str) {
    let line = line.trim_end_matches(['\n', '\r']);
    let start = prefix.min(line.len());
    match line.get(start..) {
        Some(row) => (start, row),
        None => (0, line),
    }
}

/// Sets `cells` to the byte ranges of the contents of the cells of the table row `row`, without
/// the whitespace around them. Cells are separated by the pipes not escaped with a backslash, and
/// the whitespace before a leading pipe or after a trailing one is no cell.
fn cells(row: &str, cells: &mut Vec<ops::Range<usize>>) {
    let trim = |cell: ops::Range<usize>| {
        let content = &row[cell.clone()];
        let start = cell.start + (content.len() - content.trim_start().len());
        start..start + content.trim().len()
    };
    cells.clear();
    let mut start = 0;
    let mut escaped = false;
    for (i, byte) in row.bytes().enumerate() {
        match byte {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b'|' => {
                cells.push(trim(start..i));
                start = i + 1;
            }
            _ => {}
        }
    }
    cells.push(trim(start..row.len()));
    if cells.len() > 1 && cells.last().is_some_and(ops::Range::is_empty) {
        cells.pop();
    }
    if cells.len() > 1 && cells[0].is_empty() {
        cells.remove(0);
    }
}

/// A border line across columns `widths` wide, made of the left, middle and right `corners`.
fn border(widths: &[usize], [left, middle, right]: [char; 3]) -> String {
    let mut border = String::from(left);
    for (column, &width) in widths.iter().enumerate() {
        if column > 0 {
            border.push(middle);
        }
        border.extend(iter::repeat_n('─', width + 2));
    }
    border.push(right);
    border
}

/// Conceals the whitespace of `line` as `border` indented by `indent` columns, if the line is
/// blank.
fn blank_line_border(
    text: RopeSlice,
    line: usize,
    indent: usize,
    border: &str,
    conceals: &mut Vec<Conceal>,
) {
    let start = text.line_to_char(line);
    let end = line_end_char_index(&text, line);
    if !text.slice(start..end).chars().all(char::is_whitespace) {
        return;
    }
    let shown = " ".repeat(indent) + border;
    conceals.push(Conceal::new(start, end, Replacement::Rule(shown.into())));
}

/// The columns `text` takes.
fn width(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.graphemes(true).map(grapheme_width).sum()
    }
}
