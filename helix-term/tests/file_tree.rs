//! The file tree lists the working directory, which the whole process shares, so its tests run
//! in a binary of their own and one after another, each in a workspace of its own.

#[cfg(feature = "integration")]
mod test {
    #[allow(dead_code)]
    mod helpers;

    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{Mutex, MutexGuard, PoisonError},
    };

    use helix_term::application::Application;
    use helix_view::{clipboard::ClipboardProvider, doc};
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

        /// An application with `open` open, whose clipboard is not the system's.
        fn app(&self, open: &str) -> anyhow::Result<Application> {
            let mut config = helpers::test_config();
            config.editor.clipboard_provider = ClipboardProvider::None;
            AppBuilder::new()
                .with_config(config)
                .with_file(self.path(open), None)
                .build()
        }
    }

    fn focused_path(app: &Application) -> Option<&Path> {
        doc!(app.editor).path()
    }

    /// Popups placed against the left edge of the editor stay beside the file tree.
    mod popups {
        use super::*;

        /// The column `text` starts at in the first row of the screen showing it.
        fn column_of(app: &Application, text: &str) -> Option<usize> {
            let screen = app.screen();
            screen
                .content
                .chunks(screen.area.width as usize)
                .find_map(|row| {
                    row.windows(text.len()).position(|cells| {
                        cells
                            .iter()
                            .map(|cell| cell.symbol.chars().next().unwrap_or(' '))
                            .eq(text.chars())
                    })
                })
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn shell_output_is_shown_beside_the_file_tree() -> anyhow::Result<()> {
            let workspace = Workspace::new(&["a.txt"])?;
            let mut app = workspace.app("a.txt")?;
            test_key_sequences(
                &mut app,
                vec![
                    (Some("<space>E"), None),
                    (
                        Some(":sh echo hello<ret>"),
                        Some(&|app| {
                            let editor_x = app.editor.tree.area().x as usize;
                            assert!(editor_x > 0, "the file tree isn't shown");
                            // Two columns into the editor, inside the margin of the text.
                            assert_eq!(column_of(app, "hello"), Some(editor_x + 3));
                        }),
                    ),
                ],
                false,
            )
            .await
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn moving_up_from_the_top_wraps_around() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt", "b.txt"])?;
        let mut app = workspace.app("a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                // From `a.txt` up to the root, then around to the last row.
                (
                    Some("kk<ret>"),
                    Some(&|app| {
                        assert_eq!(focused_path(app), Some(workspace.path("b.txt").as_path()));
                    }),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn other_keys_go_back_to_the_editor() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt"])?;
        let mut app = workspace.app("a.txt")?;
        test_key_sequences(
            &mut app,
            vec![(
                Some("<space>eihello<esc>"),
                Some(&|app| {
                    assert_eq!(doc!(app.editor).text().to_string(), "hello");
                }),
            )],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn renaming_keeps_the_buffer_on_its_file() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt"])?;
        let mut app = workspace.app("a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                (
                    Some("r<C-u>b.txt<ret>"),
                    Some(&|app| {
                        assert!(!workspace.path("a.txt").exists());
                        assert!(workspace.path("b.txt").exists());
                        assert_eq!(focused_path(app), Some(workspace.path("b.txt").as_path()));
                    }),
                ),
                (
                    Some("R<C-u>sub/c.txt<ret>"),
                    Some(&|app| {
                        assert!(workspace.path("sub/c.txt").exists());
                        assert_eq!(
                            focused_path(app),
                            Some(workspace.path("sub/c.txt").as_path())
                        );
                    }),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn renaming_a_directory_moves_the_buffers_in_it() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["dir/x.txt"])?;
        let mut app = workspace.app("dir/x.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                // Up from `x.txt` to its directory.
                (
                    Some("kr<C-u>moved<ret>"),
                    Some(&|app| {
                        assert!(workspace.path("moved/x.txt").exists());
                        assert_eq!(
                            focused_path(app),
                            Some(workspace.path("moved/x.txt").as_path())
                        );
                    }),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn creating_opens_new_files() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/a.txt"])?;
        let mut app = workspace.app("src/a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                // Next to the file under the cursor.
                (
                    Some("anew.txt<ret>"),
                    Some(&|app| {
                        assert!(workspace.path("src/new.txt").is_file());
                        assert_eq!(
                            focused_path(app),
                            Some(workspace.path("src/new.txt").as_path())
                        );
                    }),
                ),
                (Some("<space>e"), None),
                (
                    Some("Adir<ret>"),
                    Some(&|_| assert!(workspace.path("src/dir").is_dir())),
                ),
                // The cursor went to the new directory, so this goes in it.
                (
                    Some("adeep/<ret>"),
                    Some(&|_| assert!(workspace.path("src/dir/deep").is_dir())),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_asks_and_closes_the_buffer() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt", "b.txt"])?;
        let mut app = workspace.app("a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                (
                    Some("dn<ret>"),
                    Some(&|_| assert!(workspace.path("a.txt").exists())),
                ),
                (
                    Some("dy<ret>"),
                    Some(&|app| {
                        assert!(!workspace.path("a.txt").exists());
                        let a = workspace.path("a.txt");
                        assert!(app.editor.document_by_path(&a).is_none());
                    }),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn modified_buffers_are_not_deleted() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt"])?;
        let mut app = workspace.app("a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("ihi<esc><space>e"), None),
                (
                    Some("dy<ret>"),
                    Some(&|app| {
                        assert!(workspace.path("a.txt").exists());
                        let (message, _) = app.editor.get_status().unwrap();
                        assert!(message.contains("unsaved changes"), "{message}");
                    }),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn searching_goes_through_the_tree() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/alpha.rs", "src/beta.rs", "docs/beta.md"])?;
        let mut app = workspace.app("src/alpha.rs")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                (Some("/beta"), None),
                // The first match after `src/alpha.rs`, then the next one, wrapping around.
                (
                    Some("<ret><ret>"),
                    Some(&|app| {
                        assert_eq!(
                            focused_path(app),
                            Some(workspace.path("src/beta.rs").as_path())
                        );
                    }),
                ),
                (Some("<space>en"), None),
                (
                    Some("<ret>"),
                    Some(&|app| {
                        assert_eq!(
                            focused_path(app),
                            Some(workspace.path("docs/beta.md").as_path())
                        );
                    }),
                ),
            ],
            false,
        )
        .await
    }

    fn status(app: &Application) -> String {
        app.editor
            .get_status()
            .map(|(message, _)| message.to_string())
            .unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn copies_are_pasted_under_free_names() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/a.txt", "src/b.txt"])?;
        let a = workspace.path("src/a.txt");
        fs::write(&a, "a")?;
        let time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30);
        fs::File::options()
            .write(true)
            .open(&a)?
            .set_modified(time)?;
        let mut app = workspace.app("src/a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                (
                    Some("y"),
                    Some(&|app| assert_eq!(status(app), "Copied 'src/a.txt'")),
                ),
                (
                    Some("p<ret>"),
                    Some(&|app| {
                        let copy = workspace.path("src/a-1.txt");
                        assert_eq!(fs::read_to_string(&copy).unwrap(), "a");
                        assert_eq!(fs::metadata(&copy).unwrap().modified().unwrap(), time);
                        assert_eq!(status(app), "'src/a.txt' pasted as 'src/a-1.txt'");
                    }),
                ),
                // The cursor went to the copy, next to which the next one goes.
                (
                    Some("p<ret>"),
                    Some(&|_| assert!(workspace.path("src/a-2.txt").is_file())),
                ),
                (
                    Some("p<C-u>b.txt<ret>"),
                    Some(&|app| {
                        assert_eq!(fs::read_to_string(workspace.path("src/b.txt")).unwrap(), "");
                        assert_eq!(status(app), "'src/b.txt' already exists");
                    }),
                ),
                (
                    Some("p<esc>"),
                    Some(&|_| assert!(!workspace.path("src/a-3.txt").exists())),
                ),
                // The name typed is the name of the copy.
                (
                    Some("p<C-u>sub/c.txt<ret>"),
                    Some(&|_| {
                        let copy = workspace.path("src/sub/c.txt");
                        assert_eq!(fs::read_to_string(copy).unwrap(), "a");
                    }),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn directories_are_copied_whole_but_not_into_themselves() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["dir/x.txt", "dir/sub/y.txt", "other/"])?;
        let mut app = workspace.app("dir/x.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                // Up from `x.txt` past `sub` to `dir`.
                (Some("<space>e"), None),
                (Some("kky"), None),
                // Into `sub`, the directory under the cursor.
                (
                    Some("jp<ret>"),
                    Some(&|app| {
                        assert!(!workspace.path("dir/sub/dir").exists());
                        assert_eq!(status(app), "'dir' cannot be pasted into itself");
                    }),
                ),
                // Into `other`, the last row.
                (
                    Some("gep<ret>"),
                    Some(&|_| {
                        assert!(workspace.path("other/dir/x.txt").is_file());
                        assert!(workspace.path("other/dir/sub/y.txt").is_file());
                        assert!(workspace.path("dir/sub/y.txt").is_file());
                    }),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cut_entries_are_moved_once() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt", "dir/"])?;
        let mut app = workspace.app("a.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                (
                    Some("x"),
                    Some(&|app| assert_eq!(status(app), "Cut 'a.txt'")),
                ),
                // Into `dir`, the row above.
                (
                    Some("kp<ret>"),
                    Some(&|app| {
                        assert!(!workspace.path("a.txt").exists());
                        let moved = workspace.path("dir/a.txt");
                        assert!(moved.is_file());
                        assert_eq!(focused_path(app), Some(moved.as_path()));
                        assert_eq!(status(app), "'a.txt' moved to 'dir/a.txt'");
                    }),
                ),
                (
                    Some("p"),
                    Some(&|app| assert_eq!(status(app), "Nothing to paste")),
                ),
            ],
            false,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removed_entries_cannot_be_pasted() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["a.txt", "b.txt"])?;
        let mut app = workspace.app("b.txt")?;
        test_key_sequences(
            &mut app,
            vec![
                // Up from `b.txt` to `a.txt`.
                (Some("<space>e"), None),
                (
                    Some("ky"),
                    Some(&|_| fs::remove_file(workspace.path("a.txt")).unwrap()),
                ),
                (
                    Some("p"),
                    Some(&|app| {
                        assert_eq!(status(app), "'a.txt' no longer exists");
                        assert!(!workspace.path("a-1.txt").exists());
                    }),
                ),
            ],
            false,
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn copying_puts_the_path_in_the_clipboard() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/a.txt"])?;
        let clipboard = tempfile::NamedTempFile::new()?;
        let file = clipboard.path().display().to_string();
        let mut config = helpers::test_config();
        // Helix pastes into the clipboard to set it and yanks from it to read it.
        config.editor.clipboard_provider = serde_json::from_value(serde_json::json!({
            "custom": {
                "paste": { "command": "sh", "args": ["-c", format!("cat > '{file}'")] },
                "yank": { "command": "cat", "args": [file] },
            }
        }))?;
        let mut app = AppBuilder::new()
            .with_config(config)
            .with_file(workspace.path("src/a.txt"), None)
            .build()?;
        test_key_sequences(
            &mut app,
            vec![
                (Some("<space>e"), None),
                (
                    Some("y"),
                    Some(&|_| {
                        assert_eq!(fs::read_to_string(clipboard.path()).unwrap(), "src/a.txt");
                    }),
                ),
            ],
            false,
        )
        .await
    }
}
