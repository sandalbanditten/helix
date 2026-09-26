//! Watching the directories the tree has listed, so that it follows changes made outside of it:
//! in another program, by `git`, or by a build.

use std::{collections::HashSet, path::PathBuf, time::Duration};

use helix_event::{send_blocking, AsyncHook};
use notify::{
    event::{AccessKind, AccessMode},
    EventKind, RecommendedWatcher, RecursiveMode, Watcher as _,
};
use tokio::time::Instant;

use crate::job;

/// How long changes are collected before the tree handles them, so that a burst of them, like
/// `git checkout` rewriting many files, is handled at once.
const DEBOUNCE: Duration = Duration::from_millis(100);

pub struct Watcher {
    inner: RecommendedWatcher,
    watched: HashSet<PathBuf>,
}

impl Watcher {
    /// A watcher handing the paths that changed to the tree of the workspace `generation`.
    pub fn new(generation: u64) -> notify::Result<Self> {
        let changes = Changes {
            generation,
            paths: HashSet::new(),
        }
        .spawn();
        let inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else {
                return;
            };
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

/// Collects changed paths and hands them to the tree in batches.
struct Changes {
    generation: u64,
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
        let (generation, paths) = (self.generation, std::mem::take(&mut self.paths));
        job::dispatch_blocking(move |editor, compositor| {
            if let Some(file_tree) = super::file_tree(compositor) {
                file_tree.changed(generation, paths, editor);
            }
        });
    }
}
