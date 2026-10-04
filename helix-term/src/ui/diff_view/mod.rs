//! The diff view: two read-only panes side by side, zoomed over the editor, one per text of a
//! diff. Their rows line up, and they scroll together. A diff of many files lists them in the
//! diff tree, docked where the file tree docks.
//!
//! [`DiffView`] is owned by the [`EditorView`]. It works out how the texts line up in the
//! background, opens the panes once that is known, and keeps them in step while they are shown.

pub(crate) mod inline;
mod panes;
pub(crate) mod run;
mod set;
pub(crate) mod styles;
mod tree;

pub use panes::{changed_text, goto_end_hunk, goto_hunk, open_file, Rows};

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail};
use helix_core::{
    movement::Direction,
    syntax::{config::LanguageConfiguration, Loader},
    Position, Rope, Selection, Syntax,
};
use helix_loader::workspace_trust::TrustQuery;
use helix_stdx::path::get_relative_path;
use helix_view::{
    align_view, current_ref,
    diff_view::{builtin::Stats, Alignment, Pane, Side, Wrap},
    doc, doc_mut,
    document::from_reader,
    editor::{Action, DiffTool},
    graphics::{CursorKind, Rect},
    input::{KeyEvent, MouseEvent},
    Align, Document, DocumentId, Editor, ViewId,
};
use tokio::task::JoinHandle;
use tui::buffer::Buffer as Surface;

use self::{
    run::Outcome,
    set::{DiffSet, Reader, Text},
    tree::DiffTree,
};
use crate::{
    compositor::{Context, EventResult},
    job,
    ui::EditorView,
};

/// How long a buffer rests after a change before its diff is worked out again.
const REDIFF_DELAY: Duration = Duration::from_millis(250);
/// How often the lines added and removed of a diff of many reach the diff tree.
const STATS_INTERVAL: Duration = Duration::from_millis(100);

/// Two texts to compare, and what they are.
#[derive(Debug, Clone)]
pub struct Request {
    /// The path of the file the texts are of, relative where it can be, which picks the
    /// language.
    path: PathBuf,
    old: Rope,
    new: Rope,
    /// The names of the panes, old and new.
    names: [String; 2],
    /// The buffer whose text the new one is, and its version then.
    buffer: Option<(DocumentId, i32)>,
    /// The file `gf` opens.
    file: Option<PathBuf>,
    /// The view focused when the diff was asked for.
    origin: Option<ViewId>,
    /// A buffer shown while the diff is worked out, closed once the panes show.
    placeholder: Option<DocumentId>,
    /// The file of the diff of many it is, by its index.
    of_many: Option<usize>,
}

impl Request {
    /// The diff of the focused buffer against the version committed at HEAD.
    pub fn buffer_against_head(editor: &Editor) -> anyhow::Result<Self> {
        let (view, doc) = current_ref!(editor);
        if doc.diff_view.is_some() {
            bail!("This is a diff already");
        }
        let path = doc
            .path()
            .ok_or_else(|| anyhow!("{} has no file", doc.display_name()))?;
        let name = doc.display_name().into_owned();
        let trust = editor
            .workspace_trust
            .query(doc.workspace_root(), TrustQuery::Git)
            .is_trusted();
        let base = editor
            .diff_providers
            .get_diff_base(path, trust)
            .ok_or_else(|| anyhow!("{name} has no committed version"))?;
        let (base, ..) = from_reader(&mut base.as_slice(), Some(doc.encoding()))?;
        Ok(Self {
            path: doc.relative_path().unwrap_or(path).to_path_buf(),
            old: base,
            new: doc.text().clone(),
            names: [format!("{name} (HEAD)"), name],
            buffer: Some((doc.id(), doc.version())),
            file: Some(path.to_path_buf()),
            origin: Some(view.id),
            placeholder: None,
            of_many: None,
        })
    }

    /// The diff of the file `old` and the file `new`, either of which may be `/dev/null`, as
    /// when git runs a diff tool for a file added or deleted. They are versions of the file at
    /// `path` if given, as git's temporary files are of `$MERGED`: named after it, and `gf`
    /// opens it.
    pub fn files(
        old: &Path,
        new: &Path,
        path: Option<&Path>,
        placeholder: Option<DocumentId>,
    ) -> anyhow::Result<Self> {
        let null = Path::new("/dev/null");
        let shown = |path: &Path| get_relative_path(path).display().to_string();
        let new_file = (new != null).then(|| new.to_path_buf());
        let (path, names, file) = match path {
            Some(path) => {
                let name = shown(path);
                let file = Some(path.to_path_buf()).filter(|path| path.is_file());
                (
                    path,
                    [format!("{name} (old)"), format!("{name} (new)")],
                    file.or(new_file),
                )
            }
            None => (
                if new == null { old } else { new },
                [shown(old), shown(new)],
                new_file,
            ),
        };
        Ok(Self {
            path: get_relative_path(path).into_owned(),
            old: set::read(old)?,
            new: set::read(new)?,
            names,
            buffer: None,
            file,
            origin: None,
            placeholder,
            of_many: None,
        })
    }

    /// The diff of the `index`th file of the diff of many `files`.
    fn of_many(files: &Files, index: usize) -> anyhow::Result<Self> {
        let file = &files.set.files[index];
        Ok(Self {
            path: file.path.clone(),
            old: files.reader.read(&file.old)?,
            new: files.reader.read(&file.new)?,
            names: files.set.names(file),
            buffer: None,
            file: match &file.new {
                Text::File(path) => Some(path.clone()),
                Text::Head(_) | Text::Missing => None,
            },
            origin: files.origin,
            placeholder: files.placeholder,
            of_many: Some(index),
        })
    }

    /// The same diff of the texts as they are now, if its buffer changed since.
    fn refreshed(&self, editor: &Editor) -> Option<Self> {
        let (doc_id, version) = self.buffer?;
        let doc = editor.document(doc_id)?;
        if doc.version() == version {
            return None;
        }
        let trust = editor
            .workspace_trust
            .query(doc.workspace_root(), TrustQuery::Git)
            .is_trusted();
        let base = editor.diff_providers.get_diff_base(doc.path()?, trust)?;
        let (base, ..) = from_reader(&mut base.as_slice(), Some(doc.encoding())).ok()?;
        Some(Self {
            old: base,
            new: doc.text().clone(),
            buffer: Some((doc_id, doc.version())),
            ..self.clone()
        })
    }
}

/// A diff worked out: its texts, how they line up, and their syntax.
#[derive(Clone)]
struct Diff {
    request: Request,
    outcome: Outcome,
    parsed: Parsed,
}

/// The syntax of the texts of a diff, parsed in the background.
#[derive(Clone, Default)]
struct Parsed {
    language: Option<Arc<LanguageConfiguration>>,
    /// The syntax trees of the old and the new text.
    syntaxes: [Option<Syntax>; 2],
}

impl Parsed {
    /// The syntax of the texts the panes `docs` show.
    fn of_panes(docs: [DocumentId; 2], editor: &Editor) -> Self {
        let docs = docs.map(|doc| editor.document(doc));
        Self {
            language: docs[0].and_then(|doc| doc.language.clone()),
            syntaxes: docs.map(|doc| doc.and_then(|doc| doc.syntax.clone())),
        }
    }
}

/// The files a diff of many compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Many {
    /// The files of two directories.
    Directories(PathBuf, PathBuf),
    /// The files below a directory that changed since HEAD.
    Changes(PathBuf),
}

/// Opens the diff the command line asks for with `paths`: of a file against its committed
/// version, of two files or two directories, or of the changes since HEAD below a directory, the
/// working directory without paths. A third path names what two files are versions of, as git's
/// `$MERGED` does; an empty one, as of a directory diff, names nothing.
pub fn open_paths(
    paths: &[PathBuf],
    editor: &mut Editor,
    view: &mut DiffView,
) -> anyhow::Result<()> {
    let paths = match paths {
        [rest @ .., last] if last.as_os_str().is_empty() => rest,
        _ => paths,
    };
    let many = match paths {
        [] => Many::Changes(helix_stdx::env::current_working_dir()),
        [dir] if dir.is_dir() => Many::Changes(dir.clone()),
        [path] => {
            editor.open(path, Action::VerticalSplit)?;
            let request = Request::buffer_against_head(editor)?;
            view.open(request, editor);
            return Ok(());
        }
        [old, new] if old.is_dir() && new.is_dir() => Many::Directories(old.clone(), new.clone()),
        [old, new] | [old, new, _] if !old.is_dir() && !new.is_dir() => {
            // The editor quits without a view, so one waits for the panes.
            let placeholder = editor.new_file(Action::VerticalSplit);
            let path = paths.get(2).map(PathBuf::as_path);
            let request = Request::files(old, new, path, Some(placeholder))?;
            view.open(request, editor);
            return Ok(());
        }
        _ => bail!("--diff takes one or two files (and a path naming two), or directories"),
    };
    let placeholder = editor.new_file(Action::VerticalSplit);
    view.open_many(many, None, Some(placeholder), editor);
    Ok(())
}

/// The two panes shown.
struct Pair {
    /// The documents of the old and the new side.
    panes: [DocumentId; 2],
    views: [ViewId; 2],
    request: Request,
    outcome: Outcome,
}

/// A diff of many files, browsed in the diff tree.
struct Files {
    set: DiffSet,
    reader: Reader,
    tree: DiffTree,
    /// The file shown in the panes, or shown last when they closed to open it, by its index.
    current: Option<usize>,
    /// The view focused when the diff was asked for.
    origin: Option<ViewId>,
    /// A buffer shown until the first file's panes show.
    placeholder: Option<DocumentId>,
    /// Whether the panes closed to open their file, which keeps the tree.
    keep: bool,
    /// Whether the file being opened shows its last hunk rather than its first, as `[g` coming
    /// from the next file asks.
    last_hunk: bool,
    /// The diff of the next file in the tree, worked out ahead so that `]g` gets there fast.
    prefetch: Option<(usize, JoinHandle<()>)>,
    /// The diffs of the files next to the one shown, by their index: worked out ahead, or shown
    /// last, so that `]g` and `[g` get there fast.
    cache: Vec<(usize, Diff)>,
}

impl Files {
    /// Keeps the diff of the `index`th file, forgetting the ones no longer next to `current`.
    fn cache(&mut self, index: usize, diff: Diff, current: usize) {
        self.cache.retain(|(cached, _)| *cached != index);
        self.cache.push((index, diff));
        self.cache
            .retain(|(cached, _)| cached.abs_diff(current) <= 1);
    }
}

#[derive(Default)]
pub struct DiffView {
    /// Counts the diffs asked for, so that the late result of an earlier one is dropped.
    generation: u64,
    task: Option<JoinHandle<()>>,
    /// Whether the task works out the diff of the pair shown anew.
    refreshing: bool,
    pair: Option<Pair>,
    /// Counts the diffs of many asked for, likewise.
    many_generation: u64,
    files: Option<Files>,
}

impl DiffView {
    /// Works out the diff of `request` in the background and shows it once known.
    pub fn open(&mut self, request: Request, editor: &mut Editor) {
        if editor.config().diff.tool == DiffTool::Difftastic && run::has_difft() {
            editor.set_status(format!(
                "Diffing {} with difftastic…",
                request.path.display()
            ));
        }
        self.start(request, false, Duration::ZERO, editor);
    }

    fn start(&mut self, request: Request, refresh: bool, delay: Duration, editor: &Editor) {
        if let Some(task) = self.task.take() {
            // Dropping its `difft` stops it.
            task.abort();
        }
        self.generation += 1;
        self.refreshing = refresh;
        let generation = self.generation;
        let (tool, loader) = (editor.config().diff.tool, editor.syn_loader.load_full());
        self.task = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // Refreshed panes keep their syntax trees up to date themselves.
            let diff = work_out(tool, request, (!refresh).then_some(loader)).await;
            job::dispatch(move |editor, compositor| {
                if let Some(view) = compositor.find::<EditorView>() {
                    view.diff_view.ready(generation, diff, editor);
                }
            })
            .await;
        }));
    }

    /// Shows `diff`, asked for as the `generation`th, unless another one was asked for since.
    fn ready(&mut self, generation: u64, diff: Diff, editor: &mut Editor) {
        if generation != self.generation {
            return;
        }
        self.task = None;
        let Diff {
            request,
            outcome,
            parsed,
        } = diff;
        match &outcome.fallback {
            Some(reason) => editor.set_status(format!("{reason}: showing Helix's own diff")),
            None if !self.refreshing => editor.clear_status(),
            None => {}
        }
        if self.refreshing {
            if let Some(pair) = &mut self.pair {
                refresh(pair, request, outcome, editor);
                return;
            }
        }
        let of_many = request.of_many;
        if let Some(pair) = self.pair.take() {
            // The file left stays at hand for going back.
            if let (Some(files), Some(left), Some(index)) =
                (&mut self.files, pair.request.of_many, of_many)
            {
                let left_diff = Diff {
                    parsed: Parsed::of_panes(pair.panes, editor),
                    request: pair.request,
                    outcome: pair.outcome,
                };
                files.cache(left, left_diff, index);
            }
            editor.close_diff_panes(pair.panes, None);
        }
        self.pair = Some(show(request, outcome, parsed, editor));
        match (&mut self.files, of_many) {
            (Some(files), Some(index)) => {
                files.current = Some(index);
                files.placeholder = None;
                if std::mem::take(&mut files.last_hunk) {
                    goto_end_hunk(editor, true);
                }
                self.prefetch(index + 1, editor);
            }
            // A diff of one file ends a diff of many.
            (_, None) => self.end_many(),
            (None, Some(_)) => {}
        }
    }

    /// Notices the panes closing, and works the diff out anew once its buffer changed.
    pub fn follow(&mut self, editor: &mut Editor) {
        let Some(pair) = &self.pair else {
            return;
        };
        let intact = pair.views.iter().zip(&pair.panes).all(|(&view, &doc)| {
            editor
                .tree
                .try_get(view)
                .is_some_and(|view| view.doc == doc)
        });
        if !intact {
            let pair = self.pair.take().expect("checked above");
            editor.close_diff_panes(pair.panes, None);
            if let Some(task) = self.task.take() {
                task.abort();
            }
            // Closing the panes ends a diff of many, unless they closed to open their file.
            match &mut self.files {
                Some(files) if files.keep => files.keep = false,
                _ => self.end_many(),
            }
            return;
        }
        if self.task.is_none() {
            if let Some(request) = pair.request.refreshed(editor) {
                self.start(request, true, REDIFF_DELAY, editor);
            }
        }
    }

    /// Keeps the panes in step once the editor is laid out for a frame: they wrap their lines
    /// as their views are wide now, the one without the focus scrolls along with the focused one,
    /// and its cursor goes to the same row.
    pub fn follow_size(&mut self, editor: &mut Editor) {
        let Some(pair) = &self.pair else {
            return;
        };
        wrap(pair, editor);
        if let Some(index) = pair
            .views
            .iter()
            .position(|&view| view == editor.tree.focus)
        {
            panes::sync(
                editor,
                [pair.views[index], pair.views[1 - index]],
                [pair.panes[index], pair.panes[1 - index]],
            );
        }
    }

    /// Keeps the diff tree when the panes close next, as they do to open their file.
    pub fn keep_files(&mut self) {
        if let Some(files) = &mut self.files {
            files.keep = true;
        }
    }

    /// Looks for the files of `many` in the background and shows the first one's diff once
    /// found, with the others in the diff tree. `origin` is the view focused when it was asked
    /// for, `placeholder` a buffer shown meanwhile.
    pub fn open_many(
        &mut self,
        many: Many,
        origin: Option<ViewId>,
        placeholder: Option<DocumentId>,
        editor: &mut Editor,
    ) {
        self.many_generation += 1;
        let generation = self.many_generation;
        let config = editor.config();
        let sort = config.file_tree.sort;
        let dir = match &many {
            Many::Directories(_, new) => new.clone(),
            Many::Changes(dir) => dir.clone(),
        };
        let reader = Reader {
            providers: editor.diff_providers.clone(),
            trust: editor
                .workspace_trust
                .query(&helix_loader::find_workspace_in(&dir).0, TrustQuery::Git)
                .is_trusted(),
        };
        editor.set_status(match &many {
            Many::Directories(..) => "Comparing the directories…",
            Many::Changes(_) => "Looking for changes…",
        });
        let providers = reader.providers.clone();
        job::in_background(
            move || match &many {
                Many::Directories(old, new) => DiffSet::of_directories(old, new, sort),
                Many::Changes(dir) => DiffSet::of_changes(dir, &providers, reader.trust, sort),
            },
            move |editor, compositor, set| {
                if let Some(view) = compositor.find::<EditorView>() {
                    let found = (generation, set, origin, placeholder);
                    view.diff_view.many_ready(found, reader.clone(), editor);
                }
            },
        );
    }

    /// Shows the first file of a diff of many found, unless another one was asked for since.
    #[allow(clippy::type_complexity)]
    fn many_ready(
        &mut self,
        (generation, set, origin, placeholder): (
            u64,
            anyhow::Result<DiffSet>,
            Option<ViewId>,
            Option<DocumentId>,
        ),
        reader: Reader,
        editor: &mut Editor,
    ) {
        if generation != self.many_generation {
            return;
        }
        let set = match set {
            Ok(set) => set,
            Err(err) => {
                editor.set_error(format!("{err:#}"));
                return;
            }
        };
        if set.files.is_empty() {
            editor.set_status("No changes");
            return;
        }
        editor.clear_status();
        self.end_many();
        spawn_stats(&set, reader.clone(), generation);
        let tree = DiffTree::new(&set, &editor.config().file_tree);
        self.files = Some(Files {
            set,
            reader,
            tree,
            current: None,
            origin,
            placeholder,
            keep: false,
            last_hunk: false,
            prefetch: None,
            cache: Vec::new(),
        });
        self.show_file(0, editor);
    }

    /// Shows the diff of the `index`th file of the diff of many.
    fn show_file(&mut self, index: usize, editor: &mut Editor) {
        let Some(files) = &mut self.files else {
            return;
        };
        // Worked out ahead: shown at once.
        if let Some(cached) = files.cache.iter().position(|(cached, _)| *cached == index) {
            let (_, diff) = files.cache.swap_remove(cached);
            self.generation += 1;
            self.ready(self.generation, diff, editor);
            return;
        }
        match Request::of_many(files, index) {
            Ok(request) => self.open(request, editor),
            Err(err) => editor.set_error(format!("{err:#}")),
        }
    }

    /// Works out the diff of the `index`th file of the diff of many ahead, if there is one.
    fn prefetch(&mut self, index: usize, editor: &Editor) {
        let Some(files) = &mut self.files else {
            return;
        };
        if index >= files.set.files.len()
            || files.prefetch.as_ref().is_some_and(|(i, _)| *i == index)
            || files.cache.iter().any(|(cached, _)| *cached == index)
        {
            return;
        }
        let Ok(request) = Request::of_many(files, index) else {
            return;
        };
        if let Some((_, task)) = files.prefetch.take() {
            task.abort();
        }
        let (generation, tool) = (self.many_generation, editor.config().diff.tool);
        let loader = editor.syn_loader.load_full();
        files.prefetch = Some((
            index,
            tokio::spawn(async move {
                let diff = work_out(tool, request, Some(loader)).await;
                job::dispatch(move |_, compositor| {
                    let Some(view) = compositor.find::<EditorView>() else {
                        return;
                    };
                    let diff_view = &mut view.diff_view;
                    if let Some(files) = diff_view
                        .files
                        .as_mut()
                        .filter(|_| diff_view.many_generation == generation)
                    {
                        files.prefetch = None;
                        if let Some(current) = files.current {
                            files.cache(index, diff, current);
                        }
                    }
                })
                .await;
            }),
        ));
    }

    /// Shows the next file's diff, at its first hunk, or the previous one's at its last: where
    /// `]g` and `[g` go past the hunks of a file of a diff of many.
    pub fn goto_next_file(&mut self, direction: Direction, editor: &mut Editor) {
        let Some(files) = &mut self.files else {
            return;
        };
        let Some(current) = files.current else {
            return;
        };
        let next = match direction {
            Direction::Forward => current + 1,
            Direction::Backward => match current.checked_sub(1) {
                Some(previous) => previous,
                None => return,
            },
        };
        if next >= files.set.files.len() {
            return;
        }
        files.last_hunk = direction == Direction::Backward;
        self.show_file(next, editor);
    }

    fn stats_ready(&mut self, generation: u64, stats: Vec<(PathBuf, Stats)>) {
        if let Some(files) = self
            .files
            .as_mut()
            .filter(|_| self.many_generation == generation)
        {
            files.tree.set_stats(stats);
        }
    }

    fn end_many(&mut self) {
        if let Some(files) = self.files.take() {
            if let Some((_, task)) = files.prefetch {
                task.abort();
            }
        }
    }

    /// Whether a diff of many shows its files in the diff tree.
    pub fn has_tree(&self) -> bool {
        self.files.is_some()
    }

    pub fn tree_focused(&self) -> bool {
        self.files
            .as_ref()
            .is_some_and(|files| files.tree.is_focused())
    }

    /// Focuses the diff tree on the file shown, or gives the focus back.
    pub fn toggle_tree_focus(&mut self) {
        let Some(files) = &mut self.files else {
            return;
        };
        if files.tree.is_focused() {
            files.tree.unfocus();
        } else {
            let current = files
                .current
                .map(|index| files.set.files[index].path.clone());
            files.tree.focus(current.as_deref());
        }
    }

    pub fn toggle_tree(&mut self) {
        if let Some(files) = &mut self.files {
            files.tree.toggle();
        }
    }

    pub fn unfocus_tree(&mut self) {
        if let Some(files) = &mut self.files {
            files.tree.unfocus();
        }
    }

    /// The diff tree's area in `main`, if it is shown and fits.
    pub fn layout_tree(&mut self, main: Rect, editor: &Editor) -> Option<Rect> {
        self.files.as_mut()?.tree.layout(main, editor)
    }

    pub fn render_tree(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        if let Some(files) = &mut self.files {
            let current = files
                .current
                .map(|index| files.set.files[index].path.as_path());
            files.tree.render(area, surface, current, cx);
        }
    }

    pub fn render_tree_command_line(
        &mut self,
        area: Rect,
        surface: &mut Surface,
        cx: &mut Context,
    ) {
        if let Some(files) = &mut self.files {
            files.tree.render_command_line(area, surface, cx);
        }
    }

    pub fn tree_cursor(&self, area: Rect, editor: &Editor) -> (Option<Position>, CursorKind) {
        match &self.files {
            Some(files) => files.tree.cursor(area, editor),
            None => (None, CursorKind::Hidden),
        }
    }

    /// Handles `key` while the diff tree is focused.
    pub fn handle_tree_key(&mut self, key: KeyEvent, cx: &mut Context) -> EventResult {
        let Some(files) = &mut self.files else {
            return EventResult::Ignored(None);
        };
        let (result, request) = files.tree.handle_key(key, cx);
        self.handle_tree_request(request, cx.editor);
        result
    }

    /// Handles a mouse event over the diff tree. `None` leaves it to the editor.
    pub fn handle_tree_mouse(
        &mut self,
        event: &MouseEvent,
        editor: &mut Editor,
    ) -> Option<EventResult> {
        let request = self.files.as_mut()?.tree.handle_mouse(event, editor)?;
        self.handle_tree_request(request, editor);
        Some(EventResult::Consumed(None))
    }

    fn handle_tree_request(&mut self, request: tree::Request, editor: &mut Editor) {
        let tree::Request::Show(path) = request else {
            return;
        };
        let Some(files) = &self.files else {
            return;
        };
        let index = files.set.files.iter().position(|file| file.path == path);
        // The file shown last shows again once its panes closed.
        if let Some(index) =
            index.filter(|&index| self.pair.is_none() || files.current != Some(index))
        {
            self.show_file(index, editor);
        }
    }
}

/// Works out the diff of `request` with `tool`, parsing its texts meanwhile with `loader`.
async fn work_out(tool: DiffTool, request: Request, loader: Option<Arc<Loader>>) -> Diff {
    let path = request.path.to_string_lossy().into_owned();
    let aligned = run::align(tool, path, request.old.clone(), request.new.clone());
    let parsed = async {
        match loader {
            Some(loader) => parse(&request, loader).await,
            None => Parsed::default(),
        }
    };
    let (outcome, parsed) = tokio::join!(aligned, parsed);
    Diff {
        request,
        outcome,
        parsed,
    }
}

/// Parses the texts of `request` in the language its path names, side by side.
async fn parse(request: &Request, loader: Arc<Loader>) -> Parsed {
    let Some(language) = loader.language_for_filename(&request.path) else {
        return Parsed::default();
    };
    let config = loader.language(language).config().clone();
    let [old, new] = [&request.old, &request.new].map(|text| {
        let (text, loader) = (text.clone(), loader.clone());
        tokio::task::spawn_blocking(move || Syntax::new(text.slice(..), language, &loader).ok())
    });
    let (old, new) = tokio::join!(old, new);
    Parsed {
        language: Some(config),
        syntaxes: [old.ok().flatten(), new.ok().flatten()],
    }
}

/// Counts the lines added and removed in the files of `set` in the background, handing them to
/// the diff tree of the diff of many `generation` now and then.
fn spawn_stats(set: &DiffSet, reader: Reader, generation: u64) {
    let (root, files) = (set.root.clone(), set.files.clone());
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    std::thread::spawn(move || {
        let mut batch = Vec::new();
        let mut sent = Instant::now();
        reader.each_stats(&root, files, |path, stats| {
            batch.push((path, stats));
            if sent.elapsed() < STATS_INTERVAL {
                return true;
            }
            sent = Instant::now();
            // The editor quit when nothing receives them anymore.
            sender.blocking_send(std::mem::take(&mut batch)).is_ok()
        });
        let _ = sender.blocking_send(batch);
    });
    tokio::spawn(async move {
        while let Some(batch) = receiver.recv().await {
            job::dispatch(move |_, compositor| {
                if let Some(view) = compositor.find::<EditorView>() {
                    view.diff_view.stats_ready(generation, batch);
                }
            })
            .await;
        }
    });
}

/// Opens the panes of the diff of `request`, zoomed, with the cursor on the first hunk.
fn show(request: Request, outcome: Outcome, parsed: Parsed, editor: &mut Editor) -> Pair {
    let alignment = outcome.alignment.clone();
    let Parsed {
        language,
        syntaxes: [old_syntax, new_syntax],
    } = parsed;
    let texts = [
        (request.old.clone(), old_syntax),
        (request.new.clone(), new_syntax),
    ];
    let mut panes = texts.map(|(text, syntax)| {
        let mut doc = Document::from(text, None, editor.config.clone(), editor.syn_loader.clone());
        doc.set_parsed_language(language.clone(), syntax);
        doc.detect_indent_and_line_ending();
        doc.set_spelling_language_override(Some(Vec::new()));
        doc.detect_spelling();
        Some(doc)
    });

    let mut ids = [DocumentId::default(); 2];
    let mut views = [ViewId::default(); 2];
    for index in 0..2 {
        let doc = panes[index].take().expect("each pane once");
        ids[index] = editor.new_file_from_document(Action::VerticalSplit, doc);
        views[index] = editor.tree.focus;
    }
    for (index, side) in [Side::Old, Side::New].into_iter().enumerate() {
        doc_mut!(editor, &ids[index]).diff_view = Some(Box::new(pane(
            &request,
            side,
            alignment.clone(),
            ids[1 - index],
        )));
    }
    editor.tree.set_zoom(&views);
    if let Some(placeholder) = request.placeholder {
        let _ = editor.close_document(placeholder, true);
    }

    // The cursors go to the first hunk, the focus to the new side.
    let first = alignment.hunks().first().map_or(0, |hunk| hunk.start);
    for index in 0..2 {
        let doc = doc_mut!(editor, &ids[index]);
        let pane = doc.diff_view.as_ref().expect("set above");
        let line = alignment
            .line_at_or_after(pane.side, first)
            .unwrap_or_else(|| alignment.lines(pane.side).saturating_sub(1));
        let pos = doc.text().line_to_char(line as usize);
        doc.set_selection(views[index], Selection::point(pos));
        let view = editor.tree.get(views[index]);
        align_view(doc, view, Align::Center);
    }
    Pair {
        panes: ids,
        views,
        request,
        outcome,
    }
}

/// Gives the panes of `pair` the wrapping of their lines, as their views lay them out now.
fn wrap(pair: &Pair, editor: &mut Editor) {
    let formats = [0, 1].map(|index| {
        let (view, doc) = (
            editor.tree.get(pair.views[index]),
            doc!(editor, &pair.panes[index]),
        );
        doc.text_format(view.inner_width(doc), None)
    });
    let wraps = formats.iter().any(|format| format.soft_wrap);
    let texts = pair.panes.map(|doc| doc!(editor, &doc).text().clone());
    for doc in pair.panes {
        let Some(pane) = doc_mut!(editor, &doc).diff_view.as_mut() else {
            continue;
        };
        let current = pane.wrap.as_ref().map(Wrap::formats);
        if current != wraps.then_some(&formats) {
            pane.wrap = wraps.then(|| Wrap::new(texts.clone(), formats.clone()));
        }
    }
}

/// Shows the panes of `pair` with the texts of `request`, lined up as `outcome` says.
fn refresh(pair: &mut Pair, request: Request, outcome: Outcome, editor: &mut Editor) {
    let alignment = &outcome.alignment;
    for (index, side) in [Side::Old, Side::New].into_iter().enumerate() {
        let text = match side {
            Side::Old => &request.old,
            Side::New => &request.new,
        };
        let pane = pane(&request, side, alignment.clone(), pair.panes[1 - index]);
        doc_mut!(editor, &pair.panes[index]).replace_diff_text(text, pane, pair.views[index]);
    }
    pair.request = request;
    pair.outcome = outcome;
}

fn pane(request: &Request, side: Side, alignment: Arc<Alignment>, partner: DocumentId) -> Pane {
    let index = match side {
        Side::Old => 0,
        Side::New => 1,
    };
    Pane {
        side,
        alignment,
        name: request.names[index].clone(),
        partner,
        file: request.file.clone(),
        origin: request.origin,
        wrap: None,
    }
}
