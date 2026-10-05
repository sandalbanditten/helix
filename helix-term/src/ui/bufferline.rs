//! The bufferline: a tab for each buffer, scrolled sideways to keep the focused buffer's tab in
//! view.

use std::{borrow::Cow, ops::Range};

use helix_core::unicode::width::UnicodeWidthStr;
use helix_view::{graphics::Rect, Document, Editor};
use tui::{
    buffer::Buffer as Surface,
    text::{Span, Spans, Text},
    widgets::{Paragraph, Widget},
};

/// Covers the bar's first or last cell while more tabs lie beyond that edge.
const MORE: &str = "…";

#[derive(Default)]
pub(crate) struct Bufferline {
    /// Columns of tabs scrolled out on the left.
    offset: usize,
}

impl Bufferline {
    pub fn render(&mut self, editor: &Editor, area: Rect, surface: &mut Surface) {
        if area.width == 0 {
            return;
        }
        let theme = &editor.theme;
        surface.clear_with(
            area,
            theme
                .try_get("ui.bufferline.background")
                .unwrap_or_else(|| theme.get("ui.statusline")),
        );
        let active_style = theme
            .try_get("ui.bufferline.active")
            .unwrap_or_else(|| theme.get("ui.statusline.active"));
        let inactive_style = theme
            .try_get("ui.bufferline")
            .unwrap_or_else(|| theme.get("ui.statusline.inactive"));

        // the columns of each buffer's tab
        let current_doc = view!(editor).doc;
        let mut tabs = Vec::with_capacity(editor.documents.len());
        let mut active = 0..0;
        let mut total = 0;
        for doc in editor.documents() {
            let tab = total..total + label_width(doc);
            total = tab.end;
            if doc.id() == current_doc {
                active = tab.clone();
            }
            tabs.push(tab);
        }
        let width = area.width as usize;
        self.offset = scroll(self.offset, active, total, width, editor.config().scrolloff);
        let shown = self.offset;

        // only the tabs in view are labelled and drawn
        let first = tabs.partition_point(|tab| tab.end <= shown);
        let spans: Vec<_> = editor
            .documents()
            .zip(&tabs)
            .skip(first)
            .take_while(|(_, tab)| tab.start < shown + width)
            .map(|(doc, _)| {
                let style = if doc.id() == current_doc {
                    active_style
                } else {
                    inactive_style
                };
                Span::styled(label(doc), style)
            })
            .collect();
        let scrolled_in = tabs.get(first).map_or(0, |tab| shown - tab.start);
        Paragraph::new(&Text::from(Spans::from(spans)))
            .scroll((0, scrolled_in as u16))
            .render(area, surface);

        let more_style = theme
            .try_get("ui.bufferline.marker")
            .unwrap_or_else(|| theme.get("ui.statusline.inactive"));
        if shown > 0 {
            surface.set_stringn(area.left(), area.top(), MORE, 1, more_style);
        }
        if shown + width < total {
            surface.set_stringn(area.right() - 1, area.top(), MORE, 1, more_style);
        }
    }
}

/// A buffer's file name, or its name if it has no path, like `[scratch]`.
fn name(doc: &Document) -> Cow<'_, str> {
    doc.path()
        .and_then(|path| path.file_name())
        .map_or_else(|| doc.display_name(), |name| name.to_string_lossy())
}

const MODIFIED: &str = "[+]";

fn label(doc: &Document) -> String {
    let modified = if doc.is_modified() { MODIFIED } else { "" };
    format!(" {}{modified} ", name(doc))
}

/// The width of `label(doc)`, without building it.
fn label_width(doc: &Document) -> usize {
    let modified = if doc.is_modified() {
        MODIFIED.width()
    } else {
        0
    };
    name(doc).width() + modified + 2
}

/// The offset that moves the least from `offset` and shows the `active` columns of tabs `total`
/// columns wide in a bar `width` columns wide. Like a view scrolling sideways, it keeps `scrolloff`
/// columns of the tabs beside the active one in view, and at least the column under a `…`.
fn scroll(
    offset: usize,
    active: Range<usize>,
    total: usize,
    width: usize,
    scrolloff: usize,
) -> usize {
    if total <= width {
        return 0;
    }
    let room = width.saturating_sub(active.len());
    let margin = |more: bool| {
        if more {
            scrolloff.min(room / 2).max(1)
        } else {
            0
        }
    };
    let left = margin(active.start > 0);
    let right = margin(active.end < total);
    // when the tab doesn't fit with its margins, its start wins
    offset
        .max((active.end + right).saturating_sub(width))
        .min(active.start.saturating_sub(left))
        .min(total - width)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn scroll_to_active_tab() {
        // everything fits
        assert_eq!(scroll(5, 10..20, 50, 60, 7), 0);
        // the first and last tabs stick to their edges
        assert_eq!(scroll(30, 0..10, 100, 40, 7), 0);
        assert_eq!(scroll(0, 88..100, 100, 40, 7), 60);
        // right and left, keeping `scrolloff` columns of the next tab in view
        assert_eq!(scroll(0, 50..60, 100, 40, 7), 27);
        assert_eq!(scroll(50, 30..40, 100, 40, 7), 23);
        // a tab already in view keeps the bar still
        assert_eq!(scroll(20, 40..50, 100, 40, 7), 20);
    }

    #[test]
    fn scroll_margins() {
        // the margins share the room beside the tab
        assert_eq!(scroll(0, 30..44, 100, 20, 7), 27);
        // the column under a `…` is never the active tab's
        assert_eq!(scroll(0, 50..60, 100, 40, 0), 21);
        assert_eq!(scroll(60, 50..60, 100, 40, 0), 49);
        // a tab wider than the bar shows its start, after the `…`
        assert_eq!(scroll(0, 30..45, 100, 10, 7), 29);
    }

    #[test]
    fn scroll_back_when_tabs_close() {
        // the tabs on the right closed: the bar ends at the last tab
        assert_eq!(scroll(60, 65..75, 80, 40, 7), 40);
    }
}
