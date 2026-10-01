use std::path::{Path, PathBuf};

/// States for a file having been changed.
pub enum FileChange {
    /// Not tracked by the VCS.
    Untracked { path: PathBuf },
    /// A new file staged in the index. Only reported with [`StatusOptions::staged`].
    Added { path: PathBuf },
    /// File has been modified.
    Modified { path: PathBuf },
    /// File modification is in conflict with a different update.
    Conflict { path: PathBuf },
    /// File has been deleted.
    Deleted { path: PathBuf },
    /// File has been renamed.
    Renamed {
        from_path: PathBuf,
        to_path: PathBuf,
    },
}

/// What a status query reports besides the changes between the index and the working tree.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StatusOptions {
    /// Also report the changes staged in the index, i.e. between `HEAD` and the index.
    pub staged: bool,
}

impl FileChange {
    pub fn path(&self) -> &Path {
        match self {
            Self::Untracked { path } => path,
            Self::Added { path } => path,
            Self::Modified { path } => path,
            Self::Conflict { path } => path,
            Self::Deleted { path } => path,
            Self::Renamed { to_path, .. } => to_path,
        }
    }
}

/// The side of `git status` a change is on, like the two letters of `git status --short`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Staged: between `HEAD` and the index.
    Index,
    /// Not staged: between the index and the working tree.
    Worktree,
}

/// What changed about a path on one [`Side`], with renames seen as a deletion and an addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Added to the index, or untracked in the working tree.
    New,
    Modified,
    Deleted,
    /// A file became a link or the other way around.
    TypeChange,
    Conflict,
}

/// One change of a path on one side of `git status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideChange {
    pub path: PathBuf,
    pub side: Side,
    pub change: Change,
}

/// The git status below a directory, see
/// [`DiffProviderRegistry::status_by_side`](crate::DiffProviderRegistry::status_by_side).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DirStatus {
    /// The working tree of the repository holding the directory.
    pub workdir: PathBuf,
    pub changes: Vec<SideChange>,
    /// For each path asked about, whether the index tracks it or, for a directory, anything in it.
    pub tracked: Vec<bool>,
}
