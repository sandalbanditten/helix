//! The diff tree: the files of a diff of many, docked where the file tree docks.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use helix_core::Position;
use helix_view::{
    diff_view::builtin::Stats,
    editor::{FileTreeConfig, FileTreeSide},
    graphics::{CursorKind, Rect},
    input::{KeyEvent, MouseButton, MouseEvent, MouseEventKind},
    Editor,
};
use tui::buffer::Buffer as Surface;

use super::set::{self, DiffSet, FileDiff};
use crate::{
    compositor::{Component, Context, Event, EventResult},
    ctrl, key,
    ui::{
        dock,
        file_tree::{
            browser::{Browser, Motion},
            edit::{Edit, EditEvent, EditKind},
            git::GitStatuses,
            render::{natural_width, stats_width, BufferMarks, Scene, Styles},
            search::{Candidates, Direction, Matching},
            tree::{Kind, Leaf, Tree},
            Palette,
        },
        panel_keys::{self, bind, Bindings},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Down,
    Up,
    Expand,
    Collapse,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    First,
    Last,
    AlignCenter,
    AlignTop,
    AlignBottom,
    Open,
    Search,
    NextMatch,
    PreviousMatch,
    Grow,
    Shrink,
    Fit,
    ToggleWidth,
    Help,
    Unfocus,
}

impl Action {
    fn motion(self) -> Option<Motion> {
        Some(match self {
            Self::Down => Motion::Down,
            Self::Up => Motion::Up,
            Self::Expand => Motion::Expand,
            Self::Collapse => Motion::Collapse,
            Self::HalfPageDown => Motion::HalfPageDown,
            Self::HalfPageUp => Motion::HalfPageUp,
            Self::PageDown => Motion::PageDown,
            Self::PageUp => Motion::PageUp,
            Self::First => Motion::First,
            Self::Last => Motion::Last,
            Self::AlignCenter => Motion::AlignCenter,
            Self::AlignTop => Motion::AlignTop,
            Self::AlignBottom => Motion::AlignBottom,
            _ => return None,
        })
    }
}

impl panel_keys::Action for Action {
    fn doc(self) -> &'static str {
        match self {
            Self::Down => "Move down",
            Self::Up => "Move up",
            Self::Expand => "Expand directory",
            Self::Collapse => "Collapse directory",
            Self::HalfPageDown => "Move half a page down",
            Self::HalfPageUp => "Move half a page up",
            Self::PageDown => "Move a page down",
            Self::PageUp => "Move a page up",
            Self::First => "Go to the first row",
            Self::Last => "Go to the last row",
            Self::AlignCenter => "Align the cursor row to the center",
            Self::AlignTop => "Align the cursor row to the top",
            Self::AlignBottom => "Align the cursor row to the bottom",
            Self::Open => "Show diff or expand/collapse directory",
            Self::Search => "Search for a file",
            Self::NextMatch => "Go to the next match",
            Self::PreviousMatch => "Go to the previous match",
            Self::Grow => "Widen the diff tree",
            Self::Shrink => "Narrow the diff tree",
            Self::Fit => "Fit the width to the widest row",
            Self::ToggleWidth => "Toggle the widest and narrowest width",
            Self::Help => "Show these keys",
            Self::Unfocus => "Return focus to the editor",
        }
    }
}

const BINDINGS: Bindings<Action> = Bindings(&[
    bind(&[&[key!('j')], &[key!(Down)]], Action::Down),
    bind(&[&[key!('k')], &[key!(Up)]], Action::Up),
    bind(&[&[key!('l')], &[key!(Right)]], Action::Expand),
    bind(&[&[key!('h')], &[key!(Left)]], Action::Collapse),
    bind(&[&[ctrl!('d')]], Action::HalfPageDown),
    bind(&[&[ctrl!('u')]], Action::HalfPageUp),
    bind(&[&[key!(PageDown)]], Action::PageDown),
    bind(&[&[key!(PageUp)]], Action::PageUp),
    bind(&[&[key!('g'), key!('g')], &[key!(Home)]], Action::First),
    bind(&[&[key!('g'), key!('e')], &[key!(End)]], Action::Last),
    bind(
        &[&[key!('z'), key!('z')], &[key!('z'), key!('c')]],
        Action::AlignCenter,
    ),
    bind(&[&[key!('z'), key!('t')]], Action::AlignTop),
    bind(&[&[key!('z'), key!('b')]], Action::AlignBottom),
    bind(&[&[key!(Enter)]], Action::Open),
    bind(&[&[key!('/')]], Action::Search),
    bind(&[&[key!('n')]], Action::NextMatch),
    bind(&[&[key!('N')]], Action::PreviousMatch),
    bind(&[&[key!('+')]], Action::Grow),
    bind(&[&[key!('-')]], Action::Shrink),
    bind(&[&[key!('=')]], Action::Fit),
    bind(&[&[key!('|')]], Action::ToggleWidth),
    bind(&[&[key!('?')]], Action::Help),
    bind(&[&[key!(Esc)]], Action::Unfocus),
]);

/// What a key or a click asks of the diff view.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// Show the diff of the file whose row has this path, relative to the root.
    Show(PathBuf),
    None,
}

/// A search typed in the command line.
#[derive(Default)]
struct Search {
    edit: Option<Edit>,
    /// The last query searched for, for `n` and `N`.
    query: String,
    /// Where the cursor was when the query being typed was started.
    origin: Option<PathBuf>,
}

pub struct DiffTree {
    browser: Browser,
    git: GitStatuses,
    /// The lines added and removed below each path, the root's being empty.
    stats: HashMap<PathBuf, Stats>,
    candidates: Arc<Candidates>,
    palette: Palette,
    /// Whether the panel is shown while unfocused.
    shown: bool,
    focused: bool,
    /// The width asked for; `None` until it is fitted when first shown.
    width: Option<u16>,
    /// Whether the width was chosen by hand rather than fitted to the rows.
    chosen: bool,
    /// The widest the panel may get in the current screen.
    max_width: u16,
    /// Where the panel was laid out last.
    area: Option<Rect>,
    /// The keys of an unfinished sequence like `g`.
    pending: Vec<KeyEvent>,
    search: Search,
}

impl DiffTree {
    pub fn new(set: &DiffSet, config: &FileTreeConfig) -> Self {
        let entries: Vec<_> = set.files.iter().map(FileDiff::entry).collect();
        let leaves = set
            .files
            .iter()
            .zip(&entries)
            .map(|(file, (dir, name))| Leaf {
                dir,
                name,
                file_name: file
                    .path
                    .file_name()
                    .filter(|_| file.renamed_from.is_some()),
            });
        let tree = Tree::from_files(set.name.clone().into(), leaves, config.sort);
        let changes = set.files.iter().map(|file| file.change(&set.root));
        // In the order of the files, which is the tree's.
        let keys = entries.iter().map(|(dir, name)| dir.join(name)).collect();
        Self {
            browser: Browser::new(tree, config.flatten_dirs),
            git: GitStatuses::new(&set.root, changes),
            stats: HashMap::new(),
            candidates: Arc::new(Candidates::in_order(keys)),
            palette: Palette::default(),
            shown: true,
            focused: false,
            width: None,
            chosen: false,
            max_width: 0,
            area: None,
            pending: Vec::new(),
            search: Search::default(),
        }
    }

    /// Takes the lines added and removed of each file, by the path of its row.
    pub fn set_stats(&mut self, stats: impl IntoIterator<Item = (PathBuf, Stats)>) {
        let files: Vec<_> = stats.into_iter().collect();
        let sums = set::sums(files.iter().map(|(path, stats)| (path.as_path(), *stats)));
        for (path, stats) in sums {
            *self.stats.entry(path).or_default() += stats;
        }
        // The rows got wider: fit again unless the width was chosen.
        if !self.chosen {
            self.width = None;
        }
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn focus(&mut self, current: Option<&Path>) {
        self.focused = true;
        self.shown = true;
        if let Some(node) = current.and_then(|path| self.browser.tree.find(path)) {
            self.browser.cursor = node;
            self.browser.scroll_to = Some(node);
        }
    }

    pub fn unfocus(&mut self) {
        self.focused = false;
        self.search.edit = None;
    }

    /// Switches between showing the panel and showing it only while focused.
    pub fn toggle(&mut self) {
        self.shown = !self.shown;
        if !self.shown {
            self.unfocus();
        }
    }

    /// The panel's area in `main`, the area of the editor and the panel above the command line,
    /// or `None` if the panel is hidden or does not fit.
    pub fn layout(&mut self, main: Rect, editor: &Editor) -> Option<Rect> {
        self.area = self.place(main, editor);
        self.area
    }

    fn place(&mut self, main: Rect, editor: &Editor) -> Option<Rect> {
        if !self.shown && !self.focused {
            return None;
        }
        let config = editor.config();
        self.max_width = dock::max_width(main);
        let width = match self.width {
            Some(width) => width,
            None => *self.width.insert(self.fitted_width(config.file_tree.icons)),
        };
        let width = dock::clamp_width(width, self.max_width);
        if main.height < 2 || main.width < width + dock::MIN_EDITOR_WIDTH {
            self.focused = false;
            return None;
        }
        let x = match config.file_tree.side {
            FileTreeSide::Left => main.left(),
            FileTreeSide::Right => main.right() - width,
        };
        // The statusline below keeps the full width.
        Some(Rect::new(x, main.y, width, main.height - 1))
    }

    /// The width that shows the widest row whole, stats and all, within the limits.
    fn fitted_width(&self, icons: bool) -> u16 {
        let widest = self
            .browser
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                natural_width(row, index == 0, icons)
                    + stats_width(self.stats.get(&row.path).copied())
            })
            .max()
            .unwrap_or_default();
        dock::fitted_width(widest, self.max_width)
    }

    /// Handles `key` while the tree is focused, ignoring keys it does not bind.
    pub fn handle_key(&mut self, key: KeyEvent, cx: &mut Context) -> (EventResult, Request) {
        cx.editor.autoinfo = None;
        if let Some(edit) = &mut self.search.edit {
            match edit.handle_event(&Event::Key(key), cx) {
                EditEvent::Continue => self.search_incrementally(cx.editor),
                EditEvent::Cancel => self.finish_search(false),
                EditEvent::Submit => self.finish_search(true),
            }
            return (EventResult::Consumed(None), Request::None);
        }
        let mut sequence = std::mem::take(&mut self.pending);
        sequence.push(key);
        let request = match BINDINGS.lookup(&sequence) {
            panel_keys::Lookup::Action(action) => self.run(action, cx.editor),
            panel_keys::Lookup::Prefix => {
                cx.editor.autoinfo = Some(BINDINGS.info(&sequence, "Diff tree"));
                self.pending = sequence;
                Request::None
            }
            // A key that continues no sequence cancels it, like in the editor.
            panel_keys::Lookup::Unbound if sequence.len() > 1 => Request::None,
            panel_keys::Lookup::Unbound => {
                return (EventResult::Ignored(None), Request::None);
            }
        };
        self.update(cx.editor);
        (EventResult::Consumed(None), request)
    }

    fn run(&mut self, action: Action, editor: &mut Editor) -> Request {
        match action {
            Action::Help => editor.autoinfo = Some(BINDINGS.info(&[], "Diff tree")),
            Action::Unfocus => self.focused = false,
            Action::Grow | Action::Shrink => {
                let width = self.width.unwrap_or(dock::MIN_WIDTH);
                let width = if action == Action::Grow {
                    width.saturating_add(1)
                } else {
                    width.saturating_sub(1)
                };
                self.width = Some(dock::clamp_width(width, self.max_width));
                self.chosen = true;
            }
            Action::Fit => {
                self.width = Some(self.fitted_width(editor.config().file_tree.icons));
                self.chosen = false;
            }
            Action::ToggleWidth => {
                let width = self.width.unwrap_or(dock::MIN_WIDTH);
                self.width = Some(dock::toggled_width(width, self.max_width));
                self.chosen = true;
            }
            Action::Open => return self.open_cursor(),
            Action::Search => {
                self.search.origin = self.cursor_path();
                self.search.edit = Some(Edit::new(EditKind::Search, String::new(), editor));
            }
            Action::NextMatch => self.find_next(Direction::Forward, editor),
            Action::PreviousMatch => self.find_next(Direction::Backward, editor),
            _ => {
                if let Some(motion) = action.motion() {
                    self.browser.navigate(motion);
                }
            }
        }
        Request::None
    }

    /// Shows the diff of the file under the cursor, or expands or collapses the directory.
    fn open_cursor(&mut self) -> Request {
        let browser = &mut self.browser;
        let Some(index) = browser.rows.index_of(browser.cursor) else {
            return Request::None;
        };
        let row = &browser.rows[index];
        if browser.tree.node(row.node).kind == Kind::Directory {
            if index != 0 {
                browser.toggle_row(index);
            }
            return Request::None;
        }
        self.focused = false;
        Request::Show(row.path.clone())
    }

    /// Brings the rows up to date after a change and keeps the cursor in view.
    fn update(&mut self, editor: &Editor) {
        if std::mem::take(&mut self.browser.dirty) {
            self.browser.rebuild_rows(None);
        }
        if let Some(index) = self.browser.rows.index_of(self.browser.cursor) {
            self.browser.reveal_row(index, editor.config().scrolloff);
        }
    }

    fn cursor_path(&self) -> Option<PathBuf> {
        let index = self.browser.rows.index_of(self.browser.cursor)?;
        Some(self.browser.rows[index].path.clone())
    }

    /// Moves the cursor to the first match of the query being typed after where it started.
    fn search_incrementally(&mut self, editor: &mut Editor) {
        let Some(edit) = &self.search.edit else {
            return;
        };
        let query = edit.prompt.line().clone();
        let from = self.search.origin.clone().unwrap_or_default();
        if query.trim().is_empty() {
            self.move_to(&from, editor);
            return;
        }
        let hit = self
            .candidates
            .find(&query, &from, true, Direction::Forward);
        let target = hit.map_or(from, |hit| hit.path);
        self.move_to(&target, editor);
    }

    /// Ends the query being typed. Unless it is `kept`, the cursor goes back where it started.
    fn finish_search(&mut self, kept: bool) {
        let Some(edit) = self.search.edit.take() else {
            return;
        };
        let query = edit.prompt.line();
        if kept && !query.trim().is_empty() {
            self.search.query = query.clone();
        } else if let Some(node) = self
            .search
            .origin
            .take()
            .and_then(|origin| self.browser.tree.find(&origin))
        {
            self.browser.cursor = node;
        }
    }

    /// Moves the cursor to the next or previous match of the last search.
    fn find_next(&mut self, direction: Direction, editor: &mut Editor) {
        let Some(from) = self.cursor_path() else {
            return;
        };
        if self.search.query.is_empty() {
            return;
        }
        let from_dir = self
            .browser
            .tree
            .find(&from)
            .is_some_and(|node| self.browser.tree.node(node).kind == Kind::Directory);
        match self
            .candidates
            .find(&self.search.query, &from, from_dir, direction)
        {
            Some(hit) => {
                if hit.wrapped {
                    editor.set_status("Wrapped around diff tree");
                }
                self.move_to(&hit.path, editor);
            }
            None => editor.set_error("No more matches"),
        }
    }

    /// Puts the cursor on the entry at `path`, expanding the directories leading to it.
    fn move_to(&mut self, path: &Path, editor: &Editor) {
        let browser = &mut self.browser;
        let mut node = browser.tree.find(path);
        if let Some(found) = node {
            let mut ancestor = browser.tree.node(found).parent;
            while let Some(dir) = ancestor {
                browser.tree.expand(dir);
                ancestor = browser.tree.node(dir).parent;
            }
            browser.dirty = true;
        } else {
            node = Some(browser.tree.root());
        }
        browser.cursor = node.expect("set above");
        browser.scroll_to = node;
        self.update(editor);
    }

    /// Draws the panel into `area`, with the file shown in the panes, `current`, marked.
    pub fn render(
        &mut self,
        area: Rect,
        surface: &mut Surface,
        current: Option<&Path>,
        cx: &mut Context,
    ) {
        let config = cx.editor.config();
        let palette = self.palette.get(&config.file_tree.ls_colors);
        if std::mem::take(&mut self.browser.dirty) {
            self.browser.rebuild_rows(None);
        }
        let start = self.browser.frame(area, cx.editor);
        let height = area.height as usize;
        let marks = BufferMarks::focused(current);
        let styles = Styles::new(&cx.editor.theme);
        let matches = self.matches(start..start + height);
        let expanders = config
            .file_tree
            .expanders
            .characters()
            .map(|characters| characters.map(String::from));
        let cursor = self
            .focused
            .then(|| self.browser.rows.index_of(self.browser.cursor))
            .flatten();
        Scene {
            tree: &self.browser.tree,
            rows: &self.browser.rows,
            git: &self.git,
            marks: &marks,
            palette: palette.as_deref(),
            styles: &styles,
            cursor,
            start,
            icons: config.file_tree.icons,
            guides: config.file_tree.guides,
            expanders: expanders
                .as_ref()
                .map(|[collapsed, expanded]| [collapsed.as_str(), expanded.as_str()]),
            side: config.file_tree.side,
            edit: None,
            matches: &matches,
            stats: Some(&self.stats),
        }
        .render(area, surface);
    }

    /// The rows among `rows` of files matching the query being typed, each with the characters
    /// of its label that match.
    fn matches(&self, rows: std::ops::Range<usize>) -> Vec<(usize, Vec<usize>)> {
        let Some(edit) = &self.search.edit else {
            return Vec::new();
        };
        let Some(mut matching) = Matching::new(edit.prompt.line()) else {
            return Vec::new();
        };
        rows.filter_map(|index| {
            let row = self.browser.rows.get(index)?;
            if !self.candidates.contains(&row.path) {
                return None;
            }
            let path = row.path.to_string_lossy();
            let indices = matching.indices(&path)?;
            // The label is the end of the path.
            let offset = path.chars().count() - row.label.chars().count();
            let chars = indices
                .iter()
                .filter_map(|&i| (i as usize).checked_sub(offset))
                .collect();
            Some((index, chars))
        })
        .collect()
    }

    /// Draws the search prompt over the command line of `area`, the screen.
    pub fn render_command_line(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        if let Some(edit) = &mut self.search.edit {
            edit.prompt.render(area, surface, cx);
        }
    }

    /// Where the terminal cursor goes while the tree is focused: in the search prompt.
    pub fn cursor(&self, area: Rect, editor: &Editor) -> (Option<Position>, CursorKind) {
        match &self.search.edit {
            Some(edit) => edit.prompt.cursor(area, editor),
            None => (None, CursorKind::Hidden),
        }
    }

    /// Handles a mouse event over the panel. `None` leaves the event to the editor.
    pub fn handle_mouse(&mut self, event: &MouseEvent, editor: &Editor) -> Option<Request> {
        let area = self.area?;
        let inside = (area.left()..area.right()).contains(&event.column)
            && (area.top()..area.bottom()).contains(&event.row);
        if !inside {
            return None;
        }
        match event.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let lines = editor.config().scroll_lines.unsigned_abs();
                let down = event.kind == MouseEventKind::ScrollDown;
                self.browser.scroll(lines, down);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = self.browser.row_at(event.row) {
                    self.browser.cursor = self.browser.rows[index].node;
                    let request = self.open_cursor();
                    self.update(editor);
                    return Some(request);
                }
            }
            _ => {}
        }
        Some(Request::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::diff_view::set::Text;

    fn tree(paths: &[&str]) -> DiffTree {
        let set = DiffSet {
            root: "/repo".into(),
            name: "repo".into(),
            sides: [None, None],
            files: paths
                .iter()
                .map(|path| FileDiff {
                    path: path.into(),
                    renamed_from: None,
                    old: Text::Missing,
                    new: Text::Missing,
                })
                .collect(),
        };
        DiffTree::new(&set, &FileTreeConfig::default())
    }

    fn labels(tree: &DiffTree) -> Vec<&str> {
        tree.browser
            .rows
            .iter()
            .map(|row| row.label.as_str())
            .collect()
    }

    #[test]
    fn the_tree_shows_every_file_expanded() {
        let tree = tree(&["src/ui/a.rs", "src/b.rs", "c.rs"]);
        assert_eq!(labels(&tree), ["repo", "src", "ui", "a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn enter_on_a_file_asks_for_its_diff() {
        let mut tree = tree(&["src/a.rs"]);
        assert_eq!(labels(&tree), ["repo", "src", "a.rs"]);
        tree.browser.navigate(Motion::Last);
        assert_eq!(tree.open_cursor(), Request::Show("src/a.rs".into()));
        tree.browser.navigate(Motion::Up);
        assert_eq!(tree.open_cursor(), Request::None, "a directory toggles");
    }

    #[test]
    fn stats_add_up_and_widen_the_rows() {
        let mut tree = tree(&["src/a.rs", "b.rs"]);
        tree.max_width = dock::MAX_WIDTH;
        let narrow = tree.fitted_width(false);
        tree.set_stats([
            (
                "src/a.rs".into(),
                Stats {
                    added: 12,
                    removed: 3,
                },
            ),
            (
                "b.rs".into(),
                Stats {
                    added: 1,
                    removed: 0,
                },
            ),
        ]);
        assert_eq!(
            tree.stats[Path::new("")],
            Stats {
                added: 13,
                removed: 3
            }
        );
        assert!(tree.fitted_width(false) > narrow);
    }

    #[test]
    fn renamed_files_read_like_git_stat() {
        let set = DiffSet {
            root: "/repo".into(),
            name: "repo".into(),
            sides: [None, None],
            files: vec![
                FileDiff {
                    path: "src/ui/b.rs".into(),
                    renamed_from: None,
                    old: Text::Missing,
                    new: Text::Missing,
                },
                FileDiff {
                    path: "src/ui/a.rs".into(),
                    renamed_from: Some("src/a.rs".into()),
                    old: Text::Missing,
                    new: Text::Missing,
                },
            ],
        };
        let mut tree = DiffTree::new(&set, &FileTreeConfig::default());
        assert_eq!(
            labels(&tree),
            ["repo", "src", "ui", "b.rs", "{ => ui}/a.rs"]
        );
        tree.browser.navigate(Motion::Last);
        let key = PathBuf::from("src/{ => ui}/a.rs");
        assert_eq!(tree.open_cursor(), Request::Show(key.clone()));

        let stats = Stats {
            added: 1,
            removed: 1,
        };
        tree.set_stats([(key, stats)]);
        assert_eq!(tree.stats[Path::new("src")], stats);
        assert!(
            !tree.stats.contains_key(Path::new("src/ui")),
            "not below the directory it moved to"
        );
    }

    /// Times building the diff tree of 2000 files and taking their stats. Run it with
    /// `cargo test --release -p helix-term --lib measure_diff_tree -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_diff_tree() {
        use std::time::Instant;

        let paths: Vec<String> = (0..2000)
            .map(|index| format!("dir{}/file{index}.rs", index / 50))
            .collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let start = Instant::now();
        let mut tree = tree(&paths);
        let built = start.elapsed();
        let stats = paths.iter().map(|path| {
            let stats = Stats {
                added: 1,
                removed: 1,
            };
            (PathBuf::from(path), stats)
        });
        let start = Instant::now();
        tree.set_stats(stats);
        let fitted = tree.fitted_width(true);
        eprintln!(
            "2000 files: tree built in {built:?}, stats taken and fitted to {fitted} in {:?}",
            start.elapsed()
        );
    }
}
