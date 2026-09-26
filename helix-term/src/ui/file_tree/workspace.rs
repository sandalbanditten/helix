//! The file tree of one workspace root: its entries and rows, the cursor and scroll position,
//! edits, the search and what the git status and the watcher report.

use std::{
    cell::RefCell,
    collections::HashSet,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use helix_loader::workspace_trust::TrustQuery;
use helix_stdx::path::{canonicalize, normalize};
use helix_vcs::StatusOptions;
use helix_view::{
    editor::{Action as OpenAction, FileTreeConfig, FileTreeSort},
    graphics::Rect,
    smooth_scroll::SmoothOffset,
    Editor,
};

use super::{
    background::{in_background, ListRequest, Lister},
    edit::{Edit, EditKind},
    fs::ListOptions,
    git::GitStatuses,
    keys::Action,
    ops,
    order::Group,
    render::EditRow,
    rows::{InputRow, Rows},
    search::{Candidates, Hit, Matching},
    tree::{Kind, NodeId, Reveal, Tree},
    viewport::{self, Align},
    watch::Watcher,
};

/// The file tree of one workspace root.
pub(super) struct Workspace {
    /// Tells results for this workspace from results for an earlier one.
    pub(super) generation: u64,
    pub(super) root: Arc<Path>,
    pub(super) tree: Tree,
    pub(super) rows: Rows,
    /// The settings `rows` were built with.
    pub(super) flatten_dirs: bool,
    pub(super) sort: FileTreeSort,
    /// Whether `rows` needs rebuilding.
    pub(super) dirty: bool,
    pub(super) git: GitStatuses,
    pub(super) git_refresh: GitRefresh,
    pub(super) cursor: NodeId,
    /// The first ordinary row.
    pub(super) start: usize,
    pub(super) smooth_scroll: SmoothOffset,
    /// Whether the first listing has arrived; the panel is only shown after that.
    pub(super) ready: bool,
    /// Paths to reveal once their directories are listed.
    pub(super) reveals: Vec<PendingReveal>,
    /// A node to scroll into view once the panel height is known.
    pub(super) scroll_to: Option<NodeId>,
    /// The number of rows the panel showed last.
    pub(super) height: usize,
    /// Where the rows were drawn last, and the first ordinary row drawn, for the mouse.
    pub(super) rows_area: Rect,
    pub(super) drawn_start: usize,
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

    pub(super) fn start_search(&mut self, editor: &Editor) {
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
                    self.cursor = origin.cursor;
                }
            }
            None => editor.set_error("No more matches"),
        }
    }

    /// Ends the query being typed. Unless it is `kept`, the cursor and the scroll position go
    /// back to where they were; directories the search expanded collapse again, but for the
    /// ones leading to a kept cursor.
    pub(super) fn finish_search(&mut self, kept: bool) {
        let Some(origin) = self.search.origin.take() else {
            return;
        };
        self.search.matching = None;
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

    /// The rows among `rows` of files matching the query being typed, in order, each with the
    /// characters of its label that match.
    pub(super) fn matches(&mut self, rows: Range<usize>) -> Vec<(usize, Vec<usize>)> {
        match &self.edit {
            Some(Edit {
                kind: EditKind::Search,
                prompt,
            }) => self.search.matches(prompt.line(), &self.rows, rows),
            _ => Vec::new(),
        }
    }

    /// The row the cursor is on: the input row while a new entry is named.
    pub(super) fn cursor_row(&self) -> Option<usize> {
        match &self.edit {
            Some(Edit {
                kind: EditKind::Create { .. },
                ..
            }) => self.rows.input(),
            _ => self.rows.index_of(self.cursor),
        }
    }

    /// The absolute path of the cursor's entry.
    pub(super) fn cursor_path(&self) -> Option<PathBuf> {
        let index = self.rows.index_of(self.cursor)?;
        Some(self.root.join(&self.rows[index].path))
    }

    /// The row being typed in, for drawing.
    pub(super) fn edit_row(&self) -> Option<EditRow<'_>> {
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

    pub(super) fn cancel_edit(&mut self) {
        if self.edit.take().is_some() {
            self.dirty = true;
        }
        self.finish_search(false);
    }

    /// Runs a file operation on the cursor's entry or starts typing the name it needs.
    pub(super) fn act(&mut self, action: Action, editor: &mut Editor) -> Focus {
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
    pub(super) fn moved(&mut self, from: Option<&Path>, to: &Path) {
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
    pub(super) fn delete(&mut self, path: PathBuf, editor: &mut Editor) {
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
    pub(super) fn navigate(&mut self, action: Action) {
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
    pub(super) fn toggle_row(&mut self, index: usize) {
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
    pub(super) fn expand_row(&mut self, index: usize) {
        let row = &self.rows[index];
        let (head, mut node) = (row.head, Some(row.node));
        while let Some(id) = node {
            self.tree.expand(id);
            node = (id != head).then(|| self.tree.node(id).parent).flatten();
        }
        self.dirty = true;
    }

    /// Scrolls just enough to show the cursor with `scrolloff` rows around it.
    pub(super) fn reveal_cursor(&mut self, scrolloff: usize) {
        if let Some(cursor) = self.cursor_row() {
            let height = self.height.max(1);
            self.start = viewport::reveal(&self.rows, self.start, height, cursor, scrolloff);
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
    pub(super) fn watch(&mut self) {
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
    pub(super) fn rebuild_rows(&mut self) {
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
    pub(super) fn input_row(&self) -> Option<InputRow> {
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
    pub(super) fn shown_row(&self, node: NodeId, path: Option<&Path>) -> Option<usize> {
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
    use super::super::tree::tests::{dir, file, run};
    use super::*;

    /// root, `docs`, `src/main` (a run), `a`, `b`
    pub fn workspace() -> Workspace {
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
    fn every_matching_file_in_view_is_highlighted() {
        let mut workspace = searched_into_docs();
        workspace.rebuild_rows();
        let paths = ["a", "b", "docs/guide.md"].map(PathBuf::from).to_vec();
        workspace.search.candidates = Some(Arc::new(Candidates::new(
            paths,
            FileTreeSort::DirectoriesFirst,
        )));
        let (search, rows) = (&mut workspace.search, &workspace.rows);
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
