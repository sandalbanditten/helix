//! `helix_vcs` provides types for working with diffs from a Version Control System (VCS).
//! Currently `git` is the only supported provider for diffs, but this architecture allows
//! for other providers to be added in the future.

use anyhow::{anyhow, bail, Result};
use arc_swap::ArcSwap;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(feature = "git")]
mod git;

mod diff;

pub use diff::{DiffHandle, Hunk};

mod status;

pub use status::{Change, DirStatus, FileChange, Side, SideChange, StatusOptions};

/// Contains all active diff providers. Diff providers are compiled in via features. Currently
/// only `git` is supported.
#[derive(Clone)]
pub struct DiffProviderRegistry {
    providers: Vec<DiffProvider>,
}

impl DiffProviderRegistry {
    /// Get the given file from the VCS. This provides the unedited document as a "base"
    /// for a diff to be created.
    pub fn get_diff_base(&self, file: &Path, trust_full: bool) -> Option<Vec<u8>> {
        self.providers
            .iter()
            .find_map(|provider| match provider.get_diff_base(file, trust_full) {
                Ok(res) => Some(res),
                Err(err) => {
                    log::debug!("{err:#?}");
                    log::debug!("failed to open diff base for {}", file.display());
                    None
                }
            })
    }

    /// Get the current name of the current [HEAD](https://stackoverflow.com/questions/2304087/what-is-head-in-git).
    pub fn get_current_head_name(
        &self,
        file: &Path,
        trust_full: bool,
    ) -> Option<Arc<ArcSwap<Box<str>>>> {
        self.providers.iter().find_map(|provider| {
            match provider.get_current_head_name(file, trust_full) {
                Ok(res) => Some(res),
                Err(err) => {
                    log::debug!("{err:#?}");
                    log::debug!("failed to obtain current head name for {}", file.display());
                    None
                }
            }
        })
    }

    /// The files whose changes move HEAD of the repository holding `file`, and so the diff base
    /// and the name of HEAD.
    pub fn head_files(&self, file: &Path) -> Vec<PathBuf> {
        self.providers
            .iter()
            .find_map(|provider| match provider.head_files(file) {
                Ok(files) => Some(files),
                Err(err) => {
                    log::debug!("{err:#?}");
                    log::debug!("failed to find the HEAD files for {}", file.display());
                    None
                }
            })
            .unwrap_or_default()
    }

    /// Fire-and-forget changed file iteration. Runs everything in a background task. Keeps
    /// iteration until `on_change` returns `false`.
    pub fn for_each_changed_file(
        self,
        cwd: PathBuf,
        trust_full: bool,
        f: impl Fn(Result<FileChange>) -> bool + Send + 'static,
    ) {
        tokio::task::spawn_blocking(move || {
            if let Err(err) =
                self.for_each_status_entry(&cwd, trust_full, StatusOptions::default(), &f)
            {
                f(Err(err));
            }
        });
    }

    /// Iterates over the status of the repository containing `cwd` on the calling thread, until
    /// `f` returns `false`. Besides the changes between the index and the working tree it
    /// reports what `options` asks for.
    pub fn for_each_status_entry(
        &self,
        cwd: &Path,
        trust_full: bool,
        options: StatusOptions,
        f: impl Fn(Result<FileChange>) -> bool,
    ) -> Result<()> {
        self.providers
            .iter()
            .find_map(|provider| {
                provider
                    .for_each_status_entry(cwd, trust_full, options, &f)
                    .ok()
            })
            .ok_or_else(|| anyhow!("no diff provider returns success"))
    }

    /// The status of everything below the directory `dir` like `eza --git` sees it: each change
    /// on its side and renames as a deletion and an addition. Also tells which of the absolute
    /// `paths` the index tracks. Runs on the calling thread.
    pub fn status_by_side(
        &self,
        dir: &Path,
        trust_full: bool,
        paths: &[PathBuf],
    ) -> Result<DirStatus> {
        self.providers
            .iter()
            .find_map(|provider| provider.status_by_side(dir, trust_full, paths).ok())
            .ok_or_else(|| anyhow!("no diff provider returns success"))
    }
}

impl Default for DiffProviderRegistry {
    fn default() -> Self {
        // currently only git is supported
        // TODO make this configurable when more providers are added
        let providers = vec![
            #[cfg(feature = "git")]
            DiffProvider::Git,
            DiffProvider::None,
        ];
        DiffProviderRegistry { providers }
    }
}

/// A union type that includes all types that implement [DiffProvider]. We need this type to allow
/// cloning [DiffProviderRegistry] as `Clone` cannot be used in trait objects.
///
/// `Copy` is simply to ensure the `clone()` call is the simplest it can be.
#[derive(Copy, Clone)]
enum DiffProvider {
    #[cfg(feature = "git")]
    Git,
    None,
}

impl DiffProvider {
    fn get_diff_base(&self, file: &Path, trust_full: bool) -> Result<Vec<u8>> {
        match self {
            #[cfg(feature = "git")]
            Self::Git => git::get_diff_base(file, trust_full),
            Self::None => bail!("No diff support compiled in"),
        }
    }

    fn get_current_head_name(
        &self,
        file: &Path,
        trust_full: bool,
    ) -> Result<Arc<ArcSwap<Box<str>>>> {
        match self {
            #[cfg(feature = "git")]
            Self::Git => git::get_current_head_name(file, trust_full),
            Self::None => bail!("No diff support compiled in"),
        }
    }

    fn head_files(&self, file: &Path) -> Result<Vec<PathBuf>> {
        match self {
            #[cfg(feature = "git")]
            Self::Git => git::head_files(file),
            Self::None => bail!("No diff support compiled in"),
        }
    }

    fn for_each_status_entry(
        &self,
        cwd: &Path,
        trust_full: bool,
        options: StatusOptions,
        f: impl Fn(Result<FileChange>) -> bool,
    ) -> Result<()> {
        match self {
            #[cfg(feature = "git")]
            Self::Git => git::for_each_status_entry(cwd, trust_full, options, f),
            Self::None => bail!("No diff support compiled in"),
        }
    }

    fn status_by_side(&self, dir: &Path, trust_full: bool, paths: &[PathBuf]) -> Result<DirStatus> {
        match self {
            #[cfg(feature = "git")]
            Self::Git => git::status_by_side(dir, trust_full, paths),
            Self::None => bail!("No diff support compiled in"),
        }
    }
}
