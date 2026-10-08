//! The files of a diff of many: two directories compared, or the changes of a repository since
//! HEAD.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    path::{Component, Path, PathBuf},
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

/// One file of the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// The path of the file relative to the root: its new one, or its old one if it is gone.
    pub path: PathBuf,
    /// The path the file had on the old side, relative to the root, if it was renamed.
    pub renamed_from: Option<PathBuf>,
    pub old: Text,
    pub new: Text,
}

impl FileDiff {
    /// The directory the tree lists the file in, and its name there. A renamed file is listed
    /// below the directory its paths share, named like `git diff --stat` shows the rest, as in
    /// `{ => ui}/a.rs`.
    pub fn entry(&self) -> (PathBuf, OsString) {
        match &self.renamed_from {
            Some(from) => rename_entry(from, &self.path),
            None => (
                self.path.parent().unwrap_or(Path::new("")).to_path_buf(),
                self.path.file_name().unwrap_or_default().to_os_string(),
            ),
        }
    }

    /// The path of the file's row in the tree, relative to the root.
    pub fn key(&self) -> PathBuf {
        let (dir, name) = self.entry();
        dir.join(name)
    }

    /// The change the file tree's git column shows for it, at `root`.
    pub fn change(&self, root: &Path) -> FileChange {
        let path = root.join(self.key());
        match (&self.old, &self.new) {
            (Text::Missing, _) => FileChange::Untracked { path },
            (_, Text::Missing) => FileChange::Deleted { path },
            _ => FileChange::Modified { path },
        }
    }
}

/// Where the tree lists a file renamed from `from` to `to`: the directory both paths share, and
/// the rest as `git diff --stat` shows it. `src/a.rs` renamed to `src/ui/a.rs` is
/// `{ => ui}/a.rs` in `src`.
fn rename_entry(from: &Path, to: &Path) -> (PathBuf, OsString) {
    const SEPARATOR: char = std::path::MAIN_SEPARATOR;
    let (a, b) = (from.to_string_lossy(), to.to_string_lossy());
    // Like git's `pprint_rename`, the common prefix ends with a separator and the common suffix
    // starts with one, which may be the prefix's last.
    let prefix = a
        .char_indices()
        .zip(b.chars())
        .take_while(|((_, x), y)| x == y)
        .filter(|((_, x), _)| *x == SEPARATOR)
        .last()
        .map_or(0, |((index, _), _)| index + 1);
    let start = prefix.saturating_sub(1);
    let common: usize = a[start..]
        .chars()
        .rev()
        .zip(b[start..].chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    let suffix = a[a.len() - common..]
        .find(SEPARATOR)
        .map_or(0, |separator| common - separator);
    let middle = |path: &str| {
        let end = path.len() - suffix;
        path[prefix.min(end)..end].to_owned()
    };
    let rest = if prefix + suffix == 0 {
        format!("{a} => {b}")
    } else {
        format!(
            "{{{} => {}}}{}",
            middle(&a),
            middle(&b),
            &a[a.len() - suffix..]
        )
    };
    (PathBuf::from(&a[..start]), rest.into())
}

/// The files of a diff of many.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSet {
    /// The directory the paths are relative to, the tree's root.
    pub root: PathBuf,
    /// What the diff is called, the name of the tree's root.
    pub name: String,
    /// What the old and the new side are, shown after the files' names, if anything.
    pub sides: [Option<String>; 2],
    /// The files that differ, in the order of the tree.
    pub files: Vec<FileDiff>,
}

impl DiffSet {
    /// The files differing between the directories `old` and `new`, versions of the directory
    /// `versions_of` if given.
    pub fn of_directories(
        old: &Path,
        new: &Path,
        versions_of: Option<&Path>,
        providers: &DiffProviderRegistry,
        sort: FileTreeSort,
    ) -> anyhow::Result<Self> {
        let (old_files, new_files) = (walk(old)?, walk(new)?);
        let only = |files: &BTreeMap<PathBuf, PathBuf>, other: &BTreeMap<PathBuf, PathBuf>| {
            let only = files.keys().filter(|path| !other.contains_key(*path));
            only.cloned().collect::<Vec<_>>()
        };
        let (deleted, added) = (only(&old_files, &new_files), only(&new_files, &old_files));
        // By their new path.
        let renames: HashMap<PathBuf, PathBuf> = providers
            .renames(old, new, &deleted, &added)
            .into_iter()
            .map(|(from, to)| (to, from))
            .collect();
        let renamed: HashSet<&PathBuf> = renames.values().collect();
        let mut paths: Vec<_> = old_files.keys().chain(new_files.keys()).collect();
        paths.sort();
        paths.dedup();
        let mut files = Vec::new();
        for path in paths {
            // Listed with the file it was renamed to.
            if renamed.contains(path) {
                continue;
            }
            let renamed_from = renames.get(path).cloned();
            let old_path = renamed_from.as_ref().unwrap_or(path);
            let old = old_files
                .get(old_path)
                .cloned()
                .map_or(Text::Missing, Text::File);
            let new = new_files
                .get(path)
                .cloned()
                .map_or(Text::Missing, Text::File);
            if let (Text::File(old), Text::File(new), None) = (&old, &new, &renamed_from) {
                if same_contents(old, new) {
                    continue;
                }
            }
            files.push(FileDiff {
                path: path.clone(),
                renamed_from,
                old,
                new,
            });
        }
        sort_files(&mut files, sort);
        let (name, sides) = match versions_of {
            Some(dir) => (
                dir_name(dir),
                ["old", "new"].map(|side| Some(side.to_owned())),
            ),
            None if dir_name(old) == dir_name(new) => (
                dir_name(new),
                [old, new].map(|dir| Some(dir.display().to_string())),
            ),
            None => (
                format!("{} → {}", dir_name(old), dir_name(new)),
                [old, new].map(|dir| Some(dir_name(dir))),
            ),
        };
        Ok(Self {
            root: new.to_path_buf(),
            name,
            sides,
            files,
        })
    }

    /// The files below `dir` that changed since HEAD.
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
        let paths = changed_files(dir, changes.into_inner());
        let froms: Vec<PathBuf> = paths.values().map(|file| file.from.clone()).collect();
        // Whether each has a committed version, which only a path reported twice keeps, for
        // comparing with its file.
        let mut committed = Vec::with_capacity(froms.len());
        let mut reported = paths.values().map(|file| file.reports);
        providers.for_each_diff_base(dir, &froms, trust, |_, base| {
            let twice = reported.next().is_some_and(|reports| reports >= 2);
            committed.push(base.map(|base| twice.then_some(base)));
            true
        });
        let mut files = Vec::new();
        for ((path, ChangedFile { from, to, .. }), committed) in paths.into_iter().zip(committed) {
            // New files have no committed version, deleted ones no file.
            let exists = to.is_file();
            // A change staged and then undone in the working tree is none, unless it moved.
            if let (Some(Some(committed)), true, true) = (&committed, exists, from == to) {
                if read(&to).ok() == decode(committed).ok() {
                    continue;
                }
            }
            let renamed_from = from
                .strip_prefix(dir)
                .ok()
                .filter(|_| from != to)
                .map(Path::to_path_buf);
            files.push(FileDiff {
                path,
                renamed_from,
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
            name: dir_name(dir),
            sides: [Some("HEAD".to_owned()), None],
            files,
        })
    }

    /// The names of the panes of `file`, old and new.
    pub fn names(&self, file: &FileDiff) -> [String; 2] {
        let old = file.renamed_from.as_ref().unwrap_or(&file.path);
        let mut sides = self.sides.clone().into_iter();
        [old, &file.path].map(|path| match sides.next().flatten() {
            Some(side) => format!("{} ({side})", path.display()),
            None => path.display().to_string(),
        })
    }
}

/// A file changed since HEAD.
#[derive(Debug, PartialEq, Eq)]
struct ChangedFile {
    /// Its path at HEAD.
    from: PathBuf,
    /// Its path now.
    to: PathBuf,
    /// How many times git reported it: once for its staged change, once for its unstaged one.
    reports: usize,
}

/// The files below `dir` that `changes` report, by their path now relative to it.
fn changed_files(dir: &Path, changes: Vec<FileChange>) -> BTreeMap<PathBuf, ChangedFile> {
    // Where each file renamed was renamed from, by where it was renamed to.
    let renames: HashMap<PathBuf, PathBuf> = changes
        .iter()
        .filter_map(|change| match change {
            FileChange::Renamed { from_path, to_path } => {
                Some((to_path.clone(), from_path.clone()))
            }
            _ => None,
        })
        .collect();
    let sources: HashSet<&Path> = renames.values().map(PathBuf::as_path).collect();
    let mut files: BTreeMap<PathBuf, ChangedFile> = BTreeMap::new();
    for change in &changes {
        let to = change.path();
        let from = match change {
            FileChange::Renamed { from_path, .. } => Some(from_path),
            _ => None,
        };
        // Renamed in the index and again in the working tree: gone from in between.
        if sources.contains(to) {
            continue;
        }
        let Ok(path) = to.strip_prefix(dir) else {
            continue;
        };
        let file = files
            .entry(path.to_path_buf())
            .or_insert_with(|| ChangedFile {
                from: to.to_path_buf(),
                to: to.to_path_buf(),
                reports: 0,
            });
        file.reports += 1;
        // The index and the working tree come in any order; a rename tells where it was at HEAD.
        if let Some(from) = from {
            file.from = match renames.get(from) {
                Some(earlier) if earlier != to => earlier.clone(),
                _ => from.clone(),
            };
        }
    }
    files
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

    /// Calls `f` with the path of the row and the lines added and removed of each of `files` whose
    /// texts can be read, until it returns `false`.
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
                if !f(file.key(), stats) {
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
                        f(file.key(), builtin::stats(old.slice(..), new.slice(..)))
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

/// The lines added and removed below each directory, by its path.
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

/// The name of `dir`, or all of its path if it has none, like `/`.
fn dir_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn same_contents(a: &Path, b: &Path) -> bool {
    let size = |path: &Path| fs::metadata(path).map(|metadata| metadata.len()).ok();
    size(a) == size(b) && fs::read(a).ok() == fs::read(b).ok()
}

/// Puts `files` in the order the tree lists them.
fn sort_files(files: &mut Vec<FileDiff>, sort: FileTreeSort) {
    /// The names leading to an entry: its directory's, then its own.
    fn names<'a>(dir: &'a Path, name: &'a OsStr) -> impl Iterator<Item = &'a OsStr> {
        let dirs = dir.components().map(Component::as_os_str);
        dirs.chain(std::iter::once(name))
    }
    let mut entries: Vec<_> = std::mem::take(files)
        .into_iter()
        .map(|file| (file.entry(), file))
        .collect();
    entries.sort_by(|((a_dir, a_name), _), ((b_dir, b_name), _)| {
        order::names_cmp(
            sort,
            names(a_dir, a_name),
            false,
            names(b_dir, b_name),
            false,
        )
    });
    files.extend(entries.into_iter().map(|(_, file)| file));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn providers() -> DiffProviderRegistry {
        DiffProviderRegistry::default()
    }

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
        write(old.path(), "gone.rs", "g\n");
        write(new.path(), "src/new.rs", "n\n");
        write(new.path(), ".git/HEAD", "ref\n");
        let sort = FileTreeSort::DirectoriesFirst;
        let set =
            DiffSet::of_directories(old.path(), new.path(), None, &providers(), sort).unwrap();
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
    fn renames_read_like_git_stat() {
        // As `git diff --stat` shows them.
        let shown = |from: &str, to: &str| {
            let (dir, name) = rename_entry(from.as_ref(), to.as_ref());
            dir.join(name).to_string_lossy().into_owned()
        };
        assert_eq!(
            shown(
                "src/main/java/foo/Poisson.java",
                "src/main/java/foo/bar/Poisson.java"
            ),
            "src/main/java/foo/{ => bar}/Poisson.java"
        );
        assert_eq!(
            shown(
                "src/main/java/foo/deep/Up.java",
                "src/main/java/foo/Up.java"
            ),
            "src/main/java/foo/{deep => }/Up.java"
        );
        assert_eq!(
            shown(
                "src/main/java/foo/Fish.java",
                "src/main/java/foo/Trout.java"
            ),
            "src/main/java/foo/{Fish.java => Trout.java}"
        );
        assert_eq!(
            shown("lib/a/b/x.rs", "lib/c/y.rs"),
            "lib/{a/b/x.rs => c/y.rs}"
        );
        assert_eq!(shown("a/x.rs", "b/x.rs"), "{a => b}/x.rs");
        assert_eq!(shown("README.md", "docs.md"), "README.md => docs.md");
        assert_eq!(
            shown("a/xä", "b/xĤ"),
            "a/xä => b/xĤ",
            "characters sharing bytes"
        );
        assert_eq!(
            rename_entry("src/a.rs".as_ref(), "src/ui/a.rs".as_ref()),
            ("src".into(), "{ => ui}/a.rs".into()),
            "listed below the directory both paths share"
        );
    }

    #[test]
    fn renamed_files_are_listed_once() {
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let lines = |last: &str| format!("{}{last}\n", "line\n".repeat(9));
        for (dir, path, text) in [
            (old.path(), "src/a.rs", lines("old")),
            (new.path(), "src/ui/a.rs", lines("new")),
            (old.path(), "src/b.rs", "b\n".into()),
            (new.path(), "src/b.rs", "B\n".into()),
        ] {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let sort = FileTreeSort::DirectoriesFirst;
        let set =
            DiffSet::of_directories(old.path(), new.path(), None, &providers(), sort).unwrap();
        let keys: Vec<_> = set.files.iter().map(FileDiff::key).collect();
        assert_eq!(
            keys,
            ["src/b.rs", "src/{ => ui}/a.rs"].map(PathBuf::from),
            "a file among files, by its name"
        );
        let renamed = &set.files[1];
        assert_eq!(renamed.path, Path::new("src/ui/a.rs"));
        assert_eq!(renamed.old, Text::File(old.path().join("src/a.rs")));
        let [old_name, new_name] = set.names(renamed);
        assert!(old_name.starts_with("src/a.rs ("), "{old_name}");
        assert!(new_name.starts_with("src/ui/a.rs ("), "{new_name}");
    }

    #[test]
    fn renames_tell_where_files_were_at_head() {
        let path = |path: &str| PathBuf::from("/repo").join(path);
        let renamed = |from: &str, to: &str| FileChange::Renamed {
            from_path: path(from),
            to_path: path(to),
        };
        let changed = |changes| -> Vec<(PathBuf, PathBuf, usize)> {
            changed_files(Path::new("/repo"), changes)
                .into_values()
                .map(|file| (file.from, file.to, file.reports))
                .collect()
        };
        // Staged as a rename and edited since, reported in either order.
        let modified = || FileChange::Modified { path: path("b.rs") };
        let expected = [(path("a.rs"), path("b.rs"), 2)];
        assert_eq!(changed(vec![renamed("a.rs", "b.rs"), modified()]), expected);
        assert_eq!(changed(vec![modified(), renamed("a.rs", "b.rs")]), expected);
        // Renamed in the index, then again in the working tree.
        assert_eq!(
            changed(vec![renamed("a.rs", "b.rs"), renamed("b.rs", "c.rs")]),
            [(path("a.rs"), path("c.rs"), 1)]
        );
    }

    #[test]
    fn directory_diffs_are_named_after_what_they_compare() {
        let parent = tempfile::tempdir().unwrap();
        let dir = |path: &str| {
            let dir = parent.path().join(path);
            fs::create_dir_all(&dir).unwrap();
            dir
        };
        let (old, new) = (dir("v1.0"), dir("v1.1"));
        let sort = FileTreeSort::DirectoriesFirst;
        let set = DiffSet::of_directories(&old, &new, None, &providers(), sort).unwrap();
        assert_eq!(set.name, "v1.0 → v1.1");
        assert_eq!(set.sides, [Some("v1.0".into()), Some("v1.1".into())]);

        let (old, new) = (dir("a/src"), dir("b/src"));
        let set = DiffSet::of_directories(&old, &new, None, &providers(), sort).unwrap();
        assert_eq!(set.name, "src");
        assert_eq!(
            set.sides,
            [old, new].map(|dir| Some(dir.display().to_string())),
            "the same names tell nothing apart"
        );

        // As `git difftool -d` hands over versions of the work tree.
        let (old, new) = (dir("git-difftool.X/left"), dir("git-difftool.X/right"));
        let set = DiffSet::of_directories(
            &old,
            &new,
            Some(Path::new("/src/hotstone")),
            &providers(),
            sort,
        )
        .unwrap();
        assert_eq!(set.name, "hotstone");
        assert_eq!(set.sides, [Some("old".into()), Some("new".into())]);
        assert_eq!(set.root, new, "the files are the new directory's");
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
            name: String::new(),
            sides: [Some("left".into()), Some("right".into())],
            files: Vec::new(),
        };
        let file = FileDiff {
            path: "src/a.rs".into(),
            renamed_from: None,
            old: Text::Missing,
            new: Text::Missing,
        };
        assert_eq!(set.names(&file), ["src/a.rs (left)", "src/a.rs (right)"]);
    }

    /// Times finding the files of diffs of many and counting their lines. Run it with
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
        let set = DiffSet::of_directories(
            old.path(),
            new.path(),
            None,
            &reader.providers,
            FileTreeSort::DirectoriesFirst,
        )
        .unwrap();
        eprintln!(
            "10000 identical files in two directories: {} differ, found in {:?}",
            set.files.len(),
            start.elapsed()
        );
    }
}
