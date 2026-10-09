#[cfg(feature = "integration")]
mod test {
    mod helpers;

    use helix_core::{syntax::config::AutoPairConfig, Selection};
    use helix_term::config::Config;

    use indoc::indoc;

    use self::helpers::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn hello_world() -> anyhow::Result<()> {
        test(("#[\n|]#", "ihello world<esc>", "hello world#[|\n]#")).await?;
        Ok(())
    }

    mod auto_pairs;
    mod auto_reload;
    mod bufferline;
    mod command_line;
    mod commands;
    mod conceal;
    mod diff_view;
    mod folding;
    mod inlay_hints;
    mod movement;
    mod pager;
    mod scrollbar;
    mod smooth_scroll;
    mod spelling;
    mod splits;
    mod undo_files;
    mod undo_tree;
}
