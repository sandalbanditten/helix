//! The keys of a focused undo tree.

use helix_view::{info::Info, input::KeyEvent};

use crate::{
    ctrl, key,
    ui::panel_keys::{self, bind, Bindings},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Older,
    Newer,
    NewerSibling,
    OlderSibling,
    Undo,
    Redo,
    OlderWritten,
    NewerWritten,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    Newest,
    Oldest,
    AlignCenter,
    AlignTop,
    AlignBottom,
    Search,
    NextMatch,
    PreviousMatch,
    Grow,
    Shrink,
    Fit,
    Help,
    Keep,
    GoBack,
}

impl panel_keys::Action for Action {
    fn doc(self) -> &'static str {
        match self {
            Self::Older => "Older revision",
            Self::Newer => "Newer revision",
            Self::NewerSibling => "Newer sibling branch",
            Self::OlderSibling => "Older sibling branch",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::OlderWritten => "Older written revision",
            Self::NewerWritten => "Newer written revision",
            Self::HalfPageDown => "Move half a page down",
            Self::HalfPageUp => "Move half a page up",
            Self::PageDown => "Move a page down",
            Self::PageUp => "Move a page up",
            Self::Newest => "Go to the newest revision",
            Self::Oldest => "Go to the oldest revision",
            Self::AlignCenter => "Align the cursor row to the center",
            Self::AlignTop => "Align the cursor row to the top",
            Self::AlignBottom => "Align the cursor row to the bottom",
            Self::Search => "Search changed text",
            Self::NextMatch => "Go to the next match",
            Self::PreviousMatch => "Go to the previous match",
            Self::Grow => "Widen the undo tree",
            Self::Shrink => "Narrow the undo tree",
            Self::Fit => "Fit the width to the widest row",
            Self::Help => "Show these keys",
            Self::Keep => "Keep revision",
            Self::GoBack => "Go back to the start",
        }
    }
}

const BINDINGS: Bindings<Action> = Bindings(&[
    bind(&[&[key!('j')], &[key!(Down)]], Action::Older),
    bind(&[&[key!('k')], &[key!(Up)]], Action::Newer),
    bind(&[&[key!('h')], &[key!(Left)]], Action::NewerSibling),
    bind(&[&[key!('l')], &[key!(Right)]], Action::OlderSibling),
    bind(&[&[key!('u')]], Action::Undo),
    bind(&[&[key!('U')]], Action::Redo),
    bind(&[&[key!('[')]], Action::OlderWritten),
    bind(&[&[key!(']')]], Action::NewerWritten),
    bind(&[&[ctrl!('d')]], Action::HalfPageDown),
    bind(&[&[ctrl!('u')]], Action::HalfPageUp),
    bind(&[&[key!(PageDown)]], Action::PageDown),
    bind(&[&[key!(PageUp)]], Action::PageUp),
    bind(&[&[key!('g'), key!('g')], &[key!(Home)]], Action::Newest),
    bind(&[&[key!('g'), key!('e')], &[key!(End)]], Action::Oldest),
    bind(
        &[&[key!('z'), key!('z')], &[key!('z'), key!('c')]],
        Action::AlignCenter,
    ),
    bind(&[&[key!('z'), key!('t')]], Action::AlignTop),
    bind(&[&[key!('z'), key!('b')]], Action::AlignBottom),
    bind(&[&[key!('/')]], Action::Search),
    bind(&[&[key!('n')]], Action::NextMatch),
    bind(&[&[key!('N')]], Action::PreviousMatch),
    bind(&[&[key!('+')]], Action::Grow),
    bind(&[&[key!('-')]], Action::Shrink),
    bind(&[&[key!('=')]], Action::Fit),
    bind(&[&[key!('?')]], Action::Help),
    bind(&[&[key!(Enter)]], Action::Keep),
    bind(&[&[key!(Esc)]], Action::GoBack),
]);

pub type Lookup = panel_keys::Lookup<Action>;

pub fn lookup(sequence: &[KeyEvent]) -> Lookup {
    BINDINGS.lookup(sequence)
}

/// The keys of the undo tree, or the ones continuing `prefix`, as an infobox.
pub fn info(prefix: &[KeyEvent]) -> Info {
    BINDINGS.info(prefix, "Undo tree")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sequence_is_bound_once() {
        assert!(BINDINGS.are_unique());
    }

    #[test]
    fn sequences_resolve() {
        assert!(matches!(
            lookup(&[key!('j')]),
            Lookup::Action(Action::Older)
        ));
        assert!(matches!(lookup(&[key!('g')]), Lookup::Prefix));
        assert!(matches!(
            lookup(&[key!('g'), key!('e')]),
            Lookup::Action(Action::Oldest)
        ));
        assert!(matches!(lookup(&[key!('x')]), Lookup::Unbound));
    }
}
