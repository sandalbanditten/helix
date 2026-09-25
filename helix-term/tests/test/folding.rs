use std::io::Write;

use helix_term::application::Application;
use helix_view::{current_ref, editor::FoldingConfig};

use super::*;

const SOURCE: &str = indoc! {"\
    impl A {
        #[f|]#n f() {
            1
        }

        fn g() {
            2
        }
    }
"};

/// The text of the current view with its closed folds drawn as `…`.
fn folded(app: &Application) -> String {
    let (view, doc) = current_ref!(app.editor);
    let text = doc.text().slice(..);
    let mut folded = String::new();
    let mut pos = 0;
    for fold in doc.folds(view.id).outermost() {
        folded.extend(text.slice(pos..fold.start).chars());
        folded.push('…');
        pos = fold.end;
    }
    folded.extend(text.slice(pos..).chars());
    folded
}

/// Runs `keys` on `input` in a Rust document and checks the resulting text and selection
/// (`output`) and how the view shows it (`view`).
async fn fold_test(input: &str, keys: &str, output: &str, view: &str) -> anyhow::Result<()> {
    let app = AppBuilder::new().with_file("foo.rs", None).build()?;
    let expected = helix_core::test::print(output);
    test_key_sequence_with_input_text(
        Some(app),
        (input, keys, output),
        &|app| {
            let (view_, doc) = current_ref!(app.editor);
            assert_eq!(doc.text(), &expected.0, "{keys}");
            // the direction and sticky column of the cursor don't matter here
            let ranges = |selection: &Selection| {
                let ranges = selection.iter().map(|range| (range.from(), range.to()));
                ranges.collect::<Vec<_>>()
            };
            assert_eq!(
                ranges(doc.selection(view_.id)),
                ranges(&expected.1),
                "{keys}"
            );
            assert_eq!(folded(app), view, "{keys}");
        },
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn toggle_fold() -> anyhow::Result<()> {
    let f_folded = indoc! {"\
        impl A {
            fn f() {…}

            fn g() {
                2
            }
        }
    "};
    // a cursor on the header stays where it is
    fold_test(SOURCE, "zf", SOURCE, f_folded).await?;
    // a cursor inside the folded text moves onto the fold cell
    let in_body = indoc! {"\
        impl A {
            fn f() {
                #[1|]#
            }

            fn g() {
                2
            }
        }
    "};
    fold_test(in_body, "zf", SOURCE_AT_CELL, f_folded).await?;
    // down steps over the folded row
    fold_test(
        SOURCE,
        "zfj",
        indoc! {"\
            impl A {
                fn f() {
                    1
                }
            #[\n|]#
                fn g() {
                    2
                }
            }
        "},
        f_folded,
    )
    .await?;
    // toggling on the folded row opens it
    fold_test(SOURCE, "zfzf", SOURCE, SOURCE_TEXT).await?;
    // an `if` and its `else` fold into one row
    let if_else = indoc! {"\
        fn f() {
            #[i|]#f a {
                1
            } else {
                2
            }
        }
    "};
    fold_test(
        if_else,
        "zf/else<ret>zf",
        indoc! {"\
            fn f() {
                if a {
                    1
                } #[else|]# {
                    2
                }
            }
        "},
        indoc! {"\
            fn f() {
                if a {…} else {…}
            }
        "},
    )
    .await?;
    Ok(())
}

const SOURCE_TEXT: &str = indoc! {"\
    impl A {
        fn f() {
            1
        }

        fn g() {
            2
        }
    }
"};

const SOURCE_AT_CELL: &str = indoc! {"\
    impl A {
        fn f() {#[\n|]#        1
        }

        fn g() {
            2
        }
    }
"};

#[tokio::test(flavor = "multi_thread")]
async fn line_commands_act_on_folded_rows() -> anyhow::Result<()> {
    // `x` selects the whole folded function
    fold_test(
        SOURCE,
        "zfxd",
        indoc! {"\
            impl A {
            #[\n|]#
                fn g() {
                    2
                }
            }
        "},
        indoc! {"\
            impl A {

                fn g() {
                    2
                }
            }
        "},
    )
    .await?;
    // `o` opens a line below the folded row, `O` above it
    fold_test(
        SOURCE,
        "zfox<esc>",
        indoc! {"\
            impl A {
                fn f() {
                    1
                }
                x#[\n|]#

                fn g() {
                    2
                }
            }
        "},
        indoc! {"\
            impl A {
                fn f() {…}
                x

                fn g() {
                    2
                }
            }
        "},
    )
    .await?;
    // `gh` goes to the start of the row, and typing there keeps the fold
    fold_test(
        SOURCE,
        "zfgsipub <esc>",
        indoc! {"\
            impl A {
                pub #[f|]#n f() {
                    1
                }

                fn g() {
                    2
                }
            }
        "},
        indoc! {"\
            impl A {
                pub fn f() {…}

                fn g() {
                    2
                }
            }
        "},
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn operators_on_the_fold_cell() -> anyhow::Result<()> {
    // `d` on the fold cell (`glh` from the header) deletes the text it hides
    fold_test(
        SOURCE,
        "zfglhd",
        indoc! {"\
            impl A {
                fn f() {#[}|]#

                fn g() {
                    2
                }
            }
        "},
        indoc! {"\
            impl A {
                fn f() {}

                fn g() {
                    2
                }
            }
        "},
    )
    .await?;
    // undo restores the text but not the fold
    fold_test(SOURCE, "zfglhdu", SOURCE_AT_CELL, SOURCE_TEXT).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn yank_on_the_fold_cell() -> anyhow::Result<()> {
    // `y` on the fold cell yanks the text it hides
    let app = AppBuilder::new().with_file("foo.rs", None).build()?;
    test_key_sequence_with_input_text(
        Some(app),
        (SOURCE, "zfglhy", SOURCE_AT_CELL),
        &|app| {
            let yanked = app.editor.registers.first('"', &app.editor).unwrap();
            assert_eq!(yanked, "\n        1\n    ");
        },
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn fold_all_and_nesting() -> anyhow::Result<()> {
    let impl_header = indoc! {"\
        #[i|]#mpl A {
            fn f() {
                1
            }

            fn g() {
                2
            }
        }
    "};
    // opening the impl leaves its methods folded
    fold_test(
        impl_header,
        "zFzf",
        impl_header,
        indoc! {"\
            impl A {
                fn f() {…}

                fn g() {…}
            }
        "},
    )
    .await?;
    // `za` opens the impl with everything inside it
    fold_test(impl_header, "zFza", impl_header, SOURCE_TEXT).await?;
    // and closes it with everything inside it, so opening one level shows the methods folded
    fold_test(
        impl_header,
        "zazf",
        impl_header,
        indoc! {"\
            impl A {
                fn f() {…}

                fn g() {…}
            }
        "},
    )
    .await?;
    fold_test(impl_header, "zFzU", impl_header, SOURCE_TEXT).await?;
    // a search match inside the folds opens as few of them as needed
    fold_test(
        impl_header,
        "zF/2<ret>",
        indoc! {"\
            impl A {
                fn f() {
                    1
                }

                fn g() {
                    #[2|]#
                }
            }
        "},
        indoc! {"\
            impl A {
                fn f() {…}

                fn g() {
                    2
                }
            }
        "},
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn start_folded() -> anyhow::Result<()> {
    let mut file = tempfile::Builder::new().suffix(".rs").tempfile()?;
    file.write_all(SOURCE_TEXT.as_bytes())?;
    file.flush()?;
    let config = Config {
        editor: helix_view::editor::Config {
            folding: FoldingConfig {
                start_folded: true,
                ..Default::default()
            },
            ..test_editor_config()
        },
        ..test_config()
    };
    let mut app = AppBuilder::new()
        .with_config(config)
        .with_file(file.path(), None)
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("<C-w>v"),
                Some(&|app: &Application| {
                    assert_eq!(folded(app), "impl A {…}\n");
                    // the split has the folds of the view it was split from
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(app.editor.tree.views().count(), 2);
                    for (other, _) in app.editor.tree.views() {
                        assert_eq!(doc.folds(other.id).closed(), doc.folds(view.id).closed());
                    }
                }),
            ),
            (Some("<C-w>q"), None),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn set_start_folded() -> anyhow::Result<()> {
    let mut app = AppBuilder::new().build()?;
    test_key_sequence(
        &mut app,
        Some(":set folding.start-folded true<ret>"),
        Some(&|app| assert!(app.editor.config().folding.start_folded)),
        false,
    )
    .await
}
