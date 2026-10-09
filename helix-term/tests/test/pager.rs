use std::io::Write;

use helix_term::application::Application;
use helix_view::{doc, graphics::Color};
use tempfile::NamedTempFile;

use super::*;

/// Terminal output with a red word and a bold one, as `ls --color` and `man` write them.
const OUTPUT: &str = "\x1b[31mred\x1b[0m plain\nN\x08NA\x08AM\x08ME\x08E\n";

fn output_file() -> anyhow::Result<NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    file.write_all(OUTPUT.as_bytes())?;
    Ok(file)
}

fn pager(file: &NamedTempFile) -> anyhow::Result<Session> {
    let app = AppBuilder::new()
        .with_file(file.path(), None)
        .with_config(test_config())
        .with_pager()
        .build()?;
    Ok(Session::new(app))
}

/// The screen cell drawing the first char of `text` in the rows of the screen.
fn cell_of<'a>(app: &'a Application, text: &str) -> &'a tui::buffer::Cell {
    let screen = app.screen();
    let width = screen.area.width as usize;
    let (index, row) = screen
        .content
        .chunks(width)
        .enumerate()
        .find_map(|(y, row)| {
            let line: String = row.iter().map(|cell| cell.symbol.as_str()).collect();
            line.find(text)
                .map(|byte| (y, line[..byte].chars().count()))
        })
        .unwrap_or_else(|| panic!("{text:?} is not shown"));
    &screen.content[index * width + row]
}

fn status(app: &Application) -> String {
    app.editor
        .get_status()
        .map(|(message, _)| message.to_string())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn pagers_show_formatting_as_styles() -> anyhow::Result<()> {
    let file = output_file()?;
    let mut session = pager(&file)?;
    session.keys("").await?;
    let app = &session.app;
    assert_eq!(doc!(app.editor).text(), "red plain\nNAME\n");
    assert_eq!(cell_of(app, "red").fg, Color::Indexed(1));
    assert!(cell_of(app, "NAME")
        .modifier
        .contains(helix_view::graphics::Modifier::BOLD));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn pagers_refuse_edits_and_writes() -> anyhow::Result<()> {
    let file = output_file()?;
    let mut session = pager(&file)?;
    session.keys("i").await?;
    assert_eq!(status(&session.app), "The buffer is read-only");
    assert_eq!(session.app.editor.mode, helix_view::document::Mode::Normal);
    session.keys("xdp").await?;
    assert_eq!(doc!(session.app.editor).text(), "red plain\nNAME\n");
    assert!(!doc!(session.app.editor).is_modified());
    session.keys(":w!<ret>").await?;
    assert_eq!(
        status(&session.app),
        "Error saving: The buffer is read-only"
    );
    assert_eq!(std::fs::read_to_string(file.path())?, OUTPUT);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn every_file_of_a_pager_session_is_paged() -> anyhow::Result<()> {
    let file = output_file()?;
    let other = output_file()?;
    let mut session = pager(&file)?;
    session
        .keys(&format!(":open {}<ret>", other.path().display()))
        .await?;
    let doc = doc!(session.app.editor);
    let path = helix_stdx::path::canonicalize(other.path());
    assert_eq!(doc.path(), Some(path.as_path()));
    assert_eq!(doc.text(), "red plain\nNAME\n");
    assert!(!doc.is_modifiable());
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn man_pages_have_colors_for_their_parts() -> anyhow::Result<()> {
    let page = "LS(1)        User Commands        LS(1)\n\nN\x08NA\x08AM\x08ME\x08E\n       ls - list\n\n       -\x08--\x08-a\x08al\x08ll\x08l\n              see stat(2)\n";
    let mut file = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    file.write_all(page.as_bytes())?;
    let mut session = pager(&file)?;
    session.keys("").await?;
    let app = &session.app;
    let color = |scope| app.editor.theme.get(scope).fg.unwrap();
    assert_eq!(cell_of(app, "NAME").fg, color("markup.heading"));
    let option = cell_of(app, "--all");
    assert_eq!(option.fg, color("constant"));
    assert!(option
        .modifier
        .contains(helix_view::graphics::Modifier::BOLD));
    assert_eq!(cell_of(app, "stat").fg, color("function"));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn pages_formatted_again_keep_their_place() -> anyhow::Result<()> {
    let page = |lines: usize| -> String {
        let mut page = String::from("LS(1)        User Commands        LS(1)\n");
        page.extend((1..lines).map(|i| format!("       line {i}\n")));
        page
    };
    let mut file = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    file.write_all(page(200).as_bytes())?;
    let mut session = pager(&file)?;
    // The cursor halfway, then the page formatted narrower, in twice the lines.
    session.keys("100gg").await?;
    let doc = doc!(session.app.editor).id();
    session.app.editor.repage(doc, &page(400));
    session.keys("").await?;
    let (view, doc) = helix_view::current_ref!(session.app.editor);
    let text = doc.text().slice(..);
    assert_eq!(text.len_lines(), 401);
    let cursor = text.char_to_line(doc.selection(view.id).primary().cursor(text));
    assert!((195..=202).contains(&cursor), "{cursor}");
    assert!(!doc.is_modified());
    assert!(!doc.page.as_ref().unwrap().man.is_empty());
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn man_keys_of_themes_win_over_their_fallbacks() -> anyhow::Result<()> {
    let page =
        "LS(1)        User Commands        LS(1)\n\nNAME\n       -\x08--\x08-a\x08al\x08ll\x08l\n";
    let mut file = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    file.write_all(page.as_bytes())?;
    let mut session = pager(&file)?;
    let theme: toml::Value = toml::from_str(
        r##"
        "constant" = "green"
        "man.option" = "red"
        "markup.heading" = "blue"
        "ui.selection" = { bg = "gray" }
        "##,
    )?;
    session
        .app
        .editor
        .set_theme(helix_view::Theme::from(theme))?;
    session.keys("").await?;
    let app = &session.app;
    assert_eq!(cell_of(app, "--all").fg, Color::Red);
    // Without a key of their own, headings take `markup.heading`.
    assert_eq!(cell_of(app, "NAME").fg, Color::Blue);
    session.quit().await
}
