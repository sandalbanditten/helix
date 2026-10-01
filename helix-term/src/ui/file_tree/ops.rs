//! The file operations of the tree, on absolute paths. They go through the editor, so language
//! servers hear about them and open buffers follow them.

use std::{
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context as _, Result};
use helix_stdx::path::get_relative_path;
use helix_view::{editor::Action, Editor};

use crate::job::{Callback, Job};

/// Opens the file at `path` in a buffer.
pub fn open(editor: &mut Editor, path: &Path, action: Action) -> Result<()> {
    editor.open(path, action).map(drop).map_err(|err| {
        anyhow!(
            "Unable to open {}: {err}",
            get_relative_path(path).display()
        )
    })
}

/// Moves the entry at `from` to `to`, creating missing directories on the way, and returns where
/// it ended up. With `into_directory`, moving onto a directory moves into it, like `:move`.
pub fn rename(
    editor: &mut Editor,
    from: &Path,
    to: PathBuf,
    into_directory: bool,
) -> Result<PathBuf> {
    let to = if into_directory && to.is_dir() {
        to.join(
            from.file_name()
                .context("The workspace root cannot be moved")?,
        )
    } else {
        to
    };
    if to == from {
        return Ok(to);
    }
    refuse_existing(&to)?;
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    editor.move_path(from, &to)?;
    Ok(to)
}

/// Creates a file, or a `directory`, at `path`, creating missing directories on the way.
pub fn create(editor: &mut Editor, path: &Path, directory: bool) -> Result<()> {
    refuse_existing(path)?;
    editor.create_path(path, directory)?;
    Ok(())
}

/// Copies each `from` to its new path `to` like `cp -rp` off the main thread, one after another
/// and creating missing directories on the way, until one fails; what that one had copied is
/// removed again. Language servers hear about each copy before and after it. `then` gets the
/// number of copies made and the failure of the next one, if any, on the main thread.
///
/// Commands that close buffers or quit wait for the copies, and for `then`.
pub fn copy_in_background(
    editor: &mut Editor,
    copies: Vec<(PathBuf, PathBuf)>,
    then: impl FnOnce(&mut Editor, usize, io::Result<()>) + Send + 'static,
) -> Job {
    let copies: Vec<_> = copies
        .into_iter()
        .map(|(from, to)| {
            // A link to a directory is copied as a link.
            let is_dir = fs::symlink_metadata(&from).is_ok_and(|metadata| metadata.is_dir());
            editor.will_create_path(&to, is_dir);
            (from, to, is_dir)
        })
        .collect();
    let copy = move || {
        let mut copied = Vec::new();
        for (from, to, is_dir) in copies {
            let made = to
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| helix_stdx::fs::copy_path(&from, &to));
            if let Err(err) = made {
                return (copied, Err(err));
            }
            copied.push((to, is_dir));
        }
        (copied, Ok(()))
    };
    Job::with_callback(async move {
        let (copied, result) = tokio::task::spawn_blocking(copy).await?;
        // Waiting for jobs runs only the callbacks that need no compositor.
        let call = move |editor: &mut Editor| {
            for (path, is_dir) in &copied {
                editor.did_create_path(path, *is_dir);
            }
            then(editor, copied.len(), result);
        };
        Ok(Callback::Editor(Box::new(call)))
    })
    .wait_before_exiting()
}

/// A name for a new entry like `name`: `name` itself unless `taken`, else the first free one of
/// `stem-1.ext`, `stem-2.ext` and so on. A directory's name has no extension.
pub fn free_name(name: &OsStr, is_dir: bool, taken: impl Fn(&OsStr) -> bool) -> OsString {
    if !taken(name) {
        return name.to_owned();
    }
    let path = Path::new(name);
    let (stem, extension) = if is_dir {
        (name, None)
    } else {
        (path.file_stem().unwrap_or(name), path.extension())
    };
    (1..)
        .map(|n| {
            let mut candidate = stem.to_owned();
            candidate.push(format!("-{n}"));
            if let Some(extension) = extension {
                candidate.push(".");
                candidate.push(extension);
            }
            candidate
        })
        .find(|candidate| !taken(candidate))
        .expect("some name is free")
}

/// Deletes the entry at `path` for good and closes the buffers below it. Refuses while one of
/// them has unsaved changes.
pub fn delete(editor: &mut Editor, path: &Path) -> Result<()> {
    let below: Vec<_> = editor
        .documents()
        .filter(|doc| {
            doc.path()
                .is_some_and(|doc_path| doc_path.starts_with(path))
        })
        .collect();
    if let Some(doc) = below.iter().find(|doc| doc.is_modified()) {
        bail!("{} has unsaved changes", doc.display_name());
    }
    let below: Vec<_> = below.iter().map(|doc| doc.id()).collect();
    // Removing a directory removes everything in it; a link to one is removed itself.
    editor.delete_path(path, true)?;
    for doc in below {
        // Unmodified, so closing cannot fail.
        let _ = editor.close_document(doc, false);
    }
    Ok(())
}

fn refuse_existing(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        bail!("{} already exists", get_relative_path(path).display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free(name: &str, is_dir: bool, taken: &[&str]) -> OsString {
        free_name(name.as_ref(), is_dir, |candidate| {
            taken.iter().any(|taken| OsStr::new(taken) == candidate)
        })
    }

    #[test]
    fn free_names_number_the_stem() {
        assert_eq!(free("file.rs", false, &[]), "file.rs");
        assert_eq!(free("file.rs", false, &["file.rs"]), "file-1.rs");
        assert_eq!(
            free("file.rs", false, &["file.rs", "file-1.rs"]),
            "file-2.rs"
        );
        assert_eq!(free("file-1.rs", false, &["file-1.rs"]), "file-1-1.rs");
        assert_eq!(free(".gitignore", false, &[".gitignore"]), ".gitignore-1");
        assert_eq!(
            free("archive.tar.gz", false, &["archive.tar.gz"]),
            "archive.tar-1.gz"
        );
        assert_eq!(free("Makefile", false, &["Makefile"]), "Makefile-1");
        assert_eq!(free("my.dir", true, &["my.dir"]), "my.dir-1");
    }
}
