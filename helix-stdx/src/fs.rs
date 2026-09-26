//! Filesystem operations missing from the standard library.

use std::{fs, io, path::Path};

/// Moves `from` to `to` like [`fs::rename`]. When the two paths are on different filesystems,
/// `from` is copied (contents, permissions and symlinks) and then removed instead.
///
/// The fallback refuses to overwrite: it fails with [`io::ErrorKind::AlreadyExists`] if `to`
/// exists. When copying fails, the partial copy is removed and `from` is left untouched.
pub fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => copy_and_remove(from, to),
        result => result,
    }
}

fn copy_and_remove(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    if let Err(err) = copy_recursive(from, to) {
        // Best effort: the original error is the one worth reporting.
        let _ = remove(to);
        return Err(err);
    }
    remove(from)
}

/// Copies `from` to `to`, descending into directories. Symlinks are recreated, not followed.
fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(from)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        copy_symlink(from, to)
    } else if file_type.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        // Applied last so that a read-only directory can still be filled.
        fs::set_permissions(to, metadata.permissions())
    } else {
        fs::copy(from, to).map(drop)
    }
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)
}

#[cfg(windows)]
fn copy_symlink(from: &Path, to: &Path) -> io::Result<()> {
    let target = fs::read_link(from)?;
    if fs::metadata(from).is_ok_and(|metadata| metadata.is_dir()) {
        std::os::windows::fs::symlink_dir(target, to)
    } else {
        std::os::windows::fs::symlink_file(target, to)
    }
}

fn remove(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(path: PathBuf, contents: &str) -> PathBuf {
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn move_path_renames_on_one_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let from = write(dir.path().join("a.txt"), "a");
        let to = dir.path().join("b.txt");
        move_path(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read_to_string(to).unwrap(), "a");
    }

    #[test]
    fn copy_and_remove_moves_a_tree() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        fs::create_dir_all(from.join("nested")).unwrap();
        write(from.join("nested/file.txt"), "nested");
        write(from.join("script.sh"), "#!/bin/sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = from.join("script.sh");
            fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
            std::os::unix::fs::symlink("nested/file.txt", from.join("link")).unwrap();
        }

        let to = dir.path().join("to");
        copy_and_remove(&from, &to).unwrap();

        assert!(!from.exists());
        assert_eq!(
            fs::read_to_string(to.join("nested/file.txt")).unwrap(),
            "nested"
        );
        assert_eq!(
            fs::read_to_string(to.join("script.sh")).unwrap(),
            "#!/bin/sh"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(to.join("script.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
            assert_eq!(
                fs::read_link(to.join("link")).unwrap(),
                Path::new("nested/file.txt")
            );
        }
    }

    #[test]
    fn copy_and_remove_refuses_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let from = write(dir.path().join("from.txt"), "from");
        let to = write(dir.path().join("to.txt"), "to");
        let err = copy_and_remove(&from, &to).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(from).unwrap(), "from");
        assert_eq!(fs::read_to_string(to).unwrap(), "to");
    }

    #[cfg(unix)]
    #[test]
    fn failed_copy_leaves_the_source_untouched() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        fs::create_dir(&from).unwrap();
        write(from.join("readable.txt"), "readable");
        let unreadable = write(from.join("unreadable.txt"), "secret");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&unreadable).is_ok() {
            // Running with CAP_DAC_OVERRIDE (e.g. as root): nothing can fail to copy.
            return;
        }

        let to = dir.path().join("to");
        assert!(copy_and_remove(&from, &to).is_err());
        assert!(!to.exists());
        assert_eq!(
            fs::read_to_string(from.join("readable.txt")).unwrap(),
            "readable"
        );
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
    }
}
