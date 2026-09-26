//! Drawing the panel: rows shaped like `eza --tree`, and the rail between panel and editor.
//!
//! A row reads `[git][cursor][ancestor lanes][branch][tip][ icon ][label]…[unsaved]`, with the
//! unsaved mark in the last column so it stays visible when the label is cut short.

use std::{collections::HashSet, path::Path};

use helix_view::{
    editor::FileTreeSide,
    graphics::{Color, Modifier, Rect, Style},
    Editor, Theme,
};
use tui::buffer::Buffer as Surface;

use super::{
    git::{GitStatus, GitStatuses},
    icons,
    ls_colors::{EntryType, LsColors},
    rows::{Row, Rows},
    tree::{Children, Kind, LinkTarget, Node, Special, Tree},
    viewport,
};

/// The columns a row needs to show its label in full, unsaved mark included.
pub fn natural_width(row: &Row, root: bool, icons: bool) -> usize {
    // git and cursor marks, then the branch (non-root rows), then the unsaved mark
    let marks = if root { 3 } else { 6 + 4 * row.depth };
    let icon = match (icons, root) {
        (true, true) => 2,
        (true, false) => 3,
        (false, true) => 0,
        (false, false) => 1,
    };
    marks + icon + row.label_width
}

/// The theme styles of the panel, resolved once per frame.
pub struct Styles {
    base: Style,
    selected: Style,
    /// The color of the cursor's `>` mark.
    mark: Style,
    pinned: Style,
    active: Style,
    guide: Style,
    directory: Style,
    buffer: Style,
    buffer_focused: Style,
    unsaved: Style,
    error: Style,
    created: Style,
    modified: Style,
    deleted: Style,
    conflict: Style,
    track: Style,
    thumb: Style,
    matched: Style,
}

impl Styles {
    pub fn new(theme: &Theme) -> Self {
        // `try_get` would fall back to `ui`, so a scope of the tree falls back explicitly.
        let scope = |scope: &str, fallbacks: &[&str]| {
            theme.try_get_exact(scope).unwrap_or_else(|| {
                fallbacks
                    .iter()
                    .find_map(|fallback| theme.try_get(fallback))
                    .unwrap_or_default()
            })
        };
        let base = theme
            .try_get_exact("ui.file-tree")
            .unwrap_or_else(|| theme.get("ui.background").patch(theme.get("ui.text")));
        let track = theme.get("ui.window");
        let guide = scope(
            "ui.file-tree.guide",
            &["ui.virtual.indent-guide", "ui.virtual.whitespace"],
        );
        // Next to its `>` mark the cursor row is bold, keeping the colors of its entry.
        let selected = theme
            .try_get_exact("ui.file-tree.selected")
            .unwrap_or_else(|| Style::default().add_modifier(Modifier::BOLD));
        Self {
            base,
            selected,
            // The same on every row, also on the focused buffer's.
            mark: Style::default().fg(base.patch(selected).fg.unwrap_or(Color::Reset)),
            // Pinned rows look like the others unless the theme says otherwise.
            pinned: theme
                .try_get_exact("ui.file-tree.pinned")
                .unwrap_or_default(),
            active: scope(
                "ui.file-tree.active",
                &["ui.bufferline.active", "ui.statusline.active"],
            ),
            guide,
            directory: scope("ui.file-tree.directory", &["ui.text.directory"]),
            buffer: theme.try_get_exact("ui.file-tree.buffer").unwrap_or(guide),
            buffer_focused: scope("ui.file-tree.buffer.focused", &["info"]),
            unsaved: scope("ui.file-tree.unsaved", &["info"]),
            error: scope("ui.file-tree.error", &["error"]),
            created: theme.get("diff.plus.gutter"),
            modified: theme.get("diff.delta.gutter"),
            deleted: theme.get("diff.minus.gutter"),
            conflict: theme.get("diff.delta.conflict"),
            track,
            thumb: track.fg(theme.get("ui.menu.scroll").fg.unwrap_or(Color::Reset)),
            // Like the matches of the picker.
            matched: theme
                .try_get_exact("ui.file-tree.match")
                .unwrap_or_else(|| theme.get("special").add_modifier(Modifier::BOLD)),
        }
    }

    fn git(&self, status: GitStatus) -> Style {
        match status {
            GitStatus::Created => self.created,
            GitStatus::Modified => self.modified,
            GitStatus::Deleted => self.deleted,
            GitStatus::Conflict => self.conflict,
        }
    }
}

/// Which workspace files are open in buffers, as paths relative to the root.
#[derive(Default)]
pub struct BufferMarks<'a> {
    open: HashSet<&'a Path>,
    focused: Option<&'a Path>,
    /// Modified files and every directory holding one.
    modified: HashSet<&'a Path>,
}

impl<'a> BufferMarks<'a> {
    pub fn new(editor: &'a Editor, root: &Path) -> Self {
        let mut marks = Self::default();
        for doc in editor.documents() {
            let Some(path) = doc.path().and_then(|path| path.strip_prefix(root).ok()) else {
                continue;
            };
            marks.open.insert(path);
            if doc.is_modified() {
                marks.modified.extend(path.ancestors());
            }
        }
        let focused = editor.tree.get(editor.tree.focus).doc;
        marks.focused = editor
            .document(focused)
            .and_then(|doc| doc.path()?.strip_prefix(root).ok());
        marks
    }
}

/// A row whose label is being typed in: an entry being renamed, or the input row of a new one.
pub struct EditRow<'a> {
    pub index: usize,
    /// The name typed so far, which picks the icon of a new entry.
    pub name: &'a str,
    pub directory: bool,
}

/// One frame of the panel.
pub struct Scene<'a> {
    pub tree: &'a Tree,
    pub rows: &'a Rows,
    pub git: &'a GitStatuses,
    pub marks: &'a BufferMarks<'a>,
    pub palette: Option<&'a LsColors>,
    pub styles: &'a Styles,
    /// The cursor row, while the tree is focused.
    pub cursor: Option<usize>,
    /// The first ordinary row as drawn, i.e. while smooth scrolling.
    pub start: usize,
    pub icons: bool,
    pub guides: bool,
    /// The marks of collapsed and expanded directories, if any.
    pub expanders: Option<[&'a str; 2]>,
    pub side: FileTreeSide,
    pub edit: Option<EditRow<'a>>,
    /// The rows matching the search, in order, with the characters of their labels that match.
    pub matches: &'a [(usize, Vec<usize>)],
}

impl Scene<'_> {
    /// Draws the panel into `area`. Returns where the label of the row being edited goes, if it
    /// is in view.
    pub fn render(&self, area: Rect, surface: &mut Surface) -> Option<Rect> {
        let (content, rail) = match self.side {
            FileTreeSide::Left => (area.clip_right(1), area.right() - 1),
            FileTreeSide::Right => (area.clip_left(1), area.left()),
        };
        surface.clear_with(content, self.styles.base);

        let height = area.height as usize;
        let pinned = viewport::pinned(self.rows, self.start, height);
        let rows = pinned
            .iter()
            .map(|&index| (index, true))
            .chain((self.start..self.rows.len()).map(|index| (index, false)));
        let mut edit_area = None;
        for (y, (index, pinned)) in (content.top()..content.bottom()).zip(rows) {
            let area = Rect::new(content.x, y, content.width, 1);
            if let Some(label_area) = self.render_row(index, pinned, area, surface) {
                edit_area = Some(label_area);
            }
        }

        let thumb = viewport::thumb(self.rows, self.start, height).unwrap_or_default();
        // A half block like the scrollbars of menus, on the tree's half of the rail.
        let thumb_symbol = match self.side {
            FileTreeSide::Left => "▌",
            FileTreeSide::Right => "▐",
        };
        for (i, y) in (area.top()..area.bottom()).enumerate() {
            let (symbol, style) = if thumb.contains(&i) {
                (thumb_symbol, self.styles.thumb)
            } else {
                ("│", self.styles.track)
            };
            surface[(rail, y)].set_symbol(symbol).set_style(style);
        }
        edit_area
    }

    /// Draws row `index`, leaving out the label of the row being edited and returning its area.
    fn render_row(
        &self,
        index: usize,
        pinned: bool,
        area: Rect,
        surface: &mut Surface,
    ) -> Option<Rect> {
        if area.width < 2 {
            return None;
        }
        let styles = self.styles;
        let row = &self.rows[index];
        let edit = self.edit.as_ref().filter(|edit| edit.index == index);
        let node = self.tree.node(row.node);
        let root = index == 0;
        let path = row.path.as_path();
        // The input row borrows its directory's node and path; it is no entry of its own.
        let entry = !row.input;
        let focused_buffer = entry && self.marks.focused == Some(path);
        let failed = entry
            && (matches!(node.children, Children::Unreadable)
                || node.kind == Kind::Link(LinkTarget::Broken));

        // Each layer only replaces what it defines, so the cursor row keeps its entry's colors.
        let mut row_style = styles.base;
        if focused_buffer {
            row_style = row_style.patch(styles.active);
        }
        if pinned {
            row_style = row_style.patch(styles.pinned);
        }
        if self.cursor == Some(index) {
            row_style = row_style.patch(styles.selected);
        }
        surface.set_style(area, row_style);

        let mut parts: Vec<(&str, Style)> = Vec::with_capacity(8 + row.depth);
        let git = (entry && !failed)
            .then(|| self.git.status(path, node.kind == Kind::Directory))
            .flatten();
        parts.push(match git {
            Some(GitStatus::Deleted) => ("▔", row_style.patch(styles.deleted)),
            Some(status) => ("▍", row_style.patch(styles.git(status))),
            None => (" ", row_style),
        });
        parts.push(if self.cursor == Some(index) {
            (">", row_style.patch(styles.mark))
        } else {
            (" ", row_style)
        });

        let guide = row_style.patch(styles.guide);
        if !root {
            for continues in self.rows.lanes(index) {
                let lane = if self.guides && continues {
                    "│   "
                } else {
                    "    "
                };
                parts.push((lane, guide));
            }
            let branch = match (self.guides, row.last) {
                (false, _) => "  ",
                (true, true) => "└─",
                (true, false) => "├─",
            };
            parts.push((branch, guide));
            let expander = self
                .expanders
                .filter(|_| entry && node.kind == Kind::Directory)
                .map(|[collapsed, expanded]| if node.expanded { expanded } else { collapsed });
            parts.push(if let Some(expander) = expander {
                (expander, guide)
            } else if focused_buffer {
                ("*", row_style.patch(styles.buffer_focused))
            } else if entry && self.marks.open.contains(path) {
                ("*", row_style.patch(styles.buffer))
            } else {
                (if self.guides { "─" } else { " " }, guide)
            });
        }

        let label_style = if entry {
            let style = self.label_style(node, row, row_style, failed);
            // The focused buffer's file reads like its tab in the bufferline, over the row's
            // background.
            if focused_buffer {
                style.patch(Style {
                    bg: None,
                    ..styles.active
                })
            } else {
                style
            }
        } else {
            row_style
        };
        if self.icons {
            if !root {
                parts.push((" ", row_style));
            }
            // Like `eza`, the icon takes the label's color but not its modifiers.
            let icon_style = Style {
                fg: label_style.fg,
                ..row_style
            };
            let icon = match edit {
                Some(edit) if !entry && edit.directory => icons::directory(edit.name, false),
                Some(edit) if !entry => icons::file(edit.name),
                _ => self.icon(root, node, failed),
            };
            parts.push((icon, icon_style));
            parts.push((" ", row_style));
        } else if !root {
            parts.push((" ", row_style));
        }
        let matched = self
            .matches
            .binary_search_by_key(&index, |(index, _)| *index)
            .ok();
        match (edit, matched) {
            (Some(_), _) => {}
            (None, Some(matched)) => {
                let chars = &self.matches[matched].1;
                let matched = label_style.patch(styles.matched);
                push_highlighted(&mut parts, &row.label, chars, label_style, matched);
            }
            (None, None) => parts.push((&row.label, label_style)),
        }

        let last = area.right() - 1;
        let mut x = area.left();
        for (text, style) in parts {
            if x >= last {
                break;
            }
            (x, _) = surface.set_stringn(x, area.y, text, (last - x) as usize, style);
        }
        if edit.is_some() {
            return Some(Rect::new(x, area.y, last.saturating_sub(x), 1));
        }
        if natural_width(row, root, self.icons) > area.width as usize {
            surface[(last - 1, area.y)]
                .set_symbol("…")
                .set_style(label_style);
        }

        let (unsaved, style) = if entry && self.marks.modified.contains(path) {
            ("+", row_style.patch(styles.unsaved))
        } else {
            (" ", row_style)
        };
        surface[(last, area.y)].set_symbol(unsaved).set_style(style);
        None
    }

    fn label_style(&self, node: &Node, row: &Row, row_style: Style, failed: bool) -> Style {
        let (entry_type, target) = match node.kind {
            Kind::Directory => (EntryType::Directory, None),
            Kind::File { executable: false } => (EntryType::File, None),
            Kind::File { executable: true } => (EntryType::Executable, None),
            Kind::Link(LinkTarget::File) => (EntryType::Link, Some(EntryType::File)),
            Kind::Link(LinkTarget::Directory) => (EntryType::Link, Some(EntryType::Directory)),
            Kind::Link(LinkTarget::Broken) => (EntryType::Orphan, None),
            Kind::Special(Special::Fifo) => (EntryType::Fifo, None),
            Kind::Special(Special::Socket) => (EntryType::Socket, None),
            Kind::Special(Special::BlockDevice) => (EntryType::BlockDevice, None),
            Kind::Special(Special::CharDevice) => (EntryType::CharDevice, None),
        };
        let palette = self
            .palette
            .and_then(|palette| palette.style(&node.name.to_string_lossy(), entry_type, target));
        let mut style = match palette {
            Some(palette) => row_style.patch(palette),
            None if matches!(entry_type, EntryType::Directory)
                || target == Some(EntryType::Directory) =>
            {
                row_style.patch(self.styles.directory)
            }
            None => row_style,
        };
        // A failure replaces the color but keeps the modifiers saying what the entry is.
        if failed {
            style.fg = self.styles.error.fg.or(style.fg);
        }
        if row.ignored {
            style = style.add_modifier(Modifier::DIM);
        }
        style
    }

    fn icon(&self, root: bool, node: &Node, failed: bool) -> &'static str {
        if failed {
            return match node.kind {
                Kind::Link(_) => icons::BROKEN_LINK,
                _ => icons::UNREADABLE_DIRECTORY,
            };
        }
        if root {
            return icons::ROOT;
        }
        let name = node.name.to_string_lossy();
        match node.kind {
            Kind::Directory => icons::directory(&name, node.expanded),
            Kind::Link(LinkTarget::Directory) => icons::directory(&name, false),
            _ => icons::file(&name),
        }
    }
}

/// Pushes `label` in runs of `style`, with the characters at the indices `chars` in `matched`.
fn push_highlighted<'a>(
    parts: &mut Vec<(&'a str, Style)>,
    label: &'a str,
    chars: &[usize],
    style: Style,
    matched: Style,
) {
    let mut run: Option<(usize, bool)> = None;
    for (i, (byte, _)) in label.char_indices().enumerate() {
        let highlighted = chars.binary_search(&i).is_ok();
        match run {
            Some((_, current)) if current == highlighted => {}
            Some((start, current)) => {
                parts.push((&label[start..byte], if current { matched } else { style }));
                run = Some((byte, highlighted));
            }
            None => run = Some((byte, highlighted)),
        }
    }
    if let Some((start, current)) = run {
        parts.push((&label[start..], if current { matched } else { style }));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use helix_vcs::FileChange;

    use super::super::tree::tests::{dir, file, run, tree_with};
    use super::*;

    fn lines(surface: &Surface) -> Vec<String> {
        let area = surface.area;
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| surface[(x, y)].symbol.to_string())
                    .collect()
            })
            .collect()
    }

    fn render(width: u16, height: u16, icons: bool, side: FileTreeSide) -> Vec<String> {
        lines(&draw(width, height, icons, side, &Theme::default(), 1))
    }

    /// root: `docs/guide.md` (expanded, modified), `src/main` (a run), `README.md` (focused,
    /// changed in git).
    fn draw(
        width: u16,
        height: u16,
        icons: bool,
        side: FileTreeSide,
        theme: &Theme,
        cursor: usize,
    ) -> Surface {
        let mut tree = tree_with(vec![run("src", &["main"]), dir("docs"), file("README.md")]);
        let docs = tree.find("docs".as_ref()).unwrap();
        tree.expand(docs);
        tree.apply_listing(docs, Some(vec![file("guide.md")]));
        let rows = Rows::build(&tree, true, None);
        let git = GitStatuses::new(
            Path::new("/root"),
            [FileChange::Modified {
                path: PathBuf::from("/root/README.md"),
            }],
        );
        let mut marks = BufferMarks::default();
        marks.open.insert(Path::new("README.md"));
        marks.focused = Some(Path::new("README.md"));
        marks
            .modified
            .extend(Path::new("docs/guide.md").ancestors());
        let styles = Styles::new(theme);
        let area = Rect::new(0, 0, width, height);
        let mut surface = Surface::empty(area);
        Scene {
            tree: &tree,
            rows: &rows,
            git: &git,
            marks: &marks,
            palette: None,
            styles: &styles,
            cursor: Some(cursor),
            start: 0,
            icons,
            guides: true,
            expanders: Some(["▸", "▾"]),
            side,
            edit: None,
            matches: &[],
        }
        .render(area, &mut surface);
        surface
    }

    #[test]
    fn rows_look_like_eza() {
        assert_eq!(
            render(20, 6, false, FileTreeSide::Left),
            [
                "▍ root            +│",
                " >├─▾ docs        +│",
                "  │   └── guide.md+│",
                "  ├─▸ src/main     │",
                "▍ └─* README.md    │",
                "                   │",
            ]
        );
        // Docked right, with icons: the rail is on the left, its thumb covers the top row.
        assert_eq!(
            render(20, 2, true, FileTreeSide::Right),
            [
                "▐▍ \u{f0645} root          +",
                "│ >├─▾ \u{f115} docs      +",
            ]
        );
    }

    #[test]
    fn the_cursor_mark_keeps_its_color_on_the_focused_buffer() {
        let theme: toml::Value =
            toml::from_str("'ui.text' = 'white'\n'ui.file-tree.active' = 'green'").unwrap();
        let surface = draw(20, 6, false, FileTreeSide::Left, &theme.into(), 4);
        assert_eq!(lines(&surface)[4], "▍>└─* README.md    │");
        assert_eq!(surface[(1, 4)].fg, Color::White);
        assert_eq!(surface[(6, 4)].fg, Color::Green);
    }

    #[test]
    fn cut_labels_keep_the_unsaved_mark() {
        assert_eq!(
            render(16, 3, false, FileTreeSide::Left)[2],
            "  │   └── gui…+│"
        );
    }

    #[test]
    fn matches_are_highlighted_in_runs() {
        let (plain, matched) = (Style::default(), Style::default().fg(Color::Red));
        let mut parts = Vec::new();
        push_highlighted(&mut parts, "main.rs", &[0, 1, 5], plain, matched);
        assert_eq!(
            parts,
            [
                ("ma", matched),
                ("in.", plain),
                ("r", matched),
                ("s", plain)
            ]
        );
    }

    #[test]
    fn natural_width_counts_every_column() {
        let tree = tree_with(vec![dir("docs"), file("README.md")]);
        let rows = Rows::build(&tree, true, None);
        // `▍ root+`
        assert_eq!(natural_width(&rows[0], true, false), 7);
        // `▍ ├── README.md+` with ` icon ` instead of the single space
        assert_eq!(natural_width(&rows[2], false, false), 16);
        assert_eq!(natural_width(&rows[2], false, true), 18);
    }
}
