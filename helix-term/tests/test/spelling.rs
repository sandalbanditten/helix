use std::time::{Duration, Instant};

use helix_core::{diagnostic::DiagnosticProvider, syntax::config::SpellingConfig};
use helix_term::application::Application;
use helix_view::{current_ref, doc};

use super::*;

const TEXT: &str = "#[#|]# A heding\n\nSome `inlin` code, and a wrld.\n";

/// An app editing a Markdown document of `TEXT`, checked against `en_US`, once the document's
/// first spell check finished.
async fn app(messages: bool) -> anyhow::Result<Application> {
    let config = Config {
        editor: helix_view::editor::Config {
            spelling: SpellingConfig {
                languages: Some(vec!["en_US".parse()?]),
                messages: Some(messages),
                ..Default::default()
            },
            ..test_editor_config()
        },
        ..test_config()
    };
    let mut app = AppBuilder::new()
        .with_file("foo.md", None)
        .with_config(config)
        .with_input_text(TEXT)
        .build()?;

    // The dictionary loads and the document is checked in the background.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !doc!(app.editor)
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.provider == DiagnosticProvider::Spelling)
    {
        anyhow::ensure!(
            Instant::now() < deadline,
            "the document was not spell checked"
        );
        run_event_loop_until_idle(&mut app).await;
    }
    Ok(app)
}

/// Asserts that the primary selection is `text`, selected in the given direction.
fn assert_selected(app: &Application, text: &str, forward: bool) {
    let (view, doc) = current_ref!(app.editor);
    let range = doc.selection(view.id).primary();
    assert_eq!(range.fragment(doc.text().slice(..)), text);
    assert_eq!(range.head > range.anchor, forward, "direction of {range:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn goto_misspellings() -> anyhow::Result<()> {
    // Only the prose is checked, not the inline code.
    test_key_sequences(
        &mut app(false).await?,
        vec![
            (
                Some("]s"),
                Some(&|app| assert_selected(app, "heding", true)),
            ),
            (Some("]s"), Some(&|app| assert_selected(app, "wrld", true))),
            // There is no next misspelling.
            (Some("]s"), Some(&|app| assert_selected(app, "wrld", true))),
            (
                Some("gegl[s"),
                Some(&|app| assert_selected(app, "wrld", false)),
            ),
            (
                Some("[s"),
                Some(&|app| assert_selected(app, "heding", false)),
            ),
            (Some("]S"), Some(&|app| assert_selected(app, "wrld", true))),
            (
                Some("[S"),
                Some(&|app| assert_selected(app, "heding", true)),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn misspellings_are_only_underlined() -> anyhow::Result<()> {
    // `]d` skips misspellings without messages.
    test_key_sequence(
        &mut app(false).await?,
        Some("]d"),
        Some(&|app| assert_selected(app, "#", true)),
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn misspellings_with_messages_are_diagnostics() -> anyhow::Result<()> {
    test_key_sequence(
        &mut app(true).await?,
        Some("]d"),
        Some(&|app| assert_selected(app, "heding", true)),
        false,
    )
    .await
}
