use std::{io::Write, ops::Range};

use helix_core::{
    diagnostic::{DiagnosticProvider, Severity},
    syntax::config::SoftWrap,
    Diagnostic, Rope,
};
use helix_term::application::Application;
use helix_view::{doc, doc_mut, editor::ScrollbarConfig, graphics::Color, view};
use tempfile::NamedTempFile;

use super::*;

/// The rows of a view's text on the 120x150 test screen: all but the command line and the
/// statusline.
const TEXT_ROWS: u16 = 148;

fn scrollbar_config(enable: bool) -> Config {
    let mut config = test_config();
    config.editor.scrollbar = ScrollbarConfig {
        enable,
        ..Default::default()
    };
    config
}

/// A file named like `*.rs` or so, holding `text`.
fn text_file(suffix: &str, text: &str) -> anyhow::Result<NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    file.write_all(text.as_bytes())?;
    Ok(file)
}

fn lines(count: usize) -> String {
    (0..count).map(|i| format!("line {i}\n")).collect()
}

fn open(file: &NamedTempFile, config: Config) -> anyhow::Result<Session> {
    let app = AppBuilder::new()
        .with_file(file.path(), None)
        .with_config(config)
        .build()?;
    Ok(Session::new(app))
}

/// The symbols drawn in the screen column `x`, from the top.
fn column(app: &Application, x: u16) -> Vec<&str> {
    let screen = app.screen();
    (screen.area.top()..screen.area.bottom())
        .map(|y| screen[(x, y)].symbol.as_str())
        .collect()
}

/// The rows of the column `x` drawn as `symbol`, which must be one run.
fn rows_of(app: &Application, x: u16, symbol: &str) -> Range<u16> {
    let column = column(app, x);
    let start = column.iter().position(|&s| s == symbol).unwrap_or(0);
    let len = column[start..].iter().take_while(|&&s| s == symbol).count();
    assert!(
        !column[start + len..].contains(&symbol),
        "{symbol} is drawn in more than one run: {column:?}"
    );
    start as u16..(start + len) as u16
}

#[tokio::test(flavor = "multi_thread")]
async fn splits_carry_their_thumbs_on_their_rails() -> anyhow::Result<()> {
    let file = text_file(".txt", &lines(600))?;
    let mut session = open(&file, scrollbar_config(true))?;
    session.keys("<C-w>v").await?;
    let app = &session.app;
    let mut areas: Vec<_> = app.editor.tree.views().map(|(view, _)| view.area).collect();
    areas.sort_by_key(|area| area.x);
    let (left, right) = (areas[0], areas[1]);
    // The right view leaves the last column to its rail.
    assert_eq!(right.right(), 119);
    // 148 of 601 lines show: a thumb of 37 rows at the top, on both rails.
    for rail in [left.right(), right.right()] {
        assert_eq!(rows_of(app, rail, "▌"), 0..37);
    }
    // The separator goes through the statusline row, the statusline runs under the edge rail.
    assert_eq!(rows_of(app, left.right(), "│"), 37..TEXT_ROWS + 1);
    assert_eq!(rows_of(app, right.right(), "│"), 37..TEXT_ROWS);

    // At the end of the file, the thumb ends at the bottom of the rail.
    session.keys("ge").await?;
    assert_eq!(rows_of(&session.app, 119, "▌").end, TEXT_ROWS);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn rails_have_no_thumb_while_everything_shows() -> anyhow::Result<()> {
    let file = text_file(".txt", &lines(10))?;
    let mut session = open(&file, scrollbar_config(true))?;
    session.keys("").await?;
    let rail = column(&session.app, 119);
    assert!(
        rail[..TEXT_ROWS as usize].iter().all(|&s| s == "│"),
        "{rail:?}"
    );
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn without_scrollbars_views_take_the_whole_width() -> anyhow::Result<()> {
    let file = text_file(".txt", &lines(600))?;
    let mut session = open(&file, scrollbar_config(false))?;
    assert_eq!(view!(session.app.editor).area.width, 120);
    session.keys("<C-w>v").await?;
    let left = session.app.editor.tree.views().map(|(view, _)| view.area);
    let left = left.min_by_key(|area| area.x).unwrap();
    let separator = column(&session.app, left.right());
    assert!(separator[..149].iter().all(|&s| s == "│"), "{separator:?}");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn thumbs_measure_the_lines_on_screen() -> anyhow::Result<()> {
    // Wrapped lines: each of them takes three rows, so a third as many lines show.
    let mut config = scrollbar_config(true);
    config.editor.soft_wrap = SoftWrap {
        enable: Some(true),
        ..Default::default()
    };
    let long_lines: String = (0..600)
        .map(|_| format!("{}\n", "word ".repeat(50)))
        .collect();
    let file = text_file(".txt", &long_lines)?;
    let mut session = open(&file, config)?;
    session.keys("").await?;
    assert_eq!(rows_of(&session.app, 119, "▌"), 0..13);
    session.quit().await?;

    // A closed fold shows its lines in one row.
    let mut source = String::from("fn f() {\n");
    source.extend((0..300).map(|i| format!("    let x{i} = {i};\n")));
    source.push_str("}\n");
    source.extend((0..300).map(|i| format!("// {i}\n")));
    let file = text_file(".rs", &source)?;
    let mut session = open(&file, scrollbar_config(true))?;
    session.keys("zf").await?;
    // Lines 0..449 of 603 show.
    assert_eq!(rows_of(&session.app, 119, "▌"), 0..111);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn panels_on_the_right_share_their_rail() -> anyhow::Result<()> {
    let file = text_file(".txt", &lines(600))?;
    let mut session = open(&file, scrollbar_config(true))?;
    session.keys("<space>U").await?;
    let app = &session.app;
    // The undo tree's rail is the view's: there's no rail of the views' own.
    let rail = view!(app.editor).area.right();
    assert!(rail < 119);
    assert_eq!(rows_of(app, rail, "▌"), 0..37);
    assert_eq!(rows_of(app, rail, "│"), 37..TEXT_ROWS);
    session.quit().await
}

/// The foreground of the cell at column `x`, row `y`.
fn fg(app: &Application, x: u16, y: u16) -> Color {
    app.screen()[(x, y)].fg
}

/// The rows of the rail in column `x` that are marked in `color`, on the track or the thumb.
fn marked_rows(app: &Application, x: u16, color: Color) -> Vec<u16> {
    (0..TEXT_ROWS)
        .filter(|&y| ["┃", "▌"].contains(&app.screen()[(x, y)].symbol.as_str()))
        .filter(|&y| fg(app, x, y) == color)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn searches_are_marked_while_searching() -> anyhow::Result<()> {
    let text: String = (0..600)
        .map(|i| match i {
            100 | 500 => "needle\n".to_owned(),
            _ => format!("line {i}\n"),
        })
        .collect();
    let file = text_file(".txt", &text)?;
    let mut session = open(&file, scrollbar_config(true))?;
    let special = session.app.editor.theme.get("special").fg.unwrap();
    // Lines 100 and 500 are on rows 24 (under the thumb) and 123.
    session.keys("/needle").await?;
    assert_eq!(marked_rows(&session.app, 119, special), [24, 123]);
    assert_eq!(session.app.screen()[(119, 123)].symbol.as_str(), "┃");
    session.keys("<ret>n").await?;
    assert_eq!(marked_rows(&session.app, 119, special), [24, 123]);
    // Any other command ends the search.
    session.keys("j").await?;
    assert_eq!(marked_rows(&session.app, 119, special), [0u16; 0]);
    session.keys("n").await?;
    assert_eq!(marked_rows(&session.app, 119, special), [24, 123]);
    session.keys("j/needle<esc>").await?;
    assert_eq!(marked_rows(&session.app, 119, special), [0u16; 0]);
    // `*` searches for the selection.
    session.keys("ggjx*").await?;
    assert_eq!(marked_rows(&session.app, 119, special).len(), 1);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_and_changes_are_marked() -> anyhow::Result<()> {
    let file = text_file(".txt", &lines(600))?;
    let mut session = open(&file, scrollbar_config(true))?;
    let theme = &session.app.editor.theme;
    let color = |scope| theme.get(scope).fg.unwrap();
    let (hint, error, modified) = (color("hint"), color("error"), color("diff.delta.gutter"));
    let doc = doc_mut!(session.app.editor);
    let diagnostic = |line: usize, severity| {
        let start = doc.text().line_to_char(line);
        Diagnostic {
            range: helix_core::diagnostic::Range {
                start,
                end: start + 4,
            },
            line,
            message: String::new(),
            severity: Some(severity),
            code: None,
            provider: DiagnosticProvider::Compilation,
            tags: Vec::new(),
            source: None,
            data: None,
            starts_at_word: false,
            ends_at_word: false,
            zero_width: false,
        }
    };
    let diagnostics = [
        diagnostic(300, Severity::Hint),
        diagnostic(400, Severity::Error),
    ];
    doc.replace_diagnostics(diagnostics, &[], &DiagnosticProvider::Compilation);
    // The text differs from its base on line 200.
    let base = lines(600).replace("line 200\n", "changed\n");
    doc.set_diff_override(Some(Rope::from(base)));
    session
        .until("the diff", |app| {
            doc!(app.editor)
                .diff_handle()
                .is_some_and(|handle| !handle.load().is_empty())
        })
        .await;
    // A key draws a frame with the diff.
    session.keys("<esc>").await?;
    // Lines 200, 300 and 400 are on rows 49, 73 and 98.
    let app = &session.app;
    assert_eq!(marked_rows(app, 119, modified), [49]);
    assert_eq!(marked_rows(app, 119, hint), [73]);
    assert_eq!(marked_rows(app, 119, error), [98]);

    // Each `:set` applies to the config the one before made.
    session
        .keys(":set scrollbar.diagnostics warning<ret>")
        .await?;
    session.keys(":set scrollbar.diff false<ret>").await?;
    let app = &session.app;
    assert_eq!(marked_rows(app, 119, modified), [0u16; 0]);
    assert_eq!(marked_rows(app, 119, hint), [0u16; 0]);
    assert_eq!(marked_rows(app, 119, error), [98]);
    session.quit().await
}
