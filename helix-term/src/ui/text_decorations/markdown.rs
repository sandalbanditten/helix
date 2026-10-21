//! Rendered Markdown drawn over the text, like the text of checked tasks dimmed and struck
//! through, and between its lines, like the borders of tables.

use std::ops::Range;

use helix_core::conceal::{ConcealDecoration, DecorationStyle, LineRows, RowStyle, VirtualRow};
use helix_core::doc_formatter::FormattedGrapheme;
use helix_core::graphemes::grapheme_width;
use helix_core::line_ending::line_end_char_index;
use helix_core::unicode::segmentation::UnicodeSegmentation;
use helix_core::{Position, RopeSlice};
use helix_view::graphics::{Modifier, Rect, Style};
use helix_view::Theme;

use crate::ui::document::{LinePos, TextRenderer};
use crate::ui::text_decorations::Decoration;

/// The text of checked tasks on the lines drawn.
pub struct Markdown {
    /// Sorted.
    done: Vec<Range<usize>>,
    /// The index in `done` of the next text the renderer reaches.
    next: usize,
}

impl Markdown {
    pub fn new(decorations: &[(usize, ConcealDecoration)]) -> Self {
        let mut done: Vec<_> = decorations
            .iter()
            .filter(|(_, decoration)| decoration.style == DecorationStyle::Done)
            .map(|(_, decoration)| decoration.range.clone())
            .collect();
        done.sort_unstable_by_key(|range| range.start);
        Self { done, next: 0 }
    }
}

impl Decoration for Markdown {
    fn reset_pos(&mut self, pos: usize) -> usize {
        self.next = self.done.partition_point(|range| range.end <= pos);
        self.done
            .get(self.next)
            .map_or(usize::MAX, |range| range.start.max(pos))
    }

    fn decorate_grapheme(
        &mut self,
        renderer: &mut TextRenderer,
        grapheme: &FormattedGrapheme,
    ) -> usize {
        let char_idx = grapheme.char_idx;
        while self
            .done
            .get(self.next)
            .is_some_and(|range| range.end <= char_idx)
        {
            self.next += 1;
        }
        let Some(range) = self.done.get(self.next) else {
            return usize::MAX;
        };
        if char_idx < range.start {
            return range.start;
        }
        // virtual text is not the task's, and the grapheme after it comes at the same char
        if grapheme.is_virtual() {
            return char_idx;
        }
        let (col, width) = (grapheme.visual_pos.col, grapheme.width());
        if width > 0 && renderer.column_in_bounds(col, width) {
            let x = renderer.viewport.x + (col - renderer.offset.col) as u16;
            let cell = Rect::new(x, grapheme.visual_pos.row as u16, width as u16, 1);
            let done = Style::default().add_modifier(Modifier::DIM | Modifier::CROSSED_OUT);
            renderer.set_style(cell, done);
        }
        char_idx + grapheme.doc_chars()
    }
}

/// The virtual rows of rendered Markdown, like the borders of tables, on the lines drawn.
pub struct MarkdownRows<'a> {
    /// The lines with rows and their rows, sorted.
    rows: Vec<(usize, LineRows)>,
    text: RopeSlice<'a>,
    /// The styles rows are drawn in: like the indent guides, or like the text.
    guide: Style,
    plain: Style,
    theme: &'a Theme,
    /// The syntax styles of the chars of the text that rows show, sorted.
    styles: Vec<(usize, Style)>,
    /// Whether lines wrap, so a line may end after several visual lines.
    wraps: bool,
    /// The first visual row not drawn yet.
    next_row: u16,
    /// While lines wrap: whether the visual line drawn last ends its line, and the line whose
    /// end comes next.
    line_ends: bool,
    next_line: usize,
}

impl<'a> MarkdownRows<'a> {
    pub fn new(
        rows: Vec<(usize, LineRows)>,
        text: RopeSlice<'a>,
        guide: Style,
        plain: Style,
        theme: &'a Theme,
        styles: Vec<(usize, Style)>,
        wraps: bool,
    ) -> Self {
        Self {
            rows,
            text,
            guide,
            plain,
            theme,
            styles,
            wraps,
            next_row: 0,
            line_ends: false,
            next_line: 0,
        }
    }

    fn line_rows(&self, line: usize) -> Option<&LineRows> {
        let i = self
            .rows
            .binary_search_by_key(&line, |(line, _)| *line)
            .ok()?;
        Some(&self.rows[i].1)
    }

    /// Draws `rows` from the visual row `start` on.
    fn draw(&mut self, renderer: &mut TextRenderer, rows: &[VirtualRow], start: u16) {
        for (i, row) in rows.iter().enumerate() {
            let y = start + i as u16;
            let mut col = 0;
            for (text, style) in &row.parts {
                let style = match style {
                    RowStyle::Guide => self.guide,
                    RowStyle::Scope(scope) => self.plain.patch(self.theme.get(scope)),
                    RowStyle::Text { char_idx } => {
                        let i = self.styles.partition_point(|&(other, _)| other < *char_idx);
                        match self.styles.get(i) {
                            Some(&(other, style)) if other == *char_idx => style,
                            _ => self.plain,
                        }
                    }
                };
                for grapheme in text.graphemes(true) {
                    let width = grapheme_width(grapheme);
                    if renderer.column_in_bounds(col, width) {
                        let x = renderer.viewport.x + (col - renderer.offset.col) as u16;
                        renderer.set_string(x, y, grapheme, style);
                    }
                    col += width;
                }
            }
        }
        self.next_row = self.next_row.max(start + rows.len() as u16);
    }

    /// Asks for the grapheme where `line` ends, while lines wrap.
    fn hook_line_end(&mut self, line: usize) -> usize {
        self.next_line = line;
        if !self.wraps || line >= self.text.len_lines() {
            usize::MAX
        } else if line + 1 < self.text.len_lines() {
            line_end_char_index(&self.text, line)
        } else {
            self.text.len_chars()
        }
    }
}

impl Decoration for MarkdownRows<'_> {
    fn reset_pos(&mut self, pos: usize) -> usize {
        self.line_ends = false;
        self.hook_line_end(self.text.char_to_line(pos.min(self.text.len_chars())))
    }

    fn decorate_grapheme(&mut self, _: &mut TextRenderer, _: &FormattedGrapheme) -> usize {
        self.line_ends = true;
        self.hook_line_end(self.next_line + 1)
    }

    fn decorate_line(&mut self, renderer: &mut TextRenderer, pos: LinePos) {
        // Rows skipped since the last line are the last rows of the line before, whose end is
        // above the view, or the rows above the first line.
        let skipped = pos.visual_line.saturating_sub(self.next_row) as usize;
        if skipped > 0 {
            let rows = match pos.doc_line.checked_sub(1) {
                Some(line) => self.line_rows(line).map(|rows| rows.below.clone()),
                None => self.line_rows(0).map(|rows| rows.above.clone()),
            };
            if let Some(rows) = rows {
                let shown = &rows[rows.len().saturating_sub(skipped)..];
                let start = pos.visual_line - shown.len() as u16;
                self.draw(renderer, shown, start);
            }
        }
        self.next_row = self.next_row.max(pos.visual_line + 1);
    }

    fn render_virt_lines_above(&mut self, renderer: &mut TextRenderer, first_line: LinePos) {
        if let Some(rows) = self.line_rows(0).map(|rows| rows.above.clone()) {
            let skipped = first_line.visual_line.saturating_sub(self.next_row) as usize;
            let shown = &rows[rows.len().saturating_sub(skipped)..];
            let start = first_line.visual_line.saturating_sub(shown.len() as u16);
            self.draw(renderer, shown, start);
        }
    }

    fn render_virt_lines(
        &mut self,
        renderer: &mut TextRenderer,
        pos: LinePos,
        virt_off: Position,
    ) -> Position {
        // Where a line wraps, its rows come after its last visual line.
        let line_ends = std::mem::take(&mut self.line_ends) || !self.wraps;
        let rows = self.line_rows(pos.doc_line).map(|rows| rows.below.clone());
        let Some(rows) = rows.filter(|rows| line_ends && !rows.is_empty()) else {
            return Position::new(0, 0);
        };
        let start = pos.visual_line + virt_off.row as u16;
        self.draw(renderer, &rows, start);
        Position::new(rows.len(), 0)
    }
}
