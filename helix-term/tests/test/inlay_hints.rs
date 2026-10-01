use helix_core::text_annotations::InlineAnnotation;
use helix_term::application::Application;
use helix_view::document::{DocumentInlayHints, DocumentInlayHintsId};

use super::*;

/// The column `text` starts at in the first row of the screen showing it.
fn column_of(app: &Application, text: &str) -> Option<usize> {
    let screen = app.screen();
    screen
        .content
        .chunks(screen.area.width as usize)
        .find_map(|row| {
            row.windows(text.len()).position(|cells| {
                cells
                    .iter()
                    .map(|cell| cell.symbol.chars().next().unwrap_or(' '))
                    .eq(text.chars())
            })
        })
}

/// An app editing `input` with the inlay hint `hint` in front of char `char_idx`.
fn app_with_hint(input: &str, char_idx: usize, hint: &str) -> anyhow::Result<Application> {
    let mut app = AppBuilder::new().with_input_text(input).build()?;
    let (view, doc) = helix_view::current!(app.editor);
    let id = DocumentInlayHintsId {
        first_line: 0,
        last_line: doc.text().len_lines(),
    };
    doc.set_inlay_hints(
        view.id,
        DocumentInlayHints {
            parameter_inlay_hints: vec![InlineAnnotation::new(char_idx, hint)],
            ..DocumentInlayHints::empty_with_id(id)
        },
    );
    Ok(app)
}

#[tokio::test(flavor = "multi_thread")]
async fn popups_point_past_inlay_hints() -> anyhow::Result<()> {
    let mut app = app_with_hint("ab #[x|]#yz\n", 3, "hint: ")?;
    test_key_sequence(
        &mut app,
        Some(":tree-sitter-scopes<ret>"),
        Some(&|app| {
            let hint = column_of(app, "hint: xyz").expect("the hint isn't shown");
            // At the cursor on `x`, inside the margin of the text.
            assert_eq!(column_of(app, "[]"), Some(hint + "hint: ".len() + 1));
        }),
        false,
    )
    .await
}
