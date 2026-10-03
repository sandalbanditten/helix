use std::{
    fs,
    path::{Path, PathBuf},
};

use helix_core::Rope;
use helix_term::application::Application;
use helix_view::{doc, editor::UndoConfig, undo_file};

use super::*;

/// A project directory with a file holding `text`, and a directory for undo files. Both are
/// under Cargo's target directory: files in the temporary directory get no undo files.
struct Project {
    _dir: tempfile::TempDir,
    file: PathBuf,
    undo_dir: PathBuf,
}

impl Project {
    fn new(text: &str) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
        let root = helix_stdx::path::canonicalize(dir.path());
        let file = root.join("main.rs");
        fs::write(&file, text)?;
        Ok(Self {
            _dir: dir,
            file,
            undo_dir: root.join("undo"),
        })
    }

    fn app(&self, max_revisions: usize) -> anyhow::Result<Application> {
        let mut config = test_config();
        config.editor.undo = UndoConfig {
            persist: true,
            dir: Some(self.undo_dir.clone()),
            max_revisions,
            ..Default::default()
        };
        AppBuilder::new()
            .with_file(&self.file, None)
            .with_config(config)
            .build()
    }

    /// Keys that close the file's buffer and open it again.
    fn reopen(&self) -> String {
        format!(":bc<ret>:o {}<ret>", self.file.display())
    }

    /// The number of revisions in the undo file of the file, which holds `text`.
    fn revisions(&self, text: &str) -> Option<usize> {
        let (history, _) = undo_file::read(&self.undo_dir, &self.file, &Rope::from(text), 0)?;
        Some(history.len())
    }
}

fn text(app: &Application) -> String {
    doc!(app.editor).text().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_goes_back_past_reopening() -> anyhow::Result<()> {
    let project = Project::new("start\n")?;
    let mut app = project.app(0)?;
    let reopen = project.reopen();
    test_key_sequences(
        &mut app,
        vec![
            (Some("ione <esc>:w<ret>"), None),
            (Some("itwo <esc>:w<ret>"), None),
            (
                Some(&reopen),
                Some(&|app| {
                    assert_eq!(text(app), "one two start\n");
                    assert!(!doc!(app.editor).is_modified());
                }),
            ),
            (Some("u"), Some(&|app| assert_eq!(text(app), "one start\n"))),
            (Some("u"), Some(&|app| assert_eq!(text(app), "start\n"))),
            (Some("U"), Some(&|app| assert_eq!(text(app), "one start\n"))),
            (
                Some(":later 1f<ret>"),
                Some(&|app| assert_eq!(text(app), "one two start\n")),
            ),
        ],
        false,
    )
    .await?;
    // The second write appended its revision.
    assert_eq!(project.revisions("one two start\n"), Some(3));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_changed_elsewhere_starts_afresh() -> anyhow::Result<()> {
    let project = Project::new("start\n")?;
    let mut app = project.app(0)?;
    let file = project.file.clone();
    let reopen = project.reopen();
    test_key_sequences(
        &mut app,
        vec![
            (Some("ione <esc>:w<ret>"), None),
            (
                Some(":bc<ret>"),
                Some(&|_| fs::write(&file, "changed\n").unwrap()),
            ),
            (
                Some(&reopen[":bc<ret>".len()..]),
                Some(&|app| assert_eq!(text(app), "changed\n")),
            ),
            (Some("u"), Some(&|app| assert_eq!(text(app), "changed\n"))),
        ],
        false,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_files_keep_the_newest_revisions() -> anyhow::Result<()> {
    let project = Project::new("start\n")?;
    let mut app = project.app(3)?;
    let reopen = project.reopen();
    test_key_sequences(
        &mut app,
        vec![
            (Some("ia <esc>ib <esc>ic <esc>id <esc>:w<ret>"), None),
            (
                Some(&reopen),
                Some(&|app| assert_eq!(text(app), "a b c d start\n")),
            ),
            (
                Some("uuuu"),
                Some(&|app| assert_eq!(text(app), "a b start\n")),
            ),
        ],
        false,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn write_quit_writes_the_undo_file() -> anyhow::Result<()> {
    let project = Project::new("start\n")?;
    let mut app = project.app(0)?;
    test_key_sequence(&mut app, Some("ione <esc>:wq<ret>"), None, true).await?;
    assert_eq!(project.revisions("one start\n"), Some(2));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_files_are_kept_only_when_asked_for() -> anyhow::Result<()> {
    let project = Project::new("start\n")?;
    let mut config = test_config();
    config.editor.undo.dir = Some(project.undo_dir.clone());
    let mut app = AppBuilder::new()
        .with_file(&project.file, None)
        .with_config(config)
        .build()?;
    test_key_sequence(&mut app, Some("ione <esc>:w<ret>"), None, false).await?;
    assert!(!Path::new(&project.undo_dir).exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_a_file_takes_its_undo_file() -> anyhow::Result<()> {
    let project = Project::new("start\n")?;
    let mut app = project.app(0)?;
    let moved = project.file.with_file_name("moved.rs");
    let keys = format!(
        "ione <esc>:w<ret>:move {}<ret>:bc<ret>:o {}<ret>",
        moved.display(),
        moved.display()
    );
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(&keys),
                Some(&|app| assert_eq!(text(app), "one start\n")),
            ),
            (Some("u"), Some(&|app| assert_eq!(text(app), "start\n"))),
        ],
        false,
    )
    .await?;
    assert!(project.revisions("one start\n").is_none());
    Ok(())
}
