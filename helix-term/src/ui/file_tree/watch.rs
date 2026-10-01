//! Watching directories, so that the tree and dired buffers follow changes made outside of them:
//! in another program, by `git`, or by a build.

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use helix_event::{send_blocking, AsyncHook};
use helix_view::Editor;
use notify::{
    event::{AccessKind, AccessMode},
    EventKind, RecommendedWatcher, RecursiveMode, Watcher as _,
};
use tokio::time::Instant;

use crate::{compositor::Compositor, job};

/// What handles a batch of changed paths, on the main thread.
type Handler = Arc<dyn Fn(HashSet<PathBuf>, &mut Editor, &mut Compositor) + Send + Sync>;

/// How long changes are collected before the tree handles them, so that a burst of them, like
/// `git checkout` rewriting many files, is handled at once.
const DEBOUNCE: Duration = Duration::from_millis(100);

pub struct Watcher {
    inner: RecommendedWatcher,
    watched: HashSet<PathBuf>,
}

impl Watcher {
    /// A watcher handing the paths that changed to `handle`.
    pub fn new(
        handle: impl Fn(HashSet<PathBuf>, &mut Editor, &mut Compositor) + Send + Sync + 'static,
    ) -> notify::Result<Self> {
        let changes = Changes {
            handle: Arc::new(handle),
            paths: HashSet::new(),
        }
        .spawn();
        // A full channel is waited for with a timer of the runtime, which notify's thread has
        // to enter first: a burst of changes, like many renames, fills it.
        let runtime = tokio::runtime::Handle::current();
        let inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else {
                return;
            };
            let _runtime = runtime.enter();
            // Listing a directory opens it; only a finished write is a change.
            if let EventKind::Access(kind) = event.kind {
                if kind != AccessKind::Close(AccessMode::Write) {
                    return;
                }
            }
            for path in event.paths {
                send_blocking(&changes, path);
            }
        })?;
        Ok(Self {
            inner,
            watched: HashSet::new(),
        })
    }

    /// Watches exactly the directories `dirs`, each without its subdirectories.
    pub fn watch(&mut self, dirs: HashSet<PathBuf>) {
        for dir in self.watched.difference(&dirs) {
            // The directory may be gone already.
            let _ = self.inner.unwatch(dir);
        }
        for dir in dirs.difference(&self.watched) {
            if let Err(err) = self.inner.watch(dir, RecursiveMode::NonRecursive) {
                // Out of inotify watches, for example; focusing the terminal still refreshes.
                log::warn!("file tree cannot watch {}: {err}", dir.display());
            }
        }
        self.watched = dirs;
    }
}

/// Collects changed paths and hands them over in batches.
struct Changes {
    handle: Handler,
    paths: HashSet<PathBuf>,
}

impl AsyncHook for Changes {
    type Event = PathBuf;

    fn handle_event(&mut self, path: PathBuf, timeout: Option<Instant>) -> Option<Instant> {
        self.paths.insert(path);
        // At most `DEBOUNCE` after the first change, even while more keep coming.
        timeout.or_else(|| Some(Instant::now() + DEBOUNCE))
    }

    fn finish_debounce(&mut self) {
        let (handle, paths) = (self.handle.clone(), std::mem::take(&mut self.paths));
        job::dispatch_blocking(move |editor, compositor| handle(paths, editor, compositor));
    }
}
