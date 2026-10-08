#![allow(clippy::single_range_in_vec_init)]

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
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

/// `lines` lines like `line 3`, the one at `changed` being `with`.
fn numbered(lines: usize, changed: usize, with: &str) -> String {
    (0..lines)
        .map(|line| {
            if line == changed {
                format!("{with}\n")
            } else {
                format!("line {line}\n")
            }
        })
        .collect()
}

/// Asserts that the rows both panes show an unchanged line on show the same one.
fn assert_lined_up(app: &Application) {
    let rows = rows(app);
    let shared = rows
        .iter()
        .filter(|(old, new)| old.contains(" line ") && new.contains(" line "))
        .inspect(|(old, new)| assert_eq!(old, new, "{rows:#?}"))
        .count();
    assert!(shared > 10, "{rows:#?}");
}

/// The background of the last column of row `y` of the text of `view`.
fn background(app: &Application, view: ViewId, y: usize) -> helix_view::graphics::Color {
    let view = app.editor.tree.get(view);
    let area = view.inner_area(doc!(app.editor, &view.doc));
    let screen = app.screen();
    let x = area.right() as usize - 1;
    screen.content[(area.y as usize + y) * screen.area.width as usize + x].bg
}

#[tokio::test(flavor = "multi_thread")]
async fn wrapped_lines_line_up() -> anyhow::Result<()> {
    let long = "the quick brown fox jumps over the lazy dog ".repeat(4);
    let (_dir, path) = repository(&numbered(400, 5, long.trim_end()))?;
    let mut session = session(&path, DiffTool::Builtin)?;
    set_text(&mut session.app, &numbered(400, 5, "short"));
    session
        .keys(":set soft-wrap.enable true<ret>:diff<ret>")
        .await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    assert_lined_up(&session.app);

    // The long line wraps; the short one is padded with rows in its color.
    let shown = rows(&session.app);
    let short = shown.iter().position(|(_, new)| new == "6 short").unwrap();
    assert!(shown[short].0.starts_with("6 the quick"), "{shown:#?}");
    let (wrapped, padding) = &shown[short + 1];
    assert!(!wrapped.is_empty() && padding.is_empty(), "{shown:#?}");
    assert_eq!(
        shown[short + 4],
        ("7 line 6".to_owned(), "7 line 6".to_owned()),
        "{shown:#?}"
    );
    let [(old_view, _), (new_view, _)] = panes(&session.app).unwrap();
    let app = &session.app;
    for view in [old_view, new_view] {
        let color = background(app, view, short);
        assert_ne!(
            color,
            background(app, view, short + 4),
            "unlike an unchanged line"
        );
        for row in short + 1..short + 4 {
            assert_eq!(background(app, view, row), color, "row {row}");
        }
    }
    let padding_color = background(app, new_view, short);

    // Scrolling keeps the rows lined up, through the wrapped line too, whose padding stays in
    // its color at the top of the view.
    let mut padded_tops = 0;
    for _ in 0..8 {
        session.keys("zj").await?;
        assert_lined_up(&session.app);
        if rows(&session.app)[0].1.is_empty() {
            assert_eq!(background(&session.app, new_view, 0), padding_color);
            padded_tops += 1;
        }
    }
    assert!(padded_tops > 0);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn fillers_are_drawn_with_the_filler_character() -> anyhow::Result<()> {
    let (_dir, path) = repository("a\nb\nc\n")?;
    let mut config = test_config();
    config.editor.diff.tool = DiffTool::Builtin;
    config.editor.diff.filler_character = Some('╱');
    let app = AppBuilder::new()
        .with_file(&path, None)
        .with_config(config)
        .build()?;
    let mut session = Session::new(app);
    set_text(&mut session.app, "a\nb\nnew\n\nc\n");
    session.keys(":diff<ret>").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let app = &session.app;
    let shown = rows(app);
    assert_eq!(shown[2].1, "3 new");
    assert_eq!(shown[3].1, "4", "a blank line added");
    for (old, _) in &shown[2..4] {
        assert!(
            old.chars().all(|c| c == '╱') && old.len() > 10,
            "{shown:#?}"
        );
    }
    assert_eq!(shown[4], ("3 c".to_owned(), "5 c".to_owned()));

    // In the indent guides' color, over the fillers' gray.
    let [(old_view, _), (new_view, _)] = panes(app).unwrap();
    let view = app.editor.tree.get(old_view);
    let area = view.inner_area(doc!(app.editor, &view.doc));
    let screen = app.screen();
    let cell =
        &screen.content[(area.y as usize + 2) * screen.area.width as usize + area.x as usize];
    assert_eq!(cell.symbol.as_str(), "╱");
    assert_eq!(
        Some(cell.fg),
        app.editor.theme.get("ui.virtual.indent-guide").fg
    );
    assert_ne!(
        cell.bg,
        background(app, old_view, 4),
        "unlike an unchanged line"
    );
    // The blank line added has the color of the line added before it.
    assert_eq!(background(app, new_view, 3), background(app, new_view, 2));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn wrapped_panes_line_up_as_the_diff_tree_comes_and_goes() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (old, new) = (dir.path().join("old"), dir.path().join("new"));
    let long = "the quick brown fox jumps over the lazy dog ".repeat(4);
    for (root, line) in [(&old, long.trim_end()), (&new, "short")] {
        fs::create_dir_all(root)?;
        fs::write(root.join("file.rs"), numbered(400, 5, line))?;
    }
    let mut config = test_config();
    config.editor.diff.tool = DiffTool::Builtin;
    config.editor.soft_wrap.enable = Some(true);
    let app = AppBuilder::new()
        .with_config(config)
        .with_diff()
        .with_file(&old, None)
        .with_file(&new, None)
        .build()?;
    let mut session = Session::new(app);
    session.keys("").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    assert_lined_up(&session.app);
    let [_, (new_view, _)] = panes(&session.app).unwrap();
    let width = |app: &Application| app.editor.tree.get(new_view).area.width;

    // Hiding the tree widens the panes, and showing it narrows them again: their lines wrap
    // anew, lined up from the first frame on.
    let narrow = width(&session.app);
    session.keys("<space>E").await?;
    assert!(width(&session.app) > narrow);
    assert_lined_up(&session.app);
    session.keys("<space>E").await?;
    assert_eq!(width(&session.app), narrow);
    assert_lined_up(&session.app);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_pane_buffer_closes_the_diff() -> anyhow::Result<()> {
    let (_dir, path) = repository("a\n")?;
    let mut session = session(&path, DiffTool::Builtin)?;
    set_text(&mut session.app, "b\n");
    let (buffer, origin) = (doc!(session.app.editor).id(), session.app.editor.tree.focus);
    session.keys(":diff<ret>").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    session.keys(":buffer-close<ret>").await?;
    let app = &session.app;
    assert!(panes(app).is_none());
    assert_eq!(app.editor.documents().count(), 1, "both panes closed");
    assert_eq!(app.editor.tree.focus, origin);
    assert_eq!(doc!(app.editor).id(), buffer);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_pane_buffer_of_the_command_line_diff_leaves_a_scratch() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (old, new) = (dir.path().join("old.rs"), dir.path().join("new.rs"));
    fs::write(&old, "a\n")?;
    fs::write(&new, "b\n")?;
    let mut session = diff_session(&[&old, &new])?;
    session.keys("").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    session.keys(":buffer-close<ret>").await?;
    let app = &session.app;
    assert!(panes(app).is_none());
    assert_eq!(app.editor.tree.views().count(), 1);
    assert_eq!(doc!(app.editor).display_name(), "[scratch]");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_third_path_names_the_files() -> anyhow::Result<()> {
    // As git's difftool gives them: temporary copies, and `$MERGED`.
    let dir = tempfile::tempdir()?;
    let (old, new) = (dir.path().join("blob1"), dir.path().join("blob2"));
    let path = dir.path().join("lib.rs");
    fs::write(&old, "fn a() {}\n")?;
    fs::write(&new, "fn b() {}\n")?;
    fs::write(&path, "fn c() {}\n")?;
    let mut session = diff_session(&[&old, &new, &path])?;
    session.keys("").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let app = &session.app;
    let [(_, old_doc), (_, new_doc)] = panes(app).unwrap();
    let (old_doc, new_doc) = (doc!(app.editor, &old_doc), doc!(app.editor, &new_doc));
    assert!(old_doc.display_name().ends_with("lib.rs (old)"));
    assert!(new_doc.display_name().ends_with("lib.rs (new)"));
    assert_eq!(new_doc.language_name(), Some("rust"), "named by the path");
    let pane = new_doc.diff_view.as_ref().unwrap();
    assert_eq!(pane.file.as_deref(), Some(path.as_path()), "gf opens it");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_third_path_names_nothing() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (old, new) = (dir.path().join("old.rs"), dir.path().join("new.rs"));
    fs::write(&old, "a\n")?;
    fs::write(&new, "b\n")?;
    let mut session = diff_session(&[&old, &new, Path::new("")])?;
    session.keys("").await?;
    session.until("the panes", |app| panes(app).is_some()).await;
    let [_, (_, new_doc)] = panes(&session.app).unwrap();
    assert!(doc!(session.app.editor, &new_doc)
        .display_name()
        .ends_with("new.rs"));
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
async fn diffs_take_one_path_or_two_alike() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    fs::write(&a, "a\n")?;
    fs::write(&b, "b\n")?;
    let error = |paths: &[&Path]| diff_session(paths).err().map(|err| err.to_string());
    let expected =
        Some("--diff takes one or two files (and a path naming two), or directories".to_owned());
    assert_eq!(error(&[&a, &b, &a, &b]), expected, "four files");
    assert_eq!(error(&[&a, dir.path()]), expected, "a file and a directory");
    Ok(())
}

/// Two directories: `old` and `new`, with `sub/b.rs` and `a.rs` changed and `c.rs` added.
fn directories() -> anyhow::Result<(tempfile::TempDir, PathBuf, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let (old, new) = (dir.path().join("old"), dir.path().join("new"));
    for (root, files) in [
        (
            &old,
            [("sub/b.rs", "b\n"), ("a.rs", "a\nx\n"), ("same.rs", "s\n")].as_slice(),
        ),
        (
            &new,
            [
                ("sub/b.rs", "B\n"),
                ("a.rs", "a\ny\n"),
                ("same.rs", "s\n"),
                ("c.rs", "c\n"),
            ]
            .as_slice(),
        ),
    ] {
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, text)?;
        }
    }
    Ok((dir, old, new))
}

/// The name of the focused buffer.
fn name(app: &Application) -> String {
    doc!(app.editor).display_name().into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn hunk_jumps_go_through_the_files_of_a_diff_of_many() -> anyhow::Result<()> {
    let (_dir, old, new) = directories()?;
    let mut session = diff_session(&[&old, &new])?;
    session.keys("").await?;
    session
        .until("the first file", |app| panes(app).is_some())
        .await;
    assert_eq!(name(&session.app), "sub/b.rs (new)", "directories first");
    session.keys("]g").await?;
    session
        .until("the next file", |app| name(app) == "a.rs (new)")
        .await;
    assert_eq!(selection(&session.app), (2, 3), "at its first hunk");
    session.keys("[g").await?;
    session
        .until("the previous file", |app| name(app) == "sub/b.rs (new)")
        .await;
    session.quit().await
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

/// Whether a row of the screen holds `text`.
fn shows(app: &Application, text: &str) -> bool {
    screen(app).iter().any(|row| row.contains(text))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_diff_tree_lists_the_files_with_their_lines() -> anyhow::Result<()> {
    let (_dir, old, new) = directories()?;
    let mut session = diff_session(&[&old, &new])?;
    session.keys("").await?;
    session
        .until("the stats and the first diff", |app| {
            shows(app, "a.rs +1 -1") && panes(app).is_some()
        })
        .await;
    let app = &session.app;
    assert!(
        shows(app, "old → new +3 -2"),
        "the root is named after the directories and sums the files up"
    );
    assert!(shows(app, "b.rs +1 -1"));
    assert!(shows(app, "c.rs +1"));
    assert!(!shows(app, "same.rs"));

    // `space e` focuses the tree on the file shown; Enter shows another one's diff.
    session.keys("<space>e").await?;
    session.keys("j<ret>").await?;
    session
        .until("the diff of a.rs", |app| name(app) == "a.rs (new)")
        .await;

    // `gf` opens the file, keeping the tree.
    session.keys("gf").await?;
    let app = &session.app;
    assert!(panes(app).is_none());
    assert!(doc!(app.editor)
        .path()
        .is_some_and(|path| path.ends_with("new/a.rs")));
    assert!(shows(app, "a.rs +1 -1"));

    // Enter in the tree shows the diff again; closing it ends the diff of many.
    session.keys("<space>e<ret>").await?;
    session
        .until("the diff again", |app| panes(app).is_some())
        .await;
    session.keys(":q<ret>").await?;
    let app = &session.app;
    assert!(panes(app).is_none());
    assert!(!shows(app, "a.rs +1 -1"), "the tree is gone");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_diff_tree_shows_the_file_opened_again() -> anyhow::Result<()> {
    let (_dir, old, new) = directories()?;
    let mut session = diff_session(&[&old, &new])?;
    session.keys("").await?;
    session
        .until("the first file", |app| panes(app).is_some())
        .await;
    session.keys("gf").await?;
    assert!(panes(&session.app).is_none());
    session.keys("<space>e<ret>").await?;
    session
        .until("its diff again", |app| name(app) == "sub/b.rs (new)")
        .await;
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_since_head_are_diffed_together() -> anyhow::Result<()> {
    let (dir, path) = repository("a\nb\n")?;
    fs::write(&path, "a\nc\n")?;
    fs::write(dir.path().join("new.rs"), "n\n")?;
    let mut session = session(&path, DiffTool::Builtin)?;
    let origin = session.app.editor.tree.focus;
    let dir = helix_stdx::path::canonicalize(dir.path());
    session
        .keys(&format!(":diff-changes {}<ret>", dir.display()))
        .await?;
    session
        .until("the stats", |app| shows(app, "new.rs +1"))
        .await;
    assert_eq!(name(&session.app), "file.rs");
    assert!(shows(&session.app, "file.rs +1 -1"));
    session.keys(":q<ret>").await?;
    let app = &session.app;
    assert_eq!(app.editor.tree.focus, origin, "the layout comes back");
    assert!(!shows(app, "new.rs +1"));
    session.quit().await
}

/// The names of the panes shown, old and new.
fn pane_names(app: &Application) -> [String; 2] {
    let panes = panes(app).expect("the panes are shown");
    panes.map(|(_, doc)| {
        let doc = app.editor.document(doc).unwrap();
        doc.diff_view.as_ref().unwrap().name.clone()
    })
}

/// Nine lines and `last`.
fn ten_lines(last: &str) -> String {
    format!("{}{last}\n", "line\n".repeat(9))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_diff_tree_lists_a_renamed_file_once() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (old, new) = (dir.path().join("old"), dir.path().join("new"));
    for (path, text) in [
        (old.join("src/r.rs"), ten_lines("old")),
        (new.join("src/sub/r.rs"), ten_lines("new")),
    ] {
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, text)?;
    }
    let mut session = diff_session(&[&old, &new])?;
    session.keys("").await?;
    session
        .until("the stats and the diff", |app| {
            shows(app, "{ => sub}/r.rs +1 -1") && panes(app).is_some()
        })
        .await;
    let app = &session.app;
    assert!(!shows(app, "r.rs -10") && !shows(app, "r.rs +10"));
    assert_eq!(
        pane_names(app),
        ["src/r.rs (old)", "src/sub/r.rs (new)"].map(String::from)
    );
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_renamed_and_edited_since_head_is_listed_once() -> anyhow::Result<()> {
    let (dir, path) = repository(&ten_lines("old"))?;
    fs::create_dir(dir.path().join("sub"))?;
    git(dir.path(), &["mv", "file.rs", "sub/file.rs"]);
    // Staged as a rename, then edited: git reports it twice.
    fs::write(dir.path().join("sub/file.rs"), ten_lines("new"))?;
    let mut session = session(&path, DiffTool::Builtin)?;
    let dir = helix_stdx::path::canonicalize(dir.path());
    session
        .keys(&format!(":diff-changes {}<ret>", dir.display()))
        .await?;
    session
        .until("the stats", |app| {
            shows(app, "file.rs => sub/file.rs +1 -1")
        })
        .await;
    assert_eq!(
        pane_names(&session.app),
        ["file.rs (HEAD)", "sub/file.rs"].map(String::from)
    );
    session.quit().await
}

/// Runs the event loop until `done` holds, in steps of a millisecond. Returns how long it took,
/// and the longest step: how long the editor was busy at most, not taking keys.
async fn time_until(
    session: &mut Session,
    what: &str,
    done: impl Fn(&Application) -> bool,
) -> (Duration, Duration) {
    let (start, mut longest) = (Instant::now(), Duration::ZERO);
    while !done(&session.app) {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "timed out waiting for {what}"
        );
        let step = Instant::now();
        session.run_for(Duration::from_millis(1)).await;
        longest = longest.max(step.elapsed());
    }
    (start.elapsed(), longest)
}

/// `lines` lines of Rust: this crate's sources one after the other, over again if need be.
fn rust_source(lines: usize) -> String {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let texts: Vec<String> = files
        .iter()
        .map(|file| fs::read_to_string(file).unwrap())
        .collect();
    texts
        .iter()
        .flat_map(|text| text.lines())
        .cycle()
        .take(lines)
        .map(|line| format!("{line}\n"))
        .collect()
}

/// `text` with lines changed, removed and inserted here and there.
fn edited(text: &str) -> String {
    text.lines()
        .enumerate()
        .filter(|(index, _)| index % 251 != 7)
        .map(|(index, line)| match index {
            _ if index % 97 == 3 => format!("{line} // edited\n"),
            _ if index % 331 == 5 => format!("let inserted = {index};\n{line}\n"),
            _ => format!("{line}\n"),
        })
        .collect()
}

/// Times opening `:diff` on big buffers, the cost a frame of the diff adds, and a diff of many
/// files. Run it with `cargo test --release --features integration --test integration
/// measure_diff_view -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a check"]
async fn measure_diff_view() -> anyhow::Result<()> {
    use helix_core::Syntax;
    use helix_view::diff_view::builtin;

    const SIZES: [usize; 2] = [10_000, 50_000];
    const FRAMES: u32 = 1000;
    const FILES: usize = 2000;

    // The big files, committed, and many small files, committed and changed.
    let dir = tempfile::tempdir()?;
    let big = |lines: usize| dir.path().join(format!("big{lines}.rs"));
    let small = |index: usize| {
        dir.path()
            .join(format!("many/dir{}/file{index}.rs", index / 50))
    };
    let text = |index: usize, changed: bool| -> String {
        (0..20)
            .map(|line| {
                if line == 10 && changed {
                    format!("let changed = {index};\n")
                } else {
                    format!("let line{line} = {index};\n")
                }
            })
            .collect()
    };
    for lines in SIZES {
        fs::write(big(lines), rust_source(lines))?;
    }
    let fillers = helix_stdx::path::canonicalize(dir.path()).join("fillers.rs");
    let committed = rust_source(200);
    fs::write(&fillers, &committed)?;
    for index in 0..FILES {
        fs::create_dir_all(small(index).parent().unwrap())?;
        fs::write(small(index), text(index, false))?;
    }
    git(dir.path(), &["init"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "files"]);
    for index in 0..FILES {
        fs::write(small(index), text(index, true))?;
    }

    let mut session = session(&big(SIZES[0]), DiffTool::Builtin)?;
    for lines in SIZES {
        let path = helix_stdx::path::canonicalize(big(lines));
        session
            .keys(&format!(":open {}<ret>", path.display()))
            .await?;
        let committed = fs::read_to_string(&path)?;
        let buffer = edited(&committed);
        set_text(&mut session.app, &buffer);

        let (old, new) = (Rope::from(committed), Rope::from(buffer));
        let start = Instant::now();
        let alignment = builtin::align(old.slice(..), new.slice(..));
        let align = start.elapsed();
        let loader = session.app.editor.syn_loader.load();
        let rust = loader.language_for_name("rust".to_owned()).unwrap();
        let start = Instant::now();
        let parsed = Syntax::new(old.slice(..), rust, &loader).is_ok();
        let parse = start.elapsed();
        drop(loader);

        eprintln!(
            "{lines} lines, {} hunks: builtin diff {align:?}, a parse {parse:?}{}",
            alignment.hunks().len(),
            if parsed { "" } else { " (timed out)" },
        );
        for wrap in [false, true] {
            let label = if wrap { "wrapped" } else { "unwrapped" };
            session
                .keys(&format!(":set soft-wrap.enable {wrap}<ret>"))
                .await?;
            // Typed beforehand: each key draws the big buffer anew.
            session.keys(":diff").await?;
            session.send("<ret>")?;
            let (open, busy) =
                time_until(&mut session, "the panes", |app| panes(app).is_some()).await;
            let [(_, old_doc), _] = panes(&session.app).unwrap();
            assert_eq!(
                doc!(session.app.editor, &old_doc).syntax().is_some(),
                parsed,
                "parsed like the buffer"
            );

            // Scrolling a row a frame, the diff against the buffer beside itself.
            let scroll = "zj".repeat(FRAMES as usize);
            let start = Instant::now();
            session.keys(&scroll).await?;
            let diff = start.elapsed();
            session.keys(":q<ret>:vsplit<ret>").await?;
            let start = Instant::now();
            session.keys(&scroll).await?;
            let plain = start.elapsed();
            eprintln!(
                "{lines} lines, {label}: :diff shown after {open:?}, the editor busy {busy:?} at \
                 most; a frame scrolling the diff {:?}, a split {:?}",
                diff / FRAMES,
                plain / FRAMES,
            );
            session.keys("<C-w>o").await?;
        }
    }

    // A pane of fillers facing lines added, scrolled through drawn gray and with a character.
    session
        .keys(&format!(":open {}<ret>", fillers.display()))
        .await?;
    let (top, bottom) = committed.split_at(committed.match_indices('\n').nth(99).unwrap().0 + 1);
    let added: String = (0..4000)
        .map(|i| format!("let added{i} = {i};\n"))
        .collect();
    set_text(&mut session.app, &format!("{top}{added}{bottom}"));
    for filler in ["null", "\"╱\""] {
        session
            .keys(&format!(
                ":set diff.filler-character {filler}<ret>:diff<ret>"
            ))
            .await?;
        session.until("the panes", |app| panes(app).is_some()).await;
        session.keys(&"zj".repeat(100)).await?;
        let start = Instant::now();
        session.keys(&"zj".repeat(FRAMES as usize)).await?;
        let elapsed = start.elapsed();
        assert!(rows(&session.app)
            .iter()
            .all(|(old, _)| !old.contains(" let ")));
        let character = session.app.editor.config().diff.filler_character;
        eprintln!(
            "A pane of fillers, drawn with {character:?}: a frame scrolling {:?}",
            elapsed / FRAMES
        );
        session.keys(":q<ret>").await?;
    }

    let many = helix_stdx::path::canonicalize(dir.path().join("many"));
    session
        .keys(&format!(":diff-changes {}", many.display()))
        .await?;
    session.send("<ret>")?;
    let (shown, busy) =
        time_until(&mut session, "the first file", |app| panes(app).is_some()).await;
    let (counted, counting) = time_until(&mut session, "the stats", |app| {
        shows(app, &format!("+{FILES} -{FILES}"))
    })
    .await;
    eprintln!(
        "{FILES} changed files: the first shown after {shown:?}, busy {busy:?} at most; \
         all counted {counted:?} later, busy {counting:?} at most"
    );
    session.quit().await
}
