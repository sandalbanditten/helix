//! The file tree of one workspace root: its entries and rows, the cursor, edits and the search.

use std::{
    cell::RefCell,
    collections::{BTreeSet, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use helix_loader::workspace_trust::TrustQuery;
use helix_stdx::path::{canonicalize, get_relative_path, normalize};
use helix_vcs::StatusOptions;
use helix_view::{
    dired::Source,
    editor::{Action as OpenAction, FileTreeConfig, FileTreeSort},
    graphics::Rect,
    Editor,
};

use super::{
    background::{in_background, ListRequest, Lister},
    browser::Browser,
    edit::{Clip, Edit, EditKind},
    fs::ListOptions,
    git::GitStatuses,
    keys::Action,
    ops,
    order::Group,
    render::EditRow,
    rows::{InputRow, Rows},
    search::{Candidates, Hit, Matching},
    tree::{Kind, NodeId, Reveal, Tree},
};
use crate::watch::Watcher;

/// The file tree of one workspace root.
pub(super) struct Workspace {
    /// Tells results for this workspace from results for an earlier one.
    pub(super) generation: u64,
    pub(super) root: Arc<Path>,
    /// The entries, the rows showing them, the cursor and the scroll position.
    pub(super) browser: Browser,
    /// The order the entries were listed in.
    pub(super) sort: FileTreeSort,
    pub(super) git: GitStatuses,
    pub(super) git_refresh: GitRefresh,
    /// Whether the first listing has arrived; the panel is only shown after that.
    pub(super) ready: bool,
    /// Paths to reveal once their directories are listed.
    pub(super) reveals: Vec<PendingReveal>,
    /// A name or path being typed for a file operation.
    pub(super) edit: Option<Edit>,
    /// Where the line of an inline edit was drawn last.
    pub(super) edit_area: Option<Rect>,
    pub(super) search: Search,
    pub(super) watcher: Option<Watcher>,
    /// The repository's `.git` directory, which is watched for changes of the git status.
    pub(super) git_dir: Option<PathBuf>,
    /// Whether changes were let pass while the panel was hidden.
    pub(super) outdated: bool,
}

/// A path to reveal once the directories leading to it are listed.
pub(super) struct PendingReveal {
    pub(super) path: PathBuf,
    pub(super) purpose: Purpose,
}

/// What a path is revealed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Purpose {
    Show,
    /// Moving the cursor there.
    Cursor,
    /// Moving the cursor to a match of the search.
    Match,
}

/// The search of the tree.
#[derive(Default)]
pub(super) struct Search {
    /// The files to search, once collected.
    pub(super) candidates: Option<Arc<Candidates>>,
    /// The last query searched for, for `n` and `N`.
    pub(super) query: String,
    /// Where the cursor was when the query being typed was started.
    pub(super) origin: Option<Origin>,
    /// Tells the latest search from older ones still running.
    pub(super) generation: u64,
    /// The query being typed, ready to match the rows in view.
    pub(super) matching: Option<(String, Option<Matching>)>,
}

impl Search {
    fn matches(
        &mut self,
        query: &str,
        rows: &Rows,
        range: Range<usize>,
    ) -> Vec<(usize, Vec<usize>)> {
        let Some(candidates) = &self.candidates else {
            return Vec::new();
        };
        if self
            .matching
            .as_ref()
            .is_none_or(|(matched, _)| matched != query)
        {
            self.matching = Some((query.to_owned(), Matching::new(query)));
        }
        let Some((_, Some(matching))) = &mut self.matching else {
            return Vec::new();
        };
        range
            .filter_map(|index| {
                let row = rows.get(index)?;
                // Only the files the search goes to, which leaves out ignored ones.
                if !candidates.contains(&row.path) {
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
}

pub(super) struct Origin {
    pub(super) cursor: NodeId,
    pub(super) path: PathBuf,
    pub(super) is_dir: bool,
    pub(super) start: usize,
    /// The directories that were expanded, so the search can collapse the ones it expanded.
    pub(super) expanded: HashSet<NodeId>,
}

/// Whether the tree keeps its focus after an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Focus {
    Keep,
    Release,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum GitRefresh {
    #[default]
    Idle,
    Running,
    /// Running, and asked for again meanwhile.
    RunningStale,
}

impl Workspace {
    pub(super) fn new(root: PathBuf, generation: u64, config: &FileTreeConfig) -> Self {
        let name = root
            .file_name()
            .map_or_else(|| root.as_os_str().to_owned(), ToOwned::to_owned);
        let tree = Tree::new(name, config.sort);
        let git_dir = root
            .ancestors()
            .map(|dir| dir.join(".git"))
            .find(|git_dir| git_dir.is_dir());
        Self {
            generation,
            root: root.into(),
            browser: Browser::new(tree, config.flatten_dirs),
            sort: config.sort,
            git: GitStatuses::default(),
            git_refresh: GitRefresh::Idle,
            ready: false,
            reveals: Vec::new(),
            edit: None,
            edit_area: None,
            search: Search::default(),
            watcher: None,
            git_dir,
            outdated: false,
        }
    }

    pub(super) fn start_search(&mut self, editor: &Editor) {
        let Some(index) = self.browser.rows.index_of(self.browser.cursor) else {
            return;
        };
        let row = &self.browser.rows[index];
        self.search = Search {
            origin: Some(Origin {
                cursor: self.browser.cursor,
                path: row.path.clone(),
                is_dir: self.browser.tree.node(row.node).kind == Kind::Directory,
                start: self.browser.start,
                expanded: self.browser.tree.expanded_directories().collect(),
            }),
            query: std::mem::take(&mut self.search.query),
            ..Search::default()
        };
        self.edit = Some(Edit::new(EditKind::Search, String::new(), editor));
    }

    /// Moves the cursor to the result of a search.
    pub(super) fn found(&mut self, hit: Option<Hit>, incremental: bool, editor: &mut Editor) {
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
                    self.browser.cursor = origin.cursor;
                }
            }
            None => editor.set_error("No more matches"),
        }
    }

    /// Ends the query being typed, putting the cursor back unless it is `kept`.
    pub(super) fn finish_search(&mut self, kept: bool) {
        let Some(origin) = self.search.origin.take() else {
            return;
        };
        self.search.matching = None;
        if !kept {
            self.search.generation += 1;
            self.reveals
                .retain(|reveal| reveal.purpose != Purpose::Match);
            if self.browser.tree.contains(origin.cursor) {
                self.browser.cursor = origin.cursor;
            }
            self.browser.start = origin.start;
        }
        let expanded: Vec<_> = self
            .browser
            .tree
            .expanded_directories()
            .filter(|dir| !origin.expanded.contains(dir))
            .collect();
        for dir in expanded {
            let keep = kept
                && self.browser.tree.contains(self.browser.cursor)
                && self.browser.tree.is_within(self.browser.cursor, dir);
            if self.browser.tree.contains(dir) && !keep {
                self.browser.tree.collapse(dir);
            }
        }
        self.browser.dirty = true;
    }

    /// The rows among `rows` of files matching the query being typed, in order, each with the
    /// characters of its label that match.
    pub(super) fn matches(&mut self, rows: Range<usize>) -> Vec<(usize, Vec<usize>)> {
        match &self.edit {
            Some(Edit {
                kind: EditKind::Search,
                prompt,
            }) => self.search.matches(prompt.line(), &self.browser.rows, rows),
            _ => Vec::new(),
        }
    }

    /// The row the cursor is on: the input row while a new entry is named.
    pub(super) fn cursor_row(&self) -> Option<usize> {
        if self
            .edit
            .as_ref()
            .is_some_and(|edit| edit.kind.input().is_some())
        {
            self.browser.rows.input()
        } else {
            self.browser.rows.index_of(self.browser.cursor)
        }
    }

    /// The absolute path of the cursor's entry.
    pub(super) fn cursor_path(&self) -> Option<PathBuf> {
        let index = self.browser.rows.index_of(self.browser.cursor)?;
        Some(self.root.join(&self.browser.rows[index].path))
    }

    /// The path of the cursor's entry relative to the root, unless it is the root.
    pub(super) fn cursor_entry(&self) -> Option<&Path> {
        let index = self
            .browser
            .rows
            .index_of(self.browser.cursor)
            .filter(|&index| index != 0)?;
        Some(&self.browser.rows[index].path)
    }

    /// What dired lists for the cursor's entry, every row shown if `tree`, and the entry to put
    /// dired's cursor on.
    pub(super) fn dired_source(&self, tree: bool) -> Option<(Source, Option<PathBuf>)> {
        let index = self.browser.rows.index_of(self.browser.cursor)?;
        let row = &self.browser.rows[index];
        if tree {
            let mut expanded: BTreeSet<PathBuf> = self
                .browser
                .tree
                .expanded_directories()
                .map(|dir| self.browser.tree.path(dir))
                .filter(|path| !path.as_os_str().is_empty())
                .collect();
            // A run shown as one row lists each of its directories.
            for row in self.browser.rows.iter().filter(|row| row.head != row.node) {
                let mut dir = self.browser.tree.node(row.node).parent;
                while let Some(id) = dir {
                    expanded.insert(self.browser.tree.path(id));
                    if id == row.head {
                        break;
                    }
                    dir = self.browser.tree.node(id).parent;
                }
            }
            let root = self.root.to_path_buf();
            return Some((Source::Tree { root, expanded }, Some(row.path.clone())));
        }
        if index == 0 || self.browser.tree.node(row.node).kind == Kind::Directory {
            return Some((Source::Directory(self.root.join(&row.path)), None));
        }
        let dir = self.root.join(row.path.parent()?);
        Some((Source::Directory(dir), Some(row.path.file_name()?.into())))
    }

    /// The row being typed in, for drawing.
    pub(super) fn edit_row(&self) -> Option<EditRow<'_>> {
        let edit = self.edit.as_ref()?;
        let name = edit.prompt.line().as_str();
        let (index, directory) = match &edit.kind {
            EditKind::Rename { node, .. } => (self.browser.rows.index_of(*node)?, false),
            EditKind::Create { directory, .. } => (
                self.browser.rows.input()?,
                *directory || name.ends_with('/'),
            ),
            EditKind::Paste { directory, .. } => (self.browser.rows.input()?, *directory),
            EditKind::Move { .. } | EditKind::Delete { .. } | EditKind::Search => return None,
        };
        Some(EditRow {
            index,
            name,
            directory,
        })
    }

    pub(super) fn cancel_edit(&mut self) {
        if self.edit.take().is_some() {
            self.browser.dirty = true;
        }
        self.finish_search(false);
    }

    /// Runs a file operation on the cursor's entry or starts typing the name it needs.
    pub(super) fn act(&mut self, action: Action, editor: &mut Editor) -> Focus {
        let Some(index) = self.browser.rows.index_of(self.browser.cursor) else {
            return Focus::Keep;
        };
        let row = &self.browser.rows[index];
        let (node, path) = (row.node, row.path.clone());
        let kind = self.browser.tree.node(node).kind;
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
            Action::Open if kind == Kind::Directory && !root => self.browser.toggle_row(index),
            Action::Open => return open(editor, OpenAction::Replace),
            Action::OpenHorizontal => return open(editor, OpenAction::HorizontalSplit),
            Action::OpenVertical => return open(editor, OpenAction::VerticalSplit),
            // The workspace root is the one entry that stays put.
            Action::Rename | Action::MoveInWorkspace | Action::Move | Action::Delete if root => {}
            Action::Rename => {
                let name = self
                    .browser
                    .tree
                    .node(node)
                    .name
                    .to_string_lossy()
                    .into_owned();
                self.edit = Some(Edit::new(EditKind::Rename { node, path }, name, editor));
                self.browser.dirty = true;
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
                let dir_row = self.input_dir_row(index);
                let dir = self.browser.rows[dir_row].node;
                let kind = EditKind::Create {
                    dir,
                    dir_path: self.browser.rows[dir_row].path.clone(),
                    directory: action == Action::NewDirectory,
                };
                self.edit = Some(Edit::new(kind, String::new(), editor));
                self.browser.dirty = true;
            }
            Action::Delete => {
                let directory = kind == Kind::Directory;
                let kind = EditKind::Delete { path, directory };
                self.edit = Some(Edit::new(kind, String::new(), editor));
            }
            _ => {
                if let Some(motion) = action.motion() {
                    self.browser.navigate(motion);
                }
            }
        }
        Focus::Keep
    }

    /// The row of the directory a new entry goes in from row `index`, expanded.
    fn input_dir_row(&mut self, index: usize) -> usize {
        let row = &self.browser.rows[index];
        let dir_row = if self.browser.tree.node(row.node).kind == Kind::Directory {
            index
        } else {
            row.parent.unwrap_or(0)
        };
        if dir_row != 0
            && !self
                .browser
                .tree
                .node(self.browser.rows[dir_row].node)
                .expanded
        {
            self.browser.expand_row(dir_row);
        }
        dir_row
    }

    /// Starts naming where `clip` is pasted, in an input row with a free name.
    pub(super) fn start_paste(
        &mut self,
        clip: Clip,
        copying: &HashSet<PathBuf>,
        editor: &mut Editor,
    ) {
        let Some(index) = self.browser.rows.index_of(self.browser.cursor) else {
            return;
        };
        let Some(name) = clip.path.file_name() else {
            return;
        };
        let Ok(metadata) = fs::symlink_metadata(&clip.path) else {
            let path = get_relative_path(&clip.path);
            editor.set_error(format!("'{}' no longer exists", path.display()));
            return;
        };
        let dir_row = self.input_dir_row(index);
        let dir_path = self.browser.rows[dir_row].path.clone();
        let dir = self.root.join(&dir_path);
        let directory = metadata.is_dir();
        // A cut entry pasted where it is keeps its name.
        let name = if clip.cut && dir.join(name) == clip.path {
            name.to_owned()
        } else {
            ops::free_name(name, directory, |candidate| {
                let path = dir.join(candidate);
                fs::symlink_metadata(&path).is_ok() || copying.contains(&path)
            })
        };
        let kind = EditKind::Paste {
            clip,
            dir: self.browser.rows[dir_row].node,
            dir_path,
            directory,
        };
        let name = name.to_string_lossy().into_owned();
        self.edit = Some(Edit::new(kind, name, editor));
        self.browser.dirty = true;
    }

    /// Carries out a finished edit.
    pub(super) fn submit(&mut self, edit: Edit, editor: &mut Editor) -> Focus {
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
            // The file tree pastes, as it keeps what is pasted.
            EditKind::Search | EditKind::Paste { .. } => return Focus::Keep,
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
    pub(super) fn moved(&mut self, from: Option<&Path>, to: &Path) {
        if let Some(parent) = from.and_then(Path::parent) {
            self.browser.tree.invalidate(parent);
        }
        self.appeared(to, Purpose::Cursor);
    }

    /// Lists the directory of `to` (absolute) again after an entry appeared there, and reveals
    /// the entry for `purpose`.
    pub(super) fn appeared(&mut self, to: &Path, purpose: Purpose) {
        if let Ok(to) = to.strip_prefix(&self.root) {
            // The directories in between may be new too.
            let known = to
                .ancestors()
                .skip(1)
                .find(|dir| self.browser.tree.find(dir).is_some());
            if let Some(dir) = known {
                self.browser.tree.invalidate(dir);
            }
            self.reveal(to.to_path_buf(), purpose);
        }
    }

    /// Deletes the entry at `path`, relative to the root, moving the cursor off it first.
    pub(super) fn delete(&mut self, path: PathBuf, editor: &mut Editor) {
        if let Some(index) = self.browser.rows.index_of(self.browser.cursor) {
            let after = (index + 1..self.browser.rows.len())
                .find(|&i| !self.browser.rows[i].path.starts_with(&path));
            let neighbour = after.unwrap_or(index.saturating_sub(1));
            self.browser.cursor = self.browser.rows[neighbour].node;
        }
        match ops::delete(editor, &self.root.join(&path)) {
            Ok(()) => {
                editor.set_status(format!("'{}' deleted", path.display()));
                if let Some(parent) = path.parent() {
                    self.browser.tree.invalidate(parent);
                }
            }
            Err(err) => editor.set_error(err.to_string()),
        }
    }

    /// Scrolls just enough to show the cursor with `scrolloff` rows around it.
    pub(super) fn reveal_cursor(&mut self, scrolloff: usize) {
        if let Some(cursor) = self.cursor_row() {
            self.browser.reveal_row(cursor, scrolloff);
        }
    }

    /// Expands the directories leading to `path` (relative to the root), listing them first if
    /// needed, for `purpose`.
    pub(super) fn reveal(&mut self, path: PathBuf, purpose: Purpose) {
        self.reveals.push(PendingReveal { path, purpose });
    }

    /// Brings everything derived from the tree up to date: pending reveals, listings, rows.
    pub(super) fn update(&mut self, lister: &Lister, config: &FileTreeConfig) {
        if config.sort != self.sort {
            self.sort = config.sort;
            self.browser.tree.set_sort(config.sort);
            self.browser.dirty = true;
        }
        if config.flatten_dirs != self.browser.flatten_dirs {
            self.browser.flatten_dirs = config.flatten_dirs;
            self.browser.dirty = true;
        }

        let mut unlisted = Vec::new();
        let mut revealed = false;
        self.reveals.retain(|PendingReveal { path, purpose }| {
            match self.browser.tree.reveal(path) {
                Reveal::Found(node) => {
                    if *purpose != Purpose::Show {
                        self.browser.cursor = node;
                        self.browser.scroll_to = Some(node);
                    }
                    revealed = true;
                    false
                }
                Reveal::Unlisted(dir) => {
                    // List the whole way down at once rather than one directory per round trip.
                    let dir = self.browser.tree.path(dir);
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
            }
        });
        self.browser.dirty |= revealed;

        let requests = self.browser.tree.take_listing_requests();
        let mut dirs: Vec<_> = requests
            .into_iter()
            .map(|id| self.browser.tree.path(id))
            .collect();
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

        if std::mem::take(&mut self.browser.dirty) {
            self.rebuild_rows();
            self.watch();
        }
    }

    /// Watches the directories the tree has listed, and `.git`.
    pub(super) fn watch(&mut self) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        let mut dirs: HashSet<_> = self
            .browser
            .tree
            .loaded_directories()
            .map(|dir| self.root.join(self.browser.tree.path(dir)))
            .collect();
        dirs.extend(self.git_dir.clone());
        watcher.watch(dirs);
    }

    /// Rebuilds the rows, with the input row of a new entry being named.
    pub(super) fn rebuild_rows(&mut self) {
        let input = self.input_row();
        self.browser.rebuild_rows(input);
    }

    /// Where the input row for a new entry goes among the entries of its directory.
    pub(super) fn input_row(&self) -> Option<InputRow> {
        let (dir, directory) = self.edit.as_ref()?.kind.input()?;
        if !self.browser.tree.contains(dir) {
            return None;
        }
        let at = match (directory, self.sort) {
            (false, FileTreeSort::DirectoriesFirst) => self
                .browser
                .tree
                .children(dir)
                .iter()
                .take_while(|child| {
                    self.browser.tree.node(**child).kind.group() == Group::Directory
                })
                .count(),
            _ => 0,
        };
        Some(InputRow { dir, at })
    }

    pub(super) fn refresh_git(&mut self, editor: &Editor) {
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

#[cfg(test)]
pub(super) mod tests {
    use super::super::browser::Motion;
    use super::super::tree::tests::{dir, file, run};
    use super::*;

    /// root, `docs`, `src/main` (a run), `a`, `b`
    pub fn workspace() -> Workspace {
        let mut workspace = Workspace::new("/root".into(), 1, &FileTreeConfig::default());
        let root = workspace.browser.tree.root();
        workspace.browser.tree.apply_listing(
            root,
            Some(vec![
                run("src", &["main"]),
                dir("docs"),
                file("a"),
                file("b"),
            ]),
        );
        workspace.rebuild_rows();
        workspace.browser.height = 10;
        workspace
    }

    fn cursor_label(workspace: &Workspace) -> &str {
        let index = workspace
            .browser
            .rows
            .index_of(workspace.browser.cursor)
            .unwrap();
        &workspace.browser.rows[index].label
    }

    #[test]
    fn a_run_expands_and_collapses_as_one() {
        let mut workspace = workspace();
        let src = workspace.browser.tree.find("src".as_ref()).unwrap();
        let main = workspace.browser.tree.find("src/main".as_ref()).unwrap();
        workspace.browser.cursor = main;
        workspace.browser.navigate(Motion::Expand);
        assert!(
            workspace.browser.tree.node(src).expanded && workspace.browser.tree.node(main).expanded
        );
        workspace
            .browser
            .tree
            .apply_listing(main, Some(vec![file("lib.rs")]));
        workspace.rebuild_rows();
        let labels: Vec<_> = workspace
            .browser
            .rows
            .iter()
            .map(|row| &*row.label)
            .collect();
        assert_eq!(labels, ["root", "docs", "src/main", "lib.rs", "a", "b"]);

        workspace.browser.navigate(Motion::Collapse);
        assert!(
            !workspace.browser.tree.node(src).expanded
                && !workspace.browser.tree.node(main).expanded
        );
        workspace.rebuild_rows();
        assert_eq!(cursor_label(&workspace), "src/main");
    }

    /// Starts a search at the root, then expands `docs` and moves the cursor into it, as a match
    /// would.
    fn searched_into_docs() -> Workspace {
        let mut workspace = workspace();
        let root = workspace.browser.tree.root();
        workspace.search.origin = Some(Origin {
            cursor: root,
            path: PathBuf::new(),
            is_dir: true,
            start: 0,
            expanded: workspace.browser.tree.expanded_directories().collect(),
        });
        let docs = workspace.browser.tree.find("docs".as_ref()).unwrap();
        workspace.browser.tree.expand(docs);
        workspace
            .browser
            .tree
            .apply_listing(docs, Some(vec![file("guide.md")]));
        workspace.browser.cursor = workspace
            .browser
            .tree
            .find("docs/guide.md".as_ref())
            .unwrap();
        workspace
    }

    #[test]
    fn a_cancelled_search_goes_back_where_it_started() {
        let mut workspace = searched_into_docs();
        workspace.finish_search(false);
        let docs = workspace.browser.tree.find("docs".as_ref()).unwrap();
        assert!(!workspace.browser.tree.node(docs).expanded);
        assert_eq!(workspace.browser.cursor, workspace.browser.tree.root());
    }

    #[test]
    fn a_finished_search_keeps_the_way_to_its_match() {
        let mut workspace = searched_into_docs();
        let src = workspace.browser.tree.find("src".as_ref()).unwrap();
        workspace.browser.tree.expand(src);
        workspace.finish_search(true);
        let docs = workspace.browser.tree.find("docs".as_ref()).unwrap();
        assert!(workspace.browser.tree.node(docs).expanded);
        assert!(!workspace.browser.tree.node(src).expanded);
        assert_eq!(
            workspace.browser.cursor,
            workspace
                .browser
                .tree
                .find("docs/guide.md".as_ref())
                .unwrap()
        );
    }

    #[test]
    fn every_matching_file_in_view_is_highlighted() {
        let mut workspace = searched_into_docs();
        workspace.rebuild_rows();
        let paths = ["a", "b", "docs/guide.md"].map(PathBuf::from).to_vec();
        workspace.search.candidates = Some(Arc::new(Candidates::new(
            paths,
            FileTreeSort::DirectoriesFirst,
        )));
        let (search, rows) = (&mut workspace.search, &workspace.browser.rows);
        let labels: Vec<_> = rows.iter().map(|row| &*row.label).collect();
        assert_eq!(labels, ["root", "docs", "guide.md", "src/main", "a", "b"]);

        // Matched characters in the directory leave the label as it is.
        assert_eq!(
            search.matches("docguide", rows, 0..rows.len()),
            [(2, vec![0, 1, 2, 3, 4])]
        );
        assert_eq!(search.matches("a", rows, 0..rows.len()), [(4, vec![0])]);
        assert_eq!(search.matches("a", rows, 0..4), []);
        assert_eq!(search.matches("", rows, 0..rows.len()), []);
    }

    #[test]
    fn entries_appearing_in_new_directories_list_the_closest_known_one() {
        let mut workspace = workspace();
        let docs = workspace.browser.tree.find("docs".as_ref()).unwrap();
        workspace.browser.tree.expand(docs);
        workspace
            .browser
            .tree
            .apply_listing(docs, Some(vec![file("guide.md")]));
        workspace.browser.tree.take_listing_requests();
        workspace.appeared("/root/docs/new/deep.md".as_ref(), Purpose::Cursor);
        assert_eq!(workspace.browser.tree.take_listing_requests(), [docs]);
    }

    #[test]
    fn rebuilding_keeps_the_cursor_on_a_shown_row() {
        let mut workspace = workspace();
        let docs = workspace.browser.tree.find("docs".as_ref()).unwrap();
        workspace.browser.tree.expand(docs);
        workspace
            .browser
            .tree
            .apply_listing(docs, Some(vec![file("guide.md")]));
        workspace.rebuild_rows();
        workspace.browser.cursor = workspace
            .browser
            .tree
            .find("docs/guide.md".as_ref())
            .unwrap();

        // Collapsing `docs` hides the cursor's row: it moves to `docs`.
        workspace.browser.tree.collapse(docs);
        workspace.rebuild_rows();
        assert_eq!(workspace.browser.cursor, docs);
    }
}
