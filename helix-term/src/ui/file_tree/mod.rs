//! The file tree docked beside the editor.
//!
//! [`FileTree`] is owned by the [`EditorView`], which reserves its columns, draws it and hands it
//! keys while it is focused. Directory listings and git status are read in the background; the
//! results come back through the job queue, which finds the tree in the compositor.

// Parts of the model are only used by the navigation that follows.
#![allow(dead_code)]

mod fs;
mod git;
mod icons;
mod ls_colors;
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

use helix_loader::workspace_trust::TrustQuery;
use helix_vcs::StatusOptions;
use helix_view::{
    editor::{FileTreeConfig, FileTreeSide, FileTreeSort, LsColors as LsColorsSource},
    graphics::Rect,
    smooth_scroll::SmoothOffset,
    Editor,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tui::buffer::Buffer as Surface;

use self::{
    fs::ListOptions,
    git::GitStatuses,
    ls_colors::LsColors,
    render::{natural_width, BufferMarks, Scene, Styles},
    rows::Rows,
    tree::{Listing, NodeId, Reveal, Tree},
    viewport::Align,
};
use crate::{
    compositor::{Compositor, Context},
    job,
    ui::EditorView,
};

/// The narrowest and widest the panel gets, its rail included.
const MIN_WIDTH: u16 = 16;
const MAX_WIDTH: u16 = 64;
/// The columns the panel always leaves to the editor; with fewer it yields.
const MIN_EDITOR_WIDTH: u16 = 20;

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

    pub fn unfocus(&mut self) {
        self.focused = false;
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
        let workspace = self
            .workspace
            .as_ref()
            .filter(|workspace| workspace.ready)?;
        let config = editor.config();
        let width = *self.width.get_or_insert_with(|| {
            let widest = workspace
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| natural_width(row, index == 0, config.file_tree.icons))
                .max()
                .unwrap_or_default();
            // one more column for the rail
            (widest as u16 + 1).clamp(MIN_WIDTH, MAX_WIDTH)
        });
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

    pub fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let config = cx.editor.config();
        let lister = self.lister(cx.editor);
        let palette = self.palette.get(&config.file_tree.ls_colors);
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        workspace.update(&lister, &config.file_tree);

        let height = area.height as usize;
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
        Scene {
            tree: &workspace.tree,
            rows: &workspace.rows,
            git: &workspace.git,
            marks: &marks,
            palette: palette.as_deref(),
            styles: &styles,
            cursor: self
                .focused
                .then(|| workspace.rows.index_of(workspace.cursor))
                .flatten(),
            start,
            icons: config.file_tree.icons,
            guides: config.file_tree.guides,
            side: config.file_tree.side,
        }
        .render(area, surface);
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
        let rows = Rows::build(&tree, config.flatten_dirs);
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
    /// on their closest shown directory when they disappear from view.
    fn rebuild_rows(&mut self) {
        let anchor = self.rows.get(self.start).map(|row| row.node);
        self.rows = Rows::build(&self.tree, self.flatten_dirs);
        self.start = anchor
            .and_then(|anchor| self.shown_row(anchor))
            .unwrap_or(self.start.min(self.rows.len() - 1));
        self.cursor = self
            .shown_row(self.cursor)
            .map_or(self.tree.root(), |index| self.rows[index].node);
    }

    /// The row of `node` or of its closest ancestor that has one.
    fn shown_row(&self, node: NodeId) -> Option<usize> {
        let mut current = Some(node).filter(|node| self.tree.contains(*node));
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
