//! The git status of the workspace as the file tree presents it.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use helix_vcs::FileChange;

/// The status a git mark shows, ordered by strength: a directory shows the strongest status
/// beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitStatus {
    Created,
    Modified,
    Deleted,
    Conflict,
}

/// The status of the paths below one workspace root, keyed by their relative path.
#[derive(Debug, Default)]
pub struct GitStatuses {
    exact: HashMap<PathBuf, GitStatus>,
    /// The strongest status below each directory.
    below: HashMap<PathBuf, GitStatus>,
    /// Ignored paths; everything below an ignored directory is ignored too.
    ignored: HashSet<PathBuf>,
}

impl GitStatuses {
    /// Collects `changes` below `root`, dropping the ones outside it.
    pub fn new(root: &Path, changes: impl IntoIterator<Item = FileChange>) -> Self {
        let mut statuses = Self::default();
        for change in changes {
            let Ok(path) = change.path().strip_prefix(root) else {
                continue;
            };
            let path = path.to_path_buf();
            let status = match change {
                FileChange::Untracked { .. } | FileChange::Added { .. } => GitStatus::Created,
                FileChange::Modified { .. } | FileChange::Renamed { .. } => GitStatus::Modified,
                FileChange::Deleted { .. } => GitStatus::Deleted,
                FileChange::Conflict { .. } => GitStatus::Conflict,
                FileChange::Ignored { .. } => {
                    statuses.ignored.insert(path);
                    continue;
                }
            };
            for ancestor in path.ancestors().skip(1) {
                strengthen(&mut statuses.below, ancestor.to_path_buf(), status);
            }
            strengthen(&mut statuses.exact, path, status);
        }
        statuses
    }

    /// The mark of the entry at `path`: its own status or, for a directory, the strongest one
    /// below it. Ignored entries have none.
    pub fn status(&self, path: &Path, is_dir: bool) -> Option<GitStatus> {
        if self.is_ignored(path) {
            return None;
        }
        let exact = self.exact.get(path).copied();
        let below = is_dir.then(|| self.below.get(path).copied()).flatten();
        exact.max(below)
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        !self.ignored.is_empty()
            && path
                .ancestors()
                .take_while(|ancestor| !ancestor.as_os_str().is_empty())
                .any(|ancestor| self.ignored.contains(ancestor))
    }
}

fn strengthen(statuses: &mut HashMap<PathBuf, GitStatus>, path: PathBuf, status: GitStatus) {
    statuses
        .entry(path)
        .and_modify(|current| *current = (*current).max(status))
        .or_insert(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_show_the_strongest_status_below() {
        let root = Path::new("/workspace");
        let path = |path: &str| root.join(path);
        let statuses = GitStatuses::new(
            root,
            [
                FileChange::Untracked {
                    path: path("src/new.rs"),
                },
                FileChange::Modified {
                    path: path("src/deep/lib.rs"),
                },
                FileChange::Deleted {
                    path: path("src/deep/gone.rs"),
                },
                FileChange::Added {
                    path: path("docs/guide.md"),
                },
                FileChange::Ignored {
                    path: path("target"),
                },
                FileChange::Modified {
                    path: "/elsewhere/file".into(),
                },
            ],
        );
        let status = |path: &str, is_dir| statuses.status(Path::new(path), is_dir);
        assert_eq!(status("src/new.rs", false), Some(GitStatus::Created));
        assert_eq!(status("src/deep", true), Some(GitStatus::Deleted));
        assert_eq!(status("src", true), Some(GitStatus::Deleted));
        assert_eq!(status("", true), Some(GitStatus::Deleted));
        assert_eq!(status("docs", true), Some(GitStatus::Created));
        assert_eq!(status("README.md", false), None);
        assert!(statuses.is_ignored(Path::new("target/debug/hx")));
        assert!(!statuses.is_ignored(Path::new("targets")));
        assert_eq!(status("target", true), None);
    }
}
