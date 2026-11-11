use helix_term::application::Application;
use helix_view::doc;

use super::*;

/// The rows of the screen as text.
fn rows(app: &Application) -> Vec<String> {
    let screen = app.screen();
    screen
        .content
        .chunks(screen.area.width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol.as_str()).collect())
        .collect()
}

fn shows(app: &Application, text: &str) -> bool {
    rows(app).iter().any(|row| row.contains(text))
}

fn session(start_screen: bool) -> anyhow::Result<Session> {
    let mut config = test_config();
    config.editor.start_screen = start_screen;
    let app = AppBuilder::new().with_config(config).build()?;
    Ok(Session::new(app))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_logo_shows_until_the_first_key_which_acts() -> anyhow::Result<()> {
    let mut session = session(true)?;
    session.keys("").await?;
    let version = helix_loader::VERSION;
    assert!(
        shows(&session.app, "|_|   |_|  \\___/"),
        "{:#?}",
        rows(&session.app)
    );
    assert!(shows(&session.app, &format!("{version} • ")));

    session.keys("ix").await?;
    assert!(!shows(&session.app, "|_|   |_|"));
    assert!(!shows(&session.app, version));
    assert_eq!(doc!(session.app.editor).text(), "x\n");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_logo_can_be_turned_off() -> anyhow::Result<()> {
    let mut session = session(false)?;
    session.keys("").await?;
    assert!(!shows(&session.app, "|_|   |_|"));
    session.quit().await
}
