use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::TempDir;

use crate::git;

fn exec_git_cmd(args: &str, git_dir: &Path) {
    let res = Command::new("git")
        .arg("-C")
        .arg(git_dir) // execute the git command in this directory
        .args(args.split_whitespace())
        .env_remove("GIT_DIR")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env("GIT_TERMINAL_PROMPT", "false")
        .env("GIT_AUTHOR_DATE", "2000-01-01 00:00:00 +0000")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_NAME", "author")
        .env("GIT_COMMITTER_DATE", "2000-01-02 00:00:00 +0000")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_NAME", "committer")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_1", "main")
        .output()
        .unwrap_or_else(|_| panic!("`git {args}` failed"));
    if !res.status.success() {
        println!("{}", String::from_utf8_lossy(&res.stdout));
        eprintln!("{}", String::from_utf8_lossy(&res.stderr));
        panic!("`git {args}` failed (see output above)")
    }
}

fn create_commit(repo: &Path, add_modified: bool) {
    if add_modified {
        exec_git_cmd("add -A", repo);
    }
    exec_git_cmd("commit -m message", repo);
}

fn empty_git_repo() -> TempDir {
    let tmp = tempfile::tempdir().expect("create temp dir for git testing");
    exec_git_cmd("init", tmp.path());
    exec_git_cmd("config user.email test@helix.org", tmp.path());
    exec_git_cmd("config user.name helix-test", tmp.path());
    tmp
}

#[test]
fn missing_file() {
    let temp_git = empty_git_repo();
    let file = temp_git.path().join("file.txt");
    File::create(&file).unwrap().write_all(b"foo").unwrap();

    assert!(git::get_diff_base(&file, true).is_err());
}

#[test]
fn unmodified_file() {
    let temp_git = empty_git_repo();
    let file = temp_git.path().join("file.txt");
    let contents = b"foo".as_slice();
    File::create(&file).unwrap().write_all(contents).unwrap();
    create_commit(temp_git.path(), true);
    assert_eq!(
        git::get_diff_base(&file, true).unwrap(),
        Vec::from(contents)
    );
}

#[test]
fn modified_file() {
    let temp_git = empty_git_repo();
    let file = temp_git.path().join("file.txt");
    let contents = b"foo".as_slice();
    File::create(&file).unwrap().write_all(contents).unwrap();
    create_commit(temp_git.path(), true);
    File::create(&file).unwrap().write_all(b"bar").unwrap();

    assert_eq!(
        git::get_diff_base(&file, true).unwrap(),
        Vec::from(contents)
    );
}

#[test]
fn deleted_file() {
    let temp_git = empty_git_repo();
    let file = temp_git.path().join("file.txt");
    let contents = b"foo".as_slice();
    File::create(&file).unwrap().write_all(contents).unwrap();
    create_commit(temp_git.path(), true);
    std::fs::remove_file(&file).unwrap();

    assert_eq!(
        git::get_diff_base(&file, true).unwrap(),
        Vec::from(contents)
    );
}

#[test]
fn diff_bases_of_many_files() {
    let temp_git = empty_git_repo();
    let (committed, deleted, new) = (
        temp_git.path().join("committed.txt"),
        temp_git.path().join("sub/deleted.txt"),
        temp_git.path().join("new.txt"),
    );
    std::fs::create_dir(temp_git.path().join("sub")).unwrap();
    std::fs::write(&committed, "a").unwrap();
    std::fs::write(&deleted, "b").unwrap();
    create_commit(temp_git.path(), true);
    std::fs::remove_file(&deleted).unwrap();
    std::fs::write(&new, "c").unwrap();

    let files = [committed, deleted, new];
    let mut bases = Vec::new();
    git::for_each_diff_base(temp_git.path(), &files, true, |file, base| {
        bases.push((file.to_path_buf(), base.ok()));
        true
    })
    .unwrap();
    assert_eq!(
        bases,
        [
            (files[0].clone(), Some(b"a".to_vec())),
            (files[1].clone(), Some(b"b".to_vec())),
            (files[2].clone(), None),
        ]
    );

    let mut called = 0;
    git::for_each_diff_base(temp_git.path(), &files, true, |_, _| {
        called += 1;
        false
    })
    .unwrap();
    assert_eq!(called, 1, "stops when asked to");
}

/// Test that `get_file_head` does not return content for a directory.
/// This is important to correctly cover cases where a directory is removed and replaced by a file.
/// If the contents of the directory object were returned a diff between a path and the directory children would be produced.
#[test]
fn directory() {
    let temp_git = empty_git_repo();
    let dir = temp_git.path().join("file.txt");
    std::fs::create_dir(&dir).expect("");
    let file = dir.join("file.txt");
    let contents = b"foo".as_slice();
    File::create(file).unwrap().write_all(contents).unwrap();

    create_commit(temp_git.path(), true);

    std::fs::remove_dir_all(&dir).unwrap();
    File::create(&dir).unwrap().write_all(b"bar").unwrap();
    assert!(git::get_diff_base(&dir, true).is_err());
}

/// Test that `get_diff_base` resolves symlinks so that the same diff base is
/// used as the target file.
///
/// This is important to correctly cover cases where a symlink is removed and
/// replaced by a file. If the contents of the symlink object were returned
/// a diff between a literal file path and the actual file content would be
/// produced (bad ui).
#[cfg(any(unix, windows))]
#[test]
fn symlink() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(not(unix))]
    use std::os::windows::fs::symlink_file as symlink;

    let temp_git = empty_git_repo();
    let file = temp_git.path().join("file.txt");
    let contents = Vec::from(b"foo");
    File::create(&file).unwrap().write_all(&contents).unwrap();
    let file_link = temp_git.path().join("file_link.txt");

    symlink("file.txt", &file_link).unwrap();
    create_commit(temp_git.path(), true);

    assert_eq!(git::get_diff_base(&file_link, true).unwrap(), contents);
    assert_eq!(git::get_diff_base(&file, true).unwrap(), contents);
}

/// Test that `get_diff_base` returns content when the file is a symlink to
/// another file that is in a git repo, but the symlink itself is not.
#[cfg(any(unix, windows))]
#[test]
fn symlink_to_git_repo() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(not(unix))]
    use std::os::windows::fs::symlink_file as symlink;

    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let temp_git = empty_git_repo();

    let file = temp_git.path().join("file.txt");
    let contents = Vec::from(b"foo");
    File::create(&file).unwrap().write_all(&contents).unwrap();
    create_commit(temp_git.path(), true);

    let file_link = temp_dir.path().join("file_link.txt");
    symlink(&file, &file_link).unwrap();

    assert_eq!(git::get_diff_base(&file_link, true).unwrap(), contents);
    assert_eq!(git::get_diff_base(&file, true).unwrap(), contents);
}

/// Collects the status of `repo` as sorted `(kind, workspace-relative path)` pairs.
fn status_entries(repo: &Path, options: crate::StatusOptions) -> Vec<(&'static str, String)> {
    use crate::FileChange;
    use std::sync::Mutex;

    let entries = Mutex::new(Vec::new());
    git::for_each_status_entry(repo, true, options, |change| {
        let change = change.unwrap();
        let kind = match &change {
            FileChange::Untracked { .. } => "untracked",
            FileChange::Added { .. } => "added",
            FileChange::Modified { .. } => "modified",
            FileChange::Conflict { .. } => "conflict",
            FileChange::Deleted { .. } => "deleted",
            FileChange::Renamed { .. } => "renamed",
        };
        let path = change.path().strip_prefix(repo).unwrap();
        entries
            .lock()
            .unwrap()
            .push((kind, path.to_string_lossy().into_owned()));
        true
    })
    .unwrap();
    let mut entries = entries.into_inner().unwrap();
    entries.sort();
    entries
}

#[test]
fn status_reports_staged_entries_on_request() {
    let temp_git = empty_git_repo();
    let repo = temp_git.path();
    std::fs::write(repo.join(".gitignore"), "target/\n*.log\n").unwrap();
    std::fs::write(repo.join("committed.txt"), "one").unwrap();
    create_commit(repo, true);

    std::fs::write(repo.join("staged.txt"), "new").unwrap();
    exec_git_cmd("add staged.txt", repo);
    std::fs::write(repo.join("untracked.txt"), "new").unwrap();
    std::fs::write(repo.join("committed.txt"), "two").unwrap();
    std::fs::create_dir_all(repo.join("target/debug")).unwrap();
    std::fs::write(repo.join("target/debug/binary"), "").unwrap();
    std::fs::write(repo.join("build.log"), "").unwrap();

    let entry = |kind, path: &str| (kind, path.to_string());
    assert_eq!(
        status_entries(repo, crate::StatusOptions::default()),
        [
            entry("modified", "committed.txt"),
            entry("untracked", "untracked.txt"),
        ]
    );
    assert_eq!(
        status_entries(repo, crate::StatusOptions { staged: true }),
        [
            entry("added", "staged.txt"),
            entry("modified", "committed.txt"),
            entry("untracked", "untracked.txt"),
        ]
    );
}

/// The changes of `status_by_side` as sorted `(side, change, relative path)` triples.
fn side_changes(status: &crate::DirStatus) -> Vec<(char, char, String)> {
    use crate::{Change, Side};

    let mut changes: Vec<_> = status
        .changes
        .iter()
        .map(|change| {
            let side = match change.side {
                Side::Index => 'i',
                Side::Worktree => 'w',
            };
            let kind = match change.change {
                Change::New => 'N',
                Change::Modified => 'M',
                Change::Deleted => 'D',
                Change::TypeChange => 'T',
                Change::Conflict => 'U',
            };
            let path = change.path.strip_prefix(&status.workdir).unwrap();
            (side, kind, path.to_string_lossy().into_owned())
        })
        .collect();
    changes.sort();
    changes
}

#[test]
fn status_by_side_tells_staged_from_unstaged() {
    let temp_git = empty_git_repo();
    let repo = &gix::path::realpath(temp_git.path()).unwrap();
    std::fs::create_dir(repo.join("src")).unwrap();
    for file in [
        "both.txt",
        "staged.txt",
        "unstaged.txt",
        "renamed.txt",
        "src/lib.rs",
    ] {
        std::fs::write(repo.join(file), "one").unwrap();
    }
    std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
    create_commit(repo, true);

    std::fs::write(repo.join("both.txt"), "two").unwrap();
    std::fs::write(repo.join("staged.txt"), "two").unwrap();
    exec_git_cmd("add both.txt staged.txt", repo);
    std::fs::write(repo.join("both.txt"), "three").unwrap();
    std::fs::write(repo.join("unstaged.txt"), "two").unwrap();
    exec_git_cmd("mv renamed.txt moved.txt", repo);
    std::fs::write(repo.join("new.txt"), "").unwrap();
    std::fs::write(repo.join("build.log"), "").unwrap();
    std::fs::write(repo.join("src/lib.rs"), "two").unwrap();

    let paths = ["src", "staged.txt", "new.txt", "build.log"].map(|path| repo.join(path));
    let status = git::status_by_side(repo, true, &paths).unwrap();
    assert_eq!(&status.workdir, repo);
    let change = |side, kind, path: &str| (side, kind, path.to_string());
    assert_eq!(
        side_changes(&status),
        [
            change('i', 'D', "renamed.txt"),
            change('i', 'M', "both.txt"),
            change('i', 'M', "staged.txt"),
            change('i', 'N', "moved.txt"),
            change('w', 'M', "both.txt"),
            change('w', 'M', "src/lib.rs"),
            change('w', 'M', "unstaged.txt"),
            change('w', 'N', "new.txt"),
        ]
    );
    assert_eq!(status.tracked, [true, true, false, false]);

    // Limited to a directory, and to nothing but it even with a name that is a glob.
    let status = git::status_by_side(&repo.join("src"), true, &[]).unwrap();
    assert_eq!(side_changes(&status), [change('w', 'M', "src/lib.rs")]);
    std::fs::create_dir(repo.join("[sn]*")).unwrap();
    let status = git::status_by_side(&repo.join("[sn]*"), true, &[]).unwrap();
    assert_eq!(side_changes(&status), []);
}

#[cfg(unix)]
#[test]
fn status_by_side_reports_type_changes() {
    let temp_git = empty_git_repo();
    let repo = &gix::path::realpath(temp_git.path()).unwrap();
    std::fs::write(repo.join("target.txt"), "").unwrap();
    std::fs::write(repo.join("file.txt"), "").unwrap();
    create_commit(repo, true);
    std::fs::remove_file(repo.join("file.txt")).unwrap();
    std::os::unix::fs::symlink("target.txt", repo.join("file.txt")).unwrap();

    let change = |side, kind, path: &str| (side, kind, path.to_string());
    let status = git::status_by_side(repo, true, &[]).unwrap();
    assert_eq!(side_changes(&status), [change('w', 'T', "file.txt")]);
    exec_git_cmd("add file.txt", repo);
    let status = git::status_by_side(repo, true, &[]).unwrap();
    assert_eq!(side_changes(&status), [change('i', 'T', "file.txt")]);
}

#[test]
fn head_files_follow_the_branch() {
    let temp_git = empty_git_repo();
    let file = temp_git.path().join("file.txt");
    File::create(&file).unwrap().write_all(b"foo").unwrap();
    create_commit(temp_git.path(), true);

    let git_dir = gix::path::realpath(temp_git.path().join(".git")).unwrap();
    assert_eq!(
        git::head_files(&file).unwrap(),
        [
            git_dir.join("HEAD"),
            git_dir.join("packed-refs"),
            git_dir.join("refs/heads/main"),
        ]
    );

    exec_git_cmd("switch -c feature/x", temp_git.path());
    assert_eq!(
        git::head_files(&file).unwrap()[2],
        git_dir.join("refs/heads/feature/x")
    );

    // A detached HEAD is on no branch.
    exec_git_cmd("switch --detach", temp_git.path());
    assert_eq!(
        git::head_files(&file).unwrap(),
        [git_dir.join("HEAD"), git_dir.join("packed-refs")]
    );
}

#[test]
fn head_files_of_a_worktree() {
    let temp_git = empty_git_repo();
    let file = temp_git.path().join("file.txt");
    File::create(&file).unwrap().write_all(b"foo").unwrap();
    create_commit(temp_git.path(), true);
    let worktree = temp_git.path().join("worktree");
    exec_git_cmd(
        &format!("worktree add -b other {}", worktree.display()),
        temp_git.path(),
    );

    let git_dir = gix::path::realpath(temp_git.path().join(".git")).unwrap();
    assert_eq!(
        git::head_files(&worktree.join("file.txt")).unwrap(),
        [
            git_dir.join("worktrees/worktree/HEAD"),
            git_dir.join("packed-refs"),
            git_dir.join("refs/heads/other"),
        ]
    );
}

#[test]
fn renames_pair_files_gone_with_files_new_like_git() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let write = |dir: &Path, path: &str, text: &str| {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let lines = |count: usize, last: &str| format!("{}{last}\n", "line\n".repeat(count));
    write(old.path(), "src/foo/Poisson.java", &lines(20, "a"));
    write(new.path(), "src/foo/bar/Poisson.java", &lines(20, "a"));
    write(old.path(), "Fish.java", &lines(8, "a"));
    write(new.path(), "Trout.java", &lines(8, "b"));
    write(old.path(), "gone.rs", "fn gone() {}\n");
    write(new.path(), "new.rs", "struct New;\n");

    let paths = |paths: &[&str]| -> Vec<PathBuf> { paths.iter().map(PathBuf::from).collect() };
    let deleted = paths(&["src/foo/Poisson.java", "Fish.java", "gone.rs"]);
    let added = paths(&["new.rs", "src/foo/bar/Poisson.java", "Trout.java"]);
    let mut renames = git::renames(old.path(), new.path(), &deleted, &added).unwrap();
    renames.sort();
    assert_eq!(
        renames,
        [
            ("Fish.java".into(), "Trout.java".into()),
            (
                "src/foo/Poisson.java".into(),
                "src/foo/bar/Poisson.java".into()
            ),
        ],
        "identical, and alike but for one line in nine"
    );
    assert!(git::renames(old.path(), new.path(), &deleted, &[])
        .unwrap()
        .is_empty());
}
