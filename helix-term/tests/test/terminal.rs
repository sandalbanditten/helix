use helix_term::application::Application;
use helix_view::{current_ref, doc, document::Mode, editor::TerminalConfig};

use super::*;

fn session() -> anyhow::Result<Session> {
    session_with(|_| {})
}

fn session_with(configure: impl FnOnce(&mut TerminalConfig)) -> anyhow::Result<Session> {
    let mut config = test_config();
    config.editor.terminal.shell = vec!["sh".to_owned()];
    configure(&mut config.editor.terminal);
    let app = AppBuilder::new().with_config(config).build()?;
    Ok(Session::new(app))
}

/// The text of each terminal, in the order of their splits.
fn terminals(app: &Application) -> Vec<String> {
    app.editor
        .tree
        .views()
        .map(|(view, _)| app.editor.document(view.doc).unwrap())
        .filter(|doc| doc.terminal.is_some())
        .map(|doc| doc.text().to_string())
        .collect()
}

fn text(app: &Application) -> String {
    doc!(app.editor).text().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_go_to_the_shell_until_esc() -> anyhow::Result<()> {
    let mut session = session()?;
    session.keys(":term<ret>").await?;
    assert_eq!(session.app.editor.mode, Mode::Insert);
    assert!(doc!(session.app.editor).terminal.is_some());
    session.send("echo hi there<ret>")?;
    session
        .until("the echo", |app| text(app).contains("\nhi there\n"))
        .await;

    // Normal mode reads the text, which edits don't change.
    session.keys("<esc>").await?;
    assert_eq!(session.app.editor.mode, Mode::Normal);
    session.keys("gg").await?;
    let (view, doc) = current_ref!(session.app.editor);
    assert_eq!(
        doc.selection(view.id)
            .primary()
            .cursor(doc.text().slice(..)),
        0
    );
    let before = text(&session.app);
    session.keys("dxdp").await?;
    assert_eq!(text(&session.app), before);

    // Typing goes on at the shell's cursor.
    session.keys("i").await?;
    assert_eq!(session.app.editor.mode, Mode::Insert);
    let (view, doc) = current_ref!(session.app.editor);
    let cursor = doc
        .selection(view.id)
        .primary()
        .cursor(doc.text().slice(..));
    assert_eq!(cursor, doc.terminal.as_ref().unwrap().cursor);
    session.send("echo again<ret>")?;
    session
        .until("the second echo", |app| text(app).contains("\nagain\n"))
        .await;
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_backslash_sends_the_next_key_as_it_is() -> anyhow::Result<()> {
    let mut session = session()?;
    session.keys(":term cat -v<ret>").await?;
    session.send("a<C-\\><esc><ret>")?;
    session
        .until("the escape", |app| text(app).contains("a^[\n"))
        .await;
    assert_eq!(session.app.editor.mode, Mode::Insert);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn ended_terminals_stay_and_run_again() -> anyhow::Result<()> {
    let mut session = session()?;
    session.keys(":term echo done; exit 3<ret>").await?;
    session
        .until("the end", |app| {
            text(app).contains("done\n") && text(app).contains("Exited with code 3 at ")
        })
        .await;
    assert_eq!(session.app.editor.mode, Mode::Normal);
    assert!(!doc!(session.app.editor)
        .terminal
        .as_ref()
        .unwrap()
        .is_running());
    session.keys("i").await?;
    session
        .until("the second end", |app| {
            text(app).matches("Exited with code 3 at ").count() == 2
        })
        .await;
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_terminal_hangs_up() -> anyhow::Result<()> {
    let mut session = session()?;
    session
        .keys(":term echo pid $$; exec sleep 1000<ret>")
        .await?;
    session
        .until("the pid", |app| text(app).contains("pid "))
        .await;
    let text = text(&session.app);
    let pid = text
        .split_once("pid ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .unwrap()
        .to_owned();
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
    session.keys("<esc>:bc<ret>").await?;
    session
        .until("the sleep to stop", |_| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map_or(true, |stat| stat.contains(") Z "))
        })
        .await;
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn programs_see_the_size_of_the_view() -> anyhow::Result<()> {
    let mut session = session()?;
    session.keys(":term stty size; sleep 1000<ret>").await?;
    let (view, doc) = current_ref!(session.app.editor);
    let size = format!("{} {}\n", view.inner_height(), view.inner_width(doc) - 1);
    session
        .until("the size", |app| text(app).starts_with(&size))
        .await;
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn old_lines_go() -> anyhow::Result<()> {
    let mut session = session_with(|terminal| terminal.scrollback = 1000)?;
    session.keys(":term seq 1 3000<ret>").await?;
    session
        .until("the end", |app| text(app).contains("Finished at "))
        .await;
    let text = text(&session.app);
    assert!(text.lines().count() <= 1100 + 3, "{}", text.lines().count());
    assert!(text.contains("\n3000\n"));
    assert!(!text.starts_with("1\n"));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn terminals_open_in_splits() -> anyhow::Result<()> {
    let mut session = session()?;
    session.keys(":vterm echo left<ret>").await?;
    session.keys("<esc>:hterm echo below<ret>").await?;
    session
        .until("both commands", |app| {
            let texts = terminals(app);
            texts.len() == 2 && texts[0].starts_with("left\n") && texts[1].starts_with("below\n")
        })
        .await;
    assert_eq!(session.app.editor.tree.views().count(), 3);
    // `C-w t` and `C-w T` open shells in splits, `Space t` in the focused one.
    session.keys("<esc><C-w>t<esc><C-w>T").await?;
    assert_eq!(session.app.editor.tree.views().count(), 5);
    session.keys("<esc><space>t").await?;
    assert_eq!(session.app.editor.tree.views().count(), 5);
    assert_eq!(session.app.editor.documents().count(), 6);
    assert_eq!(terminals(&session.app).len(), 4);
    assert_eq!(session.app.editor.mode, Mode::Insert);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn terminals_open_beside_the_buffer() -> anyhow::Result<()> {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let dir = helix_stdx::path::canonicalize(dir.path());
    let file = dir.join("file.txt");
    std::fs::write(&file, "text\n")?;
    let mut config = test_config();
    config.editor.terminal.shell = vec!["sh".to_owned()];
    let app = AppBuilder::new()
        .with_file(&file, None)
        .with_config(config)
        .build()?;
    let mut session = Session::new(app);
    session.keys("<space>T").await?;
    session.send("pwd<ret>")?;
    let shown = format!("\n{}\n", dir.display());
    session
        .until("the directory", |app| text(app).contains(&shown))
        .await;
    session.quit().await
}

/// Times typing and output, in an optimized build:
/// `cargo test --profile integration --features integration --test integration terminal::measure -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a check"]
async fn measure() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    let mut session = session()?;
    session.keys(":term<ret>").await?;
    session.send("PS1='$ '<ret>")?;
    session
        .until("the prompt", |app| text(app).contains("\n$ "))
        .await;
    let mut slowest = Duration::ZERO;
    let start = Instant::now();
    for _ in 0..100 {
        let typed = Instant::now();
        let shown = text(&session.app).matches('x').count() + 1;
        session.send("x")?;
        while text(&session.app).matches('x').count() < shown {
            session.run_for(Duration::from_micros(200)).await;
        }
        slowest = slowest.max(typed.elapsed());
    }
    println!(
        "a key to its echo: {:?} on average, {slowest:?} at most",
        start.elapsed() / 100
    );

    let start = Instant::now();
    session.keys("<esc>:term seq 1 1000000<ret>").await?;
    while !text(&session.app).contains("Finished at ") {
        session.run_for(Duration::from_millis(1)).await;
    }
    println!("1M lines: {:?}", start.elapsed());
    session.quit().await
}
