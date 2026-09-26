//! Reading directories for the file tree. Everything here blocks, so it runs off the main thread.

use std::{
    fs::{self, DirEntry, FileType},
    path::{Path, PathBuf},
};

use helix_view::editor::FileTreeSort;

use super::{
    order::entry_cmp,
    tree::{Entry, Kind, LinkTarget, Listing, Special},
};
use crate::is_vcs_dir;

#[derive(Debug, Clone, Copy)]
pub struct ListOptions {
    pub sort: FileTreeSort,
    /// Whether to find out which files are executable, which costs a `stat` per file.
    pub executables: bool,
}

/// Lists the directories `dirs`, relative to `root`, in the order given.
pub fn list_all(root: &Path, dirs: Vec<PathBuf>, options: ListOptions) -> Vec<(PathBuf, Listing)> {
    dirs.into_iter()
        .map(|dir| {
            let listing = list(&root.join(&dir), options);
            (dir, listing)
        })
        .collect()
}

/// Lists the directory `dir` in tree order, probing each subdirectory for a single-child run.
/// VCS directories are left out, links are not followed.
pub fn list(dir: &Path, options: ListOptions) -> Listing {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| !is_vcs_dir(&entry.file_name()))
        .filter_map(|entry| {
            let kind = kind(&entry, entry.file_type().ok()?, options.executables)?;
            let only_child = match kind {
                Kind::Directory => probe(&entry.path()).map(Box::new),
                _ => None,
            };
            Some(Entry {
                name: entry.file_name(),
                kind,
                only_child,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        entry_cmp(
            options.sort,
            (&a.name.to_string_lossy(), a.kind.group()),
            (&b.name.to_string_lossy(), b.kind.group()),
        )
    });
    Some(entries)
}

/// The only entry of `dir` if that is a directory, probed in turn.
fn probe(dir: &Path) -> Option<Entry> {
    let mut entries = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| !is_vcs_dir(&entry.file_name()));
    let only = entries.next()?;
    if entries.next().is_some() || !only.file_type().ok()?.is_dir() {
        return None;
    }
    Some(Entry {
        name: only.file_name(),
        kind: Kind::Directory,
        only_child: probe(&only.path()).map(Box::new),
    })
}

fn kind(entry: &DirEntry, file_type: FileType, executables: bool) -> Option<Kind> {
    if file_type.is_symlink() {
        let target = match fs::metadata(entry.path()) {
            Ok(metadata) if metadata.is_dir() => LinkTarget::Directory,
            Ok(metadata) if metadata.is_file() => LinkTarget::File,
            _ => LinkTarget::Broken,
        };
        return Some(Kind::Link(target));
    }
    if file_type.is_dir() {
        return Some(Kind::Directory);
    }
    if file_type.is_file() {
        let executable = executables && is_executable(entry);
        return Some(Kind::File { executable });
    }
    special(file_type).map(Kind::Special)
}

#[cfg(unix)]
fn is_executable(entry: &DirEntry) -> bool {
    use std::os::unix::fs::PermissionsExt;
    entry
        .metadata()
        .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_entry: &DirEntry) -> bool {
    false
}

#[cfg(unix)]
fn special(file_type: FileType) -> Option<Special> {
    use std::os::unix::fs::FileTypeExt;
    if file_type.is_fifo() {
        Some(Special::Fifo)
    } else if file_type.is_socket() {
        Some(Special::Socket)
    } else if file_type.is_block_device() {
        Some(Special::BlockDevice)
    } else if file_type.is_char_device() {
        Some(Special::CharDevice)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn special(_file_type: FileType) -> Option<Special> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: ListOptions = ListOptions {
        sort: FileTreeSort::DirectoriesFirst,
        executables: true,
    };

    fn names(listing: &Listing) -> Vec<String> {
        listing
            .as_ref()
            .unwrap()
            .iter()
            .map(|entry| entry.name.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn listings_classify_and_probe_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/main/java")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/guide.md"), "").unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join("file10.txt"), "").unwrap();
        fs::write(root.join("file2.txt"), "").unwrap();

        let listing = list(root, OPTIONS);
        assert_eq!(names(&listing), ["docs", "src", "file2.txt", "file10.txt"]);
        let entries = listing.unwrap();
        assert_eq!(entries[0].only_child, None);
        let main = entries[1].only_child.as_deref().unwrap();
        assert_eq!(main.name, "main");
        assert_eq!(
            main.only_child.as_deref().map(|java| &*java.name),
            Some("java".as_ref())
        );
        assert_eq!(entries[2].kind, Kind::File { executable: false });

        assert_eq!(list(&root.join("missing"), OPTIONS), None);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_classified_by_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("adir")).unwrap();
        fs::write(root.join("script"), "").unwrap();
        fs::set_permissions(root.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("adir", root.join("dir-link")).unwrap();
        symlink("script", root.join("file-link")).unwrap();
        symlink("missing", root.join("broken-link")).unwrap();

        let entries = list(root, OPTIONS).unwrap();
        let kinds: Vec<_> = entries
            .iter()
            .map(|entry| (entry.name.to_string_lossy().into_owned(), entry.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("adir".to_owned(), Kind::Directory),
                ("dir-link".to_owned(), Kind::Link(LinkTarget::Directory)),
                ("file-link".to_owned(), Kind::Link(LinkTarget::File)),
                ("script".to_owned(), Kind::File { executable: true }),
                ("broken-link".to_owned(), Kind::Link(LinkTarget::Broken)),
            ]
        );
    }
}
