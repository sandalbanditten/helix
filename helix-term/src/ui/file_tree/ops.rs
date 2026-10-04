//! The file operations of the tree, on absolute paths.

use std::{
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context as _, Result};
use helix_stdx::path::{canonicalize, get_relative_path};
use helix_view::{editor::Action, undo_file, Editor};

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

/// Moves the entry at `from` to `to`, or into `to` if `into_directory`, and returns where it
/// ended up.
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

/// Copies each `from` to its new path `to` in the background until one fails. `then` gets the
/// number of copies made and the failure, if any, on the main thread.
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
    let undo_dir = editor.config().undo.persisted();
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
            if let Some(dir) = &undo_dir {
                let (from, to) = (canonicalize(&from), canonicalize(&to));
                if let Err(err) = undo_file::copied(dir, &from, &to) {
                    log::error!("cannot copy the undo files of {}: {err}", from.display());
                }
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

/// A name for a new entry like `name`: `name` itself unless `taken`, else one like `stem-1.ext`.
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

/// Deletes the entry at `path` and closes the buffers below it, unless one has unsaved changes.
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

    /// Times naming a copy next to its entry and a thousand copies made before, as pasting does.
    /// Run it with `cargo test --release -p helix-term --lib measure_free_names -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_free_names() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.rs"), "").unwrap();
        for n in 1..=1000 {
            fs::write(dir.path().join(format!("file-{n}.rs")), "").unwrap();
        }
        let start = std::time::Instant::now();
        let name = free_name("file.rs".as_ref(), false, |candidate| {
            fs::symlink_metadata(dir.path().join(candidate)).is_ok()
        });
        let elapsed = start.elapsed();
        assert_eq!(name, "file-1001.rs");
        eprintln!("a free name after 1000 taken ones: {elapsed:?}");
    }
}
