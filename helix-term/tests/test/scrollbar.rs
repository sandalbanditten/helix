use std::{io::Write, ops::Range, time::Instant};

use helix_core::{
    diagnostic::{DiagnosticProvider, Severity},
    syntax::config::SoftWrap,
    Diagnostic, Rope,
};
use helix_term::application::Application;
use helix_view::{
    doc, doc_mut,
    editor::ScrollbarConfig,
    graphics::{Color, Rect},
    view,
};
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

/// A diagnostic of `severity` at the start of the line `line` of `text`, as from a compilation.
fn diagnostic(text: &Rope, line: usize, severity: Severity) -> Diagnostic {
    let start = text.line_to_char(line);
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
}

/// Gives the current document diagnostics, each a line and its severity.
fn set_diagnostics(session: &mut Session, diagnostics: &[(usize, Severity)]) {
    let doc = doc_mut!(session.app.editor);
    let diagnostics: Vec<_> = diagnostics
        .iter()
        .map(|&(line, severity)| diagnostic(doc.text(), line, severity))
        .collect();
    doc.replace_diagnostics(diagnostics, &[], &DiagnosticProvider::Compilation);
}

/// Sets the base the current document's text is diffed against to `base`, and waits for the diff.
async fn set_diff_base(session: &mut Session, base: &str) {
    doc_mut!(session.app.editor).set_diff_override(Some(Rope::from(base)));
    session
        .until("the diff", |app| {
            doc!(app.editor)
                .diff_handle()
                .is_some_and(|handle| !handle.load().is_empty())
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_are_marked_but_not_changes() -> anyhow::Result<()> {
    let file = text_file(".txt", &lines(600))?;
    let mut session = open(&file, scrollbar_config(true))?;
    let theme = &session.app.editor.theme;
    let color = |scope| theme.get(scope).fg.unwrap();
    let (hint, error, modified) = (color("hint"), color("error"), color("diff.delta.gutter"));
    set_diagnostics(
        &mut session,
        &[(300, Severity::Hint), (400, Severity::Error)],
    );
    // The text differs from its base on line 200, which only minimaps mark.
    set_diff_base(&mut session, &lines(600).replace("line 200\n", "changed\n")).await;
    // A key draws a frame with the diff.
    session.keys("<esc>").await?;
    // Lines 300 and 400 are on rows 73 and 98.
    let app = &session.app;
    assert_eq!(marked_rows(app, 119, modified), [0u16; 0]);
    assert_eq!(marked_rows(app, 119, hint), [73]);
    assert_eq!(marked_rows(app, 119, error), [98]);

    session
        .keys(":set scrollbar.diagnostics warning<ret>")
        .await?;
    let app = &session.app;
    assert_eq!(marked_rows(app, 119, hint), [0u16; 0]);
    assert_eq!(marked_rows(app, 119, error), [98]);
    session.quit().await
}

#[cfg(not(windows))]
mod mouse {
    use helix_view::current_ref;
    use termina::event::{Event, Modifiers, MouseButton, MouseEvent, MouseEventKind};

    use super::*;

    fn send(session: &Session, kind: MouseEventKind, column: u16, row: u16) -> anyhow::Result<()> {
        session.send_event(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: Modifiers::NONE,
        }))
    }

    /// Drags the mouse from `from` to `to`, both columns and rows, with the left button.
    async fn drag(session: &mut Session, from: (u16, u16), to: (u16, u16)) -> anyhow::Result<()> {
        send(
            session,
            MouseEventKind::Down(MouseButton::Left),
            from.0,
            from.1,
        )?;
        send(session, MouseEventKind::Drag(MouseButton::Left), to.0, to.1)?;
        send(session, MouseEventKind::Up(MouseButton::Left), to.0, to.1)?;
        session.keys("").await
    }

    /// Presses and releases the left button at `column`, `row`.
    async fn click(session: &mut Session, column: u16, row: u16) -> anyhow::Result<()> {
        send(
            session,
            MouseEventKind::Down(MouseButton::Left),
            column,
            row,
        )?;
        send(session, MouseEventKind::Up(MouseButton::Left), column, row)?;
        session.keys("").await
    }

    /// The first document line the view shows, and the line of its cursor.
    fn lines_shown(app: &Application) -> (usize, usize) {
        let (view, doc) = current_ref!(app.editor);
        let text = doc.text().slice(..);
        let cursor = doc.selection(view.id).primary().cursor(text);
        let first = text.char_to_line(doc.view_offset(view.id).anchor);
        (first, text.char_to_line(cursor))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dragging_a_thumb_scrolls_its_split() -> anyhow::Result<()> {
        let file = text_file(".txt", &lines(600))?;
        let mut session = open(&file, scrollbar_config(true))?;
        session.keys("").await?;
        // The thumb, rows 0..37, held by its row 10 and dragged to the bottom.
        drag(&mut session, (119, 10), (119, 147)).await?;
        // The last 148 of 601 lines show, the cursor in view below the scrolloff margin.
        assert_eq!(lines_shown(&session.app), (453, 458));
        assert_eq!(rows_of(&session.app, 119, "▌"), 111..TEXT_ROWS);
        // And back up half way.
        drag(&mut session, (119, 120), (119, 64)).await?;
        let (first, cursor) = lines_shown(&session.app);
        assert!((220..235).contains(&first), "{first}");
        assert!(cursor < first + 148, "{cursor}");
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pressing_a_track_pages_and_the_wheel_scrolls() -> anyhow::Result<()> {
        let file = text_file(".txt", &lines(600))?;
        let mut session = open(&file, scrollbar_config(true))?;
        session.keys("").await?;
        click(&mut session, 119, 100).await?;
        assert_eq!(lines_shown(&session.app), (148, 153));
        click(&mut session, 119, 0).await?;
        assert_eq!(lines_shown(&session.app).0, 0);
        send(&session, MouseEventKind::ScrollDown, 119, 50)?;
        session.keys("").await?;
        assert_eq!(lines_shown(&session.app).0, 3);
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn thumbs_on_a_panels_rail_are_the_splits() -> anyhow::Result<()> {
        let file = text_file(".txt", &lines(600))?;
        let mut session = open(&file, scrollbar_config(true))?;
        session.keys("<space>U").await?;
        let rail = view!(session.app.editor).area.right();
        // A press beside the thumb is the undo tree's.
        click(&mut session, rail, 100).await?;
        assert_eq!(lines_shown(&session.app).0, 0);
        drag(&mut session, (rail, 10), (rail, 147)).await?;
        assert_eq!(lines_shown(&session.app).0, 453);
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn minimaps_jump_and_drag() -> anyhow::Result<()> {
        let file = text_file(".txt", &lines(2000))?;
        let mut config = test_config();
        config.editor.minimap.enable = true;
        let mut session = open(&file, config)?;
        session.keys("").await?;
        // Row 100 of the map stands for lines 400..404: the view centers on them.
        click(&mut session, 110, 100).await?;
        assert_eq!(lines_shown(&session.app).0, 400 - 148 / 2);
        // The map slid along: the shaded part is elsewhere now. Held there, it stays, and dragged
        // past the bottom it shows the end.
        let shaded = (0..TEXT_ROWS)
            .find(|&y| session.app.screen()[(110, y)].bg != session.app.screen()[(110, 0)].bg)
            .unwrap();
        assert!(shaded < 100, "{shaded}");
        drag(&mut session, (110, shaded + 1), (110, shaded + 1)).await?;
        assert_eq!(lines_shown(&session.app).0, 400 - 148 / 2);
        drag(&mut session, (110, shaded + 1), (110, TEXT_ROWS + 20)).await?;
        assert_eq!(lines_shown(&session.app).0, 2001 - 148);
        send(&session, MouseEventKind::ScrollUp, 110, 50)?;
        session.keys("").await?;
        assert_eq!(lines_shown(&session.app).0, 2001 - 148 - 3);
        session.quit().await
    }
}

mod minimap {
    use helix_view::{current_ref, editor::MinimapConfig};

    use super::*;

    fn minimap_config() -> Config {
        let mut config = test_config();
        config.editor.minimap = MinimapConfig {
            enable: true,
            ..Default::default()
        };
        config
    }

    /// The symbols of the screen row `y` from the column `x` on, `count` of them.
    fn symbols(app: &Application, x: u16, y: u16, count: u16) -> String {
        (x..x + count)
            .map(|x| app.screen()[(x, y)].symbol.as_str())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn documents_show_in_braille_beside_the_text() -> anyhow::Result<()> {
        // A cell is 8 columns of 4 lines, a dot 4 columns of a line; tabs are 4 wide.
        let text = "aaaaaaaa\n\n        x\n\tab\nzzzz\n";
        let file = text_file(".txt", text)?;
        let mut session = open(&file, minimap_config())?;
        session.keys("").await?;
        let app = &session.app;
        let (view, doc) = current_ref!(app.editor);
        // 12 cells and the column of changes take the right edge; the text wraps before them.
        assert_eq!(view.minimap_area(doc), Some(Rect::new(107, 0, 13, 148)));
        assert_eq!(view.inner_area(doc).right(), 107);
        assert_eq!(symbols(app, 108, 0, 3), "⢉⠄⠀");
        assert_eq!(symbols(app, 108, 1, 2), "⠁⠀");
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn maps_follow_edits() -> anyhow::Result<()> {
        let file = text_file(".txt", &lines(600))?;
        let mut session = open(&file, minimap_config())?;
        session.keys("").await?;
        // "line 0" to "line 3" fill both dots of the first cell.
        assert_eq!(symbols(&session.app, 108, 0, 2), "⣿⠀");
        // Typing on the first line changes its row, typing a line break the rows below.
        session.keys("A 1234567<esc>").await?;
        assert_eq!(symbols(&session.app, 108, 0, 2), "⣿⠉");
        session.keys("ggO<esc>").await?;
        assert_eq!(symbols(&session.app, 108, 0, 2), "⣶⠒");
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn narrow_splits_have_no_minimap() -> anyhow::Result<()> {
        let file = text_file(".txt", &lines(600))?;
        let mut session = open(&file, minimap_config())?;
        session.keys("<C-w>v").await?;
        let (view, doc) = current_ref!(session.app.editor);
        assert!(view.area.width < 80);
        assert_eq!(view.minimap_area(doc), None);
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cells_take_the_colors_of_their_text() -> anyhow::Result<()> {
        let source = "// a comment\nfn f() {}\n";
        let file = text_file(".rs", source)?;
        let mut session = open(&file, minimap_config())?;
        session.keys("").await?;
        let app = &session.app;
        let comment = app.editor.theme.get("comment").fg.unwrap();
        assert_eq!(fg(app, 108, 0), comment);
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn maps_slide_and_shade_the_lines_on_screen() -> anyhow::Result<()> {
        // 2000 lines: 500 rows of cells, on a map of 148.
        let file = text_file(".txt", &lines(2000))?;
        let mut session = open(&file, minimap_config())?;
        session.keys("").await?;
        let shade = session
            .app
            .editor
            .theme
            .get("ui.cursorline.primary")
            .bg
            .unwrap();
        let shaded = |app: &Application| {
            (0..TEXT_ROWS)
                .filter(|&y| app.screen()[(108, y)].bg == shade)
                .collect::<Vec<_>>()
        };
        // Lines 0..148 are the first 37 rows.
        assert_eq!(shaded(&session.app), (0..37).collect::<Vec<_>>());
        session.keys("ge").await?;
        assert_eq!(shaded(&session.app).last(), Some(&(TEXT_ROWS - 1)));
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn marks_tint_dots_and_changes_have_a_column() -> anyhow::Result<()> {
        let text: String = (0..600)
            .map(|i| match i {
                100 | 500 => "needle\n".to_owned(),
                _ => format!("line {i}\n"),
            })
            .collect();
        let file = text_file(".txt", &text)?;
        let mut session = open(&file, minimap_config())?;
        let special = session.app.editor.theme.get("special").fg.unwrap();
        let doc = doc_mut!(session.app.editor);
        doc.set_diff_override(Some(Rope::from(text.replace("line 300\n", "x\n"))));
        session
            .until("the diff", |app| {
                doc!(app.editor)
                    .diff_handle()
                    .is_some_and(|handle| !handle.load().is_empty())
            })
            .await;
        // Lines 100 and 500 are in rows 25 and 125, line 300 in row 75.
        session.keys("/needle").await?;
        let app = &session.app;
        let tinted: Vec<_> = (0..TEXT_ROWS)
            .filter(|&y| fg(app, 108, y) == special)
            .collect();
        assert_eq!(tinted, [25, 125]);
        let changes: Vec<_> = (0..TEXT_ROWS)
            .filter(|&y| app.screen()[(107, y)].symbol.as_str() == "▍")
            .collect();
        assert_eq!(changes, [75]);
        session.quit().await
    }
}

/// Prints how long a frame takes while scrolling, typing and searching in `text`, without and
/// with scrollbars and minimaps.
async fn measure(name: &str, text: &str) -> anyhow::Result<()> {
    const STEPS: usize = 200;
    let file = text_file(".rs", text)?;
    let mut session = open(&file, scrollbar_config(false))?;
    session.keys("").await?;
    for (scrollbar, minimap) in [(false, false), (true, false), (false, true), (true, true)] {
        session
            .keys(&format!(":set scrollbar.enable {scrollbar}<ret>"))
            .await?;
        session
            .keys(&format!(":set minimap.enable {minimap}<ret>"))
            .await?;
        session.keys("gg").await?;
        // Two keys, two frames a step.
        let start = Instant::now();
        session.keys(&"zj".repeat(STEPS)).await?;
        let scrolling = start.elapsed() / (2 * STEPS as u32);
        // Every key an edit.
        session.keys("ggi").await?;
        let start = Instant::now();
        session.keys(&"x".repeat(STEPS)).await?;
        let typing = start.elapsed() / STEPS as u32;
        session.keys("<esc>u").await?;
        // Every key a new search.
        session.keys("/").await?;
        let start = Instant::now();
        session
            .keys(&"self<backspace><backspace><backspace><backspace>".repeat(STEPS / 8))
            .await?;
        let searching = start.elapsed() / STEPS as u32;
        session.keys("<esc>").await?;
        println!(
            "{name}: scrollbar {scrollbar}, minimap {minimap}: scrolling {scrolling:?}, \
             typing {typing:?}, searching {searching:?} a key"
        );
    }
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a check"]
async fn measure_a_large_file() -> anyhow::Result<()> {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands.rs"))?;
    measure("commands.rs", &text).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a check"]
async fn measure_a_huge_file() -> anyhow::Result<()> {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands.rs"))?;
    measure("4 x commands.rs", &text.repeat(4)).await
}
