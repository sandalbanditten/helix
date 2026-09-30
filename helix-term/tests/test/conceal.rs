use helix_core::conceal::ConcealReveal;
use helix_term::application::Application;
use helix_view::{current_ref, editor::ConcealConfig};

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

/// Asserts that the screen shows each of `rows` in a row of its own, in this order.
fn assert_shows(app: &Application, rows: &[&str]) {
    let screen = screen(app);
    let mut screen_rows = screen.iter();
    for row in rows {
        assert!(
            screen_rows.any(|screen_row| screen_row.contains(row)),
            "{row:?} is not shown in order:\n{}",
            screen.join("\n")
        );
    }
}

/// An app editing a Typst document with `input` as its text and selection.
fn typst_app(input: &str, conceal: ConcealConfig) -> anyhow::Result<Application> {
    let config = Config {
        editor: helix_view::editor::Config {
            conceal,
            ..test_editor_config()
        },
        ..test_config()
    };
    AppBuilder::new()
        .with_file("foo.typ", None)
        .with_config(config)
        .with_input_text(input)
        .build()
}

/// Runs `keys` on `input` in a Typst document and checks the resulting text and selection
/// (`output`) and that the screen shows the `rows`.
async fn conceal_test(input: &str, keys: &str, output: &str, rows: &[&str]) -> anyhow::Result<()> {
    let app = AppBuilder::new().with_file("foo.typ", None).build()?;
    let expected = helix_core::test::print(output);
    test_key_sequence_with_input_text(
        Some(app),
        (input, keys, output),
        &|app| {
            let (view, doc) = current_ref!(app.editor);
            assert_eq!(doc.text(), &expected.0, "{keys}");
            let ranges = |selection: &Selection| {
                let ranges = selection.iter().map(|range| (range.from(), range.to()));
                ranges.collect::<Vec<_>>()
            };
            assert_eq!(
                ranges(doc.selection(view.id)),
                ranges(&expected.1),
                "{keys}"
            );
            assert_shows(app, rows);
        },
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn conceal_away_from_cursors() -> anyhow::Result<()> {
    let mut app = typst_app("#[$|]#2 alpha^2$ #sym.qed -- x\n", ConcealConfig::default())?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("<esc>"),
                Some(&|app| assert_shows(app, &["$2 α^2$ ∎ – x"])),
            ),
            // on the char before `alpha`
            (
                Some("ll"),
                Some(&|app| assert_shows(app, &["$2 alpha^2$ ∎ – x"])),
            ),
            // on the char after `^`
            (
                Some("7l"),
                Some(&|app| assert_shows(app, &["$2 α^2$ ∎ – x"])),
            ),
            // on the char before `#sym.qed`, whose `#` is concealed with it
            (
                Some("ll"),
                Some(&|app| assert_shows(app, &["$2 α^2$ #sym.qed – x"])),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn move_down_over_conceals() -> anyhow::Result<()> {
    // the fourth column of `$α β$` is `β`, not the `p` of `alpha`
    conceal_test(
        "abc#[d|]#ef\n$alpha beta$\n",
        "j",
        "abcdef\n$alpha #[b|]#eta$\n",
        &["abcdef", "$α beta$"],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn move_up_over_conceals() -> anyhow::Result<()> {
    conceal_test(
        "abcdef\n$alpha #[b|]#eta$\n",
        "k",
        "abc#[d|]#ef\n$alpha beta$\n",
        &["abcdef", "$α β$"],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn conceal_while_typing() -> anyhow::Result<()> {
    let mut app = typst_app("$#[$|]#\n", ConcealConfig::default())?;
    test_key_sequences(
        &mut app,
        vec![
            // the cursor is right after `alpha`
            (Some("ialpha"), Some(&|app| assert_shows(app, &["$alpha$"]))),
            (Some(" "), Some(&|app| assert_shows(app, &["$α $"]))),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn jump_labels_show_source_text() -> anyhow::Result<()> {
    let mut app = typst_app("#[x|]# $alpha + beta$\n", ConcealConfig::default())?;
    let labelled = |app: &Application| {
        let row = screen(app)
            .into_iter()
            .find(|row| row.contains('+'))
            .unwrap();
        assert!(row.contains("pha") && !row.contains('α'), "{row}");
    };
    test_key_sequences(
        &mut app,
        vec![
            (Some("gw"), Some(&labelled)),
            (
                Some("<esc>"),
                Some(&|app| assert_shows(app, &["x $α + β$"])),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn conceal_disabled() -> anyhow::Result<()> {
    let disabled = ConcealConfig {
        enable: false,
        ..ConcealConfig::default()
    };
    let mut app = typst_app("#[$|]#2 alpha^2$\n", disabled)?;
    let shows_source = |app: &Application| assert_shows(app, &["$2 alpha^2$"]);
    test_key_sequence(&mut app, Some("<esc>"), Some(&shows_source), false).await
}

#[tokio::test(flavor = "multi_thread")]
async fn reveal_line() -> anyhow::Result<()> {
    let line = ConcealConfig {
        reveal: ConcealReveal::Line,
        ..ConcealConfig::default()
    };
    let mut app = typst_app("#[$|]#alpha$ $beta$\n$gamma$\n", line)?;
    // the cursor reveals everything on its line
    let shows_line = |app: &Application| assert_shows(app, &["$alpha$ $beta$", "$γ$"]);
    test_key_sequence(&mut app, Some("<esc>"), Some(&shows_line), false).await
}

#[tokio::test(flavor = "multi_thread")]
async fn unfocused_views_conceal_everything() -> anyhow::Result<()> {
    let mut app = typst_app("$2 #[a|]#lpha^2$\n", ConcealConfig::default())?;
    // the split shows the same cursor, but only the focused view reveals at it
    let both = |app: &Application| {
        let row = screen(app)
            .into_iter()
            .find(|row| row.contains("$2"))
            .unwrap();
        assert!(
            row.contains("$2 alpha^2$") && row.contains("$2 α^2$"),
            "{row}"
        );
    };
    test_key_sequences(
        &mut app,
        vec![(Some(":vs<ret>"), Some(&both)), (Some(":qa!<ret>"), None)],
        true,
    )
    .await
}
