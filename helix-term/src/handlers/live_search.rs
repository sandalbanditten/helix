//! The search being made, whose matches the scrollbars mark: it lasts while searching and stepping
//! through matches, and ends with the first other command.

use helix_event::register_hook;
use helix_view::handlers::Handlers;

use crate::{commands::MappableCommand, events::PostCommand};

/// The commands that keep the search going.
const SEARCH_COMMANDS: &[&str] = &[
    "search",
    "rsearch",
    "search_next",
    "search_prev",
    "extend_search_next",
    "extend_search_prev",
    "search_selection",
    "search_selection_detect_word_boundaries",
    "make_search_word_bounded",
];

pub(super) fn register_hooks(_handlers: &Handlers) {
    register_hook!(move |event: &mut PostCommand<'_, '_>| {
        let searching = matches!(
            event.command,
            MappableCommand::Static { name, .. } if SEARCH_COMMANDS.contains(name)
        );
        if !searching {
            event.cx.editor.live_search = None;
        }
        Ok(())
    });
}
