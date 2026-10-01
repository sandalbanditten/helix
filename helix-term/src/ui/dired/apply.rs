//! Carrying out the plan of a dired write.
//!
//! Every path of a plan is as it was listed. Moves are done one at a time, each once its target
//! is free, and the paths of what is still to do follow every directory that moved; entries that
//! swap places go through a temporary name.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context as _, Result};
use helix_view::Editor;

use super::{
    git,
    plan::{Change, Metadata, Move, Plan},
};
use crate::ui::file_tree::ops;

/// What a write got done.
#[derive(Debug, Default)]
pub struct Applied {
    /// The steps of the plan that were done.
    pub done: usize,
    /// Every path that moved, in order, including through temporary names.
    pub moved: Vec<(PathBuf, PathBuf)>,
}

impl Applied {
    /// Where the listed `path` is now.
    pub fn path(&self, path: &Path) -> PathBuf {
        self.moved
            .iter()
            .fold(path.to_path_buf(), |path, (from, to)| {
                remapped(&path, from, to, true).unwrap_or(path)
            })
    }
}

/// Carries out `plan`, stopping at the first failure, which is returned with what was done.
pub fn apply(editor: &mut Editor, plan: &Plan) -> (Applied, Result<()>) {
    let mut applied = Applied::default();
    let result = moves(editor, plan.moves.clone(), &mut applied)
        .and_then(|()| deletions(editor, &plan.deletions, &mut applied))
        .and_then(|()| changes(&plan.changes, &mut applied))
        .and_then(|()| git(plan, &mut applied));
    (applied, result)
}

/// Runs git once for each kind of git edit, then edits the ignore files.
fn git(plan: &Plan, applied: &mut Applied) -> Result<()> {
    if let Some(repo) = &plan.repo {
        let mut actions: BTreeMap<git::Action, Vec<PathBuf>> = BTreeMap::new();
        for (action, path) in &plan.git {
            actions.entry(*action).or_default().push(applied.path(path));
        }
        for (action, paths) in actions {
            git::run(repo, action, &paths)?;
            applied.done += paths.len();
        }
    }
    for edit in &plan.ignores {
        git::apply(edit).with_context(|| format!("Cannot edit {}", shown(&edit.file)))?;
        applied.done += 1;
    }
    Ok(())
}

fn moves(editor: &mut Editor, mut pending: Vec<Move>, applied: &mut Applied) -> Result<()> {
    while !pending.is_empty() {
        // A move waits while its target is taken, or while another move makes its directory.
        // The paths are normalized, so comparing their bytes is enough, and fast.
        let ready = pending.iter().position(|step| {
            fs::symlink_metadata(&step.to).is_err()
                && !step.to.parent().is_some_and(|parent| {
                    pending
                        .iter()
                        .any(|other| other.to.as_os_str() == parent.as_os_str())
                })
        });
        let Some(ready) = ready else {
            // Each target is taken by an entry still to move: entries swap places.
            let blocked = pending
                .iter()
                .position(|step| {
                    pending
                        .iter()
                        .any(|other| other.from.as_os_str() == step.to.as_os_str())
                })
                .ok_or_else(|| anyhow!("{} already exists", shown(&pending[0].to)))?;
            let from = pending[blocked].from.clone();
            let temporary = temporary(&from);
            editor
                .move_path(&from, &temporary)
                .with_context(|| format!("Cannot move {}", shown(&from)))?;
            remap(&mut pending, &from, &temporary);
            applied.moved.push((from, temporary));
            continue;
        };
        let step = pending.remove(ready);
        if let Some(parent) = step.to.parent().filter(|parent| !parent.exists()) {
            fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create {}", shown(parent)))?;
        }
        editor
            .move_path(&step.from, &step.to)
            .with_context(|| format!("Cannot move {} to {}", shown(&step.from), shown(&step.to)))?;
        remap(&mut pending, &step.from, &step.to);
        applied.moved.push((step.from, step.to));
        applied.done += 1;
    }
    Ok(())
}

/// Makes the moves still to do follow `from` to `to`: their entries and, for a directory that
/// moved, their targets inside it.
fn remap(pending: &mut [Move], from: &Path, to: &Path) {
    for step in pending {
        if let Some(path) = remapped(&step.from, from, to, true) {
            step.from = path;
        }
        if let Some(path) = remapped(&step.to, from, to, false) {
            step.to = path;
        }
    }
}

/// Where `path` is once `from` moved to `to`, if it moved with it: when it is `from` itself (with
/// `itself`) or lies inside it.
fn remapped(path: &Path, from: &Path, to: &Path, itself: bool) -> Option<PathBuf> {
    // Most paths share no prefix with `from`, which comparing bytes tells fast: this runs for
    // every move still to do after each move.
    let (bytes, prefix) = (
        path.as_os_str().as_encoded_bytes(),
        from.as_os_str().as_encoded_bytes(),
    );
    if !bytes.starts_with(prefix) {
        return None;
    }
    let rest = path.strip_prefix(from).ok()?;
    match rest.as_os_str().is_empty() {
        // `join("")` would append a separator.
        true => itself.then(|| to.to_path_buf()),
        false => Some(to.join(rest)),
    }
}

/// A free name next to `path`.
fn temporary(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    (0..)
        .map(|i| path.with_file_name(format!(".{name}.dired-{i}")))
        .find(|candidate| fs::symlink_metadata(candidate).is_err())
        .expect("some name is free")
}

fn deletions(editor: &mut Editor, deletions: &[PathBuf], applied: &mut Applied) -> Result<()> {
    for path in deletions {
        let path = applied.path(path);
        ops::delete(editor, &path)?;
        applied.done += 1;
    }
    Ok(())
}

fn changes(changes: &[Change], applied: &mut Applied) -> Result<()> {
    // A new link comes first, then the owner, which can clear setuid bits, then the mode; the
    // time last as the others may change it.
    let order = |change: &Change| match change.metadata {
        Metadata::Link(_) => 0,
        Metadata::Owner { .. } => 1,
        Metadata::Mode(_) => 2,
        Metadata::Modified(_) => 3,
    };
    let mut changes: Vec<_> = changes.iter().collect();
    changes.sort_by_key(|change| order(change));
    for change in changes {
        let path = applied.path(&change.path);
        set(&path, &change.metadata).with_context(|| format!("Cannot change {}", shown(&path)))?;
        applied.done += 1;
    }
    Ok(())
}

#[cfg(unix)]
fn set(path: &Path, metadata: &Metadata) -> std::io::Result<()> {
    use std::os::unix::fs::{lchown, symlink, PermissionsExt};

    match metadata {
        Metadata::Link(target) => {
            // A link cannot be changed, so a new one takes its place.
            let temporary = temporary(path);
            symlink(target, &temporary)?;
            fs::rename(&temporary, path).inspect_err(|_| {
                let _ = fs::remove_file(&temporary);
            })
        }
        Metadata::Owner { uid, gid } => lchown(path, *uid, *gid),
        Metadata::Mode(mode) => fs::set_permissions(path, fs::Permissions::from_mode(*mode)),
        Metadata::Modified(time) => helix_stdx::fs::set_modified(path, *time),
    }
}

#[cfg(not(unix))]
fn set(_path: &Path, _metadata: &Metadata) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// A path for a message, relative to the working directory when it is below it.
fn shown(path: &Path) -> String {
    super::format::quote(&helix_stdx::path::get_relative_path(path).to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(from: &str, to: &str) -> Move {
        Move {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn paths_follow_the_directories_that_moved() {
        let mut pending = vec![
            step("/r/src/a", "/r/src/b"),
            step("/r/x", "/r/src/x"),
            step("/r/src", "/r/y"),
        ];
        remap(&mut pending, Path::new("/r/src"), Path::new("/r/lib"));
        assert_eq!(
            pending,
            [
                step("/r/lib/a", "/r/lib/b"),
                step("/r/x", "/r/lib/x"),
                step("/r/lib", "/r/y"),
            ]
        );
        // A target that is the moved path itself is where something else goes now.
        let mut pending = vec![step("/r/a", "/r/b")];
        remap(&mut pending, Path::new("/r/b"), Path::new("/r/c"));
        assert_eq!(pending, [step("/r/a", "/r/b")]);
    }
}
