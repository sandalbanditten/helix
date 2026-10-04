//! Reloads the buffers whose files change on disk, and refreshes their diff bases and HEAD names
//! when HEAD of their repository moves.

use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::{Duration, SystemTime},
};

use arc_swap::ArcSwap;
use helix_core::{diff::compare_ropes, encoding::Encoding, Rope, Transaction};
use helix_event::register_hook;
use helix_loader::workspace_trust::TrustQuery;
use helix_stdx::path::get_relative_path;
use helix_vcs::DiffProviderRegistry;
use helix_view::{
    document::DiskText,
    events::{ConfigDidChange, DocumentDidClose, DocumentDidOpen, DocumentPathDidChange},
    handlers::Handlers,
    Document, DocumentId, Editor,
};
use parking_lot::Mutex;

use crate::{compositor::Compositor, job, ui::EditorView, watch::Watcher};

mod head;

/// How long a check waits while the editor writes files itself.
const RETRY: Duration = Duration::from_millis(100);

/// The watched files, by what they are to the editor.
type Files = HashMap<PathBuf, Watched>;

/// What a watched file is to the editor.
enum Watched {
    /// The file of the document at this path, or of the file it links to.
    Document(PathBuf),
    /// A file whose change moves HEAD of a repository, or the closest directory to it that is
    /// missing.
    Head(Arc<Repository>),
}

/// A repository holding documents.
struct Repository {
    /// The files whose changes move its HEAD, its HEAD file first.
    head_files: Vec<PathBuf>,
    /// The paths of the documents it holds.
    docs: Vec<PathBuf>,
}

/// What watches the files of the open documents.
#[derive(Default)]
struct Watching {
    watcher: Option<Watcher>,
    /// Shared with the watcher's thread, which drops the changes of other files.
    files: Arc<ArcSwap<Files>>,
    /// The names of the file of each document path, looked up once.
    names: HashMap<PathBuf, Vec<PathBuf>>,
    /// The files moving HEAD of the repository holding each directory with documents.
    heads: HashMap<PathBuf, Vec<PathBuf>>,
}

impl Watching {
    /// Watches the files of the open documents and what moves HEAD of their repositories, or
    /// nothing when auto-reload is off.
    fn follow(this: &Arc<Mutex<Self>>, editor: &Editor) {
        let mut watching = this.lock();
        if !editor.config().auto_reload {
            *watching = Self::default();
            return;
        }
        let watching = &mut *watching;
        let paths: HashSet<&Path> = editor.documents().filter_map(followed_path).collect();
        watching
            .names
            .retain(|path, _| paths.contains(path.as_path()));
        let doc_dirs: HashSet<&Path> = paths.iter().filter_map(|path| path.parent()).collect();
        watching
            .heads
            .retain(|dir, _| doc_dirs.contains(dir.as_path()));

        let mut files = Files::new();
        let mut dirs = HashSet::new();
        // By their HEAD files.
        let mut repositories: HashMap<PathBuf, Repository> = HashMap::new();
        for path in paths {
            let names = watching
                .names
                .entry(path.to_path_buf())
                .or_insert_with(|| names(path));
            for name in names.iter() {
                dirs.extend(name.parent().map(Path::to_path_buf));
                files.insert(name.clone(), Watched::Document(path.to_path_buf()));
            }
            let Some(dir) = path.parent() else {
                continue;
            };
            let head_files = watching
                .heads
                .entry(dir.to_path_buf())
                .or_insert_with(|| editor.diff_providers.head_files(path));
            let Some(head) = head_files.first() else {
                continue;
            };
            repositories
                .entry(head.clone())
                .or_insert_with(|| Repository {
                    head_files: head_files.clone(),
                    docs: Vec::new(),
                })
                .docs
                .push(path.to_path_buf());
        }
        for repository in repositories.into_values() {
            let repository = Arc::new(repository);
            for head in &repository.head_files {
                // A loose ref of a branch whose refs are all packed doesn't exist, nor maybe its
                // directory: what shows it coming is its closest directory that exists.
                let mut file = head.clone();
                while let Some(dir) = file.parent().filter(|dir| !dir.is_dir()) {
                    let dir = dir.to_path_buf();
                    files.insert(
                        std::mem::replace(&mut file, dir),
                        Watched::Head(repository.clone()),
                    );
                }
                dirs.extend(file.parent().map(Path::to_path_buf));
                files.insert(file, Watched::Head(repository.clone()));
            }
        }

        let start = watching.watcher.is_none() && !files.is_empty();
        watching.files.store(Arc::new(files));
        if start {
            let accepted = watching.files.clone();
            let this = Arc::downgrade(this);
            watching.watcher = Watcher::filtered(
                move |path| accepted.load().contains_key(path),
                move |paths, editor, _| changed(&this, paths, editor),
            )
            .inspect_err(|err| log::warn!("cannot watch the open files: {err}"))
            .ok();
        }
        if let Some(watcher) = &mut watching.watcher {
            watcher.watch(dirs);
        }
    }
}

/// The path of the document's file if it is followed: dired and compilation buffers aren't.
fn followed_path(doc: &Document) -> Option<&Path> {
    doc.path()
        .filter(|_| doc.dired.is_none() && doc.compilation.is_none())
}

/// The names writes to the file at `path` show under.
fn names(path: &Path) -> Vec<PathBuf> {
    let mut names = vec![path.to_path_buf()];
    if path.is_symlink() {
        names.extend(std::fs::canonicalize(path));
    }
    names
}

/// What asked for a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// The watcher saw the files change.
    Watcher,
    /// The terminal got the focus back, after other programs may have changed files unseen.
    Focus,
}

/// Handles the files that the watcher saw change.
fn changed(watching: &Weak<Mutex<Watching>>, paths: HashSet<PathBuf>, editor: &mut Editor) {
    let Some(watching) = watching.upgrade() else {
        return;
    };
    let files = watching.lock().files.load_full();
    let mut docs = HashSet::new();
    // By their HEAD files.
    let mut moved = HashMap::new();
    for path in &paths {
        match files.get(path) {
            Some(Watched::Document(path)) => docs.extend(editor.document_id_by_path(path)),
            Some(Watched::Head(repository)) => {
                moved.insert(&repository.head_files[0], repository);
            }
            None => {}
        }
    }
    check(docs, Trigger::Watcher, editor);
    if moved.is_empty() {
        return;
    }
    // HEAD may be on another branch now, whose ref is another file: one lookup does for all the
    // directories of a repository.
    {
        let mut watching = watching.lock();
        for (head, repository) in &moved {
            let head_files = editor.diff_providers.head_files(&repository.docs[0]);
            for files in watching.heads.values_mut() {
                if files.first() == Some(head) {
                    files.clone_from(&head_files);
                }
            }
        }
    }
    Watching::follow(&watching, editor);
    let paths = moved
        .values()
        .flat_map(|repository| repository.docs.iter().cloned())
        .collect();
    head::refresh(paths, editor);
}

/// Checks every followed document, as files may change unseen, like those on a network drive.
pub(crate) fn check_all(editor: &mut Editor) {
    let docs = editor
        .documents()
        .filter(|doc| followed_path(doc).is_some())
        .map(Document::id)
        .collect();
    check(docs, Trigger::Focus, editor);
}

/// Compares the files of `docs` with what the documents know of them in the background, and
/// reloads those that changed.
fn check(docs: HashSet<DocumentId>, trigger: Trigger, editor: &mut Editor) {
    if docs.is_empty() || !editor.config().auto_reload {
        return;
    }
    if editor.write_count > 0 {
        return retry(docs, trigger);
    }
    let registry = editor.diff_providers.clone();
    let candidates: Vec<_> = docs
        .into_iter()
        .filter_map(|doc| Candidate::new(editor, doc))
        .collect();
    job::in_background(
        move || {
            candidates
                .into_iter()
                .map(|candidate| {
                    let outcome = candidate.read(&registry);
                    (candidate, outcome)
                })
                .collect()
        },
        move |editor, compositor, results| apply(results, trigger, editor, compositor),
    );
}

/// Checks `docs` again once the editor's own writes are done.
fn retry(docs: HashSet<DocumentId>, trigger: Trigger) {
    tokio::spawn(async move {
        tokio::time::sleep(RETRY).await;
        job::dispatch(move |editor, _| check(docs, trigger, editor)).await;
    });
}

/// What a document knows of its file, taken to compare the file with in the background.
struct Candidate {
    doc: DocumentId,
    path: PathBuf,
    /// The version of the text, which every edit changes.
    version: i32,
    last_saved_time: SystemTime,
    modified: bool,
    text: Rope,
    disk_text: Rope,
    encoding: &'static Encoding,
    trust_full: bool,
}

/// How a file compares with what its document knows of it.
enum Outcome {
    /// The file is as the document knows it.
    Known,
    Deleted,
    /// The file was written anew, at this time, with the text the document knows.
    Rewritten(SystemTime),
    /// The file changed while the document has unsaved changes.
    Conflict(DiskText),
    Changed(Reload),
    Failed(anyhow::Error),
}

/// What reloads a document without unsaved changes.
struct Reload {
    disk: DiskText,
    /// What turns the text into the file's.
    changes: Transaction,
    diff_base: Option<Vec<u8>>,
    head: Option<Arc<ArcSwap<Box<str>>>>,
}

impl Candidate {
    fn new(editor: &Editor, doc_id: DocumentId) -> Option<Self> {
        let doc = editor.document(doc_id)?;
        let trust_full = editor
            .workspace_trust
            .query(doc.workspace_root(), TrustQuery::Git)
            .is_trusted();
        Some(Self {
            doc: doc_id,
            path: followed_path(doc)?.to_path_buf(),
            version: doc.version(),
            last_saved_time: doc.last_saved_time(),
            modified: doc.is_modified(),
            text: doc.text().clone(),
            disk_text: doc.disk_text().clone(),
            encoding: doc.encoding(),
            trust_full,
        })
    }

    /// Whether `doc` is still as it was taken.
    fn is_current(&self, doc: &Document) -> bool {
        doc.version() == self.version
            && doc.last_saved_time() == self.last_saved_time
            && doc.path() == Some(&self.path)
    }

    /// Compares the file with what the document knows of it.
    fn read(&self, registry: &DiffProviderRegistry) -> Outcome {
        match std::fs::metadata(&self.path).and_then(|metadata| metadata.modified()) {
            Ok(mtime) if mtime == self.last_saved_time => return Outcome::Known,
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Outcome::Deleted,
            Err(err) => return Outcome::Failed(err.into()),
        }
        let disk = match DiskText::read(&self.path, self.encoding) {
            Ok(disk) => disk,
            Err(err) if is_not_found(&err) => return Outcome::Deleted,
            Err(err) => return Outcome::Failed(err),
        };
        if disk.text == self.disk_text {
            return Outcome::Rewritten(disk.mtime);
        }
        if self.modified {
            return Outcome::Conflict(disk);
        }
        Outcome::Changed(Reload {
            changes: compare_ropes(&self.text, &disk.text),
            disk,
            diff_base: registry.get_diff_base(&self.path, self.trust_full),
            head: registry.get_current_head_name(&self.path, self.trust_full),
        })
    }
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>()
        .is_some_and(|err| err.kind() == io::ErrorKind::NotFound)
}

/// Applies what the files turned out to be, asking about the documents with unsaved changes.
fn apply(
    results: Vec<(Candidate, Outcome)>,
    trigger: Trigger,
    editor: &mut Editor,
    compositor: &mut Compositor,
) {
    if editor.write_count > 0 {
        let docs = results.iter().map(|(candidate, _)| candidate.doc).collect();
        return retry(docs, trigger);
    }
    let Some(question) = compositor
        .find::<EditorView>()
        .map(|editor_view| &mut editor_view.reload_question)
    else {
        return;
    };
    let mut reloaded = Vec::new();
    let mut deleted = Vec::new();
    let mut stale = HashSet::new();
    for (candidate, outcome) in results {
        let Some(doc) = editor.document_mut(candidate.doc) else {
            continue;
        };
        if !candidate.is_current(doc) {
            stale.insert(candidate.doc);
            continue;
        }
        match outcome {
            Outcome::Known => {}
            Outcome::Deleted => {
                question.forget(candidate.doc);
                if trigger == Trigger::Watcher {
                    deleted.push(candidate.path);
                }
            }
            Outcome::Rewritten(mtime) => {
                // Back to what the buffer knows, like after `git stash pop`.
                question.forget(candidate.doc);
                let text = doc.disk_text().clone();
                doc.ignore_disk_change(DiskText { text, mtime });
            }
            Outcome::Conflict(disk) => question.ask(candidate.doc, disk, candidate.last_saved_time),
            Outcome::Changed(reload) => {
                editor.apply_reload(candidate.doc, reload.disk, &reload.changes);
                doc_mut!(editor, &candidate.doc).set_vcs(reload.diff_base, reload.head);
                reloaded.push(candidate.path);
            }
            Outcome::Failed(err) => {
                let path = get_relative_path(&candidate.path);
                editor.set_error(format!("Failed to reload {}: {err}", path.display()));
            }
        }
    }
    report(editor, &reloaded, &deleted);
    if !stale.is_empty() {
        retry(stale, trigger);
    }
}

/// Tells which buffers were reloaded and which files were deleted.
fn report(editor: &mut Editor, reloaded: &[PathBuf], deleted: &[PathBuf]) {
    let reloaded = match reloaded {
        [] => None,
        [path] => Some(format!("{} reloaded", get_relative_path(path).display())),
        paths => Some(format!("{} buffers reloaded", paths.len())),
    };
    let deleted = match deleted {
        [] => None,
        [path] => Some(format!(
            "{} was deleted on disk",
            get_relative_path(path).display()
        )),
        paths => Some(format!("{} files were deleted on disk", paths.len())),
    };
    match (reloaded, deleted) {
        (Some(reloaded), None) => editor.set_status(reloaded),
        (None, Some(deleted)) => editor.set_warning(deleted),
        (Some(reloaded), Some(deleted)) => editor.set_warning(format!("{reloaded}; {deleted}")),
        (None, None) => {}
    }
}

pub(super) fn register_hooks(_handlers: &Handlers) {
    let watching = Arc::new(Mutex::new(Watching::default()));

    let watching_ = watching.clone();
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        Watching::follow(&watching_, event.editor);
        Ok(())
    });

    let watching_ = watching.clone();
    register_hook!(move |event: &mut DocumentDidClose<'_>| {
        Watching::follow(&watching_, event.editor);
        Ok(())
    });

    let watching_ = watching.clone();
    register_hook!(move |event: &mut DocumentPathDidChange<'_>| {
        Watching::follow(&watching_, event.editor);
        Ok(())
    });

    register_hook!(move |event: &mut ConfigDidChange<'_>| {
        Watching::follow(&watching, event.editor);
        Ok(())
    });
}
