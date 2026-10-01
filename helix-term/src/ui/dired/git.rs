//! The git column of dired listings: what an edit of it asks for, staging, unstaging and
//! discarding through the `git` command line, and editing the ignore files.
//!
//! The column is edited one letter at a time: making the staged letter `-` unstages, making the
//! unstaged letter `-` discards the change (or un-ignores), `-X` to `X-` stages, and `-N` to `-I`
//! ignores. The next listing shows what git makes of it.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{bail, Context as _, Result};
use helix_view::dired::GitStatus;
use ignore::{
    gitignore::{gitconfig_excludes_path, Gitignore, GitignoreBuilder},
    Match,
};

/// What `git` is run for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// `MM` to `-M`: the index takes the version of `HEAD` again.
    Unstage,
    /// `MM` to `--`: both the index and the working tree take the version of `HEAD`.
    DiscardAll,
    /// `-M` to `--`: the working tree takes the version of the index.
    Discard,
    /// `-M` to `M-`.
    Stage,
}

impl Action {
    /// Whether the action throws away changes, which only `:w!` does.
    pub fn discards(self) -> bool {
        matches!(self, Self::Discard | Self::DiscardAll)
    }
}

/// What an edit of the git column asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Git(Action),
    Ignore,
    Unignore,
}

/// The edit that turns the git column `old` into `new`, if any.
pub fn edit(old: GitStatus, new: (char, char)) -> Result<Option<Edit>, &'static str> {
    let (index, worktree) = (old.index, old.worktree);
    let (new_index, new_worktree) = new;
    if (index, worktree) == new {
        return Ok(None);
    }
    if index == 'U' || worktree == 'U' {
        return Err("Conflicts are resolved with git");
    }
    if index == '-' && new_index != '-' {
        return match worktree {
            '-' | 'I' => Err("There are no changes to stage"),
            _ if new_index == worktree && new_worktree == '-' => Ok(Some(Edit::Git(Action::Stage))),
            _ => Err("Staging turns `-M` into `M-`"),
        };
    }
    if index != '-' && new_index == '-' {
        return match new_worktree {
            _ if new_worktree == worktree || (worktree == '-' && new_worktree == index) => {
                Ok(Some(Edit::Git(Action::Unstage)))
            }
            '-' => Ok(Some(Edit::Git(Action::DiscardAll))),
            _ => Err("Unstaging turns `M-` into `-M`"),
        };
    }
    if new_index != index {
        return Err("Only `-` unstages a staged change");
    }
    match (worktree, new_worktree) {
        ('M' | 'T' | 'D', '-') => Ok(Some(Edit::Git(Action::Discard))),
        ('I', '-') => Ok(Some(Edit::Unignore)),
        ('N', 'I') => Ok(Some(Edit::Ignore)),
        ('N', '-') => Err("Untracked: stage it with `N-` or ignore it with `-I`"),
        (_, 'I') => Err("Only untracked entries can be ignored"),
        _ => Err("Only `-` undoes a change"),
    }
}

/// Runs `git` for `action` on `paths` in the repository with the working tree `repo`.
pub fn run(repo: &Path, action: Action, paths: &[PathBuf]) -> Result<()> {
    let args: &[&str] = match action {
        // Without a commit there is no `HEAD` to restore from.
        Action::Unstage if !has_head(repo) => &["rm", "--cached", "-r", "--quiet"],
        Action::Unstage => &["restore", "--staged"],
        Action::DiscardAll => &["restore", "--staged", "--worktree"],
        Action::Discard => &["restore", "--worktree"],
        Action::Stage => &["add", "--all"],
    };
    let output = git(repo)
        .args(args)
        .arg("--")
        .args(paths)
        .output()
        .context("Cannot run git")?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        bail!("git {}: {}", args[0], error.trim());
    }
    Ok(())
}

fn has_head(repo: &Path) -> bool {
    git(repo)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// `git` working on the repository `repo` rather than whatever the environment points to.
fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    command
}

/// A change of one ignore file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreEdit {
    pub file: PathBuf,
    pub change: IgnoreChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreChange {
    /// Removing the last line that reads so.
    Remove(String),
    Append(String),
}

/// How to ignore the entry at `path`: an anchored pattern in the nearest `.gitignore` from its
/// directory up to the root of `repo`, or the root's own.
pub fn ignore(repo: &Path, path: &Path, is_dir: bool) -> IgnoreEdit {
    let dir = path.parent().unwrap_or(repo);
    let file = dir
        .ancestors()
        .take_while(|dir| dir.starts_with(repo))
        .map(|dir| dir.join(".gitignore"))
        .find(|file| file.is_file())
        .unwrap_or_else(|| repo.join(".gitignore"));
    let line = anchored(file.parent().unwrap_or(repo), path, is_dir);
    IgnoreEdit {
        file,
        change: IgnoreChange::Append(line),
    }
}

/// How to stop ignoring the entry at `path`: removing the pattern that ignores it when that names
/// just this entry, else adding a negation after it. A pattern in the global excludes file is
/// negated in `.git/info/exclude`, so that other repositories are not affected.
pub fn unignore(repo: &Path, path: &Path, is_dir: bool) -> Result<IgnoreEdit, String> {
    let shown = |path: &Path| {
        super::format::quote(&helix_stdx::path::get_relative_path(path).to_string_lossy())
    };
    // Git does not look into an ignored directory, so what is in it cannot be un-ignored.
    for ancestor in path.ancestors().skip(1).take_while(|dir| *dir != repo) {
        if matching(repo, ancestor, true).is_some() {
            return Err(format!(
                "Ignored as {} is, un-ignore that first",
                shown(ancestor)
            ));
        }
    }
    let Some((source, pattern, global)) = matching(repo, path, is_dir) else {
        return Err("No ignore file ignores it".to_owned());
    };
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let literal = pattern
        .trim_start_matches('/')
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|last| last == name && !pattern.contains(['*', '?', '[', '\\']));
    if literal && !global {
        return Ok(IgnoreEdit {
            file: source,
            change: IgnoreChange::Remove(pattern),
        });
    }
    let file = match global {
        true => repo.join(".git/info/exclude"),
        false => source,
    };
    let base = match global {
        true => repo,
        false => file.parent().unwrap_or(repo),
    };
    let line = format!("!{}", anchored(base, path, is_dir));
    Ok(IgnoreEdit {
        file,
        change: IgnoreChange::Append(line),
    })
}

/// The pattern that decides that `path` is ignored, where it is from, and whether that is the
/// global excludes file: the deepest `.gitignore` with a say first, then `.git/info/exclude`,
/// then the global excludes file. `None` if no pattern ignores it.
fn matching(repo: &Path, path: &Path, is_dir: bool) -> Option<(PathBuf, String, bool)> {
    let decide = |matcher: &Gitignore, global| match matcher.matched(path, is_dir) {
        Match::Ignore(glob) => Some(Some((
            glob.from().map(Path::to_path_buf).unwrap_or_default(),
            glob.original().to_owned(),
            global,
        ))),
        Match::Whitelist(_) => Some(None),
        Match::None => None,
    };
    let dir = path.parent()?;
    for dir in dir.ancestors().take_while(|dir| dir.starts_with(repo)) {
        let file = dir.join(".gitignore");
        if file.is_file() {
            if let Some(decided) = decide(&Gitignore::new(&file).0, false) {
                return decided;
            }
        }
    }
    let mut excludes = vec![(repo.join(".git/info/exclude"), false)];
    excludes.extend(gitconfig_excludes_path().map(|file| (file, true)));
    for (file, global) in excludes {
        let mut builder = GitignoreBuilder::new(repo);
        if builder.add(&file).is_some() {
            continue;
        }
        let Ok(matcher) = builder.build() else {
            continue;
        };
        if let Some(decided) = decide(&matcher, global) {
            return decided;
        }
    }
    None
}

/// The pattern matching just `path` in an ignore file of the directory `base`, like `/build.log`
/// or `/target/`.
fn anchored(base: &Path, path: &Path, is_dir: bool) -> String {
    let relative = path.strip_prefix(base).unwrap_or(path);
    let mut pattern = String::from("/");
    for (i, component) in relative.iter().enumerate() {
        if i > 0 {
            pattern.push('/');
        }
        for c in component.to_string_lossy().chars() {
            if matches!(c, '*' | '?' | '[' | '\\' | '!' | '#') {
                pattern.push('\\');
            }
            pattern.push(c);
        }
    }
    if is_dir {
        pattern.push('/');
    }
    pattern
}

/// Makes the change of `edit` to its file, creating it if needed.
pub fn apply(edit: &IgnoreEdit) -> io::Result<()> {
    let text = match fs::read_to_string(&edit.file) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let text = match &edit.change {
        IgnoreChange::Append(line) => {
            let separator = if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{text}{separator}{line}\n")
        }
        IgnoreChange::Remove(line) => {
            let mut lines: Vec<&str> = text.lines().collect();
            if let Some(index) = lines
                .iter()
                .rposition(|candidate| candidate.trim_end() == line)
            {
                lines.remove(index);
            }
            let mut text = lines.join("\n");
            if !text.is_empty() {
                text.push('\n');
            }
            text
        }
    };
    if let Some(parent) = edit.file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&edit.file, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(index: char, worktree: char) -> GitStatus {
        GitStatus { index, worktree }
    }

    #[test]
    fn edits_of_the_git_column_go_letter_by_letter() {
        let edit = |old: &str, new: &str| {
            let old: Vec<char> = old.chars().collect();
            let new: Vec<char> = new.chars().collect();
            edit(status(old[0], old[1]), (new[0], new[1]))
        };
        let git = |action| Ok(Some(Edit::Git(action)));
        assert_eq!(edit("-M", "M-"), git(Action::Stage));
        assert_eq!(edit("M-", "--"), git(Action::Unstage));
        assert_eq!(edit("MM", "-M"), git(Action::Unstage));
        assert_eq!(edit("MM", "M-"), git(Action::Discard));
        assert_eq!(edit("MM", "--"), git(Action::DiscardAll));
        assert_eq!(edit("-M", "--"), git(Action::Discard));
        assert_eq!(edit("-N", "N-"), git(Action::Stage));
        assert_eq!(edit("N-", "-N"), git(Action::Unstage));
        assert_eq!(edit("-N", "-I"), Ok(Some(Edit::Ignore)));
        assert_eq!(edit("-I", "--"), Ok(Some(Edit::Unignore)));
        assert_eq!(edit("--", "--"), Ok(None));
        assert!(edit("-N", "--").is_err());
        assert!(edit("--", "-I").is_err());
        assert!(edit("-M", "MM").is_err());
        assert!(edit("M-", "N-").is_err());
        assert!(edit("UU", "--").is_err());
    }

    fn exec(repo: &Path, args: &[&str]) {
        let output = git(repo)
            .args([
                "-c",
                "user.name=helix",
                "-c",
                "user.email=helix@helix",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    fn porcelain(repo: &Path) -> String {
        let output = git(repo)
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn git_stages_unstages_and_discards() {
        let dir = tempfile::tempdir().unwrap();
        let repo = &helix_stdx::path::canonicalize(dir.path());
        exec(repo, &["init", "-q"]);
        // Unstaging works before the first commit too.
        fs::write(repo.join("a.txt"), "one").unwrap();
        run(repo, Action::Stage, &[repo.join("a.txt")]).unwrap();
        assert_eq!(porcelain(repo), "A  a.txt\n");
        run(repo, Action::Unstage, &[repo.join("a.txt")]).unwrap();
        assert_eq!(porcelain(repo), "?? a.txt\n");

        run(repo, Action::Stage, &[repo.join("a.txt")]).unwrap();
        exec(repo, &["commit", "-qm", "init"]);
        fs::write(repo.join("a.txt"), "two").unwrap();
        run(repo, Action::Stage, &[repo.join("a.txt")]).unwrap();
        fs::write(repo.join("a.txt"), "three").unwrap();
        assert_eq!(porcelain(repo), "MM a.txt\n");
        run(repo, Action::Discard, &[repo.join("a.txt")]).unwrap();
        assert_eq!(porcelain(repo), "M  a.txt\n");
        run(repo, Action::DiscardAll, &[repo.join("a.txt")]).unwrap();
        assert_eq!(porcelain(repo), "");
        assert_eq!(fs::read_to_string(repo.join("a.txt")).unwrap(), "one");
    }

    #[test]
    fn ignore_files_are_edited_where_the_pattern_is() {
        let dir = tempfile::tempdir().unwrap();
        let repo = &helix_stdx::path::canonicalize(dir.path());
        fs::create_dir_all(repo.join(".git/info")).unwrap();
        fs::create_dir_all(repo.join("src/target")).unwrap();
        fs::write(repo.join(".gitignore"), "*.log\ntarget/\n").unwrap();
        fs::write(repo.join("src/.gitignore"), "tmp\n").unwrap();

        // A literal pattern goes, a glob is negated.
        let edit = unignore(repo, &repo.join("src/target"), true).unwrap();
        assert_eq!(edit.file, repo.join(".gitignore"));
        assert_eq!(edit.change, IgnoreChange::Remove("target/".into()));
        apply(&edit).unwrap();
        assert_eq!(
            fs::read_to_string(repo.join(".gitignore")).unwrap(),
            "*.log\n"
        );
        let edit = unignore(repo, &repo.join("src/build.log"), false).unwrap();
        assert_eq!(edit.change, IgnoreChange::Append("!/src/build.log".into()));
        apply(&edit).unwrap();
        assert_eq!(
            fs::read_to_string(repo.join(".gitignore")).unwrap(),
            "*.log\n!/src/build.log\n"
        );

        // The deeper file decides, and new patterns go to the nearest one.
        let edit = unignore(repo, &repo.join("src/tmp"), false).unwrap();
        assert_eq!(edit.file, repo.join("src/.gitignore"));
        let edit = ignore(repo, &repo.join("src/new file*.txt"), false);
        assert_eq!(edit.file, repo.join("src/.gitignore"));
        assert_eq!(edit.change, IgnoreChange::Append("/new file\\*.txt".into()));
        let edit = ignore(repo, &repo.join("docs/guide.md"), false);
        assert_eq!(edit.file, repo.join(".gitignore"));
        assert_eq!(edit.change, IgnoreChange::Append("/docs/guide.md".into()));

        // What lies in an ignored directory follows it.
        fs::write(repo.join(".gitignore"), "build/\n").unwrap();
        assert!(unignore(repo, &repo.join("build/out.o"), false).is_err());
    }
}
