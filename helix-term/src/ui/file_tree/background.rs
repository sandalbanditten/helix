//! Getting work off the main thread and its results back to the tree.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use helix_view::Editor;
use tokio::sync::mpsc::unbounded_channel;

use super::{
    file_tree,
    fs::{self, ListOptions},
    FileTree,
};
use crate::job;

/// Runs `work` in the background, then `apply` with its result on the file tree.
///
/// The work gets a thread of its own rather than one of tokio's blocking threads, as the file
/// picker's walk does: quitting waits for those, and a git status can take seconds.
pub(super) fn in_background<T: Send + 'static>(
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

pub(super) struct ListRequest {
    pub(super) generation: u64,
    pub(super) root: Arc<Path>,
    /// Relative to `root`, parents before their children.
    pub(super) dirs: Vec<PathBuf>,
    pub(super) options: ListOptions,
}

/// Hands directories to the background thread that lists them.
pub(super) struct Lister {
    pub(super) sender: std::sync::mpsc::Sender<ListRequest>,
    /// Whether listings should tell executables apart.
    pub(super) executables: bool,
}

impl Lister {
    pub(super) fn request(&self, request: ListRequest) {
        // The thread only stops once the tree is gone.
        let _ = self.sender.send(request);
    }
}

/// Starts listing directories in the background, one request after another so that the results
/// arrive in the order they were asked for. Like [`in_background`], the listing has a thread of
/// its own.
pub(super) fn spawn_lister() -> std::sync::mpsc::Sender<ListRequest> {
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
