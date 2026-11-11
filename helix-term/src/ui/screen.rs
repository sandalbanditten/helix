//! The last rows of output as a terminal shows them, which commands may redraw.

use std::{
    collections::{HashMap, VecDeque},
    mem,
    ops::Range,
};

use helix_core::unicode::width::UnicodeWidthChar;
use helix_view::{
    graphics::{Modifier, Style},
    sgr,
};

/// Text as a terminal shows it, with the styles of its chars.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shown {
    pub text: String,
    /// The chars of `text`.
    pub chars: usize,
    /// The styles of char ranges of `text`, in order and apart; other chars have none.
    pub styles: Vec<(Range<usize>, Style)>,
}

impl Shown {
    /// Adds `c` in `style`.
    pub fn push(&mut self, c: char, style: Style) {
        let at = self.chars;
        match self.styles.last_mut() {
            Some((range, last)) if range.end == at && *last == style => range.end += 1,
            _ if style == Style::default() => {}
            _ => self.styles.push((at..at + 1, style)),
        }
        self.text.push(c);
        self.chars += 1;
    }

    /// Gives the chars from `start` on `style`.
    pub fn style(&mut self, start: usize, style: Style) {
        let end = self.chars;
        match self.styles.last_mut() {
            _ if start == end || style == Style::default() => {}
            Some((range, last)) if range.end == start && *last == style => range.end = end,
            _ => self.styles.push((start..end, style)),
        }
    }

    /// Adds `other` at the end.
    pub fn append(&mut self, other: &Shown) {
        let start = self.chars;
        let styles = other.styles.iter();
        self.styles
            .extend(styles.map(|(range, style)| (range.start + start..range.end + start, *style)));
        self.text.push_str(&other.text);
        self.chars += other.chars;
    }

    /// The chars and the bytes of the whole lines that `self` and `other` start with, alike in
    /// their text and styles.
    pub fn common_lines(&self, other: &Shown) -> (usize, usize) {
        let (mut chars, mut bytes) = (0, 0);
        let (mut styles, mut other_styles) = (&self.styles[..], &other.styles[..]);
        let lines = self.text.split_inclusive('\n');
        for (line, other_line) in lines.zip(other.text.split_inclusive('\n')) {
            if line != other_line || !line.ends_with('\n') {
                break;
            }
            let end = chars + line.chars().count();
            let (own, rest) =
                styles.split_at(styles.partition_point(|(range, _)| range.start < end));
            let (others, other_rest) =
                other_styles.split_at(other_styles.partition_point(|(range, _)| range.start < end));
            if own != others {
                break;
            }
            (styles, other_styles) = (rest, other_rest);
            chars = end;
            bytes += line.len();
        }
        (chars, bytes)
    }

    /// Keeps the first `chars` chars.
    pub fn truncate(&mut self, chars: usize) {
        if chars >= self.chars {
            return;
        }
        let byte = self
            .text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(byte, _)| byte);
        self.text.truncate(byte);
        self.styles.retain_mut(|(range, _)| {
            range.end = range.end.min(chars);
            range.start < range.end
        });
        self.chars = chars;
    }

    /// Splits off the text from the char `chars`, at the byte `bytes`, which starts a line.
    pub fn split_off(&mut self, chars: usize, bytes: usize) -> Shown {
        let text = self.text.split_off(bytes);
        let at = self
            .styles
            .partition_point(|(range, _)| range.start < chars);
        let styles = self.styles.split_off(at);
        let rest = Shown {
            text,
            chars: self.chars - chars,
            styles: styles
                .into_iter()
                .map(|(range, style)| (range.start - chars..range.end - chars, style))
                .collect(),
        };
        self.chars = chars;
        rest
    }
}

/// What a cell of the screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Char(char),
    /// A char and the zero-width ones after it: one of the [`Clusters`] of the screen.
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
    /// A row of `cols` cells of `blank`, or an empty one.
    fn blank(blank: Cell, cols: usize) -> Self {
        let cells = if blank == BLANK {
            Vec::new()
        } else {
            vec![blank; cols]
        };
        Self {
            cells,
            wrapped: false,
        }
    }

    /// Whether the row, of a screen `cols` wide, is filled up with spaces to its end.
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

    /// Moves the cells from `col` on right by `count`, those past `cols` going.
    fn insert(&mut self, col: usize, count: usize, blank: Cell, cols: usize) {
        if col >= self.cells.len() && blank == BLANK {
            return;
        }
        if col > self.cells.len() {
            self.cells.resize(col, BLANK);
        }
        self.split(col);
        let count = count.min(cols - col);
        self.cells
            .splice(col..col, std::iter::repeat_n(blank, count));
        if self.cells.len() > cols {
            self.split(cols);
            self.cells.truncate(cols);
        }
    }

    /// Turns a wide char or a tab covering `col` and other cells into spaces.
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

    /// Fills the cells of `cols` with `blank`.
    fn erase(&mut self, cols: Range<usize>, blank: Cell) {
        if blank != BLANK && self.cells.len() < cols.end {
            self.cells.resize(cols.end, BLANK);
        }
        let end = cols.end.min(self.cells.len());
        if cols.start >= end {
            return;
        }
        self.split(cols.start);
        self.split(end);
        self.cells[cols.start..end].fill(blank);
    }

    /// Erases the cells from `col` on, of a row `cols` wide.
    fn truncate(&mut self, col: usize, blank: Cell, cols: usize) {
        if blank == BLANK {
            self.split(col);
            self.cells.truncate(col);
        } else {
            self.erase(col..cols, blank);
        }
        self.wrapped = false;
    }

    fn clear(&mut self) {
        self.cells.clear();
        self.wrapped = false;
    }

    /// Adds the text of the row, of a screen `cols` wide, to `shown`. Returns the cells shown.
    fn render(&self, cols: usize, clusters: &Clusters, shown: &mut Shown) -> usize {
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
        end
    }
}

/// Where the output is in an escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// After `ESC`.
    Start,
    /// After `ESC` and an intermediate byte, like the `(` of `ESC ( B`.
    Intermediate(char),
    /// In a control sequence, `ESC [ … final`.
    Control,
    /// In a string `ESC c …`, like an OSC title `ESC ] … BEL`, until `BEL` or `ESC \`.
    String(char),
    /// After an `ESC` in a string.
    StringEnd(char),
}

/// The parameters of a control sequence kept at most, which no real one has.
const MAX_PARAMS: usize = 64;
/// The longest string kept, like a title.
const MAX_STRING: usize = 4096;
/// Tabs stop at every multiple of this.
const TAB: usize = 8;

/// The modes programs set on a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modes {
    /// Cursor keys send `ESC O A` rather than `ESC [ A`.
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    /// The program draws a frame, which shows once it is done.
    pub synchronized: bool,
    pub show_cursor: bool,
    autowrap: bool,
    /// Printing inserts rather than overwrites.
    insert: bool,
    /// Rows count from the top of the scroll region.
    origin: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            bracketed_paste: false,
            synchronized: false,
            show_cursor: true,
            autowrap: true,
            insert: false,
            origin: false,
        }
    }
}

/// The cursor as `ESC 7` saves it.
#[derive(Debug, Clone, Copy, Default)]
struct Saved {
    row: usize,
    col: usize,
    style: Style,
    charsets: [bool; 2],
    shifted: bool,
}

/// The main screen while the alternate one shows.
#[derive(Debug)]
struct Main {
    rows: VecDeque<Row>,
    row: usize,
    col: usize,
}

/// A terminal screen of the last rows of output, `cols` wide and `height` high.
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
    saved: Option<Saved>,
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
    /// Whether programs draw on the screen and ask it questions, as in a terminal, rather than
    /// only write output.
    terminal: bool,
    /// The rows that scroll.
    region: Range<usize>,
    modes: Modes,
    /// Whether the charsets G0 and G1 are DEC line drawing, and whether G1 is in use.
    charsets: [bool; 2],
    shifted: bool,
    /// The char printed last, which `CSI b` repeats.
    last: Option<char>,
    main: Option<Box<Main>>,
    /// The string being read.
    string: String,
    /// The answers to the program's questions.
    replies: Vec<u8>,
    title: Option<String>,
    /// The colors of the text and the background, for programs that ask.
    palette: [Option<(u8, u8, u8)>; 2],
    /// Whether the program cleared the lines that scrolled away.
    cleared: bool,
}

impl Screen {
    pub fn new(cols: u16, height: u16, colors: bool) -> Self {
        let height = usize::from(height.max(1));
        Self {
            cols: cols.max(1).into(),
            height,
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
            terminal: false,
            region: 0..height,
            modes: Modes::default(),
            charsets: [false; 2],
            shifted: false,
            last: None,
            main: None,
            string: String::new(),
            replies: Vec::new(),
            title: None,
            palette: [None; 2],
            cleared: false,
        }
    }

    /// The screen of a terminal, on which programs draw, telling those that ask the colors of
    /// its text and its background.
    pub fn terminal(cols: u16, height: u16, palette: [Option<(u8, u8, u8)>; 2]) -> Self {
        Self {
            terminal: true,
            palette,
            ..Self::new(cols, height, true)
        }
    }

    /// Changes the size of the screen.
    pub fn resize(&mut self, cols: u16, height: u16) {
        let (cols, height) = (usize::from(cols.max(1)), usize::from(height.max(1)));
        if (cols, height) == (self.cols, self.height) {
            return;
        }
        let used = self.used().max(self.row + 1);
        // Rows below the cursor that don't fit go, as the cursor's stays.
        let over = used.saturating_sub(height).min(self.row);
        self.rows.truncate(used);
        if self.main.is_some() {
            for _ in 0..over {
                self.rows.pop_front();
            }
        } else {
            self.scroll_away(over);
        }
        self.row -= over;
        (self.cols, self.height) = (cols, height);
        self.region = 0..height;
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
                if self.escape == Escape::None && run > 0 && self.plain() {
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

    /// The lines of the screen down to the cursor or the last one written. On a terminal, every
    /// row is a line, after the start of the line that goes on in the top row.
    pub fn shown(&self) -> Shown {
        let mut shown = Shown::default();
        if self.terminal {
            if self.top() > 0 {
                shown.append(&self.held);
                shown.push('\n', Style::default());
            }
            for row in 0..self.height {
                let cells = self.rows.get(row).map_or(0, |cells| {
                    cells.render(self.cols, &self.clusters, &mut shown)
                });
                if row == self.row {
                    for _ in cells..self.col.min(self.cols) {
                        shown.push(' ', Style::default());
                    }
                }
                if row + 1 < self.height {
                    shown.push('\n', Style::default());
                }
            }
            return shown;
        }
        shown = self.held.clone();
        let last = self.used().saturating_sub(1).max(self.row);
        for (index, row) in self.rows.iter().take(last + 1).enumerate() {
            row.render(self.cols, &self.clusters, &mut shown);
            if index < last && !row.joins(self.cols) {
                shown.push('\n', Style::default());
            }
        }
        shown
    }

    /// The chars of [`shown`](Self::shown) before the top row of a terminal.
    pub fn top(&self) -> usize {
        if self.main.is_some() || self.held.chars == 0 {
            0
        } else {
            self.held.chars + 1
        }
    }

    /// The chars of [`shown`](Self::shown) before the cursor of a terminal.
    pub fn cursor(&self) -> usize {
        let mut shown = Shown::default();
        let mut chars = self.top();
        for row in self.rows.iter().take(self.row) {
            row.render(self.cols, &self.clusters, &mut shown);
            chars += 1;
        }
        let row = Row {
            cells: self.rows[self.row]
                .cells
                .iter()
                .take(self.col)
                .copied()
                .collect(),
            wrapped: false,
        };
        let cells = row.render(self.cols, &self.clusters, &mut shown);
        chars + shown.chars + self.col.min(self.cols).saturating_sub(cells)
    }

    pub fn modes(&self) -> Modes {
        self.modes
    }

    /// The title the program gave the terminal.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Takes the answers to the program's questions, to write to it.
    pub fn take_replies(&mut self) -> Vec<u8> {
        mem::take(&mut self.replies)
    }

    /// Whether the program cleared the lines that scrolled away, since this was asked last.
    pub fn take_cleared(&mut self) -> bool {
        mem::take(&mut self.cleared)
    }

    /// Ends the output and takes the lines that scrolled away.
    pub fn finish(&mut self) -> Shown {
        if !mem::take(&mut self.partial).is_empty() {
            self.put(char::REPLACEMENT_CHARACTER);
        }
        self.leave_alternate(false);
        let used = self.used();
        self.rows.truncate(used);
        self.scroll_away(used);
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

    /// Whether printable ASCII prints as itself, in its cells.
    fn plain(&self) -> bool {
        self.modes.autowrap && !self.modes.insert && !self.charsets[usize::from(self.shifted)]
    }

    /// What erasing leaves: blanks in the background color.
    fn blank(&self) -> Cell {
        Cell {
            glyph: Glyph::Char(' '),
            style: Style {
                bg: self.style.bg,
                ..Style::new()
            },
        }
    }

    fn put(&mut self, c: char) {
        self.escape = match (self.escape, c) {
            (Escape::None, '\x1b') => Escape::Start,
            (Escape::None, c) => {
                match c {
                    '\n' | '\x0b' | '\x0c' if self.terminal => self.line_feed(false),
                    '\n' => self.line_feed(true),
                    '\x0b' | '\x0c' => self.line_feed(false),
                    '\r' => self.col = 0,
                    '\x08' => self.col = self.cursor_col().saturating_sub(1),
                    '\t' => self.tab(),
                    '\x0e' => self.shifted = true,
                    '\x0f' => self.shifted = false,
                    c if c.is_control() => {}
                    c => self.print(c),
                }
                Escape::None
            }
            (Escape::Start, '[') => {
                self.params.clear();
                Escape::Control
            }
            (Escape::Start, ']' | 'P' | 'X' | '^' | '_') => {
                self.string.clear();
                Escape::String(c)
            }
            (Escape::Start, ' '..='/') => Escape::Intermediate(c),
            (Escape::Start, c) => {
                self.escape_final(c);
                Escape::None
            }
            (Escape::Intermediate(intermediate), ' '..='/') => Escape::Intermediate(intermediate),
            (Escape::Intermediate(intermediate), c) => {
                // `ESC ( 0` makes G0 line drawing, `ESC ) B` makes G1 ASCII.
                if let Some(set) = ['(', ')'].iter().position(|&set| set == intermediate) {
                    self.charsets[set] = c == '0';
                }
                Escape::None
            }
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
            (Escape::String(kind), '\x07') => {
                self.string_end(kind);
                Escape::None
            }
            (Escape::String(kind), '\x1b') => Escape::StringEnd(kind),
            (Escape::String(kind), c) => {
                if self.string.len() < MAX_STRING {
                    self.string.push(c);
                }
                Escape::String(kind)
            }
            (Escape::StringEnd(kind), '\\') => {
                self.string_end(kind);
                Escape::None
            }
            (Escape::StringEnd(kind), _) => Escape::String(kind),
        };
    }

    /// The column the cursor is in, the last one while the next char wraps.
    fn cursor_col(&self) -> usize {
        self.col.min(self.cols - 1)
    }

    fn print(&mut self, c: char) {
        let c = if self.charsets[usize::from(self.shifted)] {
            line_drawing(c)
        } else {
            c
        };
        let width = if c.is_ascii() {
            1
        } else {
            c.width().unwrap_or(0)
        };
        if width == 0 {
            self.combine(c);
            return;
        }
        if self.col + width > self.cols {
            if self.modes.autowrap && self.col > 0 {
                self.rows[self.row].wrapped = true;
                self.line_feed(true);
            } else {
                self.col = self.cols.saturating_sub(width);
            }
        }
        let cell = Cell {
            glyph: Glyph::Char(c),
            style: self.style,
        };
        if self.modes.insert {
            let blank = self.blank();
            self.rows[self.row].insert(self.col, width, blank, self.cols);
        }
        self.rows[self.row].put(self.col, cell, width);
        self.col += width;
        self.last = Some(c);
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
            self.last = now.last().map(|&byte| byte.into());
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

    /// Moves to the next tab stop. Output keeps tabs past the end of the row as tabs; a terminal
    /// shows the spaces.
    fn tab(&mut self) {
        let col = self.cursor_col();
        let stop = ((col / TAB + 1) * TAB).min(self.cols - 1);
        if stop <= col {
            return;
        }
        let row = &mut self.rows[self.row];
        if col >= row.cells.len() && !self.terminal {
            let tab = Cell {
                glyph: Glyph::Char('\t'),
                style: Style::default(),
            };
            row.put(col, tab, stop - col);
        }
        self.col = stop;
    }

    /// Moves the cursor down a row, scrolling at the bottom of the scroll region, and to the
    /// first column with `carriage_return`.
    fn line_feed(&mut self, carriage_return: bool) {
        self.col = if carriage_return {
            0
        } else {
            self.cursor_col()
        };
        if self.row + 1 == self.region.end {
            self.scroll_up(1);
        } else if self.row + 1 < self.height {
            self.move_to_row(self.row + 1);
        }
    }

    /// Moves the cursor up a row, scrolling at the top of the scroll region.
    fn reverse_index(&mut self) {
        if self.row == self.region.start {
            self.scroll_down(self.row, 1);
        } else if self.row > 0 {
            self.row -= 1;
        }
    }

    /// Moves the cursor to `row`, in the screen.
    fn move_to_row(&mut self, row: usize) {
        self.row = row.min(self.height - 1);
        self.ensure_rows(self.row + 1);
    }

    fn ensure_rows(&mut self, rows: usize) {
        if self.rows.len() < rows {
            self.rows.resize_with(rows, Row::default);
        }
    }

    /// Scrolls the scroll region up by `count` rows, those of the whole main screen scrolling
    /// away.
    fn scroll_up(&mut self, count: usize) {
        if self.region == (0..self.height) && self.main.is_none() {
            self.scroll_away(count);
            return;
        }
        let Range { start, end } = self.region.clone();
        self.ensure_rows(end);
        let blank = self.blank();
        for _ in 0..count.min(end - start) {
            self.rows.remove(start);
            self.rows.insert(end - 1, Row::blank(blank, self.cols));
        }
    }

    /// Scrolls the top `count` rows away, the cursor staying where it is on the screen.
    fn scroll_away(&mut self, count: usize) {
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

    /// Scrolls the rows of the scroll region from `top` down by `count`, the bottom ones going.
    fn scroll_down(&mut self, top: usize, count: usize) {
        let end = self.region.end;
        if top >= end {
            return;
        }
        self.ensure_rows(end);
        let blank = self.blank();
        for _ in 0..count.min(end - top) {
            self.rows.remove(end - 1);
            self.rows.insert(top, Row::blank(blank, self.cols));
        }
        self.rows.truncate(self.height);
        self.move_to_row(self.row);
    }

    fn clear_rows(&mut self, rows: Range<usize>) {
        let blank = self.blank();
        let end = rows.end.min(self.height);
        if blank != BLANK {
            self.ensure_rows(end);
        }
        for row in self
            .rows
            .range_mut(rows.start.min(self.rows.len())..end.min(self.rows.len()))
        {
            *row = Row::blank(blank, self.cols);
        }
    }

    fn save(&mut self) {
        self.saved = Some(Saved {
            row: self.row,
            col: self.col,
            style: self.style,
            charsets: self.charsets,
            shifted: self.shifted,
        });
    }

    fn restore(&mut self) {
        let saved = self.saved.unwrap_or_default();
        self.move_to_row(saved.row);
        self.col = saved.col.min(self.cols);
        self.style = saved.style;
        self.charsets = saved.charsets;
        self.shifted = saved.shifted;
    }

    /// Shows the alternate screen, a blank one, the main one kept.
    fn enter_alternate(&mut self) {
        if self.main.is_some() {
            return;
        }
        let rows = mem::replace(&mut self.rows, VecDeque::from([Row::default()]));
        self.main = Some(Box::new(Main {
            rows,
            row: self.row,
            col: self.col,
        }));
        self.move_to_row(self.row);
    }

    /// Shows the main screen again, the cursor where it was with `cursor`.
    fn leave_alternate(&mut self, cursor: bool) {
        let Some(main) = self.main.take() else {
            return;
        };
        self.rows = main.rows;
        self.rows.truncate(self.height);
        if cursor {
            self.move_to_row(main.row);
            self.col = main.col.min(self.cols);
        } else {
            self.move_to_row(self.row);
        }
    }

    /// Puts everything back as it was at the start, but the rows.
    fn reset(&mut self) {
        self.leave_alternate(false);
        self.style = Style::default();
        self.modes = Modes::default();
        self.region = 0..self.height;
        self.charsets = [false; 2];
        self.shifted = false;
        self.saved = None;
    }

    /// Carries out the escape sequence `ESC c`.
    fn escape_final(&mut self, c: char) {
        match c {
            '7' => self.save(),
            '8' => self.restore(),
            'D' => self.line_feed(false),
            'E' => self.line_feed(true),
            'M' => self.reverse_index(),
            'c' => {
                self.reset();
                self.clear_rows(0..self.height);
                (self.row, self.col) = (0, 0);
            }
            _ => {}
        }
    }

    /// Carries out the string `ESC kind … ST`: a title, or a question about colors.
    fn string_end(&mut self, kind: char) {
        if kind != ']' || !self.terminal {
            return;
        }
        let string = mem::take(&mut self.string);
        let Some((command, text)) = string.split_once(';') else {
            return;
        };
        match (command, text) {
            ("0" | "2", title) => self.title = Some(title.to_owned()),
            ("10" | "11", "?") => {
                let index = usize::from(command == "11");
                if let Some((r, g, b)) = self.palette[index] {
                    let reply = format!(
                        "\x1b]{command};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\"
                    );
                    self.replies.extend_from_slice(reply.as_bytes());
                }
            }
            _ => {}
        }
    }

    /// Answers a question of the program with `reply`, on a terminal.
    fn reply(&mut self, reply: &str) {
        if self.terminal {
            self.replies.extend_from_slice(reply.as_bytes());
        }
    }

    /// Sets the private mode `mode`, or resets it.
    fn set_mode(&mut self, mode: &str, set: bool) {
        match mode {
            "1" => self.modes.app_cursor = set,
            "6" => {
                self.modes.origin = set;
                (self.row, self.col) = (self.region.start, 0);
            }
            "7" => self.modes.autowrap = set,
            "25" => self.modes.show_cursor = set,
            "2004" => self.modes.bracketed_paste = set,
            "2026" => self.modes.synchronized = set,
            "47" | "1047" | "1049" if self.terminal => {
                if mode == "1049" && set {
                    self.save();
                }
                if set {
                    self.enter_alternate();
                    if mode != "47" {
                        self.clear_rows(0..self.height);
                    }
                } else {
                    if mode == "1047" {
                        self.clear_rows(0..self.height);
                    }
                    self.leave_alternate(mode != "1049");
                    if mode == "1049" {
                        self.restore();
                    }
                }
            }
            // A full-screen program's screen goes, once it's done with it. What the screen
            // showed before stays, as it scrolled away.
            "47" | "1047" | "1049" => {
                if set {
                    let used = self.used();
                    self.rows.truncate(used);
                    self.scroll_away(used);
                }
                self.clear_rows(0..self.height);
                (self.row, self.col) = (0, 0);
            }
            _ => {}
        }
    }

    /// The state of the private mode `mode` for `CSI ? mode $ p`: 1 set, 2 reset, 0 unknown.
    fn mode_state(&self, mode: &str) -> u8 {
        let set = match mode {
            "1" => self.modes.app_cursor,
            "6" => self.modes.origin,
            "7" => self.modes.autowrap,
            "25" => self.modes.show_cursor,
            "47" | "1047" | "1049" => self.main.is_some(),
            "2004" => self.modes.bracketed_paste,
            "2026" => self.modes.synchronized,
            _ => return 0,
        };
        if set {
            1
        } else {
            2
        }
    }

    /// Carries out the private control sequence `ESC [ params final`, like `ESC [ ? 25 l`.
    fn private_control(&mut self, final_char: char) {
        let params = mem::take(&mut self.params);
        match (params.split_at(1), final_char) {
            (("?", modes), 'h' | 'l') => {
                for mode in modes.split(';') {
                    self.set_mode(mode, final_char == 'h');
                }
            }
            (("?", mode), 'p') if mode.ends_with('$') => {
                let mode = mode.trim_end_matches('$');
                let state = self.mode_state(mode);
                self.reply(&format!("\x1b[?{mode};{state}$y"));
            }
            (("?", "6"), 'n') => {
                let (row, col) = self.position();
                self.reply(&format!("\x1b[?{row};{col}R"));
            }
            ((">", "" | "0"), 'c') => self.reply("\x1b[>1;10;0c"),
            ((">", "" | "0"), 'q') => self.reply(&format!(
                "\x1bP>|helix {}\x1b\\",
                helix_loader::VERSION_AND_GIT_HASH
            )),
            _ => {}
        }
        self.params = params;
    }

    /// The cursor's row and column for a report, counting from 1, and from the top of the
    /// scroll region in origin mode.
    fn position(&self) -> (usize, usize) {
        let top = if self.modes.origin {
            self.region.start
        } else {
            0
        };
        (self.row - top + 1, self.cursor_col() + 1)
    }

    /// Carries out the control sequence `ESC [ params final`.
    fn control(&mut self, final_char: char) {
        if self.params.starts_with(['<', '=', '>', '?']) {
            self.private_control(final_char);
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
        // Sequences with intermediate bytes, like `ESC [ ! p` or `ESC [ 2 SP q`.
        if self.params.ends_with([' ', '!', '"', '$', '\'']) {
            if self.params == "!" && final_char == 'p' {
                self.reset();
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
        // Rows of the scroll region, when the cursor is in it, bound moves up and down.
        let top = if row >= self.region.start {
            self.region.start
        } else {
            0
        };
        let bottom = if row < self.region.end {
            self.region.end
        } else {
            self.height
        };
        // Rows to move to count from the top of the scroll region in origin mode.
        let origin = if self.modes.origin {
            self.region.clone()
        } else {
            0..self.height
        };
        let blank = self.blank();
        match final_char {
            'A' => (self.row, self.col) = (row.saturating_sub(count).max(top), col),
            'B' => {
                self.move_to_row(row.saturating_add(count).min(bottom - 1));
                self.col = col;
            }
            'C' => self.col = col.saturating_add(count).min(self.cols - 1),
            'D' => self.col = col.saturating_sub(count),
            'E' => {
                self.move_to_row(row.saturating_add(count).min(bottom - 1));
                self.col = 0;
            }
            'F' => (self.row, self.col) = (row.saturating_sub(count).max(top), 0),
            'G' | '`' => self.col = (count - 1).min(self.cols - 1),
            'd' => {
                self.move_to_row((origin.start + count - 1).min(origin.end - 1));
                self.col = col;
            }
            'H' | 'f' => {
                self.move_to_row((origin.start + count - 1).min(origin.end - 1));
                self.col = (second.max(1) - 1).min(self.cols - 1);
            }
            'J' => {
                self.col = col;
                match first {
                    0 => {
                        self.rows[row].truncate(col, blank, self.cols);
                        if blank == BLANK {
                            self.rows.truncate(row + 1);
                        } else {
                            self.clear_rows(row + 1..self.height);
                        }
                    }
                    1 => {
                        self.clear_rows(0..row);
                        self.rows[row].erase(0..col + 1, blank);
                    }
                    3 => {
                        self.cleared = true;
                        self.held = Shown::default();
                        self.scrolled = Shown::default();
                    }
                    _ => self.clear_rows(0..self.height),
                }
            }
            'K' => {
                self.col = col;
                match first {
                    0 => self.rows[row].truncate(col, blank, self.cols),
                    1 => self.rows[row].erase(0..col + 1, blank),
                    _ => self.rows[row].truncate(0, blank, self.cols),
                }
            }
            '@' => {
                self.rows[row].insert(col, count, blank, self.cols);
                self.col = col;
            }
            'P' => {
                let cells = &mut self.rows[row];
                if col < cells.cells.len() {
                    let end = col.saturating_add(count).min(cells.cells.len());
                    cells.split(col);
                    cells.split(end);
                    cells.cells.drain(col..end);
                    if blank != BLANK {
                        cells.cells.resize(self.cols, blank);
                    }
                }
                self.col = col;
            }
            'X' => {
                self.rows[row].erase(col..col.saturating_add(count).min(self.cols), blank);
                self.col = col;
            }
            'L' if self.region.contains(&row) => {
                self.scroll_down(row, count);
                self.col = 0;
            }
            'M' if self.region.contains(&row) => {
                let end = self.region.end;
                self.ensure_rows(end);
                for _ in 0..count.min(end - row) {
                    self.rows.remove(row);
                    self.rows.insert(end - 1, Row::blank(blank, self.cols));
                }
                self.move_to_row(row);
                self.col = 0;
            }
            'S' => self.scroll_up(count),
            'T' => self.scroll_down(self.region.start, count),
            'b' => {
                if let Some(c) = self.last {
                    for _ in 0..count.min(self.cols * self.height) {
                        self.print(c);
                    }
                }
            }
            'I' => {
                for _ in 0..count {
                    self.tab();
                }
            }
            'Z' => self.col = (col.saturating_sub(1) / TAB * TAB).min(col),
            'r' => {
                let end = if second == 0 { self.height } else { second };
                let start = first.max(1) - 1;
                if start + 1 < end.min(self.height) {
                    self.region = start..end.min(self.height);
                    let row = if self.modes.origin { start } else { 0 };
                    self.move_to_row(row);
                    self.col = 0;
                }
            }
            'h' | 'l' if self.params == "4" => self.modes.insert = final_char == 'h',
            'c' if first == 0 => self.reply("\x1b[?62;22c"),
            'n' if first == 5 => self.reply("\x1b[0n"),
            'n' if first == 6 => {
                let (row, col) = self.position();
                self.reply(&format!("\x1b[{row};{col}R"));
            }
            't' if first == 18 => {
                let reply = format!("\x1b[8;{};{}t", self.height, self.cols);
                self.reply(&reply);
            }
            's' if self.params.is_empty() => self.save(),
            'u' if self.params.is_empty() => self.restore(),
            _ => {}
        }
    }
}

/// `c` in the DEC line drawing charset.
fn line_drawing(c: char) -> char {
    const CHARS: &str = "◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
    match c {
        '`'..='~' => CHARS.chars().nth(c as usize - '`' as usize).unwrap_or(c),
        _ => c,
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

    fn plain(text: &str) -> Shown {
        Shown {
            text: text.to_owned(),
            chars: text.chars().count(),
            styles: Vec::new(),
        }
    }

    #[test]
    fn lines_alike_and_split_off() {
        let red = Style::default().fg(Color::Indexed(1));
        let mut old = plain("one\ntwö\nthree");
        old.styles = vec![(4..7, red)];
        let mut new = plain("one\ntwö\nthree\nfour");
        assert_eq!(old.common_lines(&new), (4, 4));
        new.styles = vec![(4..7, red)];
        // The last line has no line break yet, so it may still change.
        assert_eq!(old.common_lines(&new), (8, 9));
        new.push('!', red);
        assert_eq!(new.styles, [(4..7, red), (18..19, red)]);
        let rest = new.split_off(8, 9);
        assert_eq!((new.text.as_str(), new.chars), ("one\ntwö\n", 8));
        assert_eq!((rest.text.as_str(), rest.chars), ("three\nfour!", 11));
        assert_eq!(rest.styles, [(10..11, red)]);
        new.append(&rest);
        assert_eq!(new.styles, [(4..7, red), (18..19, red)]);
    }

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

    /// A terminal `cols` wide and `height` high after `output`.
    fn terminal(cols: u16, height: u16, output: &str) -> Screen {
        let mut screen = Screen::terminal(cols, height, [None; 2]);
        screen.push(output.as_bytes());
        screen
    }

    fn replies(screen: &mut Screen) -> String {
        String::from_utf8(screen.take_replies()).unwrap()
    }

    #[test]
    fn terminals_show_every_row() {
        assert_eq!(terminal(10, 3, "a\r\nb").shown().text, "a\nb\n");
        // Output processing turns `\n` into `\r\n`, so a lone one keeps the column.
        assert_eq!(terminal(10, 3, "ab\ncd").shown().text, "ab\n  cd\n");
        // Wrapped rows stay rows, but scroll away as one line.
        let mut screen = terminal(4, 2, "abcdefgh\r\nx");
        assert_eq!(screen.shown().text, "abcd\nefgh\nx");
        assert_eq!(screen.top(), 5);
        screen.push(b"\r\ny");
        assert_eq!(screen.take_scrolled().text, "abcdefgh\n");
        assert_eq!((screen.shown().text.as_str(), screen.top()), ("x\ny", 0));
        // Tabs move over cells, which show as spaces.
        assert_eq!(terminal(20, 1, "a\tb").shown().text, "a       b");
    }

    #[test]
    fn the_cursor_is_where_the_program_put_it() {
        assert_eq!(terminal(10, 3, "$ ").cursor(), 2);
        let screen = terminal(10, 3, "ab\r\n\x1b[5C");
        assert_eq!(screen.shown().text, "ab\n     \n");
        assert_eq!(screen.cursor(), 8);
        // Wide chars are one char over two columns.
        assert_eq!(terminal(10, 2, "日本x").cursor(), 3);
        let screen = terminal(4, 2, "abcdefgh\r\nx");
        assert_eq!(screen.cursor(), 11);
    }

    #[test]
    fn scroll_regions_scroll_alone() {
        // As tmux keeps its status line below the region its window scrolls in.
        let screen = terminal(10, 4, "\x1b[1;3r\x1b[4;1Hstatus\x1b[1;1Ha\r\nb\r\nc\r\nd");
        assert_eq!(screen.shown().text, "b\nc\nd\nstatus");
        assert_eq!(screen.scrolled_len(), 0);
        let shown = |output: &str| terminal(10, 4, output).shown().text;
        assert_eq!(
            shown("\x1b[1;3r\x1b[4;1Hstatus\x1b[1;1Ha\r\nb\x1b[1;1H\x1bM"),
            "\na\nb\nstatus"
        );
        assert_eq!(
            shown("1\r\n2\r\n3\r\n4\x1b[1;3r\x1b[1;1H\x1b[M"),
            "2\n3\n\n4"
        );
        assert_eq!(
            shown("1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[2;1H\x1b[2L"),
            "1\n\n\n4"
        );
        assert_eq!(shown("\x1b[2;3r\x1b[?6h\x1b[1;1Hx"), "\nx\n\n");
        assert_eq!(shown("1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[S"), "1\n3\n\n4");
    }

    #[test]
    fn full_screen_programs_draw_on_the_alternate_screen() {
        let mut screen = terminal(10, 3, "$ vim\r\n\x1b[?1049h\x1b[H\x1b[2Jfull");
        assert_eq!(screen.shown().text, "full\n\n");
        screen.push(b"\x1b[?1049l");
        assert_eq!(screen.shown().text, "$ vim\n\n");
        assert_eq!(screen.cursor(), 6);
        // Its rows don't scroll away.
        let mut screen = terminal(10, 3, "\x1b[?1049h1\r\n2\r\n3\r\n4");
        assert_eq!(screen.shown().text, "2\n3\n4");
        assert_eq!(screen.scrolled_len(), 0);
        // Ended, the program leaves the main screen.
        assert_eq!(screen.finish().text, "");
    }

    #[test]
    fn programs_get_answers() {
        let mut screen = terminal(80, 24, "ab\x1b[c\x1b[>c\x1b[6n\x1b[?2026$p\x1b[?u\x1b[18t");
        assert_eq!(
            replies(&mut screen),
            "\x1b[?62;22c\x1b[>1;10;0c\x1b[1;3R\x1b[?2026;2$y\x1b[8;24;80t"
        );
        screen.push(b"\x1b[?2026h\x1b[?2026$p\x1b[?99$p\x1b[5n");
        assert_eq!(replies(&mut screen), "\x1b[?2026;1$y\x1b[?99;0$y\x1b[0n");
        // Colors are told when they are known.
        let mut screen = Screen::terminal(80, 24, [Some((255, 0, 16)), None]);
        screen.push(b"\x1b]10;?\x1b\\\x1b]11;?\x07");
        assert_eq!(replies(&mut screen), "\x1b]10;rgb:ffff/0000/1010\x1b\\");
        // Output has no one to answer.
        let mut output = Screen::new(80, 24, true);
        output.push(b"\x1b[c");
        assert!(output.take_replies().is_empty());
    }

    #[test]
    fn modes_titles_and_charsets() {
        let screen = terminal(80, 24, "\x1b[?1h\x1b[?2004h\x1b[?25l\x1b]0;fish\x07");
        let modes = screen.modes();
        assert!(modes.app_cursor && modes.bracketed_paste && !modes.show_cursor);
        assert_eq!(screen.title(), Some("fish"));
        let shown = |cols, output: &str| terminal(cols, 1, output).shown().text;
        assert_eq!(shown(10, "\x1b(0lqk\x1b(Bx"), "┌─┐x");
        assert_eq!(shown(10, "\x1b)0\x0eq\x0fq"), "─q");
        assert_eq!(shown(10, "abc\r\x1b[4hX\x1b[4l"), "Xabc");
        assert_eq!(shown(4, "\x1b[?7labcdef"), "abcf");
        assert_eq!(shown(10, "x\x1b[3b"), "xxxx");
        // Erasing fills with the background color, as tmux's status line does.
        let screen = terminal(6, 1, "\x1b[42m\x1b[6X ok");
        assert_eq!(screen.shown().text, " ok   ");
        let green = Style::default().bg(Color::Indexed(2));
        assert_eq!(screen.shown().styles, [(0..6, green)]);
        // Clearing what scrolled away is noted.
        let mut screen = terminal(10, 2, "1\r\n2\r\n3\x1b[3J");
        assert!(screen.take_cleared());
        assert_eq!(screen.scrolled_len(), 0);
        assert!(!screen.take_cleared());
    }

    /// Times the screen on lines of output, and on progress redrawn, in release:
    /// `cargo test --release -p helix-term --lib ui::screen::tests::measure -- --ignored --nocapture`
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
