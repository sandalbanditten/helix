//! The file operations of the tree, on absolute paths. They go through the editor, so language
//! servers hear about them and open buffers follow them.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context as _, Result};
use helix_stdx::path::get_relative_path;
use helix_view::{editor::Action, Editor};

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
