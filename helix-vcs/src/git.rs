use anyhow::{bail, Context, Result};
use arc_swap::ArcSwap;
use gix::filter::plumbing::driver::apply::Delay;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gix::bstr::ByteSlice;
use gix::diff::Rewrites;
use gix::dir::entry::Status;
use gix::objs::tree::EntryKind;
use gix::sec::trust::DefaultForLevel;
use gix::status::{
    index_worktree::Item,
    plumbing::index_as_worktree::{Change as Change_, EntryStatus},
    UntrackedFiles,
};
use gix::{Commit, ObjectId, Repository, ThreadSafeRepository};

use crate::{Change, DirStatus, FileChange, Side, SideChange, StatusOptions};

#[cfg(test)]
mod test;

/// `file` with its symlinks resolved.
fn realpath(file: &Path) -> Result<PathBuf> {
    if file.exists() {
        return gix::path::realpath(file).context("resolve symlinks");
    }
    let (dir, name) = (
        get_repo_dir(file)?,
        file.file_name().context("no file name")?,
    );
    Ok(gix::path::realpath(dir)
        .context("resolve symlinks")?
        .join(name))
}

#[inline]
fn get_repo_dir(file: &Path) -> Result<&Path> {
    file.parent().context("file has no parent directory")
}

pub fn get_diff_base(file: &Path, trust_full: bool) -> Result<Vec<u8>> {
    debug_assert!(!file.exists() || file.is_file());
    debug_assert!(file.is_absolute());
    let file = realpath(file)?;

    // TODO cache repository lookup

    let repo_dir = get_repo_dir(&file)?;
    let repo = open_repo(repo_dir, trust_full)
        .context("failed to open git repo")?
        .to_thread_local();
    committed_version(&repo, &file)
}

/// Calls `f` with the diff base of each of `files` in the repository holding `dir`, until it
/// returns `false`.
pub fn for_each_diff_base(
    dir: &Path,
    files: &[PathBuf],
    trust_full: bool,
    mut f: impl FnMut(&Path, Result<Vec<u8>>) -> bool,
) -> Result<()> {
    let repo = open_repo(dir, trust_full)
        .context("failed to open git repo")?
        .to_thread_local();
    for file in files {
        debug_assert!(file.is_absolute());
        let base = realpath(file).and_then(|real| committed_version(&repo, &real));
        if !f(file, base) {
            break;
        }
    }
    Ok(())
}

/// The version of `file`, a path in the working tree of `repo` without symlinks, committed at
/// HEAD.
fn committed_version(repo: &Repository, file: &Path) -> Result<Vec<u8>> {
    let head = repo.head_commit()?;
    let file_oid = find_file_in_commit(repo, &head, file)?;

    let file_object = repo.find_object(file_oid)?;
    let data = file_object.detach().data;
    // Get the actual data that git would make out of the git object.
    // This will apply the user's git config or attributes like crlf conversions.
    //
    // The whole filter pipeline still runs in untrusted (`Trust::Reduced`) mode so built-in
    // conversions like autocrlf keep working, but gix drops `filter.*.clean` / `filter.*.smudge`
    // drivers defined in untrusted (repository-local) config, so those external programs are not
    // executed unless the workspace was explicitly trusted. This relies on `open_repo` forcing the
    // trust level instead of letting gix re-derive it from `.git` ownership; see the note there.
    if let Some(work_dir) = repo.workdir() {
        let rela_path = file.strip_prefix(work_dir)?;
        let rela_path = gix::path::try_into_bstr(rela_path)?;
        let (mut pipeline, _) = repo.filter_pipeline(None)?;
        let mut worktree_outcome =
            pipeline.convert_to_worktree(&data, rela_path.as_ref(), Delay::Forbid)?;
        let mut buf = Vec::with_capacity(data.len());
        worktree_outcome.read_to_end(&mut buf)?;
        Ok(buf)
    } else {
        Ok(data)
    }
}

pub fn get_current_head_name(file: &Path, trust_full: bool) -> Result<Arc<ArcSwap<Box<str>>>> {
    debug_assert!(!file.exists() || file.is_file());
    debug_assert!(file.is_absolute());
    let file = gix::path::realpath(file).context("resolve symlinks")?;

    let repo_dir = get_repo_dir(&file)?;
    let repo = open_repo(repo_dir, trust_full)
        .context("failed to open git repo")?
        .to_thread_local();
    let head_ref = repo.head_ref()?;
    let head_commit = repo.head_commit()?;

    let name = match head_ref {
        Some(reference) => reference.name().shorten().to_string(),
        None => head_commit.id.to_hex_with_len(8).to_string(),
    };

    Ok(Arc::new(ArcSwap::from_pointee(name.into_boxed_str())))
}

/// The files whose changes move HEAD of the repository holding `file`.
pub fn head_files(file: &Path) -> Result<Vec<PathBuf>> {
    debug_assert!(file.is_absolute());
    let file = gix::path::realpath(file).context("resolve symlinks")?;

    let repo_dir = get_repo_dir(&file)?;
    // Only paths are read, so the repository's own configuration needs no trust.
    let repo = open_repo(repo_dir, false)
        .context("failed to open git repo")?
        .to_thread_local();
    // A worktree's common directory comes as `.git/worktrees/<name>/../..`.
    let git_dir = gix::path::realpath(repo.git_dir())?;
    let common_dir = gix::path::realpath(repo.common_dir())?;
    let mut files = vec![git_dir.join("HEAD"), common_dir.join("packed-refs")];
    if let Some(name) = repo.head_name()? {
        files.push(common_dir.join(gix::path::from_bstr(name.as_bstr())));
    }
    Ok(files)
}

pub fn for_each_status_entry(
    cwd: &Path,
    trust_full: bool,
    options: StatusOptions,
    f: impl Fn(Result<FileChange>) -> bool,
) -> Result<()> {
    status(&open_repo(cwd, trust_full)?.to_thread_local(), options, f)
}

fn open_repo(path: &Path, trust_full: bool) -> Result<ThreadSafeRepository> {
    // `trust_full` is the workspace-trust decision made by the caller, and it must be the
    // authority on the gix trust level. gix's own discovery (`discover_*`) ignores a
    // caller-supplied trust level: it always re-derives trust from `.git` ownership, so a malicious
    // `.git/config` in a user-owned directory would be opened as `Trust::Full` regardless of our
    // gate. Worse, the GIT_DIR-environment branch of that discovery panics because it never sets a
    // trust level at all. So we split discovery from opening: find the repository path ourselves,
    // then `open_opts(..).with(trust)`, which forces the trust level and skips gix's ownership
    // check. Under `Trust::Reduced`, gix then refuses to honor untrusted repository-local config
    // such as `filter.*` smudge/clean drivers.

    let trust = if trust_full {
        gix::sec::Trust::Full
    } else {
        gix::sec::Trust::Reduced
    };

    // On Windows various configuration options are bundled as part of the git installation. The
    // lookup is expensive; only do it there.
    let config = gix::open::permissions::Config {
        system: true,
        git: true,
        user: true,
        env: true,
        includes: true,
        git_binary: cfg!(windows),
    };

    let permissions = gix::open::Permissions {
        config,
        ..gix::open::Permissions::default_for_level(trust)
    };

    let discover_options = gix::discover::upwards::Options {
        dot_git_only: true,
        ..Default::default()
    };
    let (repo_path, _trust_from_ownership) = gix::discover::upwards_opts(path, discover_options)
        .context("failed to discover git repo")?;
    let (git_dir, _work_dir) = repo_path.into_repository_and_work_tree_directories();

    let options = gix::open::Options::default()
        .permissions(permissions)
        // `git_dir` is the discovered `.git` directory (or a linked-worktree git dir), so open it
        // as-is rather than letting gix append `.git` again.
        .open_path_as_is(true)
        .with(trust);

    Ok(ThreadSafeRepository::open_opts(git_dir, options)?)
}

/// Emulates the result of running `git status` from the command line.
fn status(
    repo: &Repository,
    options: StatusOptions,
    f: impl Fn(Result<FileChange>) -> bool,
) -> Result<()> {
    let work_dir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("working tree not found"))?
        .to_path_buf();

    let status_platform = repo
        .status(gix::progress::Discard)?
        // Here we discard the `status.showUntrackedFiles` config, as it makes little sense in
        // our case to not list new (untracked) files. We could have respected this config
        // if the default value weren't `Collapsed` though, as this default value would render
        // the feature unusable to many.
        .untracked_files(UntrackedFiles::Files)
        // Turn on file rename detection, which is off by default.
        .index_worktree_rewrites(Some(Rewrites {
            copies: None,
            percentage: Some(0.5),
            limit: 1000,
            ..Default::default()
        }));
    // No filtering based on path
    let empty_patterns = vec![];

    let report = |change: Option<FileChange>| change.is_none_or(|change| f(Ok(change)));
    if options.staged {
        for item in status_platform.into_iter(empty_patterns)? {
            let Ok(item) = item.map_err(|err| f(Err(err.into()))) else {
                continue;
            };
            let change = match item {
                gix::status::Item::IndexWorktree(item) => index_worktree_change(&work_dir, item)?,
                gix::status::Item::TreeIndex(change) => Some(tree_index_change(&work_dir, change)?),
            };
            if !report(change) {
                break;
            }
        }
    } else {
        for item in status_platform.into_index_worktree_iter(empty_patterns)? {
            let Ok(item) = item.map_err(|err| f(Err(err.into()))) else {
                continue;
            };
            if !report(index_worktree_change(&work_dir, item)?) {
                break;
            }
        }
    }

    Ok(())
}

/// Maps a change between the index and the working tree.
fn index_worktree_change(work_dir: &Path, item: Item) -> Result<Option<FileChange>> {
    let change = match item {
        Item::Modification {
            rela_path, status, ..
        } => {
            let path = work_dir.join(rela_path.to_path()?);
            match status {
                EntryStatus::Conflict { .. } => FileChange::Conflict { path },
                EntryStatus::Change(Change_::Removed) => FileChange::Deleted { path },
                EntryStatus::Change(Change_::Modification { .. }) => FileChange::Modified { path },
                // Files marked with `git add --intent-to-add`. Such files
                // still show up as new in `git status`, so it's appropriate
                // to show them the same way as untracked files in the
                // "changed file" picker. One example of this being used
                // is Jujutsu, a Git-compatible VCS. It marks all new files
                // with `--intent-to-add` automatically.
                EntryStatus::IntentToAdd => FileChange::Untracked { path },
                _ => return Ok(None),
            }
        }
        Item::DirectoryContents { entry, .. } => {
            let path = work_dir.join(entry.rela_path.to_path()?);
            match entry.status {
                Status::Untracked => FileChange::Untracked { path },
                _ => return Ok(None),
            }
        }
        Item::Rewrite {
            source,
            dirwalk_entry,
            ..
        } => FileChange::Renamed {
            from_path: work_dir.join(source.rela_path().to_path()?),
            to_path: work_dir.join(dirwalk_entry.rela_path.to_path()?),
        },
    };
    Ok(Some(change))
}

/// Maps a change staged in the index, i.e. between `HEAD` and the index.
fn tree_index_change(work_dir: &Path, change: gix::diff::index::Change) -> Result<FileChange> {
    use gix::diff::index::ChangeRef;

    let change = match change {
        ChangeRef::Addition { location, .. } => FileChange::Added {
            path: work_dir.join(location.to_path()?),
        },
        ChangeRef::Deletion { location, .. } => FileChange::Deleted {
            path: work_dir.join(location.to_path()?),
        },
        ChangeRef::Modification { location, .. } => FileChange::Modified {
            path: work_dir.join(location.to_path()?),
        },
        ChangeRef::Rewrite {
            source_location,
            location,
            ..
        } => FileChange::Renamed {
            from_path: work_dir.join(source_location.to_path()?),
            to_path: work_dir.join(location.to_path()?),
        },
    };
    Ok(change)
}

/// The status below `dir` with each change on its side and no rename detection, and whether the
/// index tracks each of `paths`.
pub fn status_by_side(dir: &Path, trust_full: bool, paths: &[PathBuf]) -> Result<DirStatus> {
    let repo = open_repo(dir, trust_full)?.to_thread_local();
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("working tree not found"))?
        .to_path_buf();
    let relative = |path: &Path| -> Result<gix::bstr::BString> {
        let path = path
            .strip_prefix(&workdir)
            .context("path outside of the working tree")?;
        Ok(gix::path::to_unix_separators_on_windows(gix::path::try_into_bstr(path)?).into_owned())
    };

    // `top` makes the pathspec relative to the working tree rather than the process' directory.
    let dir = relative(dir)?;
    let patterns: Vec<gix::bstr::BString> = if dir.is_empty() {
        Vec::new()
    } else {
        let mut pattern = gix::bstr::BString::from(":(top,literal)");
        pattern.extend_from_slice(&dir);
        vec![pattern]
    };
    let platform = repo
        .status(gix::progress::Discard)?
        .untracked_files(UntrackedFiles::Files)
        .index_worktree_rewrites(None)
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled);
    let mut changes = Vec::new();
    for item in platform.into_iter(patterns)? {
        let (path, side, change) = match item? {
            gix::status::Item::IndexWorktree(item) => {
                let Some((path, change)) = worktree_side_change(item) else {
                    continue;
                };
                (path, Side::Worktree, change)
            }
            gix::status::Item::TreeIndex(change) => {
                let (path, change) = index_side_change(&change);
                (path, Side::Index, change)
            }
        };
        changes.push(SideChange {
            path: workdir.join(path.to_path()?),
            side,
            change,
        });
    }

    let index = repo.index_or_empty()?;
    let tracked = paths
        .iter()
        .map(|path| {
            let path = relative(path)?;
            if path.is_empty() {
                return Ok(!index.entries().is_empty());
            }
            if index.entry_by_path(path.as_ref()).is_some() {
                return Ok(true);
            }
            let mut prefix = path;
            prefix.push(b'/');
            Ok(index.prefixed_entries(prefix.as_ref()).is_some())
        })
        .collect::<Result<_>>()?;
    Ok(DirStatus {
        workdir,
        changes,
        tracked,
    })
}

/// A change between the index and the working tree, at its path relative to the working tree.
fn worktree_side_change(item: Item) -> Option<(gix::bstr::BString, Change)> {
    match item {
        Item::Modification {
            rela_path, status, ..
        } => {
            let change = match status {
                EntryStatus::Conflict { .. } => Change::Conflict,
                EntryStatus::Change(Change_::Removed) => Change::Deleted,
                EntryStatus::Change(Change_::Type { .. }) => Change::TypeChange,
                EntryStatus::Change(Change_::Modification { .. })
                | EntryStatus::Change(Change_::SubmoduleModification(_)) => Change::Modified,
                EntryStatus::IntentToAdd => Change::New,
                EntryStatus::NeedsUpdate(_) => return None,
            };
            Some((rela_path, change))
        }
        Item::DirectoryContents { entry, .. } => {
            (entry.status == Status::Untracked).then_some((entry.rela_path, Change::New))
        }
        // Not reported without rename tracking; the destination is what the listing shows.
        Item::Rewrite { dirwalk_entry, .. } => Some((dirwalk_entry.rela_path, Change::New)),
    }
}

/// A change staged in the index, at its path relative to the working tree.
fn index_side_change(change: &gix::diff::index::Change) -> (gix::bstr::BString, Change) {
    use gix::diff::index::ChangeRef;
    use gix::objs::tree::EntryKind;

    let kind = |mode: gix::index::entry::Mode| {
        mode.to_tree_entry_mode().map(|mode| match mode.kind() {
            EntryKind::BlobExecutable => EntryKind::Blob,
            kind => kind,
        })
    };
    match change {
        ChangeRef::Addition { location, .. } | ChangeRef::Rewrite { location, .. } => {
            (location.clone().into_owned(), Change::New)
        }
        ChangeRef::Deletion { location, .. } => (location.clone().into_owned(), Change::Deleted),
        ChangeRef::Modification {
            location,
            previous_entry_mode,
            entry_mode,
            ..
        } => {
            let change = if kind(*previous_entry_mode) == kind(*entry_mode) {
                Change::Modified
            } else {
                Change::TypeChange
            };
            (location.clone().into_owned(), change)
        }
    }
}

/// Finds the object that contains the contents of a file at a specific commit.
fn find_file_in_commit(repo: &Repository, commit: &Commit, file: &Path) -> Result<ObjectId> {
    let repo_dir = repo.workdir().context("repo has no worktree")?;
    let rel_path = file.strip_prefix(repo_dir)?;
    let tree = commit.tree()?;
    let tree_entry = tree
        .lookup_entry_by_path(rel_path)?
        .context("file is untracked")?;
    match tree_entry.mode().kind() {
        // not a file, everything is new, do not show diff
        mode @ (EntryKind::Tree | EntryKind::Commit | EntryKind::Link) => {
            bail!("entry at {} is not a file but a {mode:?}", file.display())
        }
        // found a file
        EntryKind::Blob | EntryKind::BlobExecutable => Ok(tree_entry.object_id()),
    }
}
