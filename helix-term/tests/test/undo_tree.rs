use helix_term::application::Application;
use helix_view::doc;

use super::*;

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

fn text(app: &Application) -> String {
    doc!(app.editor).text().to_string()
}

/// An app whose buffer went through `x1` (from the empty root), `x12`, back to `x1` and then
/// `x13`: revision 3 branches off revision 1 beside revision 2.
fn branched() -> anyhow::Result<Application> {
    AppBuilder::new().with_input_text("#[x|]#\n").build()
}

const BRANCH: &str = "A1<esc>A2<esc>uA3<esc>";

#[tokio::test(flavor = "multi_thread")]
async fn browsing_takes_the_buffer_along_and_esc_goes_back() -> anyhow::Result<()> {
    let mut app = branched()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), Some(&|app| assert_eq!(text(app), "x13\n"))),
            (
                Some("<space>uj"),
                Some(&|app| assert_eq!(text(app), "x12\n")),
            ),
            (Some("j"), Some(&|app| assert_eq!(text(app), "x1\n"))),
            (Some("ge"), Some(&|app| assert_eq!(text(app), "\n"))),
            (Some("gg"), Some(&|app| assert_eq!(text(app), "x13\n"))),
            (Some("l"), Some(&|app| assert_eq!(text(app), "x12\n"))),
            (Some("h"), Some(&|app| assert_eq!(text(app), "x13\n"))),
            (Some("uu"), Some(&|app| assert_eq!(text(app), "\n"))),
            (Some("U"), Some(&|app| assert_eq!(text(app), "x1\n"))),
            (Some("<esc>"), Some(&|app| assert_eq!(text(app), "x13\n"))),
            // The tree has no focus any more: `j` moves the cursor in the text.
            (Some("j"), Some(&|app| assert_eq!(text(app), "x13\n"))),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_keeps_the_revision_reached() -> anyhow::Result<()> {
    let mut app = branched()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            (
                Some("<space>uj<ret>"),
                Some(&|app| assert_eq!(text(app), "x12\n")),
            ),
            // Redo follows the branch browsed to.
            (Some("uU"), Some(&|app| assert_eq!(text(app), "x12\n"))),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn searching_finds_what_revisions_changed() -> anyhow::Result<()> {
    let mut app = branched()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            (
                Some("<space>u/2<ret>"),
                Some(&|app| assert_eq!(text(app), "x12\n")),
            ),
            (Some("n"), Some(&|app| assert_eq!(text(app), "x12\n"))),
            // A search typed and abandoned goes back to where it started.
            (
                Some("gg/1<esc>"),
                Some(&|app| assert_eq!(text(app), "x13\n")),
            ),
            (Some("<ret>"), Some(&|app| assert_eq!(text(app), "x13\n"))),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panel_shows_the_history_beside_the_editor() -> anyhow::Result<()> {
    let mut app = branched()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            (
                Some("<space>U"),
                Some(&|app| {
                    let screen = screen(app);
                    let width = app.screen().area.width as usize;
                    let rows: Vec<_> = screen
                        .iter()
                        .filter_map(|row| {
                            row.chars()
                                .position(|c| c == '●' || c == '○')
                                .map(|at| (at, row))
                        })
                        .collect();
                    assert_eq!(rows.len(), 4, "{}", screen.join("\n"));
                    // On the right, the newest revision on top.
                    assert!(rows[0].0 > width / 2, "{}", screen.join("\n"));
                    assert!(rows[0].1.contains('●') && rows[0].1.ends_with('3'));
                    assert!(rows[3].1.ends_with("original"));
                    // The views make room for it.
                    assert!(app.editor.tree.area().width < width as u16);
                }),
            ),
            // Shown while unfocused, it follows edits.
            (
                Some("A4<esc>"),
                Some(&|app| {
                    let screen = screen(app).join("\n");
                    assert!(screen.contains("●   4 now   4"), "{screen}");
                }),
            ),
            (
                Some("<space>U"),
                Some(&|app| {
                    let screen = screen(app).join("\n");
                    assert!(!screen.contains("original"), "{screen}");
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_diff_gutter_compares_with_the_start_while_browsing() -> anyhow::Result<()> {
    let mut app = branched()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(BRANCH),
                Some(&|app| assert!(doc!(app.editor).diff_handle().is_none())),
            ),
            (
                Some("<space>u"),
                Some(&|app| assert!(doc!(app.editor).diff_handle().is_some())),
            ),
            (
                Some("<ret>"),
                Some(&|app| assert!(doc!(app.editor).diff_handle().is_none())),
            ),
        ],
        false,
    )
    .await
}

/// An app like [`branched`] whose undo tree shows diffs with `tool`.
fn with_diff(tool: helix_view::editor::UndoDiff) -> anyhow::Result<Application> {
    let mut config = test_config();
    config.editor.undo.diff = tool;
    config.editor.undo.diff_height = 6;
    AppBuilder::new()
        .with_config(config)
        .with_input_text("#[x|]#\n")
        .build()
}

/// Waits past the time the diff waits for the cursor to rest, and some.
fn rest(_: &Application) {
    std::thread::sleep(std::time::Duration::from_millis(600));
}

/// Keys that change nothing but have the editor handle what came in meanwhile: a step without
/// keys would wait for an event forever.
const NOTHING: &str = "zz";

#[tokio::test(flavor = "multi_thread")]
async fn the_diff_shows_what_the_revision_changed() -> anyhow::Result<()> {
    let mut app = with_diff(helix_view::editor::UndoDiff::Builtin)?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            (Some("<space>uj"), Some(&rest)),
            (
                Some(NOTHING),
                Some(&|app| {
                    let screen = screen(app).join("\n");
                    assert!(screen.contains("─ 1 → 2 ─"), "{screen}");
                    assert!(screen.contains("-x1"), "{screen}");
                    assert!(screen.contains("+x12"), "{screen}");
                }),
            ),
            // Against the revision browsing started from.
            (Some("d"), Some(&rest)),
            (
                Some(NOTHING),
                Some(&|app| {
                    let screen = screen(app).join("\n");
                    assert!(screen.contains("─ 3 → 2 ─"), "{screen}");
                    assert!(screen.contains("-x13"), "{screen}");
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn difftastic_shows_the_diff_when_installed() -> anyhow::Result<()> {
    if helix_stdx::env::which("difft").is_err() {
        return Ok(());
    }
    let mut app = with_diff(helix_view::editor::UndoDiff::Difftastic)?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            (Some("<space>uj"), Some(&rest)),
            (
                Some(NOTHING),
                Some(&|app| {
                    let screen = screen(app).join("\n");
                    // difft's own header: the name it was given and the language it took.
                    assert!(screen.contains("buffer --- Text"), "{screen}");
                    assert!(screen.contains("x12"), "{screen}");
                }),
            ),
        ],
        false,
    )
    .await
}
