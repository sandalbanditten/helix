//! The git status of the workspace as the file tree presents it.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use helix_vcs::FileChange;

/// The status a git mark shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatus {
    Created,
    Modified,
    Deleted,
    Conflict,
}

impl GitStatus {
    /// The status of a directory holding entries of `self` and of `other`: theirs if they share
    /// it, else modified, unless one is in conflict.
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Conflict, _) | (_, Self::Conflict) => Self::Conflict,
            _ if self == other => self,
            _ => Self::Modified,
        }
    }
}

/// The status of the paths below one workspace root, keyed by their relative path.
#[derive(Debug, Default)]
pub struct GitStatuses {
    exact: HashMap<PathBuf, GitStatus>,
    /// The status of the entries below each directory, joined.
    below: HashMap<PathBuf, GitStatus>,
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
            };
            for ancestor in path.ancestors().skip(1) {
                join(&mut statuses.below, ancestor.to_path_buf(), status);
            }
            join(&mut statuses.exact, path, status);
        }
        statuses
    }

    /// The mark of the entry at `path`: its own status or, for a directory, also that of the
    /// entries below it.
    pub fn status(&self, path: &Path, is_dir: bool) -> Option<GitStatus> {
        let exact = self.exact.get(path).copied();
        let below = is_dir.then(|| self.below.get(path).copied()).flatten();
        match (exact, below) {
            (Some(exact), Some(below)) => Some(exact.join(below)),
            _ => exact.or(below),
        }
    }
}

fn join(statuses: &mut HashMap<PathBuf, GitStatus>, path: PathBuf, status: GitStatus) {
    statuses
        .entry(path)
        .and_modify(|current| *current = current.join(status))
        .or_insert(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_show_the_status_their_files_share() {
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
                FileChange::Deleted {
                    path: path("src/old/a.rs"),
                },
                FileChange::Deleted {
                    path: path("src/old/b.rs"),
                },
                FileChange::Added {
                    path: path("docs/guide.md"),
                },
                FileChange::Untracked {
                    path: path("docs/notes.md"),
                },
                FileChange::Untracked {
                    path: path("tests/new.rs"),
                },
                FileChange::Conflict {
                    path: path("tests/both.rs"),
                },
                FileChange::Modified {
                    path: "/elsewhere/file".into(),
                },
            ],
        );
        let status = |path: &str, is_dir| statuses.status(Path::new(path), is_dir);
        assert_eq!(status("src/new.rs", false), Some(GitStatus::Created));
        assert_eq!(status("src/deep", true), Some(GitStatus::Modified));
        assert_eq!(status("src/old", true), Some(GitStatus::Deleted));
        assert_eq!(
            status("src", true),
            Some(GitStatus::Modified),
            "created, modified and deleted files below"
        );
        assert_eq!(
            status("docs", true),
            Some(GitStatus::Created),
            "added and new"
        );
        assert_eq!(status("tests", true), Some(GitStatus::Conflict));
        assert_eq!(status("", true), Some(GitStatus::Conflict));
        assert_eq!(status("README.md", false), None);
    }
}
