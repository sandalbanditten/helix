use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use helix_term::application::Application;
use helix_view::{
    doc,
    editor::{BufferLine, SmoothScrollConfig},
};

use super::*;

const SCROLLOFF: usize = 5;

/// Writes `count` files whose tabs, 30 columns each, don't fit the 120 columns of the screen.
fn files(dir: &Path, count: usize) -> anyhow::Result<Vec<PathBuf>> {
    (0..count)
        .map(|i| {
            let path = dir.join(format!("file-{i:02}-with-a-long-name.txt"));
            std::fs::write(&path, "text\n")?;
            Ok(helix_stdx::path::canonicalize(path))
        })
        .collect()
}

/// A session with `files` open and the first one focused.
fn session(files: &[PathBuf], smooth_scroll: bool) -> anyhow::Result<Session> {
    let mut config = test_config();
    config.editor.bufferline = BufferLine::Always;
    config.editor.scrolloff = SCROLLOFF;
    config.editor.smooth_scroll = SmoothScrollConfig {
        enable: smooth_scroll,
        duration: Duration::from_millis(500),
        hide_cursor: false,
    };
    // each step waits for the editor to idle
    config.editor.idle_timeout = Duration::from_millis(5);
    let mut builder = AppBuilder::new().with_config(config);
    for path in files {
        builder = builder.with_file(path, None);
    }
    Ok(Session::new(builder.build()?))
}

/// The bufferline as last drawn.
fn bar(app: &Application) -> String {
    let width = app.editor.tree.area().width as usize;
    app.screen().content[..width]
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect()
}

fn label(path: &Path, modified: bool) -> String {
    let name = path.file_name().unwrap().to_string_lossy();
    format!(" {name}{} ", if modified { "[+]" } else { "" })
}

/// The columns of the focused buffer's tab in the bar.
fn focused_tab(app: &Application) -> std::ops::Range<usize> {
    let doc = doc!(app.editor);
    let label = label(doc.path().unwrap(), doc.is_modified());
    let bar = bar(app);
    let start = bar
        .find(&label)
        .unwrap_or_else(|| panic!("{label:?} is not whole in {bar:?}"));
    let start = bar[..start].chars().count();
    start..start + label.len()
}

/// Asserts that the focused buffer's tab is drawn whole, `SCROLLOFF` columns away from an edge
/// that more tabs lie beyond, and that a `…` marks exactly those edges.
fn assert_in_view(app: &Application) {
    let bar = bar(app);
    let width = app.editor.tree.area().width as usize;
    let tab = focused_tab(app);
    let docs: Vec<_> = app.editor.documents().collect();
    let focused = doc!(app.editor).id();
    let first = label(docs[0].path().unwrap(), docs[0].is_modified());
    let last = docs.last().unwrap();
    let last = label(last.path().unwrap(), last.is_modified());

    assert_ne!(bar.starts_with('…'), bar.starts_with(&first), "{bar:?}");
    assert_ne!(bar.ends_with('…'), bar.ends_with(&last), "{bar:?}");
    if docs[0].id() != focused {
        assert!(tab.start >= SCROLLOFF, "{tab:?} in {bar:?}");
    }
    if docs.last().unwrap().id() != focused {
        assert!(tab.end + SCROLLOFF <= width, "{tab:?} in {bar:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_focused_tab_stays_in_view() -> anyhow::Result<()> {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut session = session(&files(dir.path(), 20)?, false)?;
    session.keys("").await?;
    assert_in_view(&session.app);
    for keys in ["gn"; 20].into_iter().chain(["gp"; 20]) {
        session.keys(keys).await?;
        assert_in_view(&session.app);
    }
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bar_moves_no_further_than_needed() -> anyhow::Result<()> {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut session = session(&files(dir.path(), 20)?, false)?;
    session.keys("").await?;
    let start = bar(&session.app);
    // the second and third tabs are in view
    session.keys("gngn").await?;
    assert_eq!(bar(&session.app), start);
    // the fourth one ends at the edge
    session.keys("gn").await?;
    let width = session.app.editor.tree.area().width as usize;
    assert_eq!(focused_tab(&session.app).end + SCROLLOFF, width);
    let scrolled = bar(&session.app);
    session.keys("gp").await?;
    assert_eq!(bar(&session.app), scrolled);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_tabs_beyond_the_bar_scrolls_it_back() -> anyhow::Result<()> {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let files = files(dir.path(), 20)?;
    let mut session = session(&files, false)?;
    // the bar at its far end, the focused tab in the middle
    session.keys("gpgpgp").await?;
    assert!(bar(&session.app).ends_with(&label(&files[19], false)));
    session
        .keys(&format!(":bc {}<ret>", files[19].display()))
        .await?;
    assert_in_view(&session.app);
    assert!(bar(&session.app).ends_with(&label(&files[18], false)));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wider_tab_stays_in_view() -> anyhow::Result<()> {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut session = session(&files(dir.path(), 20)?, false)?;
    session.keys("gp").await?;
    // `[+]` widens the tab at the edge
    session.keys("itext<esc>").await?;
    assert!(doc!(session.app.editor).is_modified());
    assert_in_view(&session.app);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_narrower_bar_keeps_the_tab_in_view() -> anyhow::Result<()> {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut session = session(&files(dir.path(), 20)?, false)?;
    session.keys("gpgpgp").await?;
    let width = session.app.editor.tree.area().width;
    session.keys("<space>U").await?;
    assert!(session.app.editor.tree.area().width < width);
    assert_in_view(&session.app);
    session.keys("<space>U").await?;
    assert_eq!(session.app.editor.tree.area().width, width);
    assert_in_view(&session.app);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bar_glides() -> anyhow::Result<()> {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let files = files(dir.path(), 20)?;
    let mut session = session(&files, true)?;
    session.keys("").await?;
    let start = bar(&session.app);
    // to the last tab, at the far end
    session.send("gp")?;
    session.run_for(Duration::from_millis(30)).await;
    let between = bar(&session.app);
    session.wait(Duration::from_millis(700)).await;
    let end = bar(&session.app);
    assert!(
        between.starts_with('…') && between.ends_with('…'),
        "{between:?}"
    );
    assert_ne!(between, start);
    assert_ne!(between, end);
    assert!(end.ends_with(&label(&files[19], false)), "{end:?}");
    assert_in_view(&session.app);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a check"]
async fn measure_switching_buffers() -> anyhow::Result<()> {
    const BUFFERS: usize = 1000;
    const STEPS: u32 = 400;
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut session = session(&files(dir.path(), BUFFERS)?, false)?;
    session.keys("").await?;
    for bufferline in ["never", "always", "never", "always"] {
        session
            .keys(&format!(":set bufferline {bufferline}<ret>"))
            .await?;
        let start = Instant::now();
        session.keys(&"gn".repeat(STEPS as usize)).await?;
        println!(
            "{BUFFERS} buffers, bufferline {bufferline}: {:?} a step",
            start.elapsed() / STEPS
        );
    }
    session.quit().await
}
