//! The files of a diff of many: two directories compared, or the changes of a repository since
//! HEAD. They are found in the background, and so are the lines added and removed in each.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context as _};
use helix_core::Rope;
use helix_vcs::{DiffProviderRegistry, FileChange, StatusOptions};
use helix_view::{
    diff_view::builtin::{self, Stats},
    document::{from_reader, read_to_string},
    editor::FileTreeSort,
};

use crate::ui::file_tree::order;

/// Where one side's text of a file comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text {
    File(PathBuf),
    /// The version committed at HEAD of the file at this path.
    Head(PathBuf),
    /// None: the file is new, or gone.
    Missing,
}

/// One file of the diff, by its path relative to the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    pub old: Text,
    pub new: Text,
}

impl FileDiff {
    /// The change the file tree's git column shows for it, at `root`.
    pub fn change(&self, root: &Path) -> FileChange {
        let path = root.join(&self.path);
        match (&self.old, &self.new) {
            (Text::Missing, _) => FileChange::Untracked { path },
            (_, Text::Missing) => FileChange::Deleted { path },
            _ => FileChange::Modified { path },
        }
    }
}

/// The files of a diff of many.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSet {
    /// The directory the paths are relative to, the tree's root.
    pub root: PathBuf,
    /// What the old and the new side are, shown after the files' names, if anything.
    pub sides: [Option<String>; 2],
    /// The files that differ, in the order of the tree.
    pub files: Vec<FileDiff>,
}

impl DiffSet {
    /// The files differing between the directories `old` and `new`, walked like the file picker
    /// walks: ignored files are left out, hidden ones kept, `.git` skipped.
    pub fn of_directories(old: &Path, new: &Path, sort: FileTreeSort) -> anyhow::Result<Self> {
        let (old_files, new_files) = (walk(old)?, walk(new)?);
        let mut paths: Vec<_> = old_files.keys().chain(new_files.keys()).collect();
        paths.sort();
        paths.dedup();
        let mut files = Vec::new();
        for path in paths {
            let text = |files: &BTreeMap<PathBuf, PathBuf>| {
                files.get(path).cloned().map_or(Text::Missing, Text::File)
            };
            let (old, new) = (text(&old_files), text(&new_files));
            if let (Text::File(old), Text::File(new)) = (&old, &new) {
                if same_contents(old, new) {
                    continue;
                }
            }
            files.push(FileDiff {
                path: path.clone(),
                old,
                new,
            });
        }
        sort_files(&mut files, sort);
        let name = |dir: &Path| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        };
        let sides = if name(old) == name(new) {
            [
                Some(old.display().to_string()),
                Some(new.display().to_string()),
            ]
        } else {
            [name(old), name(new)]
        };
        Ok(Self {
            root: new.to_path_buf(),
            sides,
            files,
        })
    }

    /// The files below `dir` that changed since HEAD, in the index or the working tree, new
    /// ones included.
    pub fn of_changes(
        dir: &Path,
        providers: &DiffProviderRegistry,
        trust: bool,
        sort: FileTreeSort,
    ) -> anyhow::Result<Self> {
        let changes = RefCell::new(Vec::new());
        providers.for_each_status_entry(dir, trust, StatusOptions { staged: true }, |change| {
            if let Ok(change) = change {
                changes.borrow_mut().push(change);
            }
            true
        })?;
        // A path changed in the index and in the working tree is reported twice.
        let mut paths: BTreeMap<PathBuf, (PathBuf, PathBuf, usize)> = BTreeMap::new();
        for change in changes.into_inner() {
            let (from, to) = match change {
                FileChange::Renamed { from_path, to_path } => (Some(from_path), to_path),
                FileChange::Conflict { path }
                | FileChange::Modified { path }
                | FileChange::Untracked { path }
                | FileChange::Added { path }
                | FileChange::Deleted { path } => (None, path),
            };
            let Ok(path) = to.strip_prefix(dir) else {
                continue;
            };
            let from = from.unwrap_or_else(|| to.clone());
            paths.entry(path.to_path_buf()).or_insert((from, to, 0)).2 += 1;
        }
        let froms: Vec<PathBuf> = paths.values().map(|(from, ..)| from.clone()).collect();
        // Whether each has a committed version, which only a path reported twice keeps, for
        // comparing with its file.
        let mut committed = Vec::with_capacity(froms.len());
        let mut reported = paths.values().map(|(.., reports)| *reports);
        providers.for_each_diff_base(dir, &froms, trust, |_, base| {
            let twice = reported.next().is_some_and(|reports| reports >= 2);
            committed.push(base.map(|base| twice.then_some(base)));
            true
        });
        let mut files = Vec::new();
        for ((path, (from, to, _)), committed) in paths.into_iter().zip(committed) {
            // New files have no committed version, deleted ones no file.
            let exists = to.is_file();
            // A change staged and then undone in the working tree is none.
            if let (Some(Some(committed)), true) = (&committed, exists) {
                if read(&to).ok() == decode(committed).ok() {
                    continue;
                }
            }
            files.push(FileDiff {
                path,
                old: committed.map_or(Text::Missing, |_| Text::Head(from)),
                new: if exists {
                    Text::File(to)
                } else {
                    Text::Missing
                },
            });
        }
        sort_files(&mut files, sort);
        Ok(Self {
            root: dir.to_path_buf(),
            sides: [Some("HEAD".to_owned()), None],
            files,
        })
    }

    /// The names of the panes of `file`, old and new.
    pub fn names(&self, file: &FileDiff) -> [String; 2] {
        let path = file.path.display();
        self.sides.clone().map(|side| match side {
            Some(side) => format!("{path} ({side})"),
            None => path.to_string(),
        })
    }
}

/// Reads the texts of a diff's files.
#[derive(Clone)]
pub struct Reader {
    pub providers: DiffProviderRegistry,
    /// Whether the workspace is trusted, which lets git run its filters on committed versions.
    pub trust: bool,
}

impl Reader {
    pub fn read(&self, text: &Text) -> anyhow::Result<Rope> {
        match text {
            Text::File(path) => read(path),
            Text::Head(path) => {
                let base = self
                    .providers
                    .get_diff_base(path, self.trust)
                    .with_context(|| format!("{} has no committed version", path.display()))?;
                decode(&base)
            }
            Text::Missing => Ok(Rope::new()),
        }
    }

    /// The lines added and removed in `file`, if both its texts can be read.
    pub fn stats(&self, file: &FileDiff) -> Option<Stats> {
        let (old, new) = (self.read(&file.old).ok()?, self.read(&file.new).ok()?);
        Some(builtin::stats(old.slice(..), new.slice(..)))
    }

    /// Calls `f` with the lines added and removed in each of `files` whose texts can be read,
    /// until it returns `false`. The committed versions are read in one go, from the repository
    /// holding `root`.
    pub fn each_stats(
        &self,
        root: &Path,
        files: Vec<FileDiff>,
        mut f: impl FnMut(PathBuf, Stats) -> bool,
    ) {
        let mut committed = Vec::new();
        for file in files {
            if let Text::Head(from) = &file.old {
                committed.push((from.clone(), file));
            } else if let Some(stats) = self.stats(&file) {
                if !f(file.path, stats) {
                    return;
                }
            }
        }
        let froms: Vec<PathBuf> = committed.iter().map(|(from, _)| from.clone()).collect();
        let mut files = committed.into_iter().map(|(_, file)| file);
        self.providers
            .for_each_diff_base(root, &froms, self.trust, |_, base| {
                let Some(file) = files.next() else {
                    return false;
                };
                let old = base.and_then(|base| decode(&base).ok());
                match (old, self.read(&file.new).ok()) {
                    (Some(old), Some(new)) => {
                        f(file.path, builtin::stats(old.slice(..), new.slice(..)))
                    }
                    _ => true,
                }
            });
    }
}

/// A committed version, decoded like a buffer's text.
fn decode(bytes: &[u8]) -> anyhow::Result<Rope> {
    Ok(from_reader(&mut &*bytes, None)?.0)
}

/// The text of the file `path`, decoded like a buffer's.
pub fn read(path: &Path) -> anyhow::Result<Rope> {
    let mut file =
        fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let (text, ..) = read_to_string(&mut file, None)?;
    if text.contains('\0') {
        bail!("{} is not a text file", path.display());
    }
    Ok(Rope::from(text))
}

/// The lines added and removed below each directory, by its path, from those of its files; the
/// root's path is empty.
pub fn sums<'a>(stats: impl IntoIterator<Item = (&'a Path, Stats)>) -> HashMap<PathBuf, Stats> {
    let mut sums: HashMap<PathBuf, Stats> = HashMap::new();
    for (path, stats) in stats {
        for ancestor in path.ancestors() {
            *sums.entry(ancestor.to_path_buf()).or_default() += stats;
        }
    }
    sums
}

/// The files below `dir` by their path relative to it.
fn walk(dir: &Path) -> anyhow::Result<BTreeMap<PathBuf, PathBuf>> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut files = BTreeMap::new();
    let walk = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walk.flatten() {
        // A link to a file counts as the file, like the working tree files `git difftool -d`
        // links to.
        if entry.path().is_file() {
            if let Ok(path) = entry.path().strip_prefix(dir) {
                files.insert(path.to_path_buf(), entry.path().to_path_buf());
            }
        }
    }
    Ok(files)
}

fn same_contents(a: &Path, b: &Path) -> bool {
    let size = |path: &Path| fs::metadata(path).map(|metadata| metadata.len()).ok();
    size(a) == size(b) && fs::read(a).ok() == fs::read(b).ok()
}

fn sort_files(files: &mut [FileDiff], sort: FileTreeSort) {
    files.sort_by(|a, b| order::path_cmp(sort, &a.path, false, &b.path, false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_differ_by_the_files_that_differ() {
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let write = |dir: &Path, path: &str, text: &str| {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(old.path(), "same.rs", "a\n");
        write(new.path(), "same.rs", "a\n");
        write(old.path(), "src/changed.rs", "a\n");
        write(new.path(), "src/changed.rs", "b\n");
        write(old.path(), "gone.rs", "a\n");
        write(new.path(), "src/new.rs", "a\n");
        write(new.path(), ".git/HEAD", "ref\n");
        let set = DiffSet::of_directories(old.path(), new.path(), FileTreeSort::DirectoriesFirst)
            .unwrap();
        let paths: Vec<_> = set.files.iter().map(|file| file.path.as_path()).collect();
        assert_eq!(
            paths,
            ["src/changed.rs", "src/new.rs", "gone.rs"].map(Path::new),
            "directories first"
        );
        assert_eq!(set.files[1].old, Text::Missing);
        assert_eq!(set.files[2].new, Text::Missing);
        assert_eq!(set.root, new.path());
    }

    #[test]
    fn directories_add_up_the_lines_of_their_files() {
        let stats = |added, removed| Stats { added, removed };
        let sums = sums([
            (Path::new("src/a.rs"), stats(2, 1)),
            (Path::new("src/ui/b.rs"), stats(3, 0)),
            (Path::new("c.rs"), stats(0, 4)),
        ]);
        assert_eq!(sums[Path::new("src/ui")], stats(3, 0));
        assert_eq!(sums[Path::new("src")], stats(5, 1));
        assert_eq!(sums[Path::new("")], stats(5, 5));
        assert_eq!(sums[Path::new("c.rs")], stats(0, 4));
    }

    #[test]
    fn panes_are_named_after_the_sides() {
        let set = DiffSet {
            root: PathBuf::new(),
            sides: [Some("left".into()), Some("right".into())],
            files: Vec::new(),
        };
        let file = FileDiff {
            path: "src/a.rs".into(),
            old: Text::Missing,
            new: Text::Missing,
        };
        assert_eq!(set.names(&file), ["src/a.rs (left)", "src/a.rs (right)"]);
    }

    /// Times finding the files of diffs of many and counting their lines: 2000 files changed
    /// since HEAD, and two directories of 10000 identical files. Run it with
    /// `cargo test --release -p helix-term --lib measure_diff_sets -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_diff_sets() {
        use std::time::Instant;

        let git = |repo: &Path, args: &[&str]| {
            let output = std::process::Command::new("git")
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
        };
        let text =
            |index: usize, line: &str| format!("{}{line}\n", "let a = 1;\n".repeat(index % 40));
        let write = |path: PathBuf, text: String| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };

        let repo = tempfile::tempdir().unwrap();
        let repo = helix_stdx::path::canonicalize(repo.path());
        let file = |index: usize| repo.join(format!("dir{}/file{index}.rs", index / 50));
        for index in 0..2000 {
            write(file(index), text(index, "old"));
        }
        git(&repo, &["init"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "files"]);
        for index in 0..2000 {
            write(file(index), text(index, "new"));
        }
        let providers = DiffProviderRegistry::default();
        let start = Instant::now();
        let set =
            DiffSet::of_changes(&repo, &providers, true, FileTreeSort::DirectoriesFirst).unwrap();
        let found = start.elapsed();
        let reader = Reader {
            providers,
            trust: true,
        };
        let start = Instant::now();
        let mut counted = 0;
        reader.each_stats(&set.root, set.files.clone(), |_, _| {
            counted += 1;
            true
        });
        eprintln!(
            "{} files changed since HEAD: found in {found:?}, {counted} counted in {:?}",
            set.files.len(),
            start.elapsed()
        );

        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for dir in [old.path(), new.path()] {
            for index in 0..10_000 {
                write(
                    dir.join(format!("dir{}/file{index}.rs", index / 100)),
                    text(index, "same"),
                );
            }
        }
        let start = Instant::now();
        let set = DiffSet::of_directories(old.path(), new.path(), FileTreeSort::DirectoriesFirst)
            .unwrap();
        eprintln!(
            "10000 identical files in two directories: {} differ, found in {:?}",
            set.files.len(),
            start.elapsed()
        );
    }
}
