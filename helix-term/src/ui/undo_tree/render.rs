//! Drawing the undo tree: a row per revision reading `[cursor][graph] number age mark change`,
//! rows joining lanes between them, and the rail between the panel and the editor.

use std::time::SystemTime;

use helix_view::{
    graphics::{Color, Modifier, Rect, Style},
    Theme,
};
use tui::buffer::Buffer as Surface;

use super::{
    graph::NODE,
    rows::{age, Rows, Snippet},
};
use crate::ui::{
    dock::{self, Side},
    scrollbar_thumb,
};

/// The theme styles of the panel, resolved once per frame.
pub struct Styles {
    pub(super) base: Style,
    selected: Style,
    /// The color of the cursor's `>` mark.
    mark: Style,
    pub(super) guide: Style,
    /// The node of the revision browsing started from, or of the current one.
    current: Style,
    /// Revision numbers and ages.
    revision: Style,
    saved: Style,
    deleted: Style,
    matched: Style,
    pub(super) track: Style,
    pub(super) thumb: Style,
}

impl Styles {
    pub fn new(theme: &Theme) -> Self {
        // The scopes of the undo tree fall back to the file tree's, then to the editor's.
        let scope = |scopes: &[&str], fallbacks: &[&str]| {
            scopes
                .iter()
                .find_map(|scope| theme.try_get_exact(scope))
                .or_else(|| {
                    fallbacks
                        .iter()
                        .find_map(|fallback| theme.try_get(fallback))
                })
                .unwrap_or_default()
        };
        let base = ["ui.undo-tree", "ui.file-tree"]
            .iter()
            .find_map(|scope| theme.try_get_exact(scope))
            .unwrap_or_else(|| theme.get("ui.background").patch(theme.get("ui.text")));
        let selected = ["ui.undo-tree.selected", "ui.file-tree.selected"]
            .iter()
            .find_map(|scope| theme.try_get_exact(scope))
            .unwrap_or_else(|| Style::default().add_modifier(Modifier::BOLD));
        let track = theme.get("ui.window");
        Self {
            base,
            selected,
            mark: Style::default().fg(base.patch(selected).fg.unwrap_or(Color::Reset)),
            guide: scope(
                &["ui.undo-tree.guide", "ui.file-tree.guide"],
                &["ui.virtual.indent-guide", "ui.virtual.whitespace"],
            ),
            current: scope(&["ui.undo-tree.current"], &["info"]),
            revision: scope(&["ui.undo-tree.revision"], &["comment"]),
            saved: scope(&["ui.undo-tree.saved"], &["diff.plus"]),
            deleted: theme.get("diff.minus"),
            matched: ["ui.undo-tree.match", "ui.file-tree.match"]
                .iter()
                .find_map(|scope| theme.try_get_exact(scope))
                .unwrap_or_else(|| theme.get("special").add_modifier(Modifier::BOLD)),
            track,
            thumb: track.fg(theme.get("ui.menu.scroll").fg.unwrap_or(Color::Reset)),
        }
    }
}

/// The columns of a row besides its graph and change.
pub struct Columns {
    number: usize,
    age: usize,
}

impl Columns {
    pub fn new(rows: &Rows, now: SystemTime) -> Self {
        let revisions = rows.parents.len();
        // The oldest revision but the root has the longest age.
        let oldest = rows
            .timestamps
            .get(1)
            .map_or(0, |&time| age(time, now).len());
        Self {
            number: (revisions - 1).to_string().len(),
            age: oldest.max(3),
        }
    }

    /// The columns a row of `rows` takes to show `revision`'s change whole, the cursor mark and
    /// the rail included.
    pub fn width(&self, rows: &Rows, revision: usize) -> usize {
        let change = rows.snippets[revision].text().chars().count();
        // mark, graph, number, age, written mark, change, rail
        1 + rows.graph.width + 1 + self.number + 1 + self.age + 3 + change + 1
    }
}

/// One frame of the panel.
pub struct Scene<'a> {
    pub rows: &'a Rows,
    pub styles: &'a Styles,
    pub columns: &'a Columns,
    /// The revision whose node is `●`.
    pub marked: usize,
    /// The cursor's row, while the tree is focused.
    pub cursor: Option<usize>,
    /// The first row as drawn, i.e. while smooth scrolling.
    pub start: usize,
    pub now: SystemTime,
    /// The revisions whose changes match the search.
    pub matches: &'a [usize],
}

impl Scene<'_> {
    pub fn render(&self, area: Rect, surface: &mut Surface) {
        let (content, _) = dock::split(area, Side::Right);
        surface.clear_with(content, self.styles.base);
        for (y, row) in (content.top()..content.bottom()).zip(self.start..self.rows.len()) {
            self.render_row(row, Rect::new(content.x, y, content.width, 1), surface);
        }
        let height = area.height as usize;
        let thumb = scrollbar_thumb(self.rows.len(), height, self.start).unwrap_or_default();
        dock::render_rail(
            surface,
            area,
            Side::Right,
            thumb,
            self.styles.track,
            self.styles.thumb,
        );
    }

    fn render_row(&self, row: usize, area: Rect, surface: &mut Surface) {
        let styles = self.styles;
        let graph_row = &self.rows.graph.rows[row];
        let mut style = styles.base;
        if self.cursor == Some(row) {
            style = style.patch(styles.selected);
        }
        surface.set_style(area, style);
        let mut parts: Vec<(String, Style)> = Vec::with_capacity(8);
        parts.push(if self.cursor == Some(row) {
            (">".into(), style.patch(styles.mark))
        } else {
            (" ".into(), style)
        });

        let revision = graph_row.node.map(|(revision, _)| revision);
        let guide = style.patch(styles.guide);
        for glyph in graph_row.graph.chars() {
            parts.push(match glyph {
                NODE if revision == Some(self.marked) => ("●".into(), style.patch(styles.current)),
                NODE => (NODE.into(), style),
                glyph => (glyph.into(), guide),
            });
        }
        let Some(revision) = revision else {
            draw(parts, area, surface);
            return;
        };
        let pad = self.rows.graph.width - graph_row.graph.chars().count();
        parts.push((" ".repeat(pad + 1), style));

        let columns = self.columns;
        let meta = style.patch(styles.revision);
        parts.push((format!("{revision:>width$} ", width = columns.number), meta));
        let snippet = &self.rows.snippets[revision];
        let age = if revision == 0 {
            String::new()
        } else {
            age(self.rows.timestamps[revision], self.now)
        };
        parts.push((format!("{age:<width$} ", width = columns.age), meta));
        let mark = match self.rows.written(revision) {
            (_, true) => "S",
            (true, false) => "s",
            (false, false) => " ",
        };
        parts.push((format!("{mark} "), style.patch(styles.saved)));
        let change = match snippet {
            Snippet::Original => meta,
            Snippet::Deleted(_) => style.patch(styles.deleted),
            Snippet::Inserted(_) => style,
        };
        let change = if self.matches.contains(&revision) {
            change.patch(styles.matched)
        } else {
            change
        };
        parts.push((snippet.text().into_owned(), change));
        draw(parts, area, surface);
    }
}

fn draw(parts: Vec<(String, Style)>, area: Rect, surface: &mut Surface) {
    let mut x = area.x;
    for (text, style) in parts {
        if x >= area.right() {
            break;
        }
        let width = (area.right() - x) as usize;
        x = surface.set_stringn(x, area.y, &text, width, style).0;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use helix_core::{
        history::{History, State},
        Rope, Selection, Transaction,
    };
    use helix_view::DocumentId;

    use super::*;

    /// The branching history of the undo tree's mockup: 3 and 4 branch off 2, 5 and 6 off 1.
    fn history() -> (History, SystemTime) {
        let mut history = History::default();
        let mut state = State {
            doc: Rope::from("fn main() {\n}\n"),
            selection: Selection::point(0),
        };
        let start = history.timestamp(0);
        let mut change = |history: &mut History, at, text: &str, seconds| {
            let transaction =
                Transaction::change(&state.doc, [(at, at, Some(text.into()))].into_iter());
            let time = start + Duration::from_secs(seconds);
            history.commit_revision_at_timestamp(&transaction, &state, time);
            transaction.apply(&mut state.doc);
        };
        change(&mut history, 12, "    let x = 1;\n", 10);
        change(&mut history, 27, "    println!(\"{x}\");\n", 70);
        history.record_save(2);
        change(&mut history, 0, "count", 130);
        history.undo();
        change(&mut history, 0, "    // todo\n", 150);
        history.undo();
        history.undo();
        change(&mut history, 0, "dbg!(x);", 180);
        change(&mut history, 0, "x = 2", 189);
        (history, start + Duration::from_secs(190))
    }

    fn drawn(cursor: Option<usize>, width: u16) -> Vec<String> {
        let (history, now) = history();
        let rows = Rows::new(DocumentId::default(), &history);
        let columns = Columns::new(&rows, now);
        let theme = Theme::default();
        let styles = Styles::new(&theme);
        let area = Rect::new(0, 0, width, rows.len() as u16);
        let mut surface = Surface::empty(area);
        Scene {
            rows: &rows,
            styles: &styles,
            columns: &columns,
            marked: 6,
            cursor,
            start: 0,
            now,
            matches: &[],
        }
        .render(area, &mut surface);
        (0..area.height)
            .map(|y| {
                let row: String = (0..area.width)
                    .map(|x| surface[(x, y)].symbol.as_str())
                    .collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    #[test]
    fn rows_show_the_graph_and_the_changes() {
        assert_eq!(
            drawn(Some(2), 34),
            [
                "│ ●     6 now   x = 2",
                "│ ○     5 10s   dbg!(x);",
                "│>│ ○   4 40s   // todo",
                "│ │ │ ○ 3 1m    count",
                "│ │ ├─┘",
                "│ │ ○   2 2m  S println!(\"{x}\");",
                "│ ├─┘",
                "│ ○     1 3m    let x = 1;",
                "│ ○     0       original",
            ]
        );
    }

    #[test]
    fn rows_are_cut_at_the_edge() {
        assert_eq!(drawn(None, 12)[5], "│ │ ○   2 2m");
    }
}
