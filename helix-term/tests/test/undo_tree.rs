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
            // Regexes, like the editor's search, from the cursor down.
            (
                Some("<space>u/[23]<ret>"),
                Some(&|app| assert_eq!(text(app), "x12\n")),
            ),
            (
                Some("n"),
                Some(&|app| {
                    assert_eq!(text(app), "x13\n");
                    let status = app
                        .editor
                        .get_status()
                        .map(|(status, _)| status.to_string());
                    assert_eq!(status.as_deref(), Some("Wrapped around the undo tree"));
                }),
            ),
            (Some("N"), Some(&|app| assert_eq!(text(app), "x12\n"))),
            // A search typed and abandoned goes back to where it started.
            (
                Some("gg/1<esc>"),
                Some(&|app| assert_eq!(text(app), "x13\n")),
            ),
            // So does one that is no regex.
            (
                Some("/x1(<ret>"),
                Some(&|app| assert_eq!(text(app), "x13\n")),
            ),
            (Some("<ret>"), Some(&|app| assert_eq!(text(app), "x13\n"))),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tree_and_the_editor_share_their_searches() -> anyhow::Result<()> {
    let mut app = branched()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            // A search in the editor, which matches nothing in its text, is the tree's `n`.
            (Some("/2<ret>"), None),
            (
                Some("<space>un"),
                Some(&|app| assert_eq!(text(app), "x12\n")),
            ),
            // A search in the tree is the editor's `n`: in `x12`, the `2`.
            (Some("gg/1[23]<ret><ret>"), None),
            (
                Some("ggn"),
                Some(&|app| {
                    let (view, doc) = helix_view::current_ref!(app.editor);
                    let selection = doc.selection(view.id).primary();
                    assert_eq!(selection.fragment(doc.text().slice(..)), "13");
                }),
            ),
            // The tree's prompt recalls earlier searches.
            (
                Some("<space>u/<C-p><C-p><ret>"),
                Some(&|app| assert_eq!(text(app), "x12\n")),
            ),
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
                    assert!(screen.contains("- x1"), "{screen}");
                    assert!(screen.contains("+ x12"), "{screen}");
                }),
            ),
            // Against the revision browsing started from.
            (Some("d"), Some(&rest)),
            (
                Some(NOTHING),
                Some(&|app| {
                    let screen = screen(app).join("\n");
                    assert!(screen.contains("─ 3 → 2 ─"), "{screen}");
                    assert!(screen.contains("- x13"), "{screen}");
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
                    assert!(screen.contains("- x1"), "{screen}");
                    assert!(screen.contains("+ x12"), "{screen}");
                }),
            ),
        ],
        false,
    )
    .await
}

/// The rows of the diff part of the panel: below its header, in the panel's columns.
fn diff_rows(app: &Application) -> Vec<String> {
    let screen = screen(app);
    let header = screen
        .iter()
        .position(|row| row.contains(" → "))
        .expect("the diff is shown");
    let column = screen[header].find('─').unwrap();
    screen[header + 1..]
        .iter()
        .take(3)
        .map(|row| row.get(column..).unwrap_or_default().trim_end().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_diff_part_takes_focus_and_scrolls() -> anyhow::Result<()> {
    let mut config = test_config();
    config.editor.undo.diff = helix_view::editor::UndoDiff::Builtin;
    config.editor.undo.diff_height = 3;
    let mut app = AppBuilder::new()
        .with_config(config)
        .with_input_text("#[x|]#\nl1\nl2\nl3\nl4\n")
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            (Some("<space>uj"), Some(&rest)),
            (
                Some(NOTHING),
                Some(&|app| assert_eq!(diff_rows(app), ["- x1", "+ x12", "  l1"])),
            ),
            // The editor's motions scroll the diff once the view below has the keys.
            (
                Some("<C-w>jj"),
                Some(&|app| {
                    assert_eq!(diff_rows(app), ["+ x12", "  l1", "  l2"]);
                    // The buffer stays at the revision.
                    assert_eq!(text(app), "x12\nl1\nl2\nl3\nl4\n");
                }),
            ),
            (
                Some("ge"),
                Some(&|app| assert_eq!(diff_rows(app), ["  l1", "  l2", "  l3"])),
            ),
            (
                Some("gg"),
                Some(&|app| assert_eq!(diff_rows(app), ["- x1", "+ x12", "  l1"])),
            ),
            // Back to the tree part, where `j` goes to an older revision again.
            (
                Some("<C-w>kj"),
                Some(&|app| assert_eq!(text(app), "x1\nl1\nl2\nl3\nl4\n")),
            ),
            // Any way to the view below works; `Esc` goes back to where browsing started.
            (
                Some("<space>wj<esc>"),
                Some(&|app| assert_eq!(text(app), "x13\nl1\nl2\nl3\nl4\n")),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_floating_tree_covers_the_views_rather_than_narrowing_them() -> anyhow::Result<()> {
    let mut config = test_config();
    config.editor.undo.float = true;
    let mut app = AppBuilder::new()
        .with_config(config)
        .with_input_text("#[x|]#\n")
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            (
                Some("<space>U"),
                Some(&|app| {
                    let width = app.screen().area.width;
                    assert_eq!(app.editor.tree.area().width, width);
                    let screen = screen(app);
                    let node = screen[0].chars().position(|c| c == '●');
                    assert!(
                        node.is_some_and(|at| at > width as usize / 2),
                        "{}",
                        screen.join("\n")
                    );
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_width_toggles_between_widest_and_narrowest() -> anyhow::Result<()> {
    let mut app = branched()?;
    let views = |app: &Application| app.editor.tree.area().width;
    // The editor keeps 20 columns, and the panel is at most 64 wide.
    let widest = |app: &Application| app.screen().area.width.saturating_sub(20).min(64);
    test_key_sequences(
        &mut app,
        vec![
            (Some(BRANCH), None),
            (
                Some("<space>u|"),
                Some(&|app| assert_eq!(views(app), app.screen().area.width - widest(app))),
            ),
            (
                Some("|"),
                Some(&|app| assert_eq!(views(app), app.screen().area.width - 16)),
            ),
        ],
        false,
    )
    .await
}
