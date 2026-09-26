//! The file tree docked beside the editor.
//!
//! [`FileTree`] is owned by the [`EditorView`], which reserves its columns, draws it and hands it
//! keys while it is focused. Directory listings and git status are read in the background; the
//! results come back through the job queue, which finds the tree in the compositor.

mod edit;
mod fs;
mod git;
mod icons;
mod keys;
mod ls_colors;
mod ops;
mod order;
mod render;
mod rows;
mod tree;
mod viewport;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    sync::Arc,
};

use helix_core::Position;
use helix_loader::workspace_trust::TrustQuery;
use helix_stdx::{
    path::{canonicalize, normalize},
    Url,
};
use helix_vcs::StatusOptions;
use helix_view::{
    editor::{
        Action as OpenAction, FileTreeConfig, FileTreeSide, FileTreeSort,
        LsColors as LsColorsSource,
    },
    graphics::{CursorKind, Rect},
    input::KeyEvent,
    smooth_scroll::SmoothOffset,
    Editor,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tui::buffer::Buffer as Surface;

use self::{
    edit::{Edit, EditEvent, EditKind},
    fs::ListOptions,
    git::GitStatuses,
    keys::{Action, Lookup},
    ls_colors::LsColors,
    order::Group,
    render::{natural_width, BufferMarks, EditRow, Scene, Styles},
    rows::{InputRow, Rows},
    tree::{Kind, Listing, NodeId, Reveal, Tree},
    viewport::Align,
};
use crate::{
    compositor::{Component, Compositor, Context, Event, EventResult},
    job,
    ui::EditorView,
};

/// The narrowest and widest the panel gets, its rail included.
const MIN_WIDTH: u16 = 16;
const MAX_WIDTH: u16 = 64;
/// The columns the panel always leaves to the editor; with fewer it yields.
const MIN_EDITOR_WIDTH: u16 = 20;
/// The columns an inline edit gets at least, reaching over the editor if the panel is narrower.
const MIN_EDIT_WIDTH: u16 = 30;

/// When the panel is shown.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    /// Only while the tree is focused.
    #[default]
    WhileFocused,
    Always,
}

#[derive(Default)]
pub struct FileTree {
    visibility: Visibility,
    focused: bool,
    /// The requested panel width; `None` until it is fitted when first shown.
    width: Option<u16>,
    /// The tree of the current workspace, created when the panel is first shown.
    workspace: Option<Workspace>,
    palette: Palette,
    lister: Option<UnboundedSender<ListRequest>>,
    last_generation: u64,
    /// The keys of an unfinished sequence like `z`.
    pending: Vec<KeyEvent>,
    /// The widest the panel may get in the current screen.
    max_width: u16,
}

impl FileTree {
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    fn is_presented(&self) -> bool {
        self.visibility == Visibility::Always || self.focused
    }

    /// Shows the panel without focusing it, as at startup.
    pub fn show(&mut self, editor: &Editor) {
        self.visibility = Visibility::Always;
        self.ensure_workspace(editor);
    }

    /// Switches between always showing the panel and showing it only while focused.
    pub fn toggle(&mut self, editor: &Editor) {
        match self.visibility {
            Visibility::Always => {
                self.visibility = Visibility::WhileFocused;
                self.focused = false;
            }
            Visibility::WhileFocused => self.show(editor),
        }
    }

    /// Focuses the tree, putting the cursor on the focused buffer's file, or gives focus back
    /// to the editor.
    pub fn toggle_focus(&mut self, editor: &Editor) {
        if self.focused {
            self.focused = false;
            return;
        }
        self.focused = true;
        self.ensure_workspace(editor);
        let lister = self.lister(editor);
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        workspace.cursor = workspace.tree.root();
        if let Some(path) = focused_document_path(editor, &workspace.root) {
            workspace.reveal(path, true);
        }
        workspace.update(&lister, &editor.config().file_tree);
    }

    /// Gives the editor its focus back, abandoning an unfinished edit.
    pub fn unfocus(&mut self) {
        self.focused = false;
        if let Some(workspace) = &mut self.workspace {
            workspace.cancel_edit();
        }
    }

    fn ensure_workspace(&mut self, editor: &Editor) {
        if self.workspace.is_some() {
            return;
        }
        self.last_generation += 1;
        let root = helix_stdx::env::current_working_dir();
        let config = editor.config();
        let mut workspace = Workspace::new(root, self.last_generation, &config.file_tree);
        let paths: Vec<_> = editor
            .documents()
            .filter_map(|doc| doc.path()?.strip_prefix(&workspace.root).ok())
            .map(Path::to_path_buf)
            .collect();
        for path in paths {
            workspace.reveal(path, false);
        }
        let lister = self.lister(editor);
        workspace.update(&lister, &config.file_tree);
        workspace.refresh_git(editor);
        self.workspace = Some(workspace);
    }

    /// The background task listing directories, started on first use.
    fn lister(&mut self, editor: &Editor) -> Lister {
        let executables = self
            .palette
            .get(&editor.config().file_tree.ls_colors)
            .is_some_and(|palette| palette.colors_executables());
        let sender = self.lister.get_or_insert_with(spawn_lister).clone();
        Lister {
            sender,
            executables,
        }
    }

    /// The panel's area in `main`, the area of the editor and the panel above the command line,
    /// or `None` if the panel is hidden or does not fit.
    pub fn layout(&mut self, main: Rect, editor: &Editor) -> Option<Rect> {
        if !self.is_presented() {
            return None;
        }
        if !self
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.ready)
        {
            return None;
        }
        let config = editor.config();
        self.max_width = main.width.saturating_sub(MIN_EDITOR_WIDTH).min(MAX_WIDTH);
        let width = match self.width {
            Some(width) => width,
            None => *self.width.insert(self.fitted_width(config.file_tree.icons)),
        };
        if main.height < 2 || main.width < width + MIN_EDITOR_WIDTH {
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

    /// The width that shows the widest row whole, within the limits.
    fn fitted_width(&self, icons: bool) -> u16 {
        let widest = self.workspace.as_ref().map_or(0, |workspace| {
            workspace
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| natural_width(row, index == 0, icons))
                .max()
                .unwrap_or_default()
        });
        // one more column for the rail
        let width = u16::try_from(widest + 1).unwrap_or(u16::MAX);
        width.min(self.max_width).max(MIN_WIDTH)
    }

    /// Handles `key` while the tree is focused. A key it does not bind is ignored, for the
    /// editor to handle.
    pub fn handle_key(&mut self, key: KeyEvent, cx: &mut Context) -> EventResult {
        cx.editor.autoinfo = None;
        if self
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.edit.is_some())
        {
            self.handle_edit(&Event::Key(key), cx);
            return EventResult::Consumed(None);
        }
        let mut sequence = std::mem::take(&mut self.pending);
        sequence.push(key);
        match keys::lookup(&sequence) {
            Lookup::Action(action) => self.run(action, cx),
            Lookup::Prefix => {
                cx.editor.autoinfo = Some(keys::info(&sequence));
                self.pending = sequence;
            }
            // A key that continues no sequence cancels it, like in the editor.
            Lookup::Unbound if sequence.len() > 1 => {}
            Lookup::Unbound => return EventResult::Ignored(None),
        }
        EventResult::Consumed(None)
    }

    /// Hands pasted text to an unfinished edit. Returns whether there was one.
    pub fn handle_paste(&mut self, text: &str, cx: &mut Context) -> bool {
        let editing = self
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.edit.is_some());
        if editing {
            self.handle_edit(&Event::Paste(text.to_owned()), cx);
        }
        editing
    }

    fn handle_edit(&mut self, event: &Event, cx: &mut Context) {
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let Some(edit) = &mut workspace.edit else {
            return;
        };
        match edit.handle_event(event, cx) {
            EditEvent::Continue => return,
            EditEvent::Cancel => workspace.cancel_edit(),
            EditEvent::Submit => {
                if let Some(edit) = workspace.edit.take() {
                    workspace.dirty = true;
                    if workspace.submit(edit, cx.editor) == Focus::Release {
                        self.focused = false;
                    }
                }
            }
        }
        self.update(cx.editor);
    }

    /// Brings the rows up to date after a change and keeps the cursor in view.
    fn update(&mut self, editor: &Editor) {
        let lister = self.lister(editor);
        if let Some(workspace) = &mut self.workspace {
            let config = editor.config();
            workspace.update(&lister, &config.file_tree);
            workspace.reveal_cursor(config.scrolloff);
        }
    }

    fn run(&mut self, action: Action, cx: &mut Context) {
        let editor = &mut *cx.editor;
        let config = editor.config();
        match action {
            Action::Help => editor.autoinfo = Some(keys::info(&[])),
            Action::Unfocus => self.focused = false,
            Action::Grow | Action::Shrink => {
                let width = self.width.unwrap_or(MIN_WIDTH);
                let width = match action {
                    Action::Grow => width.saturating_add(1),
                    _ => width.saturating_sub(1),
                };
                self.width = Some(width.min(self.max_width).max(MIN_WIDTH));
            }
            Action::Fit => self.width = Some(self.fitted_width(config.file_tree.icons)),
            Action::OpenExternally => {
                if let Some(path) = self.workspace.as_ref().and_then(Workspace::cursor_path) {
                    match Url::from_file_path(&path) {
                        Ok(url) => cx.jobs.callback(crate::open_external_url_callback(url)),
                        Err(()) => editor.set_error(format!("Cannot open {}", path.display())),
                    }
                }
            }
            _ => {
                if let Some(workspace) = &mut self.workspace {
                    if workspace.act(action, editor) == Focus::Release {
                        self.focused = false;
                    }
                }
                self.update(editor);
            }
        }
    }

    pub fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let config = cx.editor.config();
        let lister = self.lister(cx.editor);
        let palette = self.palette.get(&config.file_tree.ls_colors);
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        workspace.update(&lister, &config.file_tree);

        let height = area.height as usize;
        workspace.height = height;
        if let Some(target) = workspace.scroll_to.take() {
            if let Some(index) = workspace.rows.index_of(target) {
                if !viewport::is_visible(&workspace.rows, workspace.start, height, index) {
                    workspace.start =
                        viewport::align(&workspace.rows, height, index, Align::Center);
                }
            }
        }
        let start = viewport::clamp(&workspace.rows, workspace.start, height);
        let start = workspace.smooth_scroll.frame(start, area.height, cx.editor);

        let marks = BufferMarks::new(cx.editor, &workspace.root);
        let styles = Styles::new(&cx.editor.theme);
        let edit_area = Scene {
            tree: &workspace.tree,
            rows: &workspace.rows,
            git: &workspace.git,
            marks: &marks,
            palette: palette.as_deref(),
            styles: &styles,
            cursor: self.focused.then(|| workspace.cursor_row()).flatten(),
            start,
            icons: config.file_tree.icons,
            guides: config.file_tree.guides,
            side: config.file_tree.side,
            edit: workspace.edit_row(),
        }
        .render(area, surface);

        // A panel fitted to its rows leaves little room to type in.
        let edit_area = edit_area.map(|edit_area| {
            let width = edit_area
                .width
                .max(MIN_EDIT_WIDTH)
                .min(surface.area.right().saturating_sub(edit_area.x));
            Rect { width, ..edit_area }
        });
        workspace.edit_area = edit_area;
        if let (Some(edit_area), Some(edit)) = (edit_area, &mut workspace.edit) {
            edit.prompt.render(edit_area, surface, cx);
        }
    }

    /// Draws the command line prompt of a move over the command line of `area`, the screen.
    pub fn render_command_line(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        if let Some(edit) = self
            .workspace
            .as_mut()
            .and_then(|workspace| workspace.edit.as_mut())
            .filter(|edit| !edit.is_inline())
        {
            edit.prompt.render(area, surface, cx);
        }
    }

    /// Where the terminal cursor goes while the tree is focused: in the prompt of an edit.
    pub fn cursor(&self, area: Rect, editor: &Editor) -> (Option<Position>, CursorKind) {
        let Some(workspace) = &self.workspace else {
            return (None, CursorKind::Hidden);
        };
        match (&workspace.edit, workspace.edit_area) {
            (Some(edit), _) if !edit.is_inline() => edit.prompt.cursor(area, editor),
            (Some(edit), Some(edit_area)) => edit.prompt.cursor(edit_area, editor),
            _ => (None, CursorKind::Hidden),
        }
    }

    fn apply_listings(
        &mut self,
        generation: u64,
        listings: Vec<(PathBuf, Listing)>,
        editor: &Editor,
    ) {
        let lister = self.lister(editor);
        let Some(workspace) = self
            .workspace
            .as_mut()
            .filter(|workspace| workspace.generation == generation)
        else {
            return;
        };
        for (path, listing) in listings {
            if let Some(dir) = workspace.tree.find(&path) {
                workspace.tree.apply_listing(dir, listing);
            }
        }
        workspace.ready = true;
        workspace.dirty = true;
        workspace.update(&lister, &editor.config().file_tree);
    }

    fn apply_git(&mut self, generation: u64, statuses: GitStatuses, editor: &Editor) {
        let Some(workspace) = self
            .workspace
            .as_mut()
            .filter(|workspace| workspace.generation == generation)
        else {
            return;
        };
        workspace.git = statuses;
        if std::mem::take(&mut workspace.git_refresh) == GitRefresh::RunningStale {
            workspace.refresh_git(editor);
        }
    }
}

/// The file tree of one workspace root.
struct Workspace {
    /// Tells results for this workspace from results for an earlier one.
    generation: u64,
    root: Arc<Path>,
    tree: Tree,
    rows: Rows,
    /// The settings `rows` were built with.
    flatten_dirs: bool,
    sort: FileTreeSort,
    /// Whether `rows` needs rebuilding.
    dirty: bool,
    git: GitStatuses,
    git_refresh: GitRefresh,
    cursor: NodeId,
    /// The first ordinary row.
    start: usize,
    smooth_scroll: SmoothOffset,
    /// Whether the first listing has arrived; the panel is only shown after that.
    ready: bool,
    /// Paths to reveal once their directories are listed, and whether to move the cursor there.
    reveals: Vec<(PathBuf, bool)>,
    /// A node to scroll into view once the panel height is known.
    scroll_to: Option<NodeId>,
    /// The number of rows the panel showed last.
    height: usize,
    /// A name or path being typed for a file operation.
    edit: Option<Edit>,
    /// Where the line of an inline edit was drawn last.
    edit_area: Option<Rect>,
}

/// Whether the tree keeps its focus after an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Keep,
    Release,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum GitRefresh {
    #[default]
    Idle,
    Running,
    /// Running, and asked for again meanwhile.
    RunningStale,
}

impl Workspace {
    fn new(root: PathBuf, generation: u64, config: &FileTreeConfig) -> Self {
        let name = root
            .file_name()
            .map_or_else(|| root.as_os_str().to_owned(), ToOwned::to_owned);
        let tree = Tree::new(name, config.sort);
        let rows = Rows::build(&tree, config.flatten_dirs, None);
        Self {
            generation,
            cursor: tree.root(),
            root: root.into(),
            tree,
            rows,
            flatten_dirs: config.flatten_dirs,
            sort: config.sort,
            dirty: false,
            git: GitStatuses::default(),
            git_refresh: GitRefresh::Idle,
            start: 0,
            smooth_scroll: SmoothOffset::default(),
            ready: false,
            reveals: Vec::new(),
            scroll_to: None,
            height: 0,
            edit: None,
            edit_area: None,
        }
    }

    /// The row the cursor is on: the input row while a new entry is named.
    fn cursor_row(&self) -> Option<usize> {
        match &self.edit {
            Some(Edit {
                kind: EditKind::Create { .. },
                ..
            }) => self.rows.input(),
            _ => self.rows.index_of(self.cursor),
        }
    }

    /// The absolute path of the cursor's entry.
    fn cursor_path(&self) -> Option<PathBuf> {
        let index = self.rows.index_of(self.cursor)?;
        Some(self.root.join(&self.rows[index].path))
    }

    /// The row being typed in, for drawing.
    fn edit_row(&self) -> Option<EditRow<'_>> {
        let edit = self.edit.as_ref()?;
        let name = edit.prompt.line().as_str();
        let (index, directory) = match &edit.kind {
            EditKind::Rename { node, .. } => (self.rows.index_of(*node)?, false),
            EditKind::Create { directory, .. } => {
                (self.rows.input()?, *directory || name.ends_with('/'))
            }
            EditKind::Move { .. } | EditKind::Delete { .. } => return None,
        };
        Some(EditRow {
            index,
            name,
            directory,
        })
    }

    fn cancel_edit(&mut self) {
        if self.edit.take().is_some() {
            self.dirty = true;
        }
    }

    /// Runs a file operation on the cursor's entry or starts typing the name it needs.
    fn act(&mut self, action: Action, editor: &mut Editor) -> Focus {
        let Some(index) = self.rows.index_of(self.cursor) else {
            return Focus::Keep;
        };
        let row = &self.rows[index];
        let (node, path) = (row.node, row.path.clone());
        let kind = self.tree.node(node).kind;
        let root = index == 0;
        let open = |editor: &mut Editor, action| {
            if !kind.is_file() {
                return Focus::Keep;
            }
            match ops::open(editor, &self.root.join(&path), action) {
                Ok(()) => Focus::Release,
                Err(err) => {
                    editor.set_error(err.to_string());
                    Focus::Keep
                }
            }
        };
        match action {
            Action::Open if kind == Kind::Directory && !root => {
                self.navigate(if self.tree.node(node).expanded {
                    Action::Collapse
                } else {
                    Action::Expand
                });
            }
            Action::Open => return open(editor, OpenAction::Replace),
            Action::OpenHorizontal => return open(editor, OpenAction::HorizontalSplit),
            Action::OpenVertical => return open(editor, OpenAction::VerticalSplit),
            // The workspace root is the one entry that stays put.
            Action::Rename | Action::MoveInWorkspace | Action::Move | Action::Delete if root => {}
            Action::Rename => {
                let name = self.tree.node(node).name.to_string_lossy().into_owned();
                self.edit = Some(Edit::new(EditKind::Rename { node, path }, name, editor));
                self.dirty = true;
            }
            Action::MoveInWorkspace | Action::Move => {
                let absolute = action == Action::Move;
                let line = if absolute {
                    self.root.join(&path)
                } else {
                    path.clone()
                };
                let line = line.to_string_lossy().into_owned();
                self.edit = Some(Edit::new(EditKind::Move { path, absolute }, line, editor));
            }
            Action::NewFile | Action::NewDirectory => {
                // New entries go into the directory under the cursor, or the one holding it.
                let dir_row = match kind {
                    Kind::Directory => index,
                    _ => row.parent.unwrap_or(0),
                };
                if dir_row != 0 && !self.tree.node(self.rows[dir_row].node).expanded {
                    self.expand_row(dir_row);
                }
                let dir = self.rows[dir_row].node;
                let kind = EditKind::Create {
                    dir,
                    dir_path: self.rows[dir_row].path.clone(),
                    directory: action == Action::NewDirectory,
                };
                self.edit = Some(Edit::new(kind, String::new(), editor));
                self.dirty = true;
            }
            Action::Delete => {
                let directory = kind == Kind::Directory;
                let kind = EditKind::Delete { path, directory };
                self.edit = Some(Edit::new(kind, String::new(), editor));
            }
            _ => self.navigate(action),
        }
        Focus::Keep
    }

    /// Carries out a finished edit.
    fn submit(&mut self, edit: Edit, editor: &mut Editor) -> Focus {
        let line = edit.prompt.line();
        if line.trim().is_empty() {
            return Focus::Keep;
        }
        let result = match edit.kind {
            EditKind::Rename { path, .. } => {
                if line.contains(std::path::is_separator) || line == "." || line == ".." {
                    editor.set_error(format!("{line} is not a file name"));
                    return Focus::Keep;
                }
                let from = self.root.join(&path);
                let to = from.with_file_name(line);
                ops::rename(editor, &from, to, false).map(|to| (Some(path), to))
            }
            EditKind::Move { path, absolute } => {
                let to = if absolute {
                    canonicalize(line)
                } else {
                    normalize(self.root.join(line))
                };
                ops::rename(editor, &self.root.join(&path), to, true).map(|to| (Some(path), to))
            }
            EditKind::Create {
                dir_path,
                directory,
                ..
            } => {
                let directory = directory || line.ends_with(std::path::is_separator);
                let to = normalize(self.root.join(dir_path).join(line));
                ops::create(editor, &to, directory).and_then(|()| {
                    if !directory {
                        ops::open(editor, &to, OpenAction::Replace)?;
                    }
                    Ok((None, to))
                })
            }
            EditKind::Delete { path, .. } => {
                if line.trim().eq_ignore_ascii_case("y") {
                    self.delete(path, editor);
                }
                return Focus::Keep;
            }
        };
        match result {
            Ok((from, to)) => {
                self.moved(from.as_deref(), &to);
                if to.is_file() && from.is_none() {
                    return Focus::Release;
                }
            }
            Err(err) => editor.set_error(err.to_string()),
        }
        Focus::Keep
    }

    /// Lists the directories of `from` (relative) and `to` (absolute) again after an entry was
    /// moved, created (`from` is `None`) or deleted, and puts the cursor on `to`.
    fn moved(&mut self, from: Option<&Path>, to: &Path) {
        if let Some(parent) = from.and_then(Path::parent) {
            self.tree.invalidate(parent);
        }
        if let Ok(to) = to.strip_prefix(&self.root) {
            if let Some(parent) = to.parent() {
                self.tree.invalidate(parent);
            }
            self.reveal(to.to_path_buf(), true);
        }
    }

    /// Deletes the entry at `path`, relative to the root, moving the cursor off it first.
    fn delete(&mut self, path: PathBuf, editor: &mut Editor) {
        if let Some(index) = self.rows.index_of(self.cursor) {
            let after =
                (index + 1..self.rows.len()).find(|&i| !self.rows[i].path.starts_with(&path));
            let neighbour = after.unwrap_or(index.saturating_sub(1));
            self.cursor = self.rows[neighbour].node;
        }
        match ops::delete(editor, &self.root.join(&path)) {
            Ok(()) => {
                editor.set_status(format!("'{}' deleted", path.display()));
                if let Some(parent) = path.parent() {
                    self.tree.invalidate(parent);
                }
            }
            Err(err) => editor.set_error(err.to_string()),
        }
    }

    /// Moves the cursor or expands or collapses the directory under it.
    fn navigate(&mut self, action: Action) {
        let Some(cursor) = self.rows.index_of(self.cursor) else {
            self.cursor = self.tree.root();
            return;
        };
        let last = self.rows.len() - 1;
        let height = self.height.max(1);
        let start = viewport::clamp(&self.rows, self.start, height);
        let page = viewport::capacity(&self.rows, start, height).max(1);
        let target = match action {
            Action::Down if cursor == last => 0,
            Action::Down => cursor + 1,
            Action::Up if cursor == 0 => last,
            Action::Up => cursor - 1,
            Action::HalfPageDown => (cursor + page / 2).min(last),
            Action::HalfPageUp => cursor.saturating_sub(page / 2),
            Action::PageDown => (cursor + page).min(last),
            Action::PageUp => cursor.saturating_sub(page),
            Action::First => 0,
            Action::Last => last,
            Action::Expand => {
                self.expand_row(cursor);
                cursor
            }
            Action::Collapse => {
                let row = &self.rows[cursor];
                if cursor != 0 && self.tree.node(row.node).expanded {
                    // Collapsing the first directory of a run collapses the rest of it.
                    self.tree.collapse(row.head);
                    self.dirty = true;
                }
                cursor
            }
            Action::AlignCenter | Action::AlignTop | Action::AlignBottom => {
                let align = match action {
                    Action::AlignCenter => Align::Center,
                    Action::AlignTop => Align::Top,
                    _ => Align::Bottom,
                };
                self.start = viewport::align(&self.rows, height, cursor, align);
                cursor
            }
            // The other actions leave the cursor where it is.
            _ => cursor,
        };
        self.cursor = self.rows[target].node;
    }

    /// Expands every directory of the run that row `index` stands for.
    fn expand_row(&mut self, index: usize) {
        let row = &self.rows[index];
        let (head, mut node) = (row.head, Some(row.node));
        while let Some(id) = node {
            self.tree.expand(id);
            node = (id != head).then(|| self.tree.node(id).parent).flatten();
        }
        self.dirty = true;
    }

    /// Scrolls just enough to show the cursor with `scrolloff` rows around it.
    fn reveal_cursor(&mut self, scrolloff: usize) {
        if let Some(cursor) = self.cursor_row() {
            let height = self.height.max(1);
            self.start = viewport::reveal(&self.rows, self.start, height, cursor, scrolloff);
        }
    }

    /// Expands the directories leading to `path` (relative to the root), listing them first if
    /// needed. With `cursor` the cursor moves there once it is revealed.
    fn reveal(&mut self, path: PathBuf, cursor: bool) {
        self.reveals.push((path, cursor));
    }

    /// Brings everything derived from the tree up to date: pending reveals, listings, rows.
    fn update(&mut self, lister: &Lister, config: &FileTreeConfig) {
        if config.sort != self.sort {
            self.sort = config.sort;
            self.tree.set_sort(config.sort);
            self.dirty = true;
        }
        if config.flatten_dirs != self.flatten_dirs {
            self.flatten_dirs = config.flatten_dirs;
            self.dirty = true;
        }

        let mut unlisted = Vec::new();
        let mut revealed = false;
        self.reveals
            .retain(|(path, cursor)| match self.tree.reveal(path) {
                Reveal::Found(node) => {
                    if *cursor {
                        self.cursor = node;
                        self.scroll_to = Some(node);
                    }
                    revealed = true;
                    false
                }
                Reveal::Unlisted(dir) => {
                    // List the whole way down at once rather than one directory per round trip.
                    let dir = self.tree.path(dir);
                    unlisted.extend(
                        path.ancestors()
                            .skip(1)
                            .filter(|ancestor| ancestor.starts_with(&dir))
                            .map(Path::to_path_buf),
                    );
                    revealed = true;
                    true
                }
                Reveal::Missing => false,
            });
        self.dirty |= revealed;

        let requests = self.tree.take_listing_requests();
        let mut dirs: Vec<_> = requests.into_iter().map(|id| self.tree.path(id)).collect();
        dirs.extend(unlisted);
        if !dirs.is_empty() {
            // Parents first, so their children exist when the listings are merged.
            dirs.sort_by_key(|dir| dir.components().count());
            dirs.dedup();
            lister.request(ListRequest {
                generation: self.generation,
                root: self.root.clone(),
                dirs,
                options: ListOptions {
                    sort: self.sort,
                    executables: lister.executables,
                },
            });
        }

        if std::mem::take(&mut self.dirty) {
            self.rebuild_rows();
        }
    }

    /// Rebuilds `rows`, keeping the cursor and the first ordinary row on the same entries, or
    /// on their closest shown directory when they disappear from view or from disk.
    fn rebuild_rows(&mut self) {
        let path_of = |node| {
            self.rows
                .index_of(node)
                .map(|index| self.rows[index].path.clone())
        };
        let anchor = self
            .rows
            .get(self.start)
            .map(|row| (row.node, row.path.clone()));
        let cursor_path = path_of(self.cursor);
        self.rows = Rows::build(&self.tree, self.flatten_dirs, self.input_row());
        self.start = anchor
            .and_then(|(node, path)| self.shown_row(node, Some(&path)))
            .unwrap_or(self.start.min(self.rows.len() - 1));
        self.cursor = self
            .shown_row(self.cursor, cursor_path.as_deref())
            .map_or(self.tree.root(), |index| self.rows[index].node);
    }

    /// Where the input row for a new entry goes: after the directories for a file (when they
    /// come first), else first.
    fn input_row(&self) -> Option<InputRow> {
        let Some(Edit {
            kind: EditKind::Create { dir, directory, .. },
            ..
        }) = &self.edit
        else {
            return None;
        };
        let dir = *dir;
        if !self.tree.contains(dir) {
            return None;
        }
        let at = match (directory, self.sort) {
            (false, FileTreeSort::DirectoriesFirst) => self
                .tree
                .children(dir)
                .iter()
                .take_while(|child| self.tree.node(**child).kind.group() == Group::Directory)
                .count(),
            _ => 0,
        };
        Some(InputRow { dir, at })
    }

    /// The row of `node`, last seen at `path`, or of its closest ancestor that has one.
    fn shown_row(&self, node: NodeId, path: Option<&Path>) -> Option<usize> {
        let mut current = if self.tree.contains(node) {
            Some(node)
        } else {
            // The entry is gone; start from its closest ancestor that is still there.
            path?
                .ancestors()
                .find_map(|ancestor| self.tree.find(ancestor))
        };
        while let Some(node) = current {
            if let Some(index) = self.rows.index_of(node) {
                return Some(index);
            }
            current = self.tree.node(node).parent;
        }
        None
    }

    fn refresh_git(&mut self, editor: &Editor) {
        match self.git_refresh {
            GitRefresh::Idle => self.git_refresh = GitRefresh::Running,
            GitRefresh::Running | GitRefresh::RunningStale => {
                self.git_refresh = GitRefresh::RunningStale;
                return;
            }
        }
        let root = self.root.clone();
        let generation = self.generation;
        let trust_full = editor
            .workspace_trust
            .query(&helix_loader::find_workspace_in(&root).0, TrustQuery::Git)
            .is_trusted();
        let providers = editor.diff_providers.clone();
        tokio::spawn(async move {
            let statuses = tokio::task::spawn_blocking(move || {
                let options = StatusOptions {
                    staged: true,
                    ignored: true,
                };
                let changes = RefCell::new(Vec::new());
                // No repository simply means no marks.
                let _ = providers.for_each_status_entry(&root, trust_full, options, |change| {
                    if let Ok(change) = change {
                        changes.borrow_mut().push(change);
                    }
                    true
                });
                GitStatuses::new(&root, changes.into_inner())
            })
            .await;
            let Ok(statuses) = statuses else {
                return;
            };
            job::dispatch(move |editor, compositor| {
                if let Some(file_tree) = file_tree(compositor) {
                    file_tree.apply_git(generation, statuses, editor);
                }
            })
            .await;
        });
    }
}

/// The path of the focused buffer relative to `root`, if it lies below it.
fn focused_document_path(editor: &Editor, root: &Path) -> Option<PathBuf> {
    let doc = editor.document(editor.tree.get(editor.tree.focus).doc)?;
    Some(doc.path()?.strip_prefix(root).ok()?.to_path_buf())
}

fn file_tree(compositor: &mut Compositor) -> Option<&mut FileTree> {
    compositor
        .find::<EditorView>()
        .map(|editor_view| &mut editor_view.file_tree)
}

/// The `LS_COLORS` rules of the configured source, parsed once per source.
#[derive(Default)]
struct Palette {
    source: Option<LsColorsSource>,
    colors: Option<Arc<LsColors>>,
}

impl Palette {
    fn get(&mut self, source: &LsColorsSource) -> Option<Arc<LsColors>> {
        if self.source.as_ref() != Some(source) {
            self.colors = match source {
                LsColorsSource::Environment(false) => None,
                LsColorsSource::Environment(true) => LsColors::from_environment().map(Arc::new),
                LsColorsSource::Spec(spec) => Some(Arc::new(LsColors::parse(spec))),
            };
            self.source = Some(source.clone());
        }
        self.colors.clone()
    }
}

struct ListRequest {
    generation: u64,
    root: Arc<Path>,
    /// Relative to `root`, parents before their children.
    dirs: Vec<PathBuf>,
    options: ListOptions,
}

/// Hands directories to the background task that lists them.
struct Lister {
    sender: UnboundedSender<ListRequest>,
    /// Whether listings should tell executables apart.
    executables: bool,
}

impl Lister {
    fn request(&self, request: ListRequest) {
        // The task only ends with the runtime.
        let _ = self.sender.send(request);
    }
}

/// Starts the task that lists directories one request after another, so results arrive in the
/// order they were asked for.
fn spawn_lister() -> UnboundedSender<ListRequest> {
    let (sender, mut requests) = unbounded_channel::<ListRequest>();
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let ListRequest {
                generation,
                root,
                dirs,
                options,
            } = request;
            let listings =
                tokio::task::spawn_blocking(move || fs::list_all(&root, dirs, options)).await;
            let Ok(listings) = listings else {
                continue;
            };
            job::dispatch(move |editor, compositor| {
                if let Some(file_tree) = file_tree(compositor) {
                    file_tree.apply_listings(generation, listings, editor);
                }
            })
            .await;
        }
    });
    sender
}

#[cfg(test)]
mod tests {
    use super::tree::tests::{dir, file, run};
    use super::*;

    /// root, `docs`, `src/main` (a run), `a`, `b`
    fn workspace() -> Workspace {
        let mut workspace = Workspace::new("/root".into(), 1, &FileTreeConfig::default());
        let root = workspace.tree.root();
        workspace.tree.apply_listing(
            root,
            Some(vec![
                run("src", &["main"]),
                dir("docs"),
                file("a"),
                file("b"),
            ]),
        );
        workspace.rebuild_rows();
        workspace.height = 10;
        workspace
    }

    fn cursor_label(workspace: &Workspace) -> &str {
        let index = workspace.rows.index_of(workspace.cursor).unwrap();
        &workspace.rows[index].label
    }

    #[test]
    fn the_cursor_wraps_around() {
        let mut workspace = workspace();
        workspace.navigate(Action::Up);
        assert_eq!(cursor_label(&workspace), "b");
        workspace.navigate(Action::Down);
        assert_eq!(cursor_label(&workspace), "root");
        workspace.navigate(Action::PageDown);
        assert_eq!(cursor_label(&workspace), "b");
        workspace.navigate(Action::HalfPageUp);
        assert_eq!(cursor_label(&workspace), "root");
    }

    #[test]
    fn a_run_expands_and_collapses_as_one() {
        let mut workspace = workspace();
        let src = workspace.tree.find("src".as_ref()).unwrap();
        let main = workspace.tree.find("src/main".as_ref()).unwrap();
        workspace.cursor = main;
        workspace.navigate(Action::Expand);
        assert!(workspace.tree.node(src).expanded && workspace.tree.node(main).expanded);
        workspace
            .tree
            .apply_listing(main, Some(vec![file("lib.rs")]));
        workspace.rebuild_rows();
        let labels: Vec<_> = workspace.rows.iter().map(|row| &*row.label).collect();
        assert_eq!(labels, ["root", "docs", "src/main", "lib.rs", "a", "b"]);

        workspace.navigate(Action::Collapse);
        assert!(!workspace.tree.node(src).expanded && !workspace.tree.node(main).expanded);
        workspace.rebuild_rows();
        assert_eq!(cursor_label(&workspace), "src/main");
    }

    #[test]
    fn rebuilding_keeps_the_cursor_on_a_shown_row() {
        let mut workspace = workspace();
        let docs = workspace.tree.find("docs".as_ref()).unwrap();
        workspace.tree.expand(docs);
        workspace
            .tree
            .apply_listing(docs, Some(vec![file("guide.md")]));
        workspace.rebuild_rows();
        workspace.cursor = workspace.tree.find("docs/guide.md".as_ref()).unwrap();

        // Collapsing `docs` hides the cursor's row: it moves to `docs`.
        workspace.tree.collapse(docs);
        workspace.rebuild_rows();
        assert_eq!(workspace.cursor, docs);
    }
}
