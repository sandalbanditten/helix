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
}

impl Styles {
    pub fn new(theme: &Theme) -> Self {
        // `try_get` would fall back to `ui`, so a scope of the tree falls back explicitly.
        let scope = |scope: &str, fallbacks: &[&str]| {
            theme
                .try_get_exact(scope)
                .or_else(|| {
                    fallbacks
                        .iter()
                        .find_map(|fallback| theme.try_get(fallback))
                })
                .unwrap_or_default()
        };
        let base = theme
            .try_get_exact("ui.file-tree")
            .unwrap_or_else(|| theme.get("ui.background").patch(theme.get("ui.text")));
        let track = theme.get("ui.window");
        Self {
            base,
            selected: base.patch(scope("ui.file-tree.selected", &["ui.text.focus"])),
            pinned: base.patch(scope("ui.file-tree.pinned", &["ui.virtual.ruler"])),
            active: base.patch(scope(
                "ui.file-tree.active",
                &["ui.bufferline.active", "ui.statusline.active"],
            )),
            guide: scope(
                "ui.file-tree.guide",
                &["ui.virtual.indent-guide", "ui.virtual.whitespace"],
            ),
            directory: scope("ui.file-tree.directory", &["ui.text.directory"]),
            buffer: scope("ui.file-tree.buffer", &["hint"]),
            buffer_focused: scope("ui.file-tree.buffer.focused", &["info"]),
            unsaved: scope("ui.file-tree.unsaved", &["info"]),
            error: scope("ui.file-tree.error", &["error"]),
            created: theme.get("diff.plus.gutter"),
            modified: theme.get("diff.delta.gutter"),
            deleted: theme.get("diff.minus.gutter"),
            conflict: theme.get("diff.delta.conflict"),
            track,
            thumb: track.fg(theme.get("ui.menu.scroll").fg.unwrap_or(Color::Reset)),
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
    pub side: FileTreeSide,
}

impl Scene<'_> {
    pub fn render(&self, area: Rect, surface: &mut Surface) {
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
        for (y, (index, pinned)) in (content.top()..content.bottom()).zip(rows) {
            self.render_row(
                index,
                pinned,
                Rect::new(content.x, y, content.width, 1),
                surface,
            );
        }

        let thumb = viewport::thumb(self.rows, self.start, height).unwrap_or_default();
        for (i, y) in (area.top()..area.bottom()).enumerate() {
            let (symbol, style) = if thumb.contains(&i) {
                ("┃", self.styles.thumb)
            } else {
                ("│", self.styles.track)
            };
            surface[(rail, y)].set_symbol(symbol).set_style(style);
        }
    }

    fn render_row(&self, index: usize, pinned: bool, area: Rect, surface: &mut Surface) {
        if area.width < 2 {
            return;
        }
        let styles = self.styles;
        let row = &self.rows[index];
        let node = self.tree.node(row.node);
        let root = index == 0;
        let path = row.path.as_path();
        let focused_buffer = self.marks.focused == Some(path);
        let failed = matches!(node.children, Children::Unreadable)
            || node.kind == Kind::Link(LinkTarget::Broken);

        let row_style = if self.cursor == Some(index) {
            styles.selected
        } else if pinned {
            styles.pinned
        } else if focused_buffer {
            styles.active
        } else {
            styles.base
        };
        surface.set_style(area, row_style);

        let mut parts: Vec<(&str, Style)> = Vec::with_capacity(8 + row.depth);
        let git = (!failed)
            .then(|| self.git.status(path, node.kind == Kind::Directory))
            .flatten();
        parts.push(match git {
            Some(GitStatus::Deleted) => ("▔", row_style.patch(styles.deleted)),
            Some(status) => ("▍", row_style.patch(styles.git(status))),
            None => (" ", row_style),
        });
        parts.push((
            if self.cursor == Some(index) { ">" } else { " " },
            row_style,
        ));

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
            parts.push(if node.kind == Kind::Directory {
                (if node.expanded { "▾" } else { "▸" }, guide)
            } else if focused_buffer {
                ("*", row_style.patch(styles.buffer_focused))
            } else if self.marks.open.contains(path) {
                ("*", row_style.patch(styles.buffer))
            } else {
                (if self.guides { "─" } else { " " }, guide)
            });
        }

        let label_style = self.label_style(node, path, row_style, failed);
        if self.icons {
            if !root {
                parts.push((" ", row_style));
            }
            // Like `eza`, the icon takes the label's color but not its modifiers.
            let icon_style = Style {
                fg: label_style.fg,
                ..row_style
            };
            parts.push((self.icon(root, node, failed), icon_style));
            parts.push((" ", row_style));
        } else if !root {
            parts.push((" ", row_style));
        }
        parts.push((&row.label, label_style));

        let last = area.right() - 1;
        let mut x = area.left();
        for (text, style) in parts {
            if x >= last {
                break;
            }
            (x, _) = surface.set_stringn(x, area.y, text, (last - x) as usize, style);
        }
        if natural_width(row, root, self.icons) > area.width as usize {
            surface[(last - 1, area.y)]
                .set_symbol("…")
                .set_style(label_style);
        }

        let (unsaved, style) = if self.marks.modified.contains(path) {
            ("+", row_style.patch(styles.unsaved))
        } else {
            (" ", row_style)
        };
        surface[(last, area.y)].set_symbol(unsaved).set_style(style);
    }

    fn label_style(&self, node: &Node, path: &Path, row_style: Style, failed: bool) -> Style {
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
        if self.git.is_ignored(path) {
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

    /// root: `docs/guide.md` (expanded, modified), `src/main` (a run), `README.md` (focused,
    /// changed in git).
    fn render(width: u16, height: u16, icons: bool, side: FileTreeSide) -> Vec<String> {
        let mut tree = tree_with(vec![run("src", &["main"]), dir("docs"), file("README.md")]);
        let docs = tree.find("docs".as_ref()).unwrap();
        tree.expand(docs);
        tree.apply_listing(docs, Some(vec![file("guide.md")]));
        let rows = Rows::build(&tree, true);
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
        let theme = Theme::default();
        let styles = Styles::new(&theme);
        let area = Rect::new(0, 0, width, height);
        let mut surface = Surface::empty(area);
        Scene {
            tree: &tree,
            rows: &rows,
            git: &git,
            marks: &marks,
            palette: None,
            styles: &styles,
            cursor: Some(1),
            start: 0,
            icons,
            guides: true,
            side,
        }
        .render(area, &mut surface);
        lines(&surface)
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
                "┃▍ \u{f0645} root          +",
                "│ >├─▾ \u{f115} docs      +",
            ]
        );
    }

    #[test]
    fn cut_labels_keep_the_unsaved_mark() {
        assert_eq!(
            render(16, 3, false, FileTreeSide::Left)[2],
            "  │   └── gui…+│"
        );
    }

    #[test]
    fn natural_width_counts_every_column() {
        let tree = tree_with(vec![dir("docs"), file("README.md")]);
        let rows = Rows::build(&tree, true);
        // `▍ root+`
        assert_eq!(natural_width(&rows[0], true, false), 7);
        // `▍ ├── README.md+` with ` icon ` instead of the single space
        assert_eq!(natural_width(&rows[2], false, false), 16);
        assert_eq!(natural_width(&rows[2], false, true), 18);
    }
}
