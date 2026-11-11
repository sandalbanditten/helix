use std::{ffi::OsString, io::Write};

use helix_term::application::Application;
use helix_view::{doc, doc_mut, graphics::Color, pager};
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
async fn piped_text_is_named_pager_and_files_read_only() -> anyhow::Result<()> {
    let statusline = |app: &Application| {
        let screen = app.screen();
        let width = screen.area.width as usize;
        let row = screen.content.chunks(width).nth_back(1).unwrap();
        row.iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>()
    };
    let file = output_file()?;
    let mut session = pager(&file)?;
    session.keys("").await?;
    let name = file.path().file_name().unwrap().to_string_lossy();
    let line = statusline(&session.app);
    assert!(
        line.contains(&*name) && line.contains(" [readonly] "),
        "{line}"
    );
    // A buffer without a file, as the text piped into the pager is.
    session.keys(":new<ret>").await?;
    doc_mut!(session.app.editor).page();
    session.keys("").await?;
    let line = statusline(&session.app);
    assert!(line.contains(" [pager] "), "{line}");
    assert!(
        !line.contains("[readonly]") && !line.contains("[scratch]"),
        "{line}"
    );
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
    let (text, formatted) = pager::page(&page(400));
    session.app.editor.repage(doc, text, formatted);
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
async fn git_formats_diffs_again_for_the_width_of_the_view() -> anyhow::Result<()> {
    let repo = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let name = format!("{}.txt", "long-name-".repeat(15));
    std::fs::write(repo.path().join(&name), "")?;
    git(repo.path(), &["init"]);
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-m", "empty"]);
    std::fs::write(repo.path().join(&name), "line\n".repeat(100))?;
    // `--stat` lays the diff out for `COLUMNS`, like difftastic.
    let args = ["diff", "--no-ext-diff", "--stat"];
    let shown = std::process::Command::new("git")
        .args(args)
        .current_dir(repo.path())
        .env("COLUMNS", "200")
        .output()?;
    let mut file = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    file.write_all(&shown.stdout)?;
    let mut session = pager(&file)?;
    let doc = doc_mut!(session.app.editor);
    doc.page.as_mut().unwrap().source = Some(pager::Source {
        producer: pager::Producer::Git {
            args: args.iter().map(OsString::from).collect(),
            dir: repo.path().to_owned(),
        },
        width: None,
        formatting: None,
    });
    let widest = |app: &Application| {
        let text = doc!(app.editor).text();
        text.lines().map(|line| line.len_chars()).max().unwrap_or(0)
    };
    assert!(widest(&session.app) > 150);
    session
        .until("git to format the diff again", |app| widest(app) < 150)
        .await;
    let (view, doc) = helix_view::current_ref!(session.app.editor);
    // Lines end before the last column, which would wrap them.
    assert!(widest(&session.app) < view.inner_width(doc) as usize);
    assert!(doc.text().to_string().contains("| 100 +"));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn pages_formatted_the_same_stay_as_they_are() -> anyhow::Result<()> {
    let mut session = pager(&output_file()?)?;
    session.keys("jwl").await?;
    let (view, doc) = helix_view::current_ref!(session.app.editor);
    let (id, version) = (doc.id(), doc.version());
    let selection = doc.selection(view.id).clone();
    let (text, formatted) = pager::page(OUTPUT);
    session.app.editor.repage(id, text, formatted);
    session.keys("").await?;
    let (view, doc) = helix_view::current_ref!(session.app.editor);
    assert_eq!(doc.version(), version);
    assert_eq!(doc.selection(view.id), &selection);
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
