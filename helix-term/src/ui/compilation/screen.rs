//! The last rows of compilation output as a terminal shows them. Commands redraw them, like a
//! progress bar, until they scroll away, after which they stay as they were.

use std::{
    collections::{HashMap, VecDeque},
    mem,
};

use helix_core::unicode::width::UnicodeWidthChar;
use helix_view::graphics::{Modifier, Style};

use super::output::Shown;
use crate::ui::sgr;

/// What a cell of the screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Char(char),
    /// A char and the zero-width ones after it, like a variation selector: one of the
    /// [`Clusters`] of the screen.
    Cluster(u32),
    /// The rest of the wide char, or of the tab, in the cells before.
    Rest,
}

/// The chars with zero-width ones after them that the screen showed, each kept once.
#[derive(Debug, Default)]
struct Clusters {
    texts: Vec<Box<str>>,
    ids: HashMap<Box<str>, u32>,
}

impl Clusters {
    fn id(&mut self, text: String) -> u32 {
        let next = self.texts.len() as u32;
        let text = text.into_boxed_str();
        *self.ids.entry(text.clone()).or_insert_with(|| {
            self.texts.push(text);
            next
        })
    }

    fn get(&self, id: u32) -> &str {
        &self.texts[id as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    glyph: Glyph,
    style: Style,
}

/// What erasing leaves.
const BLANK: Cell = Cell {
    glyph: Glyph::Char(' '),
    style: Style::new(),
};

impl Cell {
    /// Whether the cell shows nothing: a space without a background, say.
    fn is_blank(&self) -> bool {
        self.glyph == Glyph::Char(' ')
            && self.style.bg.is_none()
            && self.style.underline_style.is_none()
            && !self.style.add_modifier.contains(Modifier::REVERSED)
    }
}

#[derive(Debug, Clone, Default)]
struct Row {
    cells: Vec<Cell>,
    /// Whether the output went on in the next row, written past the end of this one.
    wrapped: bool,
}

impl Row {
    /// Whether the row, of a screen `cols` wide, is filled up with spaces to its end, as some
    /// commands write lines instead of ending them. Its line ends there, without the spaces.
    fn padded(&self, cols: usize) -> bool {
        self.cells.len() >= cols && self.cells.iter().rev().take(2).all(Cell::is_blank)
    }

    /// Whether the line of the row goes on in the next row.
    fn joins(&self, cols: usize) -> bool {
        self.wrapped && !self.padded(cols)
    }

    /// Writes `cell` at `col`, then the rest of it when it is `width` cells wide.
    fn put(&mut self, col: usize, cell: Cell, width: usize) {
        if width == 1 && col == self.cells.len() {
            self.cells.push(cell);
            return;
        }
        if col > self.cells.len() {
            self.cells.resize(col, BLANK);
        }
        if col < self.cells.len() {
            self.split(col);
            self.split(col + width - 1);
        }
        let style = cell.style;
        let rest = (1..width).map(|_| Cell {
            glyph: Glyph::Rest,
            style,
        });
        for (col, cell) in (col..).zip(std::iter::once(cell).chain(rest)) {
            if col < self.cells.len() {
                self.cells[col] = cell;
            } else {
                self.cells.push(cell);
            }
        }
    }

    /// Turns a wide char or a tab covering `col` and other cells into spaces, so that `col` can
    /// change alone.
    fn split(&mut self, col: usize) {
        let rest = |col: usize| {
            self.cells
                .get(col)
                .is_some_and(|cell| cell.glyph == Glyph::Rest)
        };
        if !rest(col) && !rest(col + 1) {
            return;
        }
        let start = (0..=col).rev().find(|&col| !rest(col)).unwrap_or(0);
        let end = (col + 1..).find(|&col| !rest(col)).unwrap_or(col + 1);
        let end = end.min(self.cells.len());
        for cell in &mut self.cells[start..end] {
            cell.glyph = Glyph::Char(' ');
        }
    }

    /// Blanks the cells of `cols`.
    fn erase(&mut self, cols: std::ops::Range<usize>) {
        let end = cols.end.min(self.cells.len());
        if cols.start >= end {
            return;
        }
        self.split(cols.start);
        self.split(end);
        self.cells[cols.start..end].fill(BLANK);
    }

    /// Erases the cells from `col` on.
    fn truncate(&mut self, col: usize) {
        self.split(col);
        self.cells.truncate(col);
        self.wrapped = false;
    }

    fn clear(&mut self) {
        self.cells.clear();
        self.wrapped = false;
    }

    /// Adds the text of the row, of a screen `cols` wide, to `shown`.
    fn render(&self, cols: usize, clusters: &Clusters, shown: &mut Shown) {
        let mut end = self.cells.len();
        if self.padded(cols) {
            end -= self
                .cells
                .iter()
                .rev()
                .take_while(|cell| cell.is_blank())
                .count();
        }
        shown.text.reserve(end);
        for run in self.cells[..end].chunk_by(|cell, next| cell.style == next.style) {
            let start = shown.chars;
            for cell in run {
                match &cell.glyph {
                    Glyph::Char(c) => {
                        shown.text.push(*c);
                        shown.chars += 1;
                    }
                    Glyph::Cluster(id) => {
                        let cluster = clusters.get(*id);
                        shown.text.push_str(cluster);
                        shown.chars += cluster.chars().count();
                    }
                    Glyph::Rest => {}
                }
            }
            shown.style(start, run[0].style);
        }
    }
}

/// Where the output is in an escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// After `ESC`.
    Start,
    /// After `ESC` and intermediate bytes, like the `(` of `ESC ( B`.
    Intermediate,
    /// In a control sequence, `ESC [ … final`.
    Control,
    /// In a string, like an OSC title `ESC ] … BEL`, until `BEL` or `ESC \`.
    String,
    /// After an `ESC` in a string.
    StringEnd,
}

/// The parameters of a control sequence kept at most, which no real one has.
const MAX_PARAMS: usize = 64;
/// Tabs stop at every multiple of this.
const TAB: usize = 8;

/// A terminal screen of the last rows of output, `cols` wide and `height` high. Lines that
/// scroll off its top become output that doesn't change any more.
#[derive(Debug)]
pub struct Screen {
    cols: usize,
    height: usize,
    /// The rows from the top, down to the cursor's at least; those below the last are empty.
    rows: VecDeque<Row>,
    /// The cursor's row.
    row: usize,
    /// The cursor's column; at `cols`, the next char goes into the next row.
    col: usize,
    /// The cursor and style that `ESC 7` saved.
    saved: Option<(usize, usize, Style)>,
    clusters: Clusters,
    /// Whether the colors of the output are kept, rather than left out.
    colors: bool,
    style: Style,
    escape: Escape,
    params: String,
    /// The first bytes of a char that the next output ends.
    partial: Vec<u8>,
    /// The start of the line that goes on in the top row, which scrolled away.
    held: Shown,
    /// The lines that scrolled away since they were taken last, each ending in a line break.
    scrolled: Shown,
}

impl Screen {
    pub fn new(cols: u16, height: u16, colors: bool) -> Self {
        Self {
            cols: cols.max(1).into(),
            height: height.max(1).into(),
            rows: VecDeque::from([Row::default()]),
            row: 0,
            col: 0,
            saved: None,
            clusters: Clusters::default(),
            colors,
            style: Style::default(),
            escape: Escape::None,
            params: String::new(),
            partial: Vec::new(),
            held: Shown::default(),
            scrolled: Shown::default(),
        }
    }

    /// Changes the size of the screen. The rows that no longer fit above the cursor scroll
    /// away; rows wider than the screen stay as they are.
    pub fn resize(&mut self, cols: u16, height: u16) {
        let (cols, height) = (usize::from(cols.max(1)), usize::from(height.max(1)));
        if (cols, height) == (self.cols, self.height) {
            return;
        }
        let used = self.used().max(self.row + 1);
        // Rows below the cursor that don't fit go, as the cursor's stays.
        let over = used.saturating_sub(height).min(self.row);
        self.rows.truncate(used);
        self.scroll_up(over);
        self.row -= over;
        (self.cols, self.height) = (cols, height);
        self.rows.truncate(height);
        self.move_to_row(self.row);
        self.col = self.col.min(cols);
    }

    /// Writes `bytes` of output to the screen.
    pub fn push(&mut self, bytes: &[u8]) {
        let joined;
        let mut bytes = bytes;
        if !self.partial.is_empty() {
            let mut partial = mem::take(&mut self.partial);
            partial.extend_from_slice(bytes);
            joined = partial;
            bytes = &joined;
        }
        let (whole, partial) = bytes.split_at(whole_chars(bytes));
        self.partial.extend_from_slice(partial);
        for chunk in whole.utf8_chunks() {
            let mut text = chunk.valid();
            while let Some(c) = text.chars().next() {
                // Most output is runs of printable ASCII.
                let run = text
                    .bytes()
                    .take_while(|byte| (b' '..=b'~').contains(byte))
                    .count();
                if self.escape == Escape::None && run > 0 {
                    self.print_ascii(&text.as_bytes()[..run]);
                    text = &text[run..];
                } else {
                    self.put(c);
                    text = &text[c.len_utf8()..];
                }
            }
            if !chunk.invalid().is_empty() {
                self.put(char::REPLACEMENT_CHARACTER);
            }
        }
    }

    /// The length of the lines that scrolled away since they were taken.
    pub fn scrolled_len(&self) -> usize {
        self.scrolled.text.len()
    }

    /// Takes the lines that scrolled away since they were taken last.
    pub fn take_scrolled(&mut self) -> Shown {
        mem::take(&mut self.scrolled)
    }

    /// The screen as it shows: its lines down to the cursor or the last one written, the last
    /// without a line break.
    pub fn shown(&self) -> Shown {
        let mut shown = self.held.clone();
        let last = self.used().saturating_sub(1).max(self.row);
        for (index, row) in self.rows.iter().take(last + 1).enumerate() {
            row.render(self.cols, &self.clusters, &mut shown);
            if index < last && !row.joins(self.cols) {
                shown.push('\n', Style::default());
            }
        }
        shown
    }

    /// Ends the output, after which the lines on the screen scrolled away too, down to the last
    /// one written. Takes the lines that scrolled away.
    pub fn finish(&mut self) -> Shown {
        if !mem::take(&mut self.partial).is_empty() {
            self.put(char::REPLACEMENT_CHARACTER);
        }
        let used = self.used();
        self.rows.truncate(used);
        self.scroll_up(used);
        if !self.held.text.is_empty() {
            self.held.push('\n', Style::default());
            self.scrolled.append(&mem::take(&mut self.held));
        }
        self.take_scrolled()
    }

    /// The rows down to the last one written.
    fn used(&self) -> usize {
        self.rows
            .iter()
            .rposition(|row| !row.cells.is_empty())
            .map_or(0, |last| last + 1)
    }

    fn put(&mut self, c: char) {
        self.escape = match (self.escape, c) {
            (Escape::None, '\x1b') => Escape::Start,
            (Escape::None, c) => {
                match c {
                    '\n' => self.line_feed(true),
                    '\x0b' | '\x0c' => self.line_feed(false),
                    '\r' => self.col = 0,
                    '\x08' => self.col = self.cursor_col().saturating_sub(1),
                    '\t' => self.tab(),
                    c if c.is_control() => {}
                    c => self.print(c),
                }
                Escape::None
            }
            (Escape::Start, '[') => {
                self.params.clear();
                Escape::Control
            }
            (Escape::Start, ']' | 'P' | 'X' | '^' | '_') => Escape::String,
            (Escape::Start | Escape::Intermediate, ' '..='/') => Escape::Intermediate,
            (Escape::Start, c) => {
                self.escape_final(c);
                Escape::None
            }
            (Escape::Intermediate, _) => Escape::None,
            (Escape::Control, '@'..='~') => {
                self.control(c);
                Escape::None
            }
            (Escape::Control, '\x1b') => Escape::Start,
            (Escape::Control, c) => {
                if self.params.len() < MAX_PARAMS && !c.is_control() {
                    self.params.push(c);
                }
                Escape::Control
            }
            (Escape::String, '\x07') => Escape::None,
            (Escape::String, '\x1b') => Escape::StringEnd,
            (Escape::String, _) => Escape::String,
            (Escape::StringEnd, '\\') => Escape::None,
            (Escape::StringEnd, _) => Escape::String,
        };
    }

    /// The column the cursor is in, the last one while the next char wraps.
    fn cursor_col(&self) -> usize {
        self.col.min(self.cols - 1)
    }

    fn print(&mut self, c: char) {
        let width = if c.is_ascii() {
            1
        } else {
            c.width().unwrap_or(0)
        };
        if width == 0 {
            self.combine(c);
            return;
        }
        if self.col + width > self.cols && self.col > 0 {
            self.rows[self.row].wrapped = true;
            self.line_feed(true);
        }
        let cell = Cell {
            glyph: Glyph::Char(c),
            style: self.style,
        };
        self.rows[self.row].put(self.col, cell, width);
        self.col += width;
    }

    /// Prints `text`, printable ASCII.
    fn print_ascii(&mut self, mut text: &[u8]) {
        while !text.is_empty() {
            if self.col >= self.cols {
                self.rows[self.row].wrapped = true;
                self.line_feed(true);
            }
            let (now, rest) = text.split_at((self.cols - self.col).min(text.len()));
            let style = self.style;
            let cells = now.iter().map(|&byte| Cell {
                glyph: Glyph::Char(byte.into()),
                style,
            });
            let row = &mut self.rows[self.row];
            if self.col == row.cells.len() {
                row.cells.extend(cells);
            } else {
                for (col, cell) in (self.col..).zip(cells) {
                    row.put(col, cell, 1);
                }
            }
            self.col += now.len();
            text = rest;
        }
    }

    /// Adds the zero-width `c` to the char before the cursor.
    fn combine(&mut self, c: char) {
        let row = &mut self.rows[self.row];
        let Some(col) = (0..self.col.min(row.cells.len()))
            .rev()
            .find(|&col| row.cells[col].glyph != Glyph::Rest)
        else {
            return;
        };
        let glyph = &mut row.cells[col].glyph;
        let cluster = match *glyph {
            Glyph::Char(base) => format!("{base}{c}"),
            Glyph::Cluster(id) => format!("{}{c}", self.clusters.get(id)),
            Glyph::Rest => return,
        };
        *glyph = Glyph::Cluster(self.clusters.id(cluster));
    }

    /// Moves to the next tab stop. Tabs past the end of the row are kept as tabs.
    fn tab(&mut self) {
        let col = self.cursor_col();
        let stop = ((col / TAB + 1) * TAB).min(self.cols - 1);
        if stop <= col {
            return;
        }
        let row = &mut self.rows[self.row];
        if col >= row.cells.len() {
            let tab = Cell {
                glyph: Glyph::Char('\t'),
                style: Style::default(),
            };
            row.put(col, tab, stop - col);
        }
        self.col = stop;
    }

    /// Moves the cursor down a row, scrolling at the bottom, and to the first column with
    /// `carriage_return`.
    fn line_feed(&mut self, carriage_return: bool) {
        self.col = if carriage_return {
            0
        } else {
            self.cursor_col()
        };
        if self.row + 1 >= self.height {
            self.scroll_up(1);
        } else {
            self.move_to_row(self.row + 1);
        }
    }

    /// Moves the cursor to `row`, in the screen.
    fn move_to_row(&mut self, row: usize) {
        self.row = row.min(self.height - 1);
        if self.rows.len() <= self.row {
            self.rows.resize_with(self.row + 1, Row::default);
        }
    }

    /// Scrolls the top `count` rows away, the cursor staying where it is on the screen.
    fn scroll_up(&mut self, count: usize) {
        for _ in 0..count.min(self.height) {
            let Some(mut row) = self.rows.pop_front() else {
                break;
            };
            if row.joins(self.cols) {
                row.render(self.cols, &self.clusters, &mut self.held);
            } else if self.held.text.is_empty() {
                row.render(self.cols, &self.clusters, &mut self.scrolled);
                self.scrolled.push('\n', Style::default());
            } else {
                row.render(self.cols, &self.clusters, &mut self.held);
                self.held.push('\n', Style::default());
                self.scrolled.append(&mem::take(&mut self.held));
            }
            row.clear();
            // The row goes back in below, with its cells allocated.
            if self.rows.len() <= self.row {
                self.rows.push_back(row);
            }
        }
        self.move_to_row(self.row);
    }

    /// Scrolls the rows down by `count` from `top`, the bottom ones going.
    fn scroll_down(&mut self, top: usize, count: usize) {
        for _ in 0..count.min(self.height) {
            self.rows.insert(top.min(self.rows.len()), Row::default());
        }
        self.rows.truncate(self.height);
        self.move_to_row(self.row);
    }

    fn clear_rows(&mut self, rows: std::ops::Range<usize>) {
        for row in self
            .rows
            .range_mut(rows.start.min(self.rows.len())..rows.end.min(self.rows.len()))
        {
            row.clear();
        }
    }

    /// Carries out the escape sequence `ESC c`.
    fn escape_final(&mut self, c: char) {
        match c {
            '7' => self.saved = Some((self.row, self.col, self.style)),
            '8' => self.restore(),
            'D' => self.line_feed(false),
            'E' => self.line_feed(true),
            'M' if self.row == 0 => self.scroll_down(0, 1),
            'M' => self.row -= 1,
            'c' => {
                self.clear_rows(0..self.height);
                (self.row, self.col, self.style) = (0, 0, Style::default());
            }
            _ => {}
        }
    }

    /// Erases the screen, after the cursor with `0`, before it with `1`, else all of it.
    fn erase_display(&mut self, which: usize) {
        let (row, col) = (self.row, self.col);
        match which {
            0 => {
                self.rows[row].truncate(col);
                self.rows.truncate(row + 1);
            }
            1 => {
                self.clear_rows(0..row);
                self.rows[row].erase(0..col + 1);
            }
            _ => self.clear_rows(0..self.height),
        }
    }

    fn restore(&mut self) {
        let (row, col, style) = self.saved.unwrap_or_default();
        self.move_to_row(row);
        self.col = col.min(self.cols);
        self.style = style;
    }

    /// Carries out the control sequence `ESC [ params final`.
    fn control(&mut self, final_char: char) {
        // Private sequences, like `ESC [ ? 25 l`, hide the cursor and the like.
        if self.params.starts_with(['<', '=', '>', '?']) {
            if let Some(modes) = self.params.strip_prefix('?') {
                let alternate = modes
                    .split(';')
                    .any(|mode| matches!(mode, "47" | "1047" | "1049"));
                // A full-screen program's screen goes, once it's done with it. What the screen
                // showed before stays, as it scrolled away.
                if alternate && final_char == 'h' {
                    let used = self.used();
                    self.rows.truncate(used);
                    self.scroll_up(used);
                }
                if alternate && matches!(final_char, 'h' | 'l') {
                    self.clear_rows(0..self.height);
                    (self.row, self.col) = (0, 0);
                }
            }
            return;
        }
        if final_char == 'm' {
            // `ESC [ 1 ; 31 m`, but not other sequences ending in `m`, like `ESC [ > 4 m`.
            let sgr = self
                .params
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b';' | b':'));
            if self.colors && sgr {
                self.style = sgr::apply(self.style, &self.params);
            }
            return;
        }
        let mut params = self
            .params
            .split(';')
            .map(|param| param.parse::<usize>().unwrap_or(0));
        let (first, second) = (params.next().unwrap_or(0), params.next().unwrap_or(0));
        // How many times the sequence moves, inserts or deletes.
        let count = first.max(1);
        let (row, col) = (self.row, self.cursor_col());
        match final_char {
            'A' => (self.row, self.col) = (row.saturating_sub(count), col),
            'B' => {
                self.move_to_row(row.saturating_add(count));
                self.col = col;
            }
            'C' => self.col = col.saturating_add(count).min(self.cols - 1),
            'D' => self.col = col.saturating_sub(count),
            'E' => {
                self.move_to_row(row.saturating_add(count));
                self.col = 0;
            }
            'F' => (self.row, self.col) = (row.saturating_sub(count), 0),
            'G' | '`' => self.col = (count - 1).min(self.cols - 1),
            'd' => {
                self.move_to_row(count - 1);
                self.col = col;
            }
            'H' | 'f' => {
                self.move_to_row(count - 1);
                self.col = (second.max(1) - 1).min(self.cols - 1);
            }
            'J' => {
                self.col = col;
                self.erase_display(first);
            }
            'K' => {
                self.col = col;
                match first {
                    0 => self.rows[row].truncate(col),
                    1 => self.rows[row].erase(0..col + 1),
                    _ => self.rows[row].clear(),
                }
            }
            '@' => {
                let cells = &mut self.rows[row];
                if col < cells.cells.len() {
                    cells.split(col);
                    let count = count.min(self.cols - col);
                    cells.cells.splice(col..col, vec![BLANK; count]);
                    if cells.cells.len() > self.cols {
                        cells.truncate(self.cols);
                    }
                }
                self.col = col;
            }
            'P' => {
                let cells = &mut self.rows[row];
                if col < cells.cells.len() {
                    let end = col.saturating_add(count).min(cells.cells.len());
                    cells.split(col);
                    cells.split(end);
                    cells.cells.drain(col..end);
                }
                self.col = col;
            }
            'X' => {
                self.rows[row].erase(col..col.saturating_add(count));
                self.col = col;
            }
            'L' => {
                self.scroll_down(row, count);
                self.col = 0;
            }
            'M' => {
                for _ in 0..count.min(self.rows.len() - row) {
                    self.rows.remove(row);
                }
                self.move_to_row(row);
                self.col = 0;
            }
            'S' => self.scroll_up(count),
            'T' => self.scroll_down(0, count),
            's' if self.params.is_empty() => self.saved = Some((row, self.col, self.style)),
            'u' if self.params.is_empty() => self.restore(),
            _ => {}
        }
    }
}

/// The length of `bytes` without the bytes of a char that they end in before it ends.
fn whole_chars(bytes: &[u8]) -> usize {
    let len = bytes.len();
    for (back, &byte) in bytes.iter().rev().take(3).enumerate() {
        // A continuation byte.
        if byte & 0xc0 == 0x80 {
            continue;
        }
        let needed = match byte {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        return if back + 1 < needed {
            len - back - 1
        } else {
            len
        };
    }
    len
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use helix_view::graphics::Color;

    use super::*;

    /// What a screen `cols` wide and `height` high shows of `output`: the lines that scrolled
    /// away, and the screen.
    fn run(cols: u16, height: u16, output: &str) -> (String, String) {
        let mut screen = Screen::new(cols, height, false);
        screen.push(output.as_bytes());
        (screen.take_scrolled().text, screen.shown().text)
    }

    /// The lines of `output` once it ended.
    fn finished(cols: u16, height: u16, output: &str) -> String {
        let mut screen = Screen::new(cols, height, false);
        screen.push(output.as_bytes());
        screen.finish().text
    }

    #[test]
    fn lines_scroll_away_from_the_last_rows() {
        let output = "one\ntwo\nthree\nfour\nfi";
        assert_eq!(
            run(80, 3, output),
            ("one\ntwo\n".to_owned(), "three\nfour\nfi".to_owned())
        );
        assert_eq!(finished(80, 3, output), "one\ntwo\nthree\nfour\nfi\n");
        // The screen shows down to the cursor, but the output ends with its last line.
        assert_eq!(run(80, 5, "a\n\n").1, "a\n\n");
        assert_eq!(finished(80, 5, "a\n\n"), "a\n");
        assert_eq!(finished(80, 2, "a\n\nb\n"), "a\n\nb\n");
        assert_eq!(finished(80, 2, ""), "");
        // Spaces that don't fill the row stay, like those of a prompt.
        assert_eq!(run(80, 2, "Password: ").1, "Password: ");
    }

    #[test]
    fn lines_show_as_a_terminal_shows_them() {
        let shown = |output: &str| finished(80, 4, output);
        // Colors, a title, a link, a charset switch and the cursor hidden.
        assert_eq!(
            shown("\x1b[0m\x1b[1m\x1b[38;5;9merror[E0425]\x1b[0m: cannot find"),
            "error[E0425]: cannot find\n"
        );
        assert_eq!(
            shown("\x1b]0;cargo\x07done \x1b]8;;file:///a\x1b\\a\x1b]8;;\x1b\\"),
            "done a\n"
        );
        assert_eq!(shown("\x1b(Bplain\x1b[?25l"), "plain\n");
        // Progress drawn over itself, and other control characters.
        assert_eq!(
            shown("Downloading 10%\rDownloading 100%"),
            "Downloading 100%\n"
        );
        assert_eq!(shown("abcdef\rXY"), "XYcdef\n");
        assert_eq!(shown("crlf\r\n"), "crlf\n");
        assert_eq!(shown("a\tb\x07\x08c"), "a\tc\n");
        // Tabs that skip text move over it.
        assert_eq!(shown("abcdefghij\r\tX"), "abcdefghXj\n");
        assert_eq!(shown("a\tb\rabcdefghij"), "abcdefghij\n");
        // Wide chars take two columns, combining ones none. Half of one overwritten leaves a space.
        assert_eq!(shown("日本\r\x1b[2Cx"), "日x \n");
        assert_eq!(shown("✔\u{fe0f} ok\x1b[3D\x1b[P"), "✔\u{fe0f}ok\n");
        // Invalid UTF-8 shows replaced, and chars split between reads whole.
        let mut screen = Screen::new(80, 4, false);
        screen.push(b"bad \xff byte\n");
        let arrow = "┌─ main.typ:5:12\n".as_bytes();
        screen.push(&arrow[..2]);
        screen.push(&arrow[2..]);
        assert_eq!(
            screen.finish().text,
            "bad \u{fffd} byte\n┌─ main.typ:5:12\n"
        );
    }

    #[test]
    fn colors_are_kept() {
        let fg = |index| Style::default().fg(Color::Indexed(index));
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut screen = Screen::new(80, 2, true);
        // As cargo writes an error, and a color that goes on to the next line.
        screen.push(
            b"\x1b[0m\x1b[1m\x1b[91merror\x1b[0m\x1b[1m: bad\x1b[0m\n\x1b[32mgreen\nstill\x1b[0m plain\n",
        );
        let scrolled = screen.take_scrolled();
        assert_eq!(scrolled.text, "error: bad\ngreen\n");
        assert_eq!(
            scrolled.styles,
            [
                (0..5, bold.fg(Color::Indexed(9))),
                (5..10, bold),
                (11..16, fg(2)),
            ]
        );
        // The screen shows in the style in effect, and overwritten chars in the new one.
        screen.push(b"\x1b[33mwa\rX");
        assert_eq!(screen.shown().styles, [(0..5, fg(2)), (12..14, fg(3))]);
        let mut overwritten = Screen::new(80, 2, true);
        overwritten.push(b"abc\r\x1b[31mX");
        assert_eq!(overwritten.shown().styles, [(0..1, fg(1))]);
        // Private sequences are no colors, and without colors there are none.
        let mut private = Screen::new(80, 2, true);
        private.push(b"\x1b[>4;2ma");
        assert_eq!(private.shown().styles, []);
        let mut plain = Screen::new(80, 2, false);
        plain.push(b"\x1b[31mred\n");
        assert_eq!(plain.finish().styles, []);
        // Rows filled up with spaces end without them, but for those with a background.
        let mut marked = Screen::new(12, 2, true);
        marked.push(b"trailing\x1b[41m  \x1b[0m  ");
        assert_eq!(marked.shown().text, "trailing  ");
    }

    #[test]
    fn long_lines_wrap_and_join_again() {
        // A line wider than the screen wraps, and is one line again once it scrolls away.
        assert_eq!(
            run(10, 2, "src/main.rs:12:5: error\nnext\n"),
            ("src/main.rs:12:5: error\n".to_owned(), "next\n".to_owned())
        );
        assert_eq!(run(10, 3, "src/main.rs:12:5").1, "src/main.rs:12:5");
        // Rows filled up with spaces, as some commands write lines, end their lines.
        assert_eq!(finished(10, 3, "PASS a    FAIL b    "), "PASS a\nFAIL b\n");
        // A wide char that doesn't fit goes to the next row.
        assert_eq!(run(5, 3, "abcd日\x1b[2;1Hx").1, "abcdx ");
    }

    #[test]
    fn progress_is_drawn_over() {
        // As cargo draws its progress bar under the messages it prints.
        let output =
            "   Compiling demo v0.1.0 (/tmp/demo)\n    Building [      ] 0/2: demo    \r\x1b[K\
            warning: unused variable\n    Building [===>  ] 1/2: demo(bin)\r\x1b[K\
            \x20   Finished `dev` profile in 0.89s\n";
        assert_eq!(
            finished(40, 10, output),
            "   Compiling demo v0.1.0 (/tmp/demo)\nwarning: unused variable\n    Finished `dev` profile in 0.89s\n"
        );
        assert_eq!(
            run(40, 10, &output[..60]).1,
            "   Compiling demo v0.1.0 (/tmp/demo)\n    Building [      ] 0"
        );

        // As cargo-nextest draws its two rows of progress, filled up with spaces, below the
        // results it prints.
        let bar = |done| {
            format!(
                "{:20}{:20}",
                format!("Running {done}/2"),
                "[0s] tests::slow"
            )
        };
        let clear = "\x1b[1A\r\x1b[2K\x1b[1B\r\x1b[2K\x1b[1A";
        let output = format!("{}{clear}{:20}{}", bar(0), "PASS tests::quick", bar(1));
        assert_eq!(
            run(20, 5, &output),
            (
                String::new(),
                "PASS tests::quick\nRunning 1/2\n[0s] tests::slow".to_owned()
            )
        );
        let output = format!("{output}{clear}{:20}{:20}", "PASS tests::slow", "2 passed");
        assert_eq!(
            finished(20, 5, &output),
            "PASS tests::quick\nPASS tests::slow\n2 passed\n"
        );

        // As Gradle draws its progress below the lines it prints: up, the bar, back and down.
        let bar = |percent, work| {
            format!(
                "\x1b[2A\x1b[1m<===> {percent}% EXECUTING\x1b[m\x1b[0K\x1b[19D\x1b[1B> {work}\x1b[0K\
                 \x1b[20D\x1b[1B"
            )
        };
        let running = format!(
            "\n\n{}\x1b[2A\x1b[0K\n> Task :compileJava\x1b[0K\n\n\n{}",
            bar(10, ":compileJava"),
            bar(50, "IDLE"),
        );
        assert_eq!(
            run(40, 6, &running).1,
            "\n> Task :compileJava\n<===> 50% EXECUTING\n> IDLE\n"
        );
        let output =
            format!("{running}\x1b[2A\x1b[2K\x1b[1B\x1b[2K\x1b[1ABUILD SUCCESSFUL in 1s\n");
        assert_eq!(
            finished(40, 6, &output),
            "\n> Task :compileJava\nBUILD SUCCESSFUL in 1s\n"
        );
    }

    #[test]
    fn erasing() {
        let shown = |output: &str| run(10, 4, output).1;
        assert_eq!(shown("abcdef\x1b[3D\x1b[K"), "abc");
        assert_eq!(shown("abcdef\x1b[3D\x1b[1K"), "    ef");
        assert_eq!(shown("abcdef\x1b[3D\x1b[2Kx"), "   x");
        assert_eq!(shown("abcdef\x1b[5D\x1b[2X"), "a  def");
        assert_eq!(shown("abcdef\x1b[5D\x1b[2P"), "adef");
        assert_eq!(shown("abcdef\x1b[5D\x1b[2@"), "a  bcdef");
        assert_eq!(shown("abcdefghij\r\x1b[2@"), "  abcdefgh");
        assert_eq!(shown("one\ntwo\nthree\x1b[1;2H\x1b[J"), "o");
        assert_eq!(shown("one\ntwo\nthree\x1b[2;2H\x1b[1J"), "\n  o\nthree");
        assert_eq!(shown("one\ntwo\nthree\x1b[2J"), "\n\n");
        assert_eq!(shown("one\ntwo\nthree\x1b[H\x1b[2J"), "");
        // Lines inserted push the bottom ones off the screen, and deleted ones pull them up.
        assert_eq!(shown("1\n2\n3\n4\x1b[2;1H\x1b[2L"), "1\n\n\n2");
        assert_eq!(shown("1\n2\n3\n4\x1b[2;1H\x1b[2M"), "1\n4");
        // Scrolling up scrolls lines away, scrolling down pushes them off the bottom.
        assert_eq!(
            run(10, 4, "1\n2\n3\x1b[2S"),
            ("1\n2\n".to_owned(), "3\n\n".to_owned())
        );
        assert_eq!(shown("1\n2\n3\n4\x1b[2T"), "\n\n1\n2");
        assert_eq!(shown("1\n2\x1b[1;1H\x1bM0"), "0\n1\n2");
        // A full-screen program's screen goes, the lines before it stay.
        assert_eq!(
            run(10, 4, "before\n\x1b[?1049hfull\nscreen\x1b[?1049lafter"),
            ("before\n".to_owned(), "after".to_owned())
        );
    }

    #[test]
    fn cursor_moves_and_returns() {
        let shown = |output: &str| run(10, 4, output).1;
        assert_eq!(shown("a\x1b[3Cb\x1b[2;3Hc"), "a   b\n  c");
        assert_eq!(shown("ab\x1b7\ncd\x1b8x"), "abx\ncd");
        assert_eq!(shown("ab\x1b[s\ncd\x1b[ux"), "abx\ncd");
        assert_eq!(shown("abc\x1b[2Gx\x1b[99Gy"), "axc      y");
        assert_eq!(shown("1\n2\n3\x1b[2Fa\x1b[Eb\x1b[3dc"), "a\nb\n3c");
        // The cursor stays on the screen, and on the row at its end until the next char.
        assert_eq!(shown("x\x1b[99A\x1b[99Dy"), "y");
        assert_eq!(shown("abcdefghij\x1b[Kk"), "abcdefghik");
        assert_eq!(shown("abcdefghijk\x1b[1;1Hx"), "xbcdefghijk");
        assert_eq!(shown("abcdefghij\rk"), "kbcdefghij");
    }

    #[test]
    fn resizing() {
        let mut screen = Screen::new(10, 4, false);
        screen.push(b"1\n2\n3\n4");
        // Rows that no longer fit above the cursor scroll away.
        screen.resize(10, 2);
        assert_eq!(screen.take_scrolled().text, "1\n2\n");
        assert_eq!(screen.shown().text, "3\n4");
        // Lines written later wrap at the new width; rows written before stay.
        screen.resize(3, 2);
        screen.push(b"\nabcd");
        assert_eq!(screen.take_scrolled().text, "3\n4\n");
        assert_eq!(screen.shown().text, "abcd");
        screen.push(b"\x1b[1;1Hlonger");
        assert_eq!(screen.shown().text, "longer");
        // Rows below the cursor that don't fit go.
        let mut screen = Screen::new(10, 4, false);
        screen.push(b"1\n2\n3\n4\x1b[1;1H");
        screen.resize(10, 2);
        assert_eq!(screen.take_scrolled().text, "");
        assert_eq!(screen.shown().text, "1\n2");
    }

    /// Times the screen on lines of output, and on progress redrawn, in release:
    /// `cargo test --release -p helix-term --lib compilation::screen::tests::measure -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure() {
        let line =
            "\x1b[1m\x1b[92m   Compiling\x1b[0m helix-term v25.7.1 (/home/user/helix/helix-term)\n";
        let output = line.repeat(1_000_000);
        for (cols, height) in [(100, 40), (u16::MAX, 1)] {
            let mut screen = Screen::new(cols, height, true);
            let start = Instant::now();
            for chunk in output.as_bytes().chunks(64 * 1024) {
                screen.push(chunk);
                screen.take_scrolled();
            }
            screen.finish();
            println!("1M colored lines on {cols}x{height}: {:?}", start.elapsed());
        }
        let bar = |done| {
            format!(
                "{:100}{:100}",
                format!("Running {done}/2"),
                "[0s] tests::slow"
            )
        };
        let clear = "\x1b[1A\r\x1b[2K\x1b[1B\r\x1b[2K\x1b[1A";
        let output: String = (0..10_000)
            .map(|done| format!("{}{clear}", bar(done)))
            .collect();
        let mut screen = Screen::new(100, 40, true);
        let start = Instant::now();
        screen.push(output.as_bytes());
        screen.shown();
        println!("10k redraws: {:?}", start.elapsed());
    }
}
