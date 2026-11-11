use helix_view::{doc, document::Mode};

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn blocks_follow_motions() -> anyhow::Result<()> {
    test((
        "#[a|]#bcd\nefgh\nijkl\n",
        "<A-v>jjl",
        "#(ab|)#cd\n#(ef|)#gh\n#[ij|]#kl\n",
    ))
    .await?;
    // Back over the anchor, the block flips.
    test(("ab#[c|]#d\nefgh\n", "<A-v>jhh", "#(|abc)#d\n#[|efg]#h\n")).await?;
    // Flipping swaps the corners.
    test((
        "#[a|]#bcd\nefgh\n",
        "<A-v>jl<A-;>",
        "#[|ab]#cd\n#(|ef)#gh\n",
    ))
    .await?;
    // Short lines are left out.
    test((
        "ab#[c|]#d\nx\nefgh\n",
        "<A-v>jjl",
        "ab#(cd|)#\nx\nef#[gh|]#\n",
    ))
    .await?;
    // The cursor on a short line takes the block to its end, as in Vim.
    test((
        "ab#[c|]#d\nefgh\nij\n",
        "<A-v>jj",
        "a#(|bc)#d\ne#(|fg)#h\ni#[|j]#\n",
    ))
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn line_ends_make_blocks_ragged() -> anyhow::Result<()> {
    test((
        "#[l|]#et a = 1;\nab\nlet beta = 22;\n",
        "<A-v>jjgl",
        "#(let a = 1;|)#\n#(ab|)#\n#[let beta = 22;|]#\n",
    ))
    .await?;
    // Up and down, they stay ragged.
    test((
        "#[l|]#et a = 1;\nab\nlet beta = 22;\n",
        "<A-v>jjglk",
        "#(let a = 1;|)#\n#[ab|]#\nlet beta = 22;\n",
    ))
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_act_on_the_block() -> anyhow::Result<()> {
    test(("#[a|]#bcd\nefgh\n", "<A-v>jld", "#(c|)#d\n#[g|]#h\n")).await?;
    test((
        "#[a|]#bcd\nefgh\n",
        "<A-v>jl<esc>",
        "#(ab|)#cd\n#[ef|]#gh\n",
    ))
    .await?;
    test((
        "#[a|]#bcd\nefgh\n",
        "<A-v>jlix<esc>",
        "x#(|ab)#cd\nx#[|ef]#gh\n",
    ))
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn other_commands_end_block_select() -> anyhow::Result<()> {
    let app = AppBuilder::new()
        .with_input_text("#[a|]#bcd\nebgh\n")
        .with_config(test_config())
        .build()?;
    let mut session = Session::new(app);
    session.keys("<A-v>jl").await?;
    assert!(session.app.editor.block_select.is_some());
    let shows_blk = |app: &helix_term::application::Application| {
        app.screen()
            .content
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>()
            .contains(" BLK ")
    };
    assert!(shows_blk(&session.app));
    // Searching adds a range, as in select mode.
    session.keys("/g<ret>").await?;
    assert!(session.app.editor.block_select.is_none());
    session.keys("<esc>gg<A-v>jl").await?;
    assert!(session.app.editor.block_select.is_some());
    session.keys("sb<ret>").await?;
    assert!(session.app.editor.block_select.is_none());
    assert_eq!(session.app.editor.mode, Mode::Select);
    assert_eq!(
        doc!(session.app.editor)
            .selection(session.app.editor.tree.focus)
            .len(),
        2
    );
    assert!(!shows_blk(&session.app));
    // A-v toggles it off, back to normal mode.
    session.keys("<esc><A-v><A-v>").await?;
    assert!(session.app.editor.block_select.is_none());
    assert_eq!(session.app.editor.mode, Mode::Normal);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_go_by_lines_of_text_through_soft_wrap() -> anyhow::Result<()> {
    let mut config = test_config();
    config.editor.soft_wrap.enable = Some(true);
    let long = "x".repeat(300);
    test_with_config(
        AppBuilder::new().with_config(config),
        (
            format!("#[a|]#{long}\nyz\n"),
            "<A-v>j",
            format!("#(a|)#{long}\n#[y|]#z\n"),
        ),
    )
    .await
}
