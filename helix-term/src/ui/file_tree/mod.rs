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
mod mouse;
mod ops;
mod order;
mod render;
mod rows;
mod search;
mod tree;
mod viewport;
mod watch;

use std::{
    cell::RefCell,
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use helix_core::Position;
use helix_loader::workspace_trust::TrustQuery;
use helix_stdx::{
    path::{canonicalize, get_relative_path, normalize},
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
use tokio::sync::mpsc::unbounded_channel;
use tui::buffer::Buffer as Surface;

use self::{
    edit::{Edit, EditEvent, EditKind, Placement},
    fs::ListOptions,
    git::GitStatuses,
    keys::{Action, Lookup},
    ls_colors::LsColors,
    mouse::Gesture,
    order::Group,
    render::{natural_width, BufferMarks, EditRow, Scene, Styles},
    rows::{InputRow, Rows},
    search::{Candidates, Direction, Hit},
    tree::{Kind, Listing, NodeId, Reveal, Tree},
    viewport::Align,
    watch::Watcher,
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
    lister: Option<std::sync::mpsc::Sender<ListRequest>>,
    last_generation: u64,
    /// The keys of an unfinished sequence like `z`.
    pending: Vec<KeyEvent>,
    /// The widest the panel may get in the current screen.
    max_width: u16,
    /// Where the panel was laid out last.
    area: Option<Rect>,
    /// A press on the rail and the drag following it.
    gesture: Option<Gesture>,
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
        self.catch_up(editor);
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
        self.catch_up(editor);
        let lister = self.lister(editor);
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        // Files in collapsed directories are not watched, so their status may be old.
        workspace.refresh_git(editor);
        workspace.cursor = workspace.tree.root();
        if let Some(path) = focused_document_path(editor, &workspace.root) {
            workspace.reveal(path, Purpose::Cursor);
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
            workspace.reveal(path, Purpose::Show);
        }
        workspace.watcher = Watcher::new(workspace.generation)
            .inspect_err(|err| log::warn!("file tree cannot watch for changes: {err}"))
            .ok();
        let lister = self.lister(editor);
        workspace.update(&lister, &config.file_tree);
        workspace.refresh_git(editor);
        self.workspace = Some(workspace);
    }

    /// Starts over when the editor's working directory is no longer the tree's root.
    pub fn follow_working_directory(&mut self, editor: &Editor) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        if *workspace.root != helix_stdx::env::current_working_dir() {
            self.workspace = None;
            self.pending.clear();
            if self.is_presented() {
                self.ensure_workspace(editor);
            }
        }
    }

    /// Lists every expanded directory again and refreshes the git status, e.g. when the
    /// terminal gets focus back after other programs ran.
    pub fn refresh(&mut self, editor: &Editor) {
        if !self.is_presented() {
            if let Some(workspace) = &mut self.workspace {
                workspace.outdated = true;
            }
            return;
        }
        if let Some(workspace) = &mut self.workspace {
            workspace.tree.invalidate_all();
            workspace.refresh_git(editor);
        }
        self.update(editor);
    }

    /// Refreshes the git status, e.g. after a save.
    pub fn refresh_git(&mut self, editor: &Editor) {
        let presented = self.is_presented();
        if let Some(workspace) = &mut self.workspace {
            if presented {
                workspace.refresh_git(editor);
            } else {
                workspace.outdated = true;
            }
        }
    }

    /// Catches up with the changes that were let pass while the panel was hidden.
    fn catch_up(&mut self, editor: &Editor) {
        if self
            .workspace
            .as_mut()
            .is_some_and(|workspace| std::mem::take(&mut workspace.outdated))
        {
            self.refresh(editor);
        }
    }

    /// Handles the changes the watcher saw in the workspace `generation`.
    fn changed(&mut self, generation: u64, paths: HashSet<PathBuf>, editor: &Editor) {
        let presented = self.is_presented();
        let Some(workspace) = self.workspace_of(generation) else {
            return;
        };
        // A hidden tree does not need to follow along; it catches up when shown.
        if !presented {
            workspace.outdated = true;
            return;
        }
        for path in &paths {
            if let Ok(path) = path.strip_prefix(&workspace.root) {
                // A changed directory, or an entry that appeared or went in one.
                workspace.tree.invalidate(path);
                if let Some(parent) = path.parent() {
                    workspace.tree.invalidate(parent);
                }
            }
        }
        // Changes to files and to `.git` alike can change the git status.
        workspace.refresh_git(editor);
        self.update(editor);
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
        self.area = self.place(main, editor);
        self.area
    }

    fn place(&mut self, main: Rect, editor: &Editor) -> Option<Rect> {
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
            EditEvent::Continue if matches!(edit.kind, EditKind::Search) => {
                self.search_incrementally(cx.editor);
                return;
            }
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
        self.update_rows(editor);
        if let Some(workspace) = &mut self.workspace {
            workspace.reveal_cursor(editor.config().scrolloff);
        }
    }

    /// Brings the rows up to date after a change, leaving the scroll position alone.
    fn update_rows(&mut self, editor: &Editor) {
        let lister = self.lister(editor);
        if let Some(workspace) = &mut self.workspace {
            workspace.update(&lister, &editor.config().file_tree);
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
            Action::Search => self.start_search(editor),
            Action::NextMatch => self.find_next(Direction::Forward, editor),
            Action::PreviousMatch => self.find_next(Direction::Backward, editor),
            Action::OpenExternally => {
                if let Some(path) = self.workspace.as_ref().and_then(Workspace::cursor_path) {
                    let name = get_relative_path(&path).display().to_string();
                    match Url::from_file_path(&path) {
                        Ok(url) => {
                            editor
                                .set_status(format!("Opening '{name}' in the default application"));
                            cx.jobs.callback(crate::open_external_url_callback(url));
                        }
                        Err(()) => editor.set_error(format!("Cannot open '{name}'")),
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

    /// Opens the search prompt, collecting the files to search in the background.
    fn start_search(&mut self, editor: &Editor) {
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        workspace.start_search(editor);
        let (root, generation, sort) =
            (workspace.root.clone(), workspace.generation, workspace.sort);
        let config = editor.config().file_picker.clone();
        in_background(
            move || Candidates::collect(&root, &config, sort),
            move |file_tree, candidates, editor| {
                let Some(workspace) = file_tree.workspace_of(generation) else {
                    return;
                };
                workspace.search.candidates = Some(Arc::new(candidates));
                file_tree.search_incrementally(editor);
            },
        );
    }

    /// Moves the cursor to the first match of the query being typed after where it started.
    fn search_incrementally(&mut self, editor: &mut Editor) {
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let (Some(edit), Some(origin)) = (&workspace.edit, &workspace.search.origin) else {
            return;
        };
        let query = edit.prompt.line().clone();
        let (from, from_dir) = (origin.path.clone(), origin.is_dir);
        if query.trim().is_empty() {
            workspace.search.generation += 1;
            workspace.search.hit = None;
            workspace
                .reveals
                .retain(|reveal| reveal.purpose != Purpose::Match);
            workspace.cursor = origin.cursor;
            self.update(editor);
            return;
        }
        self.find(query, from, from_dir, Direction::Forward, true, editor);
    }

    /// Moves the cursor to the next or previous match of the last search.
    fn find_next(&mut self, direction: Direction, editor: &mut Editor) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let Some(index) = workspace.rows.index_of(workspace.cursor) else {
            return;
        };
        let row = &workspace.rows[index];
        let from_dir = workspace.tree.node(row.node).kind == Kind::Directory;
        let (query, from) = (workspace.search.query.clone(), row.path.clone());
        if !query.is_empty() {
            self.find(query, from, from_dir, direction, false, editor);
        }
    }

    /// Looks for `query` in the background. An `incremental` search moves the cursor back where
    /// it started when nothing matches; otherwise the search reports it like the editor's.
    fn find(
        &mut self,
        query: String,
        from: PathBuf,
        from_dir: bool,
        direction: Direction,
        incremental: bool,
        editor: &mut Editor,
    ) {
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        // Without candidates yet, the search runs once they are collected.
        let Some(candidates) = workspace.search.candidates.clone() else {
            return;
        };
        workspace.search.generation += 1;
        let (generation, search) = (workspace.generation, workspace.search.generation);
        in_background(
            move || candidates.find(&query, &from, from_dir, direction),
            move |file_tree, hit, editor| {
                let Some(workspace) = file_tree
                    .workspace_of(generation)
                    .filter(|workspace| workspace.search.generation == search)
                else {
                    return;
                };
                workspace.found(hit, incremental, editor);
                file_tree.update(editor);
            },
        );
        self.update(editor);
    }

    /// The workspace, if it is still the one of `generation`.
    fn workspace_of(&mut self, generation: u64) -> Option<&mut Workspace> {
        self.workspace
            .as_mut()
            .filter(|workspace| workspace.generation == generation)
    }

    pub fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let config = cx.editor.config();
        let lister = self.lister(cx.editor);
        let palette = self.palette.get(&config.file_tree.ls_colors);
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        workspace.update(&lister, &config.file_tree);

        // The search line goes above the rows.
        let placement = workspace.edit.as_ref().map(Edit::placement);
        let (top, area) = match placement {
            Some(Placement::Top) if area.height > 1 => {
                (Some(area.with_height(1)), area.clip_top(1))
            }
            _ => (None, area),
        };
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
        (workspace.rows_area, workspace.drawn_start) = (area, start);

        let marks = BufferMarks::new(cx.editor, &workspace.root);
        let styles = Styles::new(&cx.editor.theme);
        let highlight = workspace.highlight();
        let expanders = config
            .file_tree
            .expanders
            .characters()
            .map(|characters| characters.map(String::from));
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
            expanders: expanders
                .as_ref()
                .map(|[collapsed, expanded]| [collapsed.as_str(), expanded.as_str()]),
            side: config.file_tree.side,
            edit: workspace.edit_row(),
            highlight: highlight
                .as_ref()
                .map(|(index, chars)| (*index, chars.as_slice())),
        }
        .render(area, surface);

        // A panel fitted to its rows leaves little room to type in.
        let edit_area = top.or(edit_area).map(|edit_area| {
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
            .filter(|edit| edit.placement() == Placement::CommandLine)
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
            (Some(edit), _) if edit.placement() == Placement::CommandLine => {
                edit.prompt.cursor(area, editor)
            }
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
    /// Paths to reveal once their directories are listed.
    reveals: Vec<PendingReveal>,
    /// A node to scroll into view once the panel height is known.
    scroll_to: Option<NodeId>,
    /// The number of rows the panel showed last.
    height: usize,
    /// Where the rows were drawn last, and the first ordinary row drawn, for the mouse.
    rows_area: Rect,
    drawn_start: usize,
    /// A name or path being typed for a file operation.
    edit: Option<Edit>,
    /// Where the line of an inline edit was drawn last.
    edit_area: Option<Rect>,
    search: Search,
    watcher: Option<Watcher>,
    /// The repository's `.git` directory, which is watched for changes of the git status.
    git_dir: Option<PathBuf>,
    /// Whether changes were let pass while the panel was hidden.
    outdated: bool,
}

/// A path to reveal once the directories leading to it are listed.
struct PendingReveal {
    path: PathBuf,
    purpose: Purpose,
}

/// What a path is revealed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Show,
    /// Moving the cursor there.
    Cursor,
    /// Moving the cursor to a match of the search.
    Match,
}

/// The search of the tree.
#[derive(Default)]
struct Search {
    /// The files to search, once collected.
    candidates: Option<Arc<Candidates>>,
    /// The last query searched for, for `n` and `N`.
    query: String,
    /// Where the cursor was when the query being typed was started.
    origin: Option<Origin>,
    /// Tells the latest search from older ones still running.
    generation: u64,
    /// The latest match of the query being typed.
    hit: Option<Hit>,
}

struct Origin {
    cursor: NodeId,
    path: PathBuf,
    is_dir: bool,
    start: usize,
    /// The directories that were expanded, so the search can collapse the ones it expanded.
    expanded: HashSet<NodeId>,
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
        let git_dir = root
            .ancestors()
            .map(|dir| dir.join(".git"))
            .find(|git_dir| git_dir.is_dir());
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
            rows_area: Rect::default(),
            drawn_start: 0,
            edit: None,
            edit_area: None,
            search: Search::default(),
            watcher: None,
            git_dir,
            outdated: false,
        }
    }

    fn start_search(&mut self, editor: &Editor) {
        let Some(index) = self.rows.index_of(self.cursor) else {
            return;
        };
        let row = &self.rows[index];
        self.search = Search {
            origin: Some(Origin {
                cursor: self.cursor,
                path: row.path.clone(),
                is_dir: self.tree.node(row.node).kind == Kind::Directory,
                start: self.start,
                expanded: self.tree.expanded_directories().collect(),
            }),
            query: std::mem::take(&mut self.search.query),
            ..Search::default()
        };
        self.edit = Some(Edit::new(EditKind::Search, String::new(), editor));
    }

    /// Moves the cursor to the result of a search.
    fn found(&mut self, hit: Option<Hit>, incremental: bool, editor: &mut Editor) {
        self.reveals
            .retain(|reveal| reveal.purpose != Purpose::Match);
        match &hit {
            Some(hit) => {
                self.reveal(hit.path.clone(), Purpose::Match);
                if hit.wrapped && !incremental {
                    editor.set_status("Wrapped around file tree");
                }
            }
            None if incremental => {
                if let Some(origin) = &self.search.origin {
                    self.cursor = origin.cursor;
                }
            }
            None => editor.set_error("No more matches"),
        }
        if incremental {
            self.search.hit = hit;
        }
    }

    /// Ends the query being typed. Unless it is `kept`, the cursor and the scroll position go
    /// back to where they were; directories the search expanded collapse again, but for the
    /// ones leading to a kept cursor.
    fn finish_search(&mut self, kept: bool) {
        let Some(origin) = self.search.origin.take() else {
            return;
        };
        self.search.hit = None;
        if !kept {
            self.search.generation += 1;
            self.reveals
                .retain(|reveal| reveal.purpose != Purpose::Match);
            if self.tree.contains(origin.cursor) {
                self.cursor = origin.cursor;
            }
            self.start = origin.start;
        }
        let expanded: Vec<_> = self
            .tree
            .expanded_directories()
            .filter(|dir| !origin.expanded.contains(dir))
            .collect();
        for dir in expanded {
            let keep =
                kept && self.tree.contains(self.cursor) && self.tree.is_within(self.cursor, dir);
            if self.tree.contains(dir) && !keep {
                self.tree.collapse(dir);
            }
        }
        self.dirty = true;
    }

    /// The row of the latest match and the characters of its label that matched.
    fn highlight(&self) -> Option<(usize, Vec<usize>)> {
        let hit = self.search.hit.as_ref()?;
        let index = self.rows.index_of(self.tree.find(&hit.path)?)?;
        // The label is the end of the matched path.
        let path_len = hit.path.to_string_lossy().chars().count();
        let offset = path_len - self.rows[index].label.chars().count();
        let chars = hit
            .indices
            .iter()
            .filter_map(|&i| (i as usize).checked_sub(offset))
            .collect();
        Some((index, chars))
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
            EditKind::Move { .. } | EditKind::Delete { .. } | EditKind::Search => return None,
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
        self.finish_search(false);
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
            Action::Open if kind == Kind::Directory && !root => self.toggle_row(index),
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
        if let EditKind::Search = edit.kind {
            let kept = !line.trim().is_empty();
            if kept {
                self.search.query = line.clone();
            }
            self.finish_search(kept);
            return Focus::Keep;
        }
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
            EditKind::Search => return Focus::Keep,
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
            self.reveal(to.to_path_buf(), Purpose::Cursor);
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

    /// Expands or collapses the directory of row `index`.
    fn toggle_row(&mut self, index: usize) {
        let row = &self.rows[index];
        if self.tree.node(row.node).expanded {
            // Collapsing the first directory of a run collapses the rest of it.
            self.tree.collapse(row.head);
            self.dirty = true;
        } else {
            self.expand_row(index);
        }
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
    /// needed, for `purpose`.
    fn reveal(&mut self, path: PathBuf, purpose: Purpose) {
        self.reveals.push(PendingReveal { path, purpose });
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
        self.reveals.retain(
            |PendingReveal { path, purpose }| match self.tree.reveal(path) {
                Reveal::Found(node) => {
                    if *purpose != Purpose::Show {
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
            },
        );
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
            self.watch();
        }
    }

    /// Watches the directories the tree has listed, and `.git`.
    fn watch(&mut self) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        let mut dirs: HashSet<_> = self
            .tree
            .loaded_directories()
            .map(|dir| self.root.join(self.tree.path(dir)))
            .collect();
        dirs.extend(self.git_dir.clone());
        watcher.watch(dirs);
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
        in_background(
            move || {
                let options = StatusOptions { staged: true };
                let changes = RefCell::new(Vec::new());
                // No repository simply means no marks.
                let _ = providers.for_each_status_entry(&root, trust_full, options, |change| {
                    if let Ok(change) = change {
                        changes.borrow_mut().push(change);
                    }
                    true
                });
                GitStatuses::new(&root, changes.into_inner())
            },
            move |file_tree, statuses, editor| file_tree.apply_git(generation, statuses, editor),
        );
    }
}

/// Runs `work` in the background, then `apply` with its result on the file tree.
///
/// The work gets a thread of its own rather than one of tokio's blocking threads, as the file
/// picker's walk does: quitting waits for those, and a git status can take seconds.
fn in_background<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    apply: impl FnOnce(&mut FileTree, T, &mut Editor) + Send + 'static,
) {
    let (sender, result) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    tokio::spawn(async move {
        let Ok(result) = result.await else {
            return;
        };
        job::dispatch(move |editor, compositor| {
            if let Some(file_tree) = file_tree(compositor) {
                apply(file_tree, result, editor);
            }
        })
        .await;
    });
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
                LsColorsSource::Environment(true) => Some(Arc::new(
                    LsColors::from_environment().unwrap_or_else(LsColors::gnu),
                )),
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

/// Hands directories to the background thread that lists them.
struct Lister {
    sender: std::sync::mpsc::Sender<ListRequest>,
    /// Whether listings should tell executables apart.
    executables: bool,
}

impl Lister {
    fn request(&self, request: ListRequest) {
        // The thread only stops once the tree is gone.
        let _ = self.sender.send(request);
    }
}

/// Starts listing directories in the background, one request after another so that the results
/// arrive in the order they were asked for. Like [`in_background`], the listing has a thread of
/// its own.
fn spawn_lister() -> std::sync::mpsc::Sender<ListRequest> {
    let (sender, requests) = std::sync::mpsc::channel::<ListRequest>();
    let (listed, mut results) = unbounded_channel();
    std::thread::spawn(move || {
        for request in requests {
            let listings = fs::list_all(&request.root, request.dirs, request.options);
            if listed.send((request.generation, listings)).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some((generation, listings)) = results.recv().await {
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
    pub(super) fn workspace() -> Workspace {
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

    /// Starts a search at the root, then expands `docs` and moves the cursor into it, as a match
    /// would.
    fn searched_into_docs() -> Workspace {
        let mut workspace = workspace();
        let root = workspace.tree.root();
        workspace.search.origin = Some(Origin {
            cursor: root,
            path: PathBuf::new(),
            is_dir: true,
            start: 0,
            expanded: workspace.tree.expanded_directories().collect(),
        });
        let docs = workspace.tree.find("docs".as_ref()).unwrap();
        workspace.tree.expand(docs);
        workspace
            .tree
            .apply_listing(docs, Some(vec![file("guide.md")]));
        workspace.cursor = workspace.tree.find("docs/guide.md".as_ref()).unwrap();
        workspace
    }

    #[test]
    fn a_cancelled_search_goes_back_where_it_started() {
        let mut workspace = searched_into_docs();
        workspace.finish_search(false);
        let docs = workspace.tree.find("docs".as_ref()).unwrap();
        assert!(!workspace.tree.node(docs).expanded);
        assert_eq!(workspace.cursor, workspace.tree.root());
    }

    #[test]
    fn a_finished_search_keeps_the_way_to_its_match() {
        let mut workspace = searched_into_docs();
        let src = workspace.tree.find("src".as_ref()).unwrap();
        workspace.tree.expand(src);
        workspace.finish_search(true);
        let docs = workspace.tree.find("docs".as_ref()).unwrap();
        assert!(workspace.tree.node(docs).expanded);
        assert!(!workspace.tree.node(src).expanded);
        assert_eq!(
            workspace.cursor,
            workspace.tree.find("docs/guide.md".as_ref()).unwrap()
        );
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
