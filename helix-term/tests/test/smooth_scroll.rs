use std::{cell::RefCell, time::Duration};

use helix_core::syntax::config::SoftWrap;
use helix_term::application::Application;
use helix_view::{current_ref, editor::SmoothScrollConfig, view::ViewPosition};

use super::*;

/// `lines` numbered lines, each followed by `words` words, with the cursor at the start.
fn document(lines: usize, words: usize) -> String {
    let text: String = (0..lines)
        .map(|line| format!("line {line}{}\n", " word".repeat(words)))
        .collect();
    format!("#[l|]#{}", &text[1..])
}

fn app(document: &str, smooth_scroll: bool, soft_wrap: bool) -> anyhow::Result<Application> {
    let config = Config {
        editor: helix_view::editor::Config {
            smooth_scroll: SmoothScrollConfig {
                enable: smooth_scroll,
                duration: Duration::from_millis(40),
                hide_cursor: false,
            },
            soft_wrap: SoftWrap {
                enable: Some(soft_wrap),
                ..Default::default()
            },
            ..test_editor_config()
        },
        ..test_config()
    };
    AppBuilder::new()
        .with_config(config)
        .with_input_text(document)
        .build()
}

/// Sends each of `steps`, letting the editor settle in between, and returns the view's offset
/// and selection at the end, making sure nothing is left mid-animation.
async fn scroll(
    document: &str,
    steps: &[&str],
    smooth_scroll: bool,
    soft_wrap: bool,
) -> anyhow::Result<(ViewPosition, Selection)> {
    let mut app = app(document, smooth_scroll, soft_wrap)?;
    let result = RefCell::new(None);
    let record = |app: &Application| {
        let (view, doc) = current_ref!(app.editor);
        assert_eq!(
            view.render_offset(doc),
            doc.view_offset(view.id),
            "{steps:?}"
        );
        assert_eq!(
            view.render_selection(doc),
            doc.selection(view.id),
            "{steps:?}"
        );
        *result.borrow_mut() = Some((doc.view_offset(view.id), doc.selection(view.id).clone()));
    };

    // the first frame records where the view starts from, as the editor's first render does
    let mut inputs: Vec<(Option<&str>, Option<&dyn Fn(&Application)>)> =
        vec![(Some("<esc>"), None)];
    inputs.extend(steps.iter().map(|&step| (Some(step), None)));
    inputs.last_mut().unwrap().1 = Some(&record);
    // steps may leave several splits open
    inputs.push((Some(":qa!<ret>"), None));
    test_key_sequences(&mut app, inputs, true).await?;

    Ok(result.into_inner().expect("the view state is recorded"))
}

/// Smooth scrolling only changes what is drawn in between: every sequence of `steps` has to end
/// in the same state as without it.
async fn assert_ends_like_instant_scrolling(
    document: &str,
    soft_wrap: bool,
    cases: &[&[&str]],
) -> anyhow::Result<()> {
    for steps in cases {
        let instant = scroll(document, steps, false, soft_wrap).await?;
        let smooth = scroll(document, steps, true, soft_wrap).await?;
        assert_eq!(instant, smooth, "{steps:?}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn smooth_page_scrolling() -> anyhow::Result<()> {
    assert_ends_like_instant_scrolling(
        &document(1000, 0),
        false,
        &[
            &["<C-d>"],
            &["<C-d>", "<C-u>"],
            &["<C-d>", "<C-d>", "<C-d>"],
            // presses during an animation retarget it
            &["<C-d><C-d><C-d>"],
            &["<C-d><C-u>"],
            &["v<C-d>"],
            &["<C-f>", "<C-b>"],
            &["<pagedown>", "<pagedown>", "<pageup>"],
            &["10zj", "10zk"],
            &["ge", "<C-d>"],
            &["ge", "<C-u>"],
        ],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn smooth_align_view() -> anyhow::Result<()> {
    assert_ends_like_instant_scrolling(
        &document(1000, 0),
        false,
        &[
            &["60j", "zt"],
            &["<C-d>", "zb"],
            &["<C-d>", "zz"],
            &["<C-d>", "zt", "zb", "zz"],
        ],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn smooth_jumps() -> anyhow::Result<()> {
    assert_ends_like_instant_scrolling(
        &document(1000, 0),
        false,
        &[
            &["ge"],
            &["ge", "gg"],
            &[":500<ret>"],
            &["/line 900<ret>"],
            &["ge", "<C-o>", "<C-i>"],
            &["<C-w>v", "<C-d>"],
        ],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn smooth_scrolling_with_soft_wrap() -> anyhow::Result<()> {
    assert_ends_like_instant_scrolling(
        &document(400, 60),
        true,
        &[
            &["<C-d>"],
            &["<C-d>", "<C-u>"],
            &["<pagedown>"],
            &["<C-d>", "zz"],
            &["ge", "gg"],
        ],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn smooth_horizontal_scrolling() -> anyhow::Result<()> {
    assert_ends_like_instant_scrolling(
        &document(400, 60),
        false,
        &[&["gl"], &["gl", "gh"], &["30jgl", "<C-d>"]],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn set_smooth_scroll() -> anyhow::Result<()> {
    let mut app = app(&document(10, 0), false, false)?;
    test_key_sequence(
        &mut app,
        Some(":set smooth-scroll true<ret>"),
        Some(&|app| assert!(app.editor.config().smooth_scroll.enable)),
        false,
    )
    .await
}
