//! What searches share, in the editor and in the undo tree: the regex of a query and the prompt it
//! is typed in.

use std::{borrow::Cow, fmt::Display, sync::Arc};

use helix_stdx::rope;
use helix_view::{editor::SearchConfig, Editor};

use crate::{
    compositor::{self, Compositor},
    job::{self, Callback},
    ui::{
        prompt::{Completion, PromptEvent},
        Popup, Prompt, Text,
    },
};

/// The register searches are kept in, unless another one is chosen.
pub const REGISTER: char = '/';

/// Why a query is no regex.
pub type RegexError = Box<dyn std::error::Error + Send + Sync>;

/// The regex a search for `query` looks for, as `config` says.
pub fn regex(query: &str, config: &SearchConfig, crlf: bool) -> Result<rope::Regex, RegexError> {
    let case_insensitive = config.smart_case && !query.chars().any(char::is_uppercase);
    rope::RegexBuilder::new()
        .syntax(
            rope::Config::new()
                .case_insensitive(case_insensitive)
                .multi_line(true)
                .crlf(crlf),
        )
        .build(query)
        .map_err(Into::into)
}

/// Completes the line of a search with the searches kept in `register` that it begins.
pub fn completion(
    editor: &Editor,
    register: char,
) -> impl FnMut(&Editor, &str) -> Vec<Completion> + 'static {
    let mut searches: Vec<String> =
        editor
            .registers
            .read(register, editor)
            .map_or(Vec::new(), |searches| {
                searches
                    .take(200)
                    .map(|search| search.to_string())
                    .collect()
            });
    searches.sort_unstable();
    searches.dedup();
    move |_, line| {
        searches
            .iter()
            .filter(|search| search.starts_with(line))
            .map(|search| (0.., search.clone().into()))
            .collect()
    }
}

/// A prompt labelled `label` for a regex, which it highlights, keeping what is entered in
/// `register`.
pub fn regex_prompt(
    label: Cow<'static, str>,
    register: Option<char>,
    completion_fn: impl FnMut(&Editor, &str) -> Vec<Completion> + 'static,
    callback_fn: impl FnMut(&mut compositor::Context, &str, PromptEvent) + 'static,
    editor: &Editor,
) -> Prompt {
    let mut prompt = Prompt::new(label, register, completion_fn, callback_fn)
        .with_language("regex", Arc::clone(&editor.syn_loader));
    prompt.recalculate_completion(editor);
    prompt
}

/// Shows why a query is no regex, `error`, in a popup above the command line.
pub fn show_invalid(cx: &mut compositor::Context, error: impl Display + Send + 'static) {
    let callback = async move {
        let call: job::Callback = Callback::EditorCompositor(Box::new(
            move |editor: &mut Editor, compositor: &mut Compositor| {
                let contents = Text::new(format!("{error}"));
                let size = compositor.size();
                let popup = Popup::new("invalid-regex", contents)
                    .position(Some(helix_core::Position::new(
                        size.height as usize - 2, // 2 = statusline + commandline
                        editor.tree.area().x as usize,
                    )))
                    .auto_close(true);
                compositor.replace_or_push("invalid-regex", popup);
            },
        ));
        Ok(call)
    };
    cx.jobs.callback(callback);
}

#[cfg(test)]
mod tests {
    use helix_stdx::rope::RegexInput;

    use super::*;

    #[test]
    fn searches_are_smart_case_regexes_over_lines() {
        let config = SearchConfig::default();
        let matches = |query: &str, text: &str| {
            regex(query, &config, false)
                .unwrap()
                .is_match(RegexInput::new(text))
        };
        assert!(matches("x1[23]", "x13"));
        assert!(matches("todo", "// TODO"));
        assert!(!matches("Todo", "// TODO"));
        assert!(matches("^b$", "a\nb\nc"));
        assert!(regex("foo(", &config, false).is_err());
        let exact = SearchConfig {
            smart_case: false,
            ..SearchConfig::default()
        };
        assert!(!regex("todo", &exact, false)
            .unwrap()
            .is_match(RegexInput::new("TODO")));
    }
}
