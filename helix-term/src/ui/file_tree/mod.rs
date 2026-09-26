//! The file tree docked beside the editor.
//!
//! [`FileTree`] is owned by the [`EditorView`], which reserves its columns, draws it and hands it
//! keys while it is focused. Directory listings and git status are read in the background; the
//! results come back through the job queue, which finds the tree in the compositor.

mod background;
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
mod workspace;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use helix_core::Position;
use helix_stdx::{path::get_relative_path, Url};
use helix_view::{
    editor::{FileTreeSide, LsColors as LsColorsSource},
    graphics::{CursorKind, Rect},
    input::KeyEvent,
    Editor,
};
use tui::buffer::Buffer as Surface;

use self::{
    background::{in_background, spawn_lister, ListRequest, Lister},
    edit::{Edit, EditEvent, EditKind, Placement},
    git::GitStatuses,
    keys::{Action, Lookup},
    ls_colors::LsColors,
    mouse::Gesture,
    render::{natural_width, BufferMarks, Scene, Styles},
    search::{Candidates, Direction},
    tree::{Kind, Listing},
    viewport::Align,
    watch::Watcher,
    workspace::{Focus, GitRefresh, Purpose, Workspace},
};
use crate::{
    compositor::{Component, Compositor, Context, Event, EventResult},
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
