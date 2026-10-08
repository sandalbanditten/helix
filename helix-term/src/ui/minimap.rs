//! The minimap of a split: its document in braille beside the text, a dot for every four columns
//! of a line, colored like the text. The lines on screen are shaded, changes are marked in a
//! column of their own, and diagnostics and the matches of a search tint their dots.

use std::{borrow::Cow, collections::HashMap, ops::Range};

use helix_core::{diagnostic::Severity, graphemes::Grapheme, syntax::Loader};
use helix_stdx::rope::RopeSliceExt;
use helix_view::{
    annotations::diagnostics::DiagnosticFilter,
    graphics::{Color, Rect, Style, UnderlineStyle},
    Document, DocumentId, Editor, Theme, ViewId,
};
use tui::buffer::Buffer as Surface;

use crate::ui::{
    document::{SyntaxHighlighter, SyntaxHighlighting},
    overview::{self, Change, Mark, Overview},
    scrollbar,
};

/// The text columns a dot stands for. A cell is two dots wide and four lines tall.
const DOT_COLUMNS: usize = 4;
const CELL_LINES: usize = 4;

/// The braille dot of each row of a cell's lines, in its left and its right column.
const DOTS: [[u8; 2]; CELL_LINES] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// A cell: its dots and the color most of their text has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    dots: u8,
    color: Color,
}

impl Cell {
    fn symbol(self) -> char {
        char::from_u32(0x2800 + u32::from(self.dots)).expect("braille patterns are chars")
    }
}

/// What the cells of a document depend on besides its text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    version: i32,
    language: Option<String>,
    theme: String,
    tab_width: usize,
    width: usize,
}

/// The rows of cells of a document worked out so far, each standing for four lines.
struct Cells {
    key: Key,
    rows: HashMap<usize, Box<[Cell]>>,
}

/// The minimaps of the splits: the cells of their documents, worked out as they are shown and
/// kept until the documents change, and the maps drawn last.
#[derive(Default)]
pub struct Minimaps {
    docs: HashMap<DocumentId, Cells>,
    drawn: Vec<Drawn>,
}

/// A minimap as drawn, for the mouse.
#[derive(Debug, Clone)]
pub struct Drawn {
    pub view: ViewId,
    pub area: Rect,
    /// The rows of cells drawn, from the top of `area`.
    rows: Range<usize>,
    /// The document lines on screen, of how many.
    pub shown: Range<usize>,
    pub len: usize,
}

impl Drawn {
    /// The screen rows of the cells.
    pub fn cell_rows(&self) -> Range<u16> {
        self.area.y..self.area.y + self.rows.len() as u16
    }

    /// The screen rows of the cells of the lines on screen.
    pub fn shaded_rows(&self) -> Range<u16> {
        let rows = self.rows.start.max(self.shown.start / CELL_LINES)
            ..self.rows.end.min(self.shown.end.div_ceil(CELL_LINES));
        let top = |row: usize| self.area.y + (row - self.rows.start) as u16;
        top(rows.start)..top(rows.end.max(rows.start))
    }

    /// The first document line of the cells on the screen row `row`.
    pub fn line_at(&self, row: u16) -> usize {
        (self.rows.start + usize::from(row.saturating_sub(self.area.y))) * CELL_LINES
    }
}

/// The theme styles of a minimap, resolved once per frame.
struct Styles {
    base: Style,
    text: Color,
    /// The background of the lines on screen.
    viewport: Option<Color>,
    marks: overview::Colors,
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        let base = theme
            .try_get_exact("ui.minimap")
            .unwrap_or_else(|| theme.get("ui.background"));
        let viewport = theme
            .try_get_exact("ui.minimap.viewport")
            .and_then(|style| style.bg)
            .or_else(|| theme.get("ui.cursorline.primary").bg)
            .or_else(|| theme.get("ui.selection").bg);
        Self {
            base,
            text: theme.get("ui.text").fg.unwrap_or(Color::Reset),
            viewport,
            marks: overview::Colors::new(theme, &["ui.minimap.search", "ui.scrollbar.search"]),
        }
    }
}

impl Minimaps {
    /// Forgets the maps drawn and the documents no longer open, before a frame.
    pub fn start_frame(&mut self, editor: &Editor) {
        self.docs
            .retain(|doc, _| editor.documents.contains_key(doc));
        self.drawn.clear();
    }

    /// The map drawn last at the screen cell `column`, `row`.
    pub fn at(&self, column: u16, row: u16) -> Option<&Drawn> {
        self.drawn
            .iter()
            .find(|map| scrollbar::contains(map.area, column, row))
    }

    /// The map of `view` drawn last.
    pub fn of(&self, view: ViewId) -> Option<&Drawn> {
        self.drawn.iter().find(|map| map.view == view)
    }

    /// Draws the minimap of `view` showing `doc` into `area`, the document lines `shown` being on
    /// screen and the one of the cursor `cursor_line`.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        area: Rect,
        surface: &mut Surface,
        view: ViewId,
        doc: &Document,
        shown: Range<usize>,
        cursor_line: usize,
        overview: &Overview,
        theme: &Theme,
        loader: &Loader,
    ) {
        let styles = Styles::new(theme);
        surface.set_style(area, styles.base);
        // The column of changes, then the cells.
        let width = usize::from(area.width.saturating_sub(1));
        if width == 0 {
            return;
        }
        let text = doc.text().slice(..);
        let len = text.len_lines();
        let groups = len.div_ceil(CELL_LINES);
        let height = usize::from(area.height);
        let first = first_row(groups, height, len, &shown);
        let rows = first..(first + height).min(groups);
        self.drawn.push(Drawn {
            view,
            area,
            rows: rows.clone(),
            shown: shown.clone(),
            len,
        });

        let key = Key {
            version: doc.version(),
            language: doc.language_name().map(ToOwned::to_owned),
            theme: theme.name().to_owned(),
            tab_width: doc.tab_width(),
            width,
        };
        let cells = self.docs.entry(doc.id()).or_insert_with(|| Cells {
            key: key.clone(),
            rows: HashMap::new(),
        });
        if cells.key != key {
            *cells = Cells {
                key,
                rows: HashMap::new(),
            };
        }
        cells.fill(rows.clone(), doc, styles.text, theme, loader);

        // The marks of each row: changes in the column, the others on the dots.
        let mut changes = vec![None; rows.len()];
        let mut tints = vec![None; rows.len()];
        let filter = overview::Filter {
            diagnostics: DiagnosticFilter::Enable(Severity::Hint),
            changes: true,
            search: true,
        };
        overview.marks(doc, filter, |lines, mark| {
            let groups = lines.start / CELL_LINES..(lines.end - 1) / CELL_LINES + 1;
            let groups = groups.start.max(rows.start)..groups.end.min(rows.end);
            for group in groups {
                let row = group - rows.start;
                match mark {
                    Mark::Change(change) => changes[row] = changes[row].max(Some(change)),
                    _ => tints[row] = tints[row].max(Some(mark)),
                }
            }
        });

        let cursor_group = cursor_line / CELL_LINES;
        for (i, group) in rows.enumerate() {
            let y = area.y + i as u16;
            if let Some(change) = changes[i] {
                let color = styles.marks.of(Mark::Change(change));
                surface[(area.x, y)]
                    .set_symbol(change_symbol(change))
                    .set_style(styles.base.fg(color));
            }
            let mut style = styles.base;
            let lines = group * CELL_LINES..(group + 1) * CELL_LINES;
            if let Some(viewport) = styles.viewport.filter(|_| overlaps(&lines, &shown)) {
                style = style.bg(viewport);
            }
            if group == cursor_group {
                style = style.underline_style(UnderlineStyle::Line);
            }
            let tint = tints[i].map(|mark| styles.marks.of(mark));
            let row = cells.rows.get(&group).map_or(&[][..], |row| &row[..]);
            for (j, x) in (area.x + 1..area.right()).enumerate() {
                let cell = row.get(j).copied().unwrap_or(Cell {
                    dots: 0,
                    color: styles.text,
                });
                surface[(x, y)]
                    .set_char(cell.symbol())
                    .set_style(style.fg(tint.unwrap_or(cell.color)));
            }
        }
    }
}

impl Cells {
    /// Works out the rows among `rows` not known yet, in one pass of the highlighter.
    fn fill(
        &mut self,
        rows: Range<usize>,
        doc: &Document,
        text_color: Color,
        theme: &Theme,
        loader: &Loader,
    ) {
        let mut missing = rows.filter(|row| !self.rows.contains_key(row));
        let Some(start) = missing.next() else {
            return;
        };
        let end = missing.next_back().unwrap_or(start) + 1;
        let width = self.key.width;
        let tab_width = self.key.tab_width as u16;
        let computed = cells(doc, start..end, width, tab_width, text_color, theme, loader);
        self.rows.extend((start..end).zip(computed));
    }
}

/// The cells of the rows `rows` of `doc`, `width` cells each.
fn cells(
    doc: &Document,
    rows: Range<usize>,
    width: usize,
    tab_width: u16,
    text_color: Color,
    theme: &Theme,
    loader: &Loader,
) -> Vec<Box<[Cell]>> {
    let text = doc.text().slice(..);
    let lines = rows.start * CELL_LINES..(rows.end * CELL_LINES).min(text.len_lines());
    let (start, end) = (text.line_to_char(lines.start), text.line_to_char(lines.end));
    let highlighting = doc.syntax().map(|syntax| SyntaxHighlighting {
        syntax,
        loader,
        range: text.char_to_byte(start) as u32..text.char_to_byte(end) as u32,
    });
    let text_style = Style::default().fg(text_color);
    let mut highlighter = SyntaxHighlighter::new(highlighting, text, theme, text_style);
    let columns = width * 2 * DOT_COLUMNS;

    let mut dots = vec![vec![0u8; width]; rows.len()];
    // The columns of each color in each cell.
    let mut colors: Vec<Vec<Vec<(Color, usize)>>> = vec![vec![Vec::new(); width]; rows.len()];
    for line in lines {
        let row = line / CELL_LINES - rows.start;
        let line_dots = DOTS[line % CELL_LINES];
        let mut char_idx = text.line_to_char(line);
        let mut column = 0;
        for slice in text.line(line).graphemes() {
            let grapheme = Grapheme::new(Cow::from(slice).into(), column, tab_width);
            if matches!(grapheme, Grapheme::Newline) || column >= columns {
                break;
            }
            let grapheme_width = grapheme.width();
            if !grapheme.is_whitespace() {
                while char_idx >= highlighter.pos {
                    highlighter.advance();
                }
                let color = highlighter.style.fg.unwrap_or(text_color);
                for column in column..(column + grapheme_width).min(columns) {
                    let dot = column / DOT_COLUMNS;
                    let cell = dot / 2;
                    dots[row][cell] |= line_dots[dot % 2];
                    let counts = &mut colors[row][cell];
                    match counts.iter_mut().find(|(known, _)| *known == color) {
                        Some((_, count)) => *count += 1,
                        None => counts.push((color, 1)),
                    }
                }
            }
            column += grapheme_width;
            char_idx += slice.len_chars();
        }
    }
    dots.into_iter()
        .zip(colors)
        .map(|(dots, colors)| {
            dots.into_iter()
                .zip(colors)
                .map(|(dots, counts)| Cell {
                    dots,
                    color: counts
                        .iter()
                        .max_by_key(|(_, count)| *count)
                        .map_or(text_color, |&(color, _)| color),
                })
                .collect()
        })
        .collect()
}

/// The first row of a map `height` rows tall showing rows of cells for `groups` four lines
/// each. When they don't fit, the map slides along with the lines `shown` of `len`: from its
/// first rows at the start of the document to its last rows at the end.
fn first_row(groups: usize, height: usize, len: usize, shown: &Range<usize>) -> usize {
    let hidden = groups.saturating_sub(height);
    let scrollable = len.saturating_sub(shown.len()).max(1);
    hidden * shown.start.min(scrollable) / scrollable
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// The symbol of a change in the column of changes, like the diff gutter's.
fn change_symbol(change: Change) -> &'static str {
    match change {
        Change::Added | Change::Modified => "▍",
        Change::Deleted => "▔",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_map_slides_with_the_lines_shown() {
        // 400 lines in 100 rows of cells, on a map of 25 rows: 75 rows hidden.
        assert_eq!(first_row(100, 25, 400, &(0..40)), 0);
        assert_eq!(first_row(100, 25, 400, &(180..220)), 37);
        assert_eq!(first_row(100, 25, 400, &(360..400)), 75);
        // Scrolled past the end, the map stays at its end.
        assert_eq!(first_row(100, 25, 400, &(390..400)), 75);
        // A short document fits.
        assert_eq!(first_row(10, 25, 40, &(0..40)), 0);
    }

    #[test]
    fn cells_show_text_as_dots() {
        use helix_core::unicode::width::UnicodeWidthChar;

        let cell = |dots| Cell {
            dots,
            color: Color::Reset,
        };
        assert_eq!(cell(0).symbol(), '⠀');
        assert_eq!(cell(0xff).symbol(), '⣿');
        assert_eq!(cell(DOTS[0][0] | DOTS[3][1]).symbol(), '⢁');
        assert!('⣿'.width() == Some(1));
    }
}
