//! Markdown's pipe tables, shown with their columns aligned and their borders drawn with
//! box-drawing characters.
//!
//! A column is as wide as its widest cell as it is drawn, after the conceals in it, like those of
//! links, so its rows line up. The borders around a table go over the blank lines next to it, or
//! else on rows of their own. A table wider than the text wraps its widest columns' cells onto
//! rows of their own below their row.

use std::borrow::Cow;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::iter;
use std::ops::Range;

use helix_stdx::rope::RopeSliceExt;
use tree_house::tree_sitter::Node;
use unicode_width::UnicodeWidthStr;

use super::{bytes_of, Renderer};
use crate::conceal::{Cached, RowStyle, TableWidths, VirtualRow};
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

/// A part of a cell as it is drawn: a grapheme of its text, or what a conceal in it shows.
#[derive(Debug)]
struct Unit {
    chars: Range<usize>,
    shown: String,
    width: usize,
    style: RowStyle,
    space: bool,
}

/// The content of a cell, without the whitespace around it: the hash of its text, and its chars
/// and units if its row is rendered.
struct Cell {
    key: u64,
    content: Range<usize>,
    units: Vec<Unit>,
}

fn units_width(units: &[Unit]) -> usize {
    units.iter().map(|unit| unit.width).sum()
}

impl Renderer<'_, '_> {
    /// Shows the rows of the pipe table `table` in the chars `window` with their columns aligned
    /// and borders around them, its cells wrapped to fit `wrap` columns if the text soft-wraps
    /// there. The widths of its columns come from `table_widths`, or else those of its cells, and
    /// those of the cells not known yet from the conceals that `measure` renders of their chars.
    pub(in crate::conceal) fn table(
        &mut self,
        table: &Node,
        wrap: Option<u16>,
        window: Range<usize>,
        table_widths: &mut TableWidths,
        measure: impl FnOnce(Range<usize>) -> Vec<Cached>,
    ) {
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
        let window_lines = text.char_to_line(window.start)..=text.char_to_line(window.end);
        let in_window =
            |row: &Node| window_lines.contains(&text.byte_to_line(row.start_byte() as usize));

        // the conceals of the cells, which are drawn as their replacements
        let inline = |conceals: &[Cached]| {
            let mut inline: Vec<Conceal> = conceals
                .iter()
                .map(|cached| cached.conceal.clone())
                .collect();
            inline.sort_by_key(|conceal| (conceal.start, conceal.end));
            inline
        };
        let window_inline = inline(&self.rendered.conceals);

        let table_key = {
            let mut hasher = DefaultHasher::new();
            for chunk in text.byte_slice(bytes_of(table)).chunks() {
                hasher.write(chunk.as_bytes());
            }
            hasher.finish()
        };
        let known = table_widths.columns.contains_key(&table_key);
        let cell_widths = &mut table_widths.cells;
        let mut aligns = Vec::new();
        let mut row_cells = Vec::with_capacity(rows.len());
        // the cells outside the window whose width is not known yet
        let mut unknown: Vec<(u64, Range<usize>)> = Vec::new();
        for row in &rows {
            if row.kind() == "pipe_table_delimiter_row" {
                aligns = row
                    .children()
                    .filter(|cell| cell.kind() == "pipe_table_delimiter_cell")
                    .map(|cell| alignment(&cell))
                    .collect();
                row_cells.push(Vec::new());
                continue;
            }
            // the rows outside the window are only measured, and only once for a tree
            let rendered = in_window(row);
            if !rendered && known {
                row_cells.push(Vec::new());
                continue;
            }
            let cells: Vec<_> = row
                .children()
                .filter(|cell| cell.kind() == "pipe_table_cell")
                .map(|cell| {
                    let (bytes, key) = self.content(&cell);
                    let chars = || text.byte_to_char(bytes.start)..text.byte_to_char(bytes.end);
                    if rendered {
                        let content = chars();
                        let units = self.units(content.clone(), &window_inline);
                        cell_widths.insert(key, units_width(&units));
                        Cell {
                            key,
                            content,
                            units,
                        }
                    } else {
                        if !cell_widths.contains_key(&key) {
                            unknown.push((key, chars()));
                        }
                        Cell {
                            key,
                            content: 0..0,
                            units: Vec::new(),
                        }
                    }
                })
                .collect();
            row_cells.push(cells);
        }
        if let (Some((_, first)), Some((_, last))) = (unknown.first(), unknown.last()) {
            let measured = inline(&measure(first.start..last.end));
            for (key, content) in unknown {
                let units = self.units(content, &measured);
                cell_widths.insert(key, units_width(&units));
            }
        }
        let mut widths = match table_widths.columns.get(&table_key) {
            Some(widths) => widths.clone(),
            None => {
                let mut widths: Vec<usize> = Vec::new();
                for cells in &row_cells {
                    for (column, cell) in cells.iter().enumerate() {
                        let width = table_widths.cells.get(&cell.key).copied().unwrap_or(0);
                        match widths.get_mut(column) {
                            Some(widest) => *widest = (*widest).max(width),
                            None => widths.push(width),
                        }
                    }
                }
                table_widths.columns.insert(table_key, widths.clone());
                widths
            }
        };
        widths.resize(widths.len().max(aligns.len()), 0);
        for width in &mut widths {
            *width = (*width).max(1);
        }
        let prefix = self.prefix(first);
        if let Some(wrap) = wrap {
            // the last column stays free, so no row soft-wraps
            let room = usize::from(wrap).saturating_sub(prefix.width() + 1);
            fit(&mut widths, room);
        }
        let align = |column: usize| aligns.get(column).copied().unwrap_or(Align::None);

        for (row, cells) in rows.iter().zip(&row_cells) {
            if !in_window(row) {
                continue;
            }
            let bytes = bytes_of(row);
            if row.kind() == "pipe_table_delimiter_row" {
                let border = border(&widths, DELIMITER, &aligns);
                self.conceal(bytes, Replacement::from(border), ConcealStyle::Guide);
                continue;
            }
            let row_chars = text.byte_to_char(bytes.start)..text.byte_to_char(bytes.end);
            // the units of each cell on each of the row's rows
            let lines: Vec<Vec<Range<usize>>> = cells
                .iter()
                .zip(&widths)
                .map(|(cell, &width)| lines(&cell.units, width))
                .collect();
            let height = lines.iter().map(Vec::len).max().unwrap_or(1).max(1);
            // a wrapped row is revealed as a whole, its cells being on several rows
            let wrapped = (height > 1).then(|| row_chars.clone());

            // everything but what the cells show on the row is concealed, from one cell to the
            // next, and is revealed with the cells on both sides of it
            let mut start = row_chars.start;
            let mut previous_start = row_chars.start;
            let mut shown = String::from("│ ");
            for (column, &width) in widths.iter().enumerate() {
                if column > 0 {
                    shown.push_str(" │ ");
                }
                let cell = cells.get(column).filter(|cell| !cell.units.is_empty());
                let first_line = cell.zip(lines.get(column).and_then(|lines| lines.first()));
                match first_line {
                    Some((cell, line)) => {
                        let units = &cell.units[line.clone()];
                        let shown_width: usize = units.iter().map(|unit| unit.width).sum();
                        let (before, after) =
                            align(column).padding(width.saturating_sub(shown_width));
                        shown.extend(iter::repeat_n(' ', before));
                        let replacement = Replacement::from(std::mem::take(&mut shown));
                        let content_start = cell.content.start;
                        let shown_end = units.last().map_or(content_start, |unit| unit.chars.end);
                        self.element = wrapped.clone().or(Some(previous_start..shown_end));
                        self.conceal_chars(start..content_start, replacement);
                        shown.extend(iter::repeat_n(' ', after));
                        (start, previous_start) = (shown_end, content_start);
                    }
                    None => shown.extend(iter::repeat_n(' ', width)),
                }
            }
            shown.push_str(" │");
            self.element = wrapped.clone().or(Some(previous_start..row_chars.end));
            self.conceal_chars(start..row_chars.end, Replacement::from(shown));
            self.element = None;

            // the rest of the cells on rows of their own
            for k in 1..height {
                let mut parts = vec![(prefix.clone(), RowStyle::Guide)];
                let mut guide = String::from("│ ");
                for (column, &width) in widths.iter().enumerate() {
                    if column > 0 {
                        guide.push_str(" │ ");
                    }
                    let line = cells
                        .get(column)
                        .zip(lines.get(column).and_then(|l| l.get(k)));
                    let units = line.map_or(&[][..], |(cell, line)| &cell.units[line.clone()]);
                    let shown_width: usize = units.iter().map(|unit| unit.width).sum();
                    let (before, after) = align(column).padding(width.saturating_sub(shown_width));
                    guide.extend(iter::repeat_n(' ', before));
                    parts.push((std::mem::take(&mut guide), RowStyle::Guide));
                    parts.extend(units.iter().map(|unit| (unit.shown.clone(), unit.style)));
                    guide.extend(iter::repeat_n(' ', after));
                }
                guide.push_str(" │");
                parts.push((guide, RowStyle::Guide));
                let line = text.char_to_line(row_chars.start);
                let row = VirtualRow::new(parts, wrapped.clone());
                self.rendered.rows.push((line, false, row));
            }
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

    /// The bytes of the content of a cell, without the whitespace around it, and a hash of its
    /// text.
    fn content(&self, cell: &Node) -> (Range<usize>, u64) {
        let bytes = bytes_of(cell);
        let written = self.slice(bytes.clone());
        let content = written.trim();
        let start = bytes.start + (written.len() - written.trim_start().len());
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        (start..start + content.len(), hasher.finish())
    }

    /// The units of the chars `range` as they are drawn, with the conceals in `inline`.
    fn units(&self, range: Range<usize>, inline: &[Conceal]) -> Vec<Unit> {
        let text = self.text;
        let mut units = Vec::new();
        let graphemes = |range: Range<usize>, units: &mut Vec<Unit>| {
            let mut char_idx = range.start;
            for grapheme in text.slice(range).graphemes() {
                let shown = Cow::from(grapheme);
                let chars = char_idx..char_idx + grapheme.len_chars();
                char_idx = chars.end;
                units.push(Unit {
                    width: grapheme_width(&shown),
                    space: shown.chars().all(char::is_whitespace),
                    style: RowStyle::Text {
                        char_idx: chars.start,
                    },
                    shown: shown.into_owned(),
                    chars,
                });
            }
        };
        let first = inline.partition_point(|conceal| conceal.start < range.start);
        let mut pos = range.start;
        for conceal in inline[first..]
            .iter()
            .take_while(|conceal| conceal.end <= range.end)
        {
            if conceal.start < pos {
                continue;
            }
            graphemes(pos..conceal.start, &mut units);
            let shown = match &conceal.replacement {
                Replacement::Text(shown) => shown.to_string(),
                Replacement::Fill(_) | Replacement::Hidden => String::new(),
            };
            let style = match conceal.style {
                ConcealStyle::Guide => RowStyle::Guide,
                ConcealStyle::Scope(scope) => RowStyle::Scope(scope),
                ConcealStyle::Text => RowStyle::Text {
                    char_idx: conceal.start,
                },
            };
            units.push(Unit {
                chars: conceal.start..conceal.end,
                width: shown.width(),
                space: false,
                style,
                shown,
            });
            pos = conceal.end;
        }
        graphemes(pos..range.end, &mut units);
        units
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
        let row = VirtualRow::new(vec![(border, RowStyle::Guide)], None);
        self.rendered.rows.push((line, above, row));
    }
}

fn alignment(cell: &Node) -> Align {
    let marks: Vec<_> = cell.children().map(|mark| mark.kind()).collect();
    let left = marks.contains(&"pipe_table_align_left");
    let right = marks.contains(&"pipe_table_align_right");
    match (left, right) {
        (true, true) => Align::Center,
        (true, false) => Align::Left,
        (false, true) => Align::Right,
        (false, false) => Align::None,
    }
}

/// Narrows the widest of the columns `widths` until the table fits `room` columns, if it can
/// with each column at least one wide.
fn fit(widths: &mut [usize], room: usize) {
    let borders = 3 * widths.len() + 1;
    let total: usize = widths.iter().sum::<usize>() + borders;
    if total <= room || room < borders + widths.len() {
        return;
    }
    let budget = room - borders;
    // the widest a column may be, so the columns take up to `budget`
    let fits = |cap: usize| widths.iter().map(|&width| width.min(cap)).sum::<usize>() <= budget;
    let widest = widths.iter().copied().max().unwrap_or(1);
    let cap = (1..=widest).rev().find(|&cap| fits(cap)).unwrap_or(1);
    let mut spare = budget - widths.iter().map(|&width| width.min(cap)).sum::<usize>();
    for width in widths.iter_mut() {
        let narrowed = (*width).min(cap);
        let extra = usize::from(*width > cap && spare > 0);
        spare -= extra;
        *width = narrowed + extra;
    }
}

/// The units of each row of a cell wrapped to `width` columns, at spaces or else inside a word,
/// without the spaces where it wraps.
fn lines(units: &[Unit], width: usize) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut i = 0;
    while i < units.len() {
        while i < units.len() && units[i].space {
            i += 1;
        }
        if i == units.len() {
            break;
        }
        let start = i;
        let mut used = 0;
        // where the last word on the row ends
        let mut word_end = None;
        let mut j = i;
        while j < units.len() {
            if used + units[j].width > width && j > start {
                break;
            }
            if units[j].space && !units[j - 1].space {
                word_end = Some(j);
            }
            used += units[j].width;
            j += 1;
        }
        let end = if j < units.len() && !units[j].space {
            word_end.unwrap_or(j)
        } else {
            j
        };
        let mut shown_end = end;
        while shown_end > start && units[shown_end - 1].space {
            shown_end -= 1;
        }
        lines.push(start..shown_end);
        i = end;
    }
    if lines.is_empty() {
        lines.push(0..0);
    }
    lines
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn fitting() {
        let mut widths = [4, 20, 6];
        // borders take 10 columns, the columns get 30: the widest gives way
        fit(&mut widths, 40);
        assert_eq!(widths, [4, 20, 6]);
        fit(&mut widths, 30);
        assert_eq!(widths, [4, 10, 6]);
        // too narrow for every column, so nothing changes
        let mut widths = [4, 20, 6];
        fit(&mut widths, 12);
        assert_eq!(widths, [4, 20, 6]);
    }
}
