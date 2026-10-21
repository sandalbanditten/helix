//! Rendered Markdown drawn over the text: the text of checked tasks, dimmed and struck through.

use std::ops::Range;

use helix_core::conceal::{ConcealDecoration, DecorationStyle};
use helix_core::doc_formatter::FormattedGrapheme;
use helix_view::graphics::{Modifier, Rect, Style};

use crate::ui::document::TextRenderer;
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
