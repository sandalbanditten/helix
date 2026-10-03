#![allow(clippy::single_range_in_vec_init)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use helix_core::{diff::compare_ropes, Rope};
use helix_term::application::Application;
use helix_view::{
    diff_view::{LineChange, Side},
    doc,
    document::Mode,
    editor::DiffTool,
    graphics::Rect,
    view::ViewPosition,
    DocumentId, ViewId,
};

use super::*;

/// A repository holding `file.rs` committed as `committed`, and the file's path.
fn repository(committed: &str) -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let path = helix_stdx::path::canonicalize(dir.path().join("file.rs"));
    fs::write(&path, committed)?;
    git(dir.path(), &["init"]);
    git(dir.path(), &["add", "file.rs"]);
    git(dir.path(), &["commit", "-m", "one"]);
    Ok((dir, path))
}

/// A session on `path` diffing with `tool`.
fn session(path: &Path, tool: DiffTool) -> anyhow::Result<Session> {
    let mut config = test_config();
    config.editor.diff.tool = tool;
    let app = AppBuilder::new()
        .with_file(path, None)
        .with_config(config)
        .build()?;
    Ok(Session::new(app))
}

/// Changes the text of the focused buffer to `text`.
fn set_text(app: &mut Application, text: &str) {
    let view = app.editor.tree.focus;
    let doc = helix_view::doc_mut!(app.editor);
    let transaction = compare_ropes(doc.text(), &Rope::from(text));
    doc.apply(&transaction, view);
}

/// The panes of the diff shown, old and new, with their views.
fn panes(app: &Application) -> Option<[(ViewId, DocumentId); 2]> {
    let mut panes = app.editor.tree.views().filter_map(|(view, _)| {
        let side = app.editor.document(view.doc)?.diff_view.as_ref()?.side;
        Some((side, view.id, view.doc))
    });
    let (first, second) = (panes.next()?, panes.next()?);
    let [old, new] = if first.0 == Side::Old {
        [first, second]
    } else {
        [second, first]
    };
    Some([(old.1, old.2), (new.1, new.2)])
}

/// The rows the panes show, left and right, with runs of spaces as one.
fn rows(app: &Application) -> Vec<(String, String)> {
    let [(old, _), (new, _)] = panes(app).expect("the panes are shown");
    let (old, new) = (app.editor.tree.get(old), app.editor.tree.get(new));
    let screen = app.screen();
    let width = screen.area.width as usize;
    let text = |area: Rect, y: usize| -> String {
        let row = &screen.content[y * width..(y + 1) * width];
        let text: String = row[area.x as usize..area.right() as usize]
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let height = old.inner_area(doc!(app.editor, &old.doc)).height as usize;
    (0..height)
        .map(|y| (text(old.area, y), text(new.area, y)))
        .collect()
}

fn status(app: &Application) -> String {
    app.editor
        .get_status()
        .map(|(status, _)| status.to_string())
        .unwrap_or_default()
}

fn offset(app: &Application, (view, doc): (ViewId, DocumentId)) -> ViewPosition {
    doc!(app.editor, &doc).view_offset(view)
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_shows_the_buffer_beside_its_committed_version() -> anyhow::Result<()> {
    let (_dir, path) = repository("fn main() {\n    a();\n    b();\n}\n")?;
    let mut session = session(&path, DiffTool::Builtin)?;
    // Lines added above the first one, and one changed.
    set_text(
        &mut session.app,
        "use x;\nuse y;\nfn main() {\n    a();\n    c();\n}\n",
    );
    let origin = session.app.editor.tree.focus;
    session.keys(":diff<ret>").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let app = &session.app;
    let [(_, old_doc), (new_view, _)] = panes(app).unwrap();
    assert_eq!(
        app.editor.tree.focus, new_view,
        "the new side has the focus"
    );
    assert_eq!(app.editor.tree.zoomed(), Some(new_view));
    assert_eq!(app.editor.tree.visible_views().count(), 2);
    assert!(doc!(app.editor, &old_doc)
        .display_name()
        .ends_with("file.rs (HEAD)"));
    let rows: Vec<_> = rows(app).into_iter().take(6).collect();
    let rows: Vec<_> = rows
        .iter()
        .map(|(old, new)| (old.as_str(), new.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("", "1 use x;"),
            ("", "2 use y;"),
            ("1 fn main() {", "3 fn main() {"),
            ("2 a();", "4 a();"),
            ("3 b();", "5 c();"),
            ("4 }", "6 }"),
        ],
        "the old side shows rows above its first line"
    );

    // Closing either pane closes both and brings the layout back.
    session.keys(":q<ret>").await?;
    let app = &session.app;
    assert!(panes(app).is_none());
    assert_eq!(app.editor.tree.focus, origin);
    assert_eq!(app.editor.tree.zoomed(), None);
    assert!(app.editor.documents().all(|doc| doc.diff_view.is_none()));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn panes_scroll_together_and_refuse_edits() -> anyhow::Result<()> {
    let committed: String = (1..=60).map(|i| format!("line {i}\n")).collect();
    let (_dir, path) = repository(&committed)?;
    let mut session = session(&path, DiffTool::Builtin)?;
    let added: String = (1..=10).map(|i| format!("new {i}\n")).collect();
    set_text(&mut session.app, &(added + committed.as_str()));
    session.keys(":diff<ret>").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let [old, new] = panes(&session.app).unwrap();
    let line = |app: &Application, doc: DocumentId, line: usize| {
        doc!(app.editor, &doc).text().line_to_char(line)
    };

    // The old side shows the row the new side shows at its top: below the ten added lines a
    // line of its own, among them a filler.
    session.keys("40ggzt").await?;
    let app = &session.app;
    let top = doc!(app.editor, &new.1)
        .text()
        .char_to_line(offset(app, new).anchor);
    assert!(top > 10, "{top}");
    let at = |anchor, vertical_offset| ViewPosition {
        anchor,
        horizontal_offset: 0,
        vertical_offset,
    };
    assert_eq!(offset(app, old), at(line(app, old.1, top - 10), 0));
    session.keys("gg3zj").await?;
    assert_eq!(
        offset(&session.app, new).anchor,
        line(&session.app, new.1, 3)
    );
    assert_eq!(offset(&session.app, old), at(0, 3));
    let rows = rows(&session.app);
    assert!(rows[..7].iter().all(|(old, _)| old.is_empty()), "{rows:?}");
    assert_eq!(rows[7].0, "1 line 1");

    // The cursor goes along to the same row when the focus moves.
    session.keys("13gg<C-w>h").await?;
    let app = &session.app;
    assert_eq!(app.editor.tree.focus, old.0);
    let doc = doc!(app.editor, &old.1);
    let cursor = doc.selection(old.0).primary().cursor(doc.text().slice(..));
    assert_eq!(doc.text().char_to_line(cursor), 2);

    // The panes are read-only.
    let text = doc.text().clone();
    session.keys("i").await?;
    assert_eq!(session.app.editor.mode, Mode::Normal);
    assert_eq!(status(&session.app), "The diff view is read-only");
    session.keys("xd").await?;
    assert_eq!(*doc!(session.app.editor, &old.1).text(), text);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn diffs_follow_their_buffer() -> anyhow::Result<()> {
    let (_dir, path) = repository("a\nb\n")?;
    let mut session = session(&path, DiffTool::Builtin)?;
    let (buffer, origin) = (doc!(session.app.editor).id(), session.app.editor.tree.focus);
    session.keys(":diff<ret>").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let [_, (_, new_doc)] = panes(&session.app).unwrap();

    // The buffer changes while its diff is shown, as when it is reloaded.
    let doc = session.app.editor.documents.get_mut(&buffer).unwrap();
    let transaction = compare_ropes(doc.text(), &Rope::from("z\na\nb\n"));
    doc.apply(&transaction, origin);
    session.keys("zz").await?;
    session
        .until("the diff anew", |app| {
            *doc!(app.editor, &new_doc).text() == "z\na\nb\n"
        })
        .await;
    let pane = doc!(session.app.editor, &new_doc)
        .diff_view
        .as_ref()
        .unwrap();
    assert_eq!(pane.alignment.hunks(), [0..1]);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn buffers_without_a_committed_version_have_no_diff() -> anyhow::Result<()> {
    let (dir, _) = repository("a\n")?;
    let untracked = helix_stdx::path::canonicalize(dir.path().join("new.rs"));
    fs::write(&untracked, "b\n")?;
    let mut session = session(&untracked, DiffTool::Builtin)?;
    session.keys(":diff<ret>").await?;
    assert!(panes(&session.app).is_none());
    let error = status(&session.app);
    assert!(
        error.ends_with("new.rs has no committed version"),
        "{error}"
    );
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn difftastic_lines_up_the_diff_when_installed() -> anyhow::Result<()> {
    if helix_stdx::env::which("difft").is_err() {
        return Ok(());
    }
    let (_dir, path) = repository("fn main() {\n    let x = Self { a, b, c };\n}\n")?;
    let mut session = session(&path, DiffTool::Difftastic)?;
    set_text(
        &mut session.app,
        "fn main() {\n    let x = Self {\n        a,\n        b,\n    };\n}\n",
    );
    session.keys(":diff<ret>").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let [(_, old_doc), _] = panes(&session.app).unwrap();
    let pane = doc!(session.app.editor, &old_doc)
        .diff_view
        .as_ref()
        .unwrap();
    // difftastic sees that only `c` went.
    assert_eq!(
        pane.alignment.change(Side::Old, 1),
        Some(&LineChange::Parts(vec![25..26]))
    );
    assert_eq!(status(&session.app), "");
    session.quit().await
}

/// The primary selection of the focused buffer, as anchor and head.
fn selection(app: &Application) -> (usize, usize) {
    let view = app.editor.tree.focus;
    let range = doc!(app.editor).selection(view).primary();
    (range.anchor, range.head)
}

#[tokio::test(flavor = "multi_thread")]
async fn hunks_are_jumped_to_and_the_file_opened() -> anyhow::Result<()> {
    let (_dir, path) = repository("a\nb\nc\nd\ne\nf\ng\n")?;
    let mut session = session(&path, DiffTool::Builtin)?;
    // Hunks on rows 1, 4 (a filler on the old side) and 7.
    set_text(&mut session.app, "a\nB\nc\nd\nnew\ne\nf\nG\n");
    let (buffer, origin) = (doc!(session.app.editor).id(), session.app.editor.tree.focus);
    session.keys(":diff<ret>").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    assert_eq!(
        selection(&session.app),
        (2, 3),
        "the cursor starts on the first hunk"
    );
    session.keys("]g").await?;
    assert_eq!(selection(&session.app), (8, 12));
    session.keys("]g").await?;
    assert_eq!(selection(&session.app), (16, 18));
    session.keys("[G").await?;
    assert_eq!(selection(&session.app), (2, 4));
    session.keys("]G").await?;
    assert_eq!(selection(&session.app), (16, 18));
    session.keys("[g").await?;
    assert_eq!(selection(&session.app), (12, 8));

    // From the old side the file opens at the line on the cursor's row, in the diff's place.
    session.keys("<C-w>h").await?;
    assert_eq!(selection(&session.app), (6, 7), "the old side's `d`");
    session.keys("gf").await?;
    let app = &session.app;
    assert!(panes(app).is_none());
    assert_eq!(app.editor.tree.focus, origin);
    assert_eq!(doc!(app.editor).id(), buffer);
    assert_eq!(selection(app), (6, 7));

    // So does Enter.
    session.keys(":diff<ret>").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    session.keys("j<ret>").await?;
    let app = &session.app;
    assert!(panes(app).is_none());
    assert_eq!(doc!(app.editor).id(), buffer);
    assert_eq!(selection(app), (4, 5));
    session.quit().await
}

/// A session started like `hx --diff` with `paths`, diffing with the builtin diff.
fn diff_session(paths: &[&Path]) -> anyhow::Result<Session> {
    let mut config = test_config();
    config.editor.diff.tool = DiffTool::Builtin;
    let mut builder = AppBuilder::new().with_config(config).with_diff();
    for path in paths {
        builder = builder.with_file(*path, None);
    }
    Ok(Session::new(builder.build()?))
}

#[tokio::test(flavor = "multi_thread")]
async fn two_files_are_diffed_from_the_command_line() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (old, new) = (dir.path().join("old.rs"), dir.path().join("new.rs"));
    fs::write(&old, "a\nb\n")?;
    fs::write(&new, "a\nc\n")?;
    let mut session = diff_session(&[&old, &new])?;
    session.keys("").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let app = &session.app;
    // Only the panes are left: closing them quits, as git's diff tools do.
    assert_eq!(app.editor.tree.views().count(), 2);
    assert_eq!(app.editor.documents().count(), 2);
    let rows: Vec<_> = rows(app).into_iter().take(2).collect();
    assert_eq!(rows[1], ("2 b".to_owned(), "2 c".to_owned()));
    let [(_, old_doc), (_, new_doc)] = panes(app).unwrap();
    assert!(doc!(app.editor, &old_doc)
        .display_name()
        .ends_with("old.rs"));
    let pane = doc!(app.editor, &new_doc).diff_view.as_ref().unwrap();
    assert_eq!(pane.file.as_deref(), Some(new.as_path()));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn added_files_are_diffed_against_nothing() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let new = dir.path().join("new.rs");
    fs::write(&new, "a\nb\n")?;
    let mut session = diff_session(&[Path::new("/dev/null"), &new])?;
    session.keys("").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let rows: Vec<_> = rows(&session.app).into_iter().take(2).collect();
    assert_eq!(
        rows,
        [
            ("".to_owned(), "1 a".to_owned()),
            ("".to_owned(), "2 b".to_owned())
        ]
    );
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn one_file_is_diffed_against_its_committed_version() -> anyhow::Result<()> {
    let (_dir, path) = repository("a\nb\n")?;
    fs::write(&path, "a\nc\n")?;
    let mut session = diff_session(&[&path])?;
    session.keys("").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let [(_, old_doc), _] = panes(&session.app).unwrap();
    assert!(doc!(session.app.editor, &old_doc)
        .display_name()
        .ends_with("file.rs (HEAD)"));
    // Closing the diff leaves the file open.
    session.keys(":q<ret>").await?;
    let app = &session.app;
    assert!(panes(app).is_none());
    assert_eq!(doc!(app.editor).path(), Some(path.as_path()));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn diffs_take_one_file_or_two() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    fs::write(&a, "a\n")?;
    fs::write(&b, "b\n")?;
    let error = |paths: &[&Path]| diff_session(paths).err().map(|err| err.to_string());
    let expected = Some("--diff takes one file or two".to_owned());
    assert_eq!(error(&[&a, &b, Path::new("/dev/null")]), expected);
    assert_eq!(error(&[dir.path()]), expected);
    Ok(())
}
