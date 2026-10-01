//! The directory listing a dired buffer shows.
//!
//! A dired buffer is a document whose lines list files the way `eza --git -aolg` does. Writing
//! it applies the edits of its lines to the files, so the listing keeps what each line showed.

use helix_core::Rope;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// What a dired buffer lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The entries of a directory.
    Directory(PathBuf),
    /// A directory and, nested below them, the entries of the directories in `expanded`
    /// (relative to `root`), the way the file tree shows them.
    Tree {
        root: PathBuf,
        expanded: BTreeSet<PathBuf>,
    },
}

impl Source {
    /// The directory the paths of the listing are relative to.
    pub fn root(&self) -> &Path {
        match self {
            Self::Directory(root) | Self::Tree { root, .. } => root,
        }
    }
}

/// The columns the lines of a listing have besides the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    /// The octal permissions, the permissions, the user and the group.
    pub unix: bool,
    /// The git status, for a listing in a git working tree.
    pub git: bool,
    /// An icon before each name.
    pub icons: bool,
}

/// The entries a dired buffer listed, one per line, as they were when listed.
#[derive(Debug, Clone)]
pub struct Listing {
    pub source: Source,
    pub columns: Columns,
    /// The working tree of the git repository holding the listed directory.
    pub repo: Option<PathBuf>,
    pub entries: Vec<Entry>,
    /// The text the entries were shown as, one line each, which a write compares the edited
    /// text with.
    pub text: Rope,
}

impl Listing {
    /// How the buffer is called, like `[dired] src/`.
    pub fn display_name(&self) -> String {
        let root = helix_stdx::path::get_relative_path(self.source.root());
        let root = root.to_string_lossy();
        match root.as_ref() {
            "" => "[dired] ./".to_owned(),
            "/" => "[dired] /".to_owned(),
            root => format!("[dired] {root}/"),
        }
    }
}

/// One listed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the root of the listing; empty for the root line of a tree listing.
    pub path: PathBuf,
    pub kind: Kind,
    /// The permission bits, including setuid, setgid and sticky.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// The user and group as shown: their names, or their ids when they have none.
    pub user: String,
    pub group: String,
    pub size: Size,
    pub modified: SystemTime,
    /// The device and inode, which tell whether the path still holds the listed file.
    pub id: (u64, u64),
    pub link: Option<Link>,
    pub git: Option<GitStatus>,
    /// The tree guides before the name, like `│   ├── `.
    pub guides: String,
    pub icon: Option<&'static str>,
}

/// What an entry is. Links are not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Directory,
    File,
    Link,
    Fifo,
    Socket,
    BlockDevice,
    CharDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Directories and links show no size.
    None,
    Bytes(u64),
    Device {
        major: u32,
        minor: u32,
    },
}

/// Where a symbolic link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// As stored in the link, relative or absolute.
    pub target: PathBuf,
    /// What the target is, or `None` if it is missing.
    pub target_kind: Option<Kind>,
}

/// The git status of an entry like `eza --git` shows it: the letters of its staged and of its
/// unstaged change (`-` for none, or `M`, `N`, `D`, `R`, `T`, `I` and `U`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitStatus {
    pub index: char,
    pub worktree: char,
}
