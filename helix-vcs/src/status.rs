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
