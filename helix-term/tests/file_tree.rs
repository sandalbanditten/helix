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
    }

    fn focused_path(app: &Application) -> Option<&Path> {
        doc!(app.editor).path()
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
}
