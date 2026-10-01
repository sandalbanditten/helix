//! Reading what a dired buffer lists. Everything here blocks, so a listing is read off the main
//! thread when a buffer opens.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs::{self, Metadata},
    path::{Path, PathBuf},
};

use helix_core::Rope;
use helix_vcs::{Change, DiffProviderRegistry, Side};
use helix_view::{
    dired::{Columns, Entry, GitStatus, Kind, Link, Listing, Size, Source},
    editor::FileTreeSort,
};

use crate::{
    is_vcs_dir,
    ui::file_tree::{
        fs::not_ignored,
        icons,
        order::{entry_cmp, Group},
    },
};

/// How a listing is read.
#[derive(Clone)]
pub struct Options {
    pub sort: FileTreeSort,
    pub icons: bool,
    pub providers: DiffProviderRegistry,
    /// Whether the workspace is trusted to run git with its own configuration.
    pub trust_git: bool,
}

/// Reads the entries `source` lists, in the order of their lines.
pub fn read(source: &Source, options: &Options) -> Listing {
    let mut users = Names::default();
    let mut entries = Vec::new();
    match source {
        Source::Directory(dir) => {
            for (name, metadata) in list(dir, options.sort) {
                let path = PathBuf::from(&name);
                entries.push(entry(
                    dir,
                    path,
                    &metadata,
                    String::new(),
                    options,
                    &mut users,
                ));
            }
        }
        Source::Tree { root, expanded } => {
            if let Ok(metadata) = fs::symlink_metadata(root) {
                let mut entry = entry(
                    root,
                    PathBuf::new(),
                    &metadata,
                    String::new(),
                    options,
                    &mut users,
                );
                entry.icon = options.icons.then(|| icons::directory(".", false));
                entries.push(entry);
            }
            walk(
                root,
                Path::new(""),
                "",
                expanded,
                options,
                &mut users,
                &mut entries,
            );
        }
    }

    let root = source.root();
    let ignored = ignored_entries(root, source, &entries);
    let candidates: Vec<_> = ignored
        .iter()
        .map(|&i| root.join(&entries[i].path))
        .collect();
    let status = options
        .providers
        .status_by_side(root, options.trust_git, &candidates)
        .ok();
    let repo = status.as_ref().map(|status| status.workdir.clone());
    if let Some(status) = status {
        let marks = Marks::new(root, &status.changes);
        let ignored: HashSet<_> = ignored
            .into_iter()
            .zip(&status.tracked)
            .filter(|(_, tracked)| !**tracked)
            .map(|(index, _)| index)
            .collect();
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.git = Some(marks.status(&entry.path, entry.kind, ignored.contains(&index)));
        }
    }
    Listing {
        source: source.clone(),
        columns: Columns {
            unix: cfg!(unix),
            git: repo.is_some(),
            icons: options.icons,
        },
        repo,
        entries,
        text: Rope::new(),
        yanked: None,
        writing: false,
    }
}

/// Appends the entries of the directory `dir` (relative to `root`) and, below each expanded
/// directory, its own entries, with the tree guides of `eza --tree`. `lanes` are the guides of
/// the directories `dir` hangs from.
#[allow(clippy::too_many_arguments)]
fn walk(
    root: &Path,
    dir: &Path,
    lanes: &str,
    expanded: &std::collections::BTreeSet<PathBuf>,
    options: &Options,
    users: &mut Names,
    entries: &mut Vec<Entry>,
) {
    let listed = list(&root.join(dir), options.sort);
    let count = listed.len();
    for (i, (name, metadata)) in listed.into_iter().enumerate() {
        let last = i + 1 == count;
        let path = dir.join(&name);
        let guides = format!("{lanes}{}", if last { "└── " } else { "├── " });
        let is_dir = metadata.is_dir();
        entries.push(entry(root, path.clone(), &metadata, guides, options, users));
        if is_dir && expanded.contains(&path) {
            let lanes = format!("{lanes}{}", if last { "    " } else { "│   " });
            walk(root, &path, &lanes, expanded, options, users, entries);
        }
    }
}

/// The entries of `dir` but VCS directories, sorted like the file tree sorts them.
fn list(dir: &Path, sort: FileTreeSort) -> Vec<(OsString, Metadata)> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = read
        .filter_map(Result::ok)
        .filter(|entry| !is_vcs_dir(&entry.file_name()))
        .filter_map(|entry| Some((entry.file_name(), fs::symlink_metadata(entry.path()).ok()?)))
        .map(|(name, metadata)| {
            let group = group(&dir.join(&name), &metadata);
            (name, metadata, group)
        })
        .collect();
    entries.sort_by(|a, b| {
        entry_cmp(
            sort,
            (&a.0.to_string_lossy(), a.2),
            (&b.0.to_string_lossy(), b.2),
        )
    });
    entries
        .into_iter()
        .map(|(name, metadata, _)| (name, metadata))
        .collect()
}

/// Where an entry sorts, the way the file tree groups it.
fn group(path: &Path, metadata: &Metadata) -> Group {
    let metadata = if metadata.is_symlink() {
        match fs::metadata(path) {
            Ok(target) => target,
            Err(_) => return Group::Other,
        }
    } else {
        metadata.clone()
    };
    if metadata.is_dir() {
        Group::Directory
    } else if metadata.is_file() {
        Group::File
    } else {
        Group::Other
    }
}

fn entry(
    root: &Path,
    path: PathBuf,
    metadata: &Metadata,
    guides: String,
    options: &Options,
    users: &mut Names,
) -> Entry {
    let absolute = root.join(&path);
    let kind = kind(metadata);
    let link = (kind == Kind::Link).then(|| Link {
        target: fs::read_link(&absolute).unwrap_or_default(),
        target_kind: fs::metadata(&absolute)
            .ok()
            .map(|target| self::kind(&target)),
    });
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let target = link.as_ref().map(|link| link.target_kind);
    let icon = options.icons.then(|| match (kind, target) {
        (Kind::Directory, _) | (_, Some(Some(Kind::Directory))) => icons::directory(&name, false),
        (_, Some(None)) => icons::BROKEN_LINK,
        _ => icons::file(&name),
    });
    let size = match kind {
        Kind::Directory | Kind::Link => Size::None,
        Kind::BlockDevice | Kind::CharDevice => device(metadata),
        _ => Size::Bytes(metadata.len()),
    };
    let (mode, uid, gid, id) = unix_metadata(metadata);
    Entry {
        path,
        kind,
        mode,
        uid,
        gid,
        user: users.user(uid),
        group: users.group(gid),
        size,
        modified: metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        id,
        link,
        git: None,
        guides,
        icon,
    }
}

#[cfg(unix)]
fn kind(metadata: &Metadata) -> Kind {
    use std::os::unix::fs::FileTypeExt;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        Kind::Link
    } else if file_type.is_dir() {
        Kind::Directory
    } else if file_type.is_fifo() {
        Kind::Fifo
    } else if file_type.is_socket() {
        Kind::Socket
    } else if file_type.is_block_device() {
        Kind::BlockDevice
    } else if file_type.is_char_device() {
        Kind::CharDevice
    } else {
        Kind::File
    }
}

#[cfg(not(unix))]
fn kind(metadata: &Metadata) -> Kind {
    if metadata.is_symlink() {
        Kind::Link
    } else if metadata.is_dir() {
        Kind::Directory
    } else {
        Kind::File
    }
}

#[cfg(unix)]
fn device(metadata: &Metadata) -> Size {
    use std::os::unix::fs::MetadataExt;
    let (major, minor) = helix_stdx::fs::device_numbers(metadata.rdev());
    Size::Device { major, minor }
}

#[cfg(not(unix))]
fn device(_metadata: &Metadata) -> Size {
    Size::None
}

/// The mode, owner, group and identity of an entry.
#[cfg(unix)]
fn unix_metadata(metadata: &Metadata) -> (u32, u32, u32, (u64, u64)) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.mode() & 0o7777,
        metadata.uid(),
        metadata.gid(),
        (metadata.dev(), metadata.ino()),
    )
}

#[cfg(not(unix))]
fn unix_metadata(_metadata: &Metadata) -> (u32, u32, u32, (u64, u64)) {
    (0, 0, 0, (0, 0))
}

/// The names of users and groups, looked up once each.
#[derive(Default)]
struct Names {
    users: HashMap<u32, String>,
    groups: HashMap<u32, String>,
}

impl Names {
    fn user(&mut self, uid: u32) -> String {
        self.users
            .entry(uid)
            .or_insert_with(|| user_name(uid).unwrap_or_else(|| uid.to_string()))
            .clone()
    }

    fn group(&mut self, gid: u32) -> String {
        self.groups
            .entry(gid)
            .or_insert_with(|| group_name(gid).unwrap_or_else(|| gid.to_string()))
            .clone()
    }
}

#[cfg(unix)]
use helix_stdx::users::{group_name, user_name};

#[cfg(not(unix))]
fn user_name(_uid: u32) -> Option<String> {
    None
}

#[cfg(not(unix))]
fn group_name(_gid: u32) -> Option<String> {
    None
}

/// The indices of the entries the ignore files match, including everything in a directory they
/// match, by the rules the file tree follows. Whether git tracks them is not known yet.
fn ignored_entries(root: &Path, source: &Source, entries: &[Entry]) -> Vec<usize> {
    let root_ignored = within_ignored(root);
    let mut ignored_dirs: HashSet<PathBuf> = HashSet::new();
    let mut kept: HashMap<PathBuf, HashSet<OsString>> = HashMap::new();
    let mut ignored = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(parent) = entry.path.parent() else {
            // The root line of a tree.
            if root_ignored {
                ignored.push(index);
            }
            continue;
        };
        let inherited = root_ignored || parent.ancestors().any(|dir| ignored_dirs.contains(dir));
        let matched = inherited || {
            let kept = kept
                .entry(parent.to_path_buf())
                .or_insert_with(|| not_ignored(&root.join(parent)));
            !kept.contains(entry.path.file_name().unwrap_or_default())
        };
        if matched {
            ignored.push(index);
            if entry.kind == Kind::Directory && matches!(source, Source::Tree { .. }) {
                ignored_dirs.insert(entry.path.clone());
            }
        }
    }
    ignored
}

/// Whether `dir` lies in a directory the ignore files match, up to its repository's root.
fn within_ignored(dir: &Path) -> bool {
    for dir in dir.ancestors() {
        if dir.join(".git").exists() {
            return false;
        }
        let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
            return false;
        };
        if !not_ignored(parent).contains(name) {
            return true;
        }
    }
    false
}

/// The changes below the root of a listing, by relative path: their own and, for directories,
/// all of those below them.
struct Marks {
    exact: HashMap<PathBuf, (u8, u8)>,
    below: HashMap<PathBuf, (u8, u8)>,
}

const NEW: u8 = 1;
const MODIFIED: u8 = 1 << 1;
const DELETED: u8 = 1 << 2;
const TYPE_CHANGE: u8 = 1 << 3;
const CONFLICT: u8 = 1 << 4;

impl Marks {
    fn new(root: &Path, changes: &[helix_vcs::SideChange]) -> Self {
        let mut marks = Self {
            exact: HashMap::new(),
            below: HashMap::new(),
        };
        for change in changes {
            let Ok(path) = change.path.strip_prefix(root) else {
                continue;
            };
            let bit = match change.change {
                Change::New => NEW,
                Change::Modified => MODIFIED,
                Change::Deleted => DELETED,
                Change::TypeChange => TYPE_CHANGE,
                Change::Conflict => CONFLICT,
            };
            let add = |marks: &mut (u8, u8)| match change.side {
                Side::Index => marks.0 |= bit,
                Side::Worktree => marks.1 |= bit,
            };
            add(marks.exact.entry(path.to_path_buf()).or_default());
            for ancestor in path.ancestors().skip(1) {
                add(marks.below.entry(ancestor.to_path_buf()).or_default());
            }
        }
        marks
    }

    /// The status `eza --git` shows for the entry at `path`.
    fn status(&self, path: &Path, kind: Kind, ignored: bool) -> GitStatus {
        let (mut index, mut worktree) = self.exact.get(path).copied().unwrap_or_default();
        if kind == Kind::Directory {
            let (below_index, below_worktree) = self.below.get(path).copied().unwrap_or_default();
            index |= below_index;
            worktree |= below_worktree;
        }
        let letter = |bits: u8, ignored: bool| {
            [
                (NEW, 'N'),
                (MODIFIED, 'M'),
                (DELETED, 'D'),
                (TYPE_CHANGE, 'T'),
            ]
            .into_iter()
            .find(|(bit, _)| bits & bit != 0)
            .map(|(_, letter)| letter)
            .or(ignored.then_some('I'))
            .or((bits & CONFLICT != 0).then_some('U'))
            .unwrap_or('-')
        };
        GitStatus {
            index: letter(index, false),
            worktree: letter(worktree, ignored),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env_remove("GIT_DIR")
            .env("GIT_CONFIG_COUNT", "3")
            .env("GIT_CONFIG_KEY_0", "user.email")
            .env("GIT_CONFIG_VALUE_0", "test@helix.org")
            .env("GIT_CONFIG_KEY_1", "user.name")
            .env("GIT_CONFIG_VALUE_1", "helix")
            .env("GIT_CONFIG_KEY_2", "commit.gpgsign")
            .env("GIT_CONFIG_VALUE_2", "false")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    fn options() -> Options {
        Options {
            sort: FileTreeSort::DirectoriesFirst,
            icons: false,
            providers: DiffProviderRegistry::default(),
            trust_git: true,
        }
    }

    fn statuses(listing: &Listing) -> Vec<(String, String)> {
        listing
            .entries
            .iter()
            .map(|entry| {
                let git = entry
                    .git
                    .map(|git| format!("{}{}", git.index, git.worktree))
                    .unwrap_or_default();
                (format!("{}{}", entry.guides, entry.path.display()), git)
            })
            .collect()
    }

    /// The git column `eza --git -aolg --tree` showed for the same repository.
    #[test]
    fn tree_listings_show_the_git_status_like_eza() {
        let dir = tempfile::tempdir().unwrap();
        let repo = &helix_stdx::path::canonicalize(dir.path());
        git(repo, &["init", "-q"]);
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        fs::create_dir(repo.join("ignored")).unwrap();
        fs::write(repo.join(".gitignore"), "*.log\nignored/\n").unwrap();
        for file in ["staged.txt", "both.txt", "src/deep/lib.rs"] {
            fs::write(repo.join(file), "one").unwrap();
        }
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "init"]);
        fs::write(repo.join("staged.txt"), "two").unwrap();
        fs::write(repo.join("both.txt"), "two").unwrap();
        git(repo, &["add", "staged.txt", "both.txt"]);
        fs::write(repo.join("both.txt"), "three").unwrap();
        fs::write(repo.join("src/deep/lib.rs"), "two").unwrap();
        fs::write(repo.join("new.txt"), "").unwrap();
        fs::write(repo.join("build.log"), "").unwrap();
        fs::write(repo.join("ignored/inner.txt"), "").unwrap();

        let expanded = ["src", "src/deep", "ignored"].map(PathBuf::from).into();
        let source = Source::Tree {
            root: repo.clone(),
            expanded,
        };
        let listing = read(&source, &options());
        assert!(listing.columns.git);
        assert_eq!(listing.repo.as_ref(), Some(repo));
        let line = |path: &str, git: &str| (path.to_owned(), git.to_owned());
        assert_eq!(
            statuses(&listing),
            [
                line("", "MN"),
                line("├── ignored", "-I"),
                line("│   └── ignored/inner.txt", "-I"),
                line("├── src", "-M"),
                line("│   └── src/deep", "-M"),
                line("│       └── src/deep/lib.rs", "-M"),
                line("├── .gitignore", "--"),
                line("├── both.txt", "MM"),
                line("├── build.log", "-I"),
                line("├── new.txt", "-N"),
                line("└── staged.txt", "M-"),
            ]
        );

        // Listed on its own, an ignored directory's entries are ignored too.
        let listing = read(&Source::Directory(repo.join("ignored")), &options());
        assert_eq!(statuses(&listing), [line("inner.txt", "-I")]);
    }

    #[test]
    fn listings_outside_repositories_have_no_git_column() {
        let dir = tempfile::tempdir().unwrap();
        let root = helix_stdx::path::canonicalize(dir.path());
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("file"), "12345").unwrap();
        let listing = read(&Source::Directory(root.clone()), &options());
        assert!(!listing.columns.git);
        assert_eq!(listing.repo, None);
        let entries: Vec<_> = listing
            .entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.kind, entry.size))
            .collect();
        assert_eq!(
            entries,
            [
                (PathBuf::from("sub"), Kind::Directory, Size::None),
                (PathBuf::from("file"), Kind::File, Size::Bytes(5)),
            ]
        );
    }
}
