//! Dired buffers list directories of the working directory, which the whole process shares, so
//! their tests run in a binary of their own and one after another, each in a workspace of its own.

#[cfg(feature = "integration")]
mod test {
    #[allow(dead_code)]
    mod helpers;

    use std::{
        fs,
        path::PathBuf,
        process::Command,
        sync::{Mutex, MutexGuard, PoisonError},
    };

    use helix_term::application::Application;
    use helix_view::doc;
    use tempfile::TempDir;

    use self::helpers::{test_key_sequences, AppBuilder};

    static WORKING_DIRECTORY: Mutex<()> = Mutex::new(());

    /// A temporary workspace that is the working directory while it lives.
    struct Workspace {
        dir: TempDir,
        _working_directory: MutexGuard<'static, ()>,
    }

    impl Workspace {
        /// Creates the files, and the directories ending in `/`, at `paths`.
        fn new(paths: &[&str]) -> anyhow::Result<Self> {
            let working_directory = WORKING_DIRECTORY
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let dir = tempfile::tempdir()?;
            for path in paths {
                let path = dir.path().join(path);
                if path.to_string_lossy().ends_with('/') {
                    fs::create_dir_all(&path)?;
                } else {
                    fs::create_dir_all(path.parent().unwrap())?;
                    fs::write(&path, "")?;
                }
            }
            helix_stdx::env::set_current_working_dir(dir.path())?;
            Ok(Self {
                dir,
                _working_directory: working_directory,
            })
        }

        fn path(&self, path: &str) -> PathBuf {
            helix_stdx::path::canonicalize(self.dir.path().join(path))
        }

        fn app(&self, open: &str) -> anyhow::Result<Application> {
            AppBuilder::new().with_file(self.path(open), None).build()
        }

        fn git(&self, args: &[&str]) {
            let output = Command::new("git")
                .arg("-C")
                .arg(self.dir.path())
                .args(["-c", "user.name=helix", "-c", "user.email=helix@helix"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .env_remove("GIT_DIR")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
    }

    /// The rows of the screen as last drawn, without trailing spaces.
    fn screen(app: &Application) -> Vec<String> {
        let screen = app.screen();
        screen
            .content
            .chunks(screen.area.width as usize)
            .map(|row| {
                let row: String = row.iter().map(|cell| cell.symbol.as_str()).collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    fn dired_text(app: &Application) -> String {
        let doc = doc!(app.editor);
        assert!(doc.dired.is_some(), "not in a dired buffer");
        doc.text().to_string()
    }

    fn status(app: &Application) -> String {
        app.editor
            .get_status()
            .map(|(status, _)| status.to_string())
            .unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn renames_are_written_to_the_files() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt", "b.txt", "sub/"])?;
        let mut app = workspace.app("a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (
                    Some(":dired<ret>"),
                    Some(&|app| {
                        let text = dired_text(app);
                        assert_eq!(text.lines().count(), 3, "{text}");
                        assert!(text.lines().next().unwrap().ends_with(" sub"), "{text}");
                    }),
                ),
                (
                    Some("/b\\.txt<ret>cc.txt<esc>:w<ret>"),
                    Some(&|app| {
                        assert!(!workspace.path("b.txt").exists());
                        assert!(workspace.path("c.txt").exists());
                        assert_eq!(status(app), "Applied 1 change");
                        assert!(dired_text(app).contains(" c.txt\n"));
                        assert!(!doc!(app.editor).is_modified());
                    }),
                ),
                // Moving into a directory that does not exist takes `:w!`; the buffer on the
                // file follows it.
                (
                    Some("/a\\.txt<ret>cnew/dir/a.txt<esc>:w<ret>"),
                    Some(&|app| {
                        assert!(workspace.path("a.txt").exists());
                        assert!(
                            status(app).contains("Creates the directory"),
                            "{}",
                            status(app)
                        );
                        assert!(doc!(app.editor).is_modified());
                    }),
                ),
                (
                    Some(":w!<ret>"),
                    Some(&|app| {
                        assert!(workspace.path("new/dir/a.txt").exists());
                        let moved = app
                            .editor
                            .documents()
                            .any(|doc| doc.path() == Some(&workspace.path("new/dir/a.txt")));
                        assert!(moved, "the buffer stays on its file");
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deletions_and_discards_take_force() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["keep.txt", "gone/inside.txt"])?;
        let mut app = workspace.app("keep.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some(":dired<ret>"), None),
                (
                    Some("ggxd:w<ret>"),
                    Some(&|app| {
                        assert!(workspace.path("gone").exists());
                        assert!(status(app).ends_with("Deletes gone (use :w! to apply)"));
                        assert_eq!(doc!(app.editor).diagnostics().len(), 1);
                    }),
                ),
                (
                    Some(":w!<ret>"),
                    Some(&|app| {
                        assert!(!workspace.path("gone").exists());
                        assert_eq!(dired_text(app).lines().count(), 1);
                        assert!(doc!(app.editor).diagnostics().is_empty());
                    }),
                ),
                // Undo cannot bring back what a write did.
                (
                    Some("u"),
                    Some(&|app| {
                        assert_eq!(dired_text(app).lines().count(), 1);
                        assert!(!doc!(app.editor).is_modified());
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_tree_opens_dired_over_the_whole_editor() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt", "src/main.rs", "src/lib.rs"])?;
        let mut app = workspace.app("src/main.rs")?;
        test_key_sequences(
            &mut app,
            vec![
                // The tree puts its cursor on the focused file once its directory is listed.
                (Some(":vsplit<ret><space>e"), None),
                (
                    Some("e"),
                    Some(&|app| {
                        // The directory of the focused file, its cursor on it.
                        let text = dired_text(app);
                        assert_eq!(text.lines().count(), 2, "{text}");
                        let screen = screen(app);
                        assert_eq!(app.editor.tree.zoomed(), Some(app.editor.tree.focus));
                        assert!(screen[0].contains("0644"), "{}", screen.join("\n"));
                        // One split over everything: no tree, no other split beside it.
                        assert!(!screen[0].contains('│'), "{}", screen.join("\n"));
                        assert!(screen.iter().any(|row| row.contains("[dired] src/")));
                        let (view, doc) = helix_view::current_ref!(app.editor);
                        let text = doc.text().slice(..);
                        let line = text.char_to_line(doc.selection(view.id).primary().head);
                        assert!(text.line(line).to_string().contains("main.rs"));
                    }),
                ),
                (
                    Some("<space>e"),
                    Some(&|app| {
                        assert!(app.editor.documents().all(|doc| doc.dired.is_none()));
                        assert_eq!(app.editor.tree.zoomed(), None);
                        assert_eq!(app.editor.tree.views().count(), 2);
                    }),
                ),
                // The tree as shown, nested.
                (
                    Some("E"),
                    Some(&|app| {
                        let text = dired_text(app);
                        let names: Vec<_> = text
                            .lines()
                            .map(|line| line.rsplit_once(' ').unwrap().1)
                            .collect();
                        assert_eq!(names, [".", "src", "lib.rs", "main.rs", "a.txt"], "{text}");
                        assert!(text.contains("│   └── "), "{text}");
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_git_column_stages_and_ignores() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["tracked.txt", "new.txt", "noise.log"])?;
        workspace.git(&["init", "-q"]);
        fs::write(workspace.path(".gitignore"), "*.log\n")?;
        workspace.git(&["add", "tracked.txt", ".gitignore"]);
        workspace.git(&["commit", "-qm", "init"]);
        fs::write(workspace.path("tracked.txt"), "changed")?;
        let mut app = workspace.app("tracked.txt")?;
        let porcelain = || {
            let output = Command::new("git")
                .arg("-C")
                .arg(workspace.dir.path())
                .args(["status", "--porcelain=v1", "--ignored"])
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        test_key_sequences(
            &mut app,
            vec![
                (
                    Some(":dired<ret>"),
                    Some(&|app| {
                        let text = dired_text(app);
                        assert!(text.contains(" -N ") && text.contains(" -M "), "{text}");
                    }),
                ),
                (
                    Some("/new<ret>xs -N <ret>c N- <esc>/tracked<ret>xs -M <ret>c M- <esc>/noise<ret>xs -I <ret>c -- <esc>:w<ret>"),
                    Some(&|app| {
                        assert_eq!(status(app), "Applied 3 changes");
                        assert_eq!(
                            porcelain(),
                            " M .gitignore\nA  new.txt\nM  tracked.txt\n?? noise.log\n"
                        );
                        assert!(dired_text(app).contains(" N- "));
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unedited_buffers_follow_the_files() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt"])?;
        let mut app = workspace.app("a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (
                    Some(":dired<ret>"),
                    Some(&|app| {
                        assert_eq!(dired_text(app).lines().count(), 1);
                        fs::write(workspace.path("b.txt"), "").unwrap();
                    }),
                ),
                // Give the watcher and the listing time.
                (Some("<esc>"), None),
                (
                    Some("<esc>"),
                    Some(&|app| {
                        let text = dired_text(app);
                        assert!(text.contains("b.txt"), "{text}");
                        assert!(!doc!(app.editor).is_modified());
                    }),
                ),
                // An edited buffer keeps its edits.
                (
                    Some("ggxd"),
                    Some(&|_| fs::write(workspace.path("c.txt"), "").unwrap()),
                ),
                (Some("<esc>"), None),
                (
                    Some("<esc>"),
                    Some(&|app| {
                        let text = dired_text(app);
                        assert_eq!(text.lines().count(), 1, "{text}");
                        assert!(!text.contains("c.txt"), "{text}");
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pasted_lines_copy_their_entries() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt", "dir/inside.txt"])?;
        fs::write(workspace.path("a.txt"), "a")?;
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30);
        helix_stdx::fs::set_modified(&workspace.path("a.txt"), time)?;
        let modified = |path: &str| {
            fs::metadata(workspace.path(path))
                .unwrap()
                .modified()
                .unwrap()
        };
        let mut app = workspace.app("dir/inside.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some(":dired .<ret>"), None),
                // A pasted line copies, keeping the time; the copy is named on the line.
                (
                    Some("/a\\.txt<ret>xypsa\\.txt<ret>cb.txt<esc>:w<ret>"),
                    Some(&|app| {
                        assert_eq!(status(app), "Applied 1 change");
                        assert_eq!(fs::read_to_string(workspace.path("b.txt")).unwrap(), "a");
                        assert_eq!(modified("b.txt"), time);
                        assert!(workspace.path("a.txt").exists());
                        assert!(!doc!(app.editor).is_modified());
                    }),
                ),
                // Directories are copied with everything in them.
                (
                    Some("ggxypsdir<ret>cdir2<esc>:w<ret>"),
                    Some(&|app| {
                        assert_eq!(status(app), "Applied 1 change");
                        assert!(workspace.path("dir2/inside.txt").exists());
                        assert!(workspace.path("dir/inside.txt").exists());
                    }),
                ),
                // A line yanked in another dired buffer copies into this one's directory, even
                // once that buffer is closed.
                (Some(":dired dir<ret>"), None),
                (Some("/inside<ret>xy:bc<ret>:dired .<ret>"), None),
                (
                    Some("ggp:w<ret>"),
                    Some(&|app| {
                        assert_eq!(status(app), "Applied 1 change");
                        assert!(workspace.path("inside.txt").exists());
                        assert!(workspace.path("dir/inside.txt").exists());
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pasted_lines_copy_the_entry_yanked_of_those_alike() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["x/a.txt", "y/a.txt"])?;
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30);
        for (path, contents) in [("x/a.txt", "x"), ("y/a.txt", "y")] {
            fs::write(workspace.path(path), contents)?;
            helix_stdx::fs::set_modified(&workspace.path(path), time)?;
        }
        let mut app = workspace.app("x/a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some(":dired x<ret>"), None),
                // Yanked in `x`, whose buffer is closed then, and pasted next to the `a.txt` of
                // `y`, which reads the same.
                (Some("xy:bc<ret>:dired y<ret>"), None),
                (
                    Some("pxsa\\.txt<ret><A-c>b.txt<esc>:w<ret>"),
                    Some(&|app| {
                        assert_eq!(status(app), "Applied 1 change");
                        let copy = fs::read_to_string(workspace.path("y/b.txt")).unwrap();
                        assert_eq!(copy, "x");
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cut_lines_pasted_into_a_directory_move() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["b.txt", "dir/inside.txt"])?;
        fs::write(workspace.path("b.txt"), "b")?;
        let mut app = workspace.app("dir/inside.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                (
                    Some("E"),
                    Some(&|app| {
                        let text = dired_text(app);
                        assert_eq!(text.lines().count(), 4, "{text}");
                    }),
                ),
                // Below `inside.txt`, the line goes into `dir`, whatever its guides say.
                (
                    Some("/b\\.txt<ret>xd/inside<ret>p:w<ret>"),
                    Some(&|app| {
                        assert_eq!(status(app), "Applied 1 change");
                        assert!(!workspace.path("b.txt").exists());
                        let moved = fs::read_to_string(workspace.path("dir/b.txt")).unwrap();
                        assert_eq!(moved, "b");
                        let text = dired_text(app);
                        let nested =
                            |line: &str| line.contains("    ├── ") && line.ends_with("b.txt");
                        assert!(text.lines().any(nested), "{text}");
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn copies_are_made_in_the_background() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt", "dir/inside.txt"])?;
        fs::write(workspace.path("a.txt"), "a")?;
        let mut app = workspace.app("dir/inside.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some(":dired .<ret>"), None),
                // Once the copy is made, the buffer is listed anew.
                (
                    Some("/a\\.txt<ret>xypsa\\.txt<ret>cb.txt<esc>:w<ret>"),
                    Some(&|app| {
                        assert_eq!(status(app), "Applied 1 change");
                        assert_eq!(fs::read_to_string(workspace.path("b.txt")).unwrap(), "a");
                        assert!(dired_text(app).contains("b.txt"));
                        assert!(!doc!(app.editor).is_modified());
                    }),
                ),
                // Keys come before the copy is done: the second write is refused, and the
                // buffer edited meanwhile keeps its text.
                (
                    Some("/b\\.txt<ret>xypsb\\.txt<ret>cc.txt<esc>:w<ret>:w<ret>ggO<esc>"),
                    Some(&|app| {
                        assert_eq!(
                            status(app),
                            "Applied 1 change; :reload lists the edited buffer anew"
                        );
                        assert_eq!(fs::read_to_string(workspace.path("c.txt")).unwrap(), "a");
                        assert!(doc!(app.editor).is_modified());
                        assert!(dired_text(app).starts_with('\n'));
                    }),
                ),
                (
                    Some(":reload<ret>"),
                    Some(&|app| {
                        assert!(dired_text(app).contains("c.txt"));
                        assert!(!doc!(app.editor).is_modified());
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn failed_copies_keep_the_editor_open() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let workspace = Workspace::new(&["open.txt", "secret.txt"])?;
        let secret = workspace.path("secret.txt");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000))?;
        if fs::read(&secret).is_ok() {
            // Running with CAP_DAC_OVERRIDE (e.g. as root): nothing can fail to copy.
            return Ok(());
        }
        let mut app = workspace.app("open.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some(":dired .<ret>"), None),
                (
                    Some("/secret<ret>xypssecret<ret>ccopy<esc>:w<ret>"),
                    Some(&|app| {
                        let status = status(app);
                        assert!(
                            status.starts_with("Applied 0 of 1 changes: Cannot copy"),
                            "{status}"
                        );
                        assert!(!workspace.path("copy.txt").exists());
                        assert!(!doc!(app.editor).is_modified());
                    }),
                ),
                // Writing and quitting copies first, so the failure keeps the editor open.
                (
                    Some("/secret<ret>xypssecret<ret>ccopy<esc>:wq<ret>"),
                    Some(&|app| {
                        let status = status(app);
                        assert!(
                            status.ends_with("Applied 0 of 1 changes: Cannot copy secret.txt to copy.txt: Permission denied (os error 13)"),
                            "{status}"
                        );
                        assert!(!workspace.path("copy.txt").exists());
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await
    }

    /// Times writing many renames at once, from the end of one step to the end of the next, of
    /// which an empty step shows what waiting for the editor to be idle costs. Run it with
    /// `cargo integration-test -- dired::test::measure_writes --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_writes() -> anyhow::Result<()> {
        const FILES: usize = 2000;
        let names: Vec<String> = (0..FILES).map(|i| format!("file-{i:04}.txt")).collect();
        let paths: Vec<&str> = names.iter().map(String::as_str).collect();
        let workspace = Workspace::new(&paths)?;
        let mut app = workspace.app("file-0000.txt")?;
        let last = std::cell::Cell::new(std::time::Instant::now());
        let lap = |what: &str| {
            let now = std::time::Instant::now();
            println!("{what}: {:?}", now - last.replace(now));
        };
        test_key_sequences(
            &mut app,
            vec![
                (Some(":dired<ret>"), Some(&|_| lap("open"))),
                (Some("<esc>"), Some(&|_| lap("an empty step"))),
                (Some("%s\\.txt<ret>c.md<esc>"), Some(&|_| lap("edit"))),
                (
                    Some(":w<ret>"),
                    Some(&|app| {
                        lap(&format!("write {FILES} renames"));
                        assert_eq!(status(app), format!("Applied {FILES} changes"));
                    }),
                ),
                // Every line pasted below the others, the copies named back.
                (Some("%ygep"), Some(&|_| lap("paste"))),
                (Some("s\\.md<ret>c.txt<esc>"), Some(&|_| lap("edit"))),
                (
                    Some(":w<ret>"),
                    Some(&|app| {
                        lap(&format!("write {FILES} copies"));
                        assert_eq!(status(app), format!("Applied {FILES} changes"));
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await?;
        assert!(workspace.path("file-1999.md").exists());
        assert!(workspace.path("file-1999.txt").exists());
        Ok(())
    }
}
