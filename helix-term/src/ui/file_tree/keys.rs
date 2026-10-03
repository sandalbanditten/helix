//! The keys of a focused file tree.

use helix_view::{info::Info, input::KeyEvent};

use crate::{
    ctrl, key,
    ui::panel_keys::{self, bind, Bindings},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Down,
    Up,
    Expand,
    Collapse,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    First,
    Last,
    AlignCenter,
    AlignTop,
    AlignBottom,
    Open,
    OpenHorizontal,
    OpenVertical,
    OpenExternally,
    Rename,
    MoveInWorkspace,
    Move,
    NewFile,
    NewDirectory,
    Delete,
    Copy,
    Cut,
    Paste,
    EditDirectory,
    EditTree,
    Search,
    NextMatch,
    PreviousMatch,
    Grow,
    Shrink,
    Fit,
    ToggleWidth,
    Help,
    Unfocus,
}

impl panel_keys::Action for Action {
    fn doc(self) -> &'static str {
        match self {
            Self::Down => "Move down",
            Self::Up => "Move up",
            Self::Expand => "Expand directory",
            Self::Collapse => "Collapse directory",
            Self::HalfPageDown => "Move half a page down",
            Self::HalfPageUp => "Move half a page up",
            Self::PageDown => "Move a page down",
            Self::PageUp => "Move a page up",
            Self::First => "Go to the first row",
            Self::Last => "Go to the last row",
            Self::AlignCenter => "Align the cursor row to the center",
            Self::AlignTop => "Align the cursor row to the top",
            Self::AlignBottom => "Align the cursor row to the bottom",
            Self::Open => "Open file or expand/collapse directory",
            Self::OpenHorizontal => "Open file in a horizontal split",
            Self::OpenVertical => "Open file in a vertical split",
            Self::OpenExternally => "Open in the default application",
            Self::Rename => "Rename",
            Self::MoveInWorkspace => "Move to a path in the workspace",
            Self::Move => "Move to a full path",
            Self::NewFile => "New file",
            Self::NewDirectory => "New directory",
            Self::Delete => "Delete for good",
            Self::Copy => "Copy",
            Self::Cut => "Cut",
            Self::Paste => "Paste",
            Self::EditDirectory => "Edit directory in dired",
            Self::EditTree => "Edit tree in dired",
            Self::Search => "Search for a file",
            Self::NextMatch => "Go to the next match",
            Self::PreviousMatch => "Go to the previous match",
            Self::Grow => "Widen the file tree",
            Self::Shrink => "Narrow the file tree",
            Self::Fit => "Fit the width to the widest row",
            Self::ToggleWidth => "Toggle the widest and narrowest width",
            Self::Help => "Show these keys",
            Self::Unfocus => "Return focus to the editor",
        }
    }
}

const BINDINGS: Bindings<Action> = Bindings(&[
    bind(&[&[key!('j')], &[key!(Down)]], Action::Down),
    bind(&[&[key!('k')], &[key!(Up)]], Action::Up),
    bind(&[&[key!('l')], &[key!(Right)]], Action::Expand),
    bind(&[&[key!('h')], &[key!(Left)]], Action::Collapse),
    bind(&[&[ctrl!('d')]], Action::HalfPageDown),
    bind(&[&[ctrl!('u')]], Action::HalfPageUp),
    bind(&[&[key!(PageDown)]], Action::PageDown),
    bind(&[&[key!(PageUp)]], Action::PageUp),
    bind(&[&[key!('g'), key!('g')], &[key!(Home)]], Action::First),
    bind(&[&[key!('g'), key!('e')], &[key!(End)]], Action::Last),
    bind(
        &[&[key!('z'), key!('z')], &[key!('z'), key!('c')]],
        Action::AlignCenter,
    ),
    bind(&[&[key!('z'), key!('t')]], Action::AlignTop),
    bind(&[&[key!('z'), key!('b')]], Action::AlignBottom),
    bind(&[&[key!(Enter)]], Action::Open),
    bind(&[&[ctrl!('s')]], Action::OpenHorizontal),
    bind(&[&[ctrl!('v')]], Action::OpenVertical),
    bind(&[&[key!('o')]], Action::OpenExternally),
    bind(&[&[key!('r')]], Action::Rename),
    bind(&[&[key!('R')]], Action::MoveInWorkspace),
    bind(&[&[ctrl!('r')]], Action::Move),
    bind(&[&[key!('a')]], Action::NewFile),
    bind(&[&[key!('A')]], Action::NewDirectory),
    bind(&[&[key!('d')]], Action::Delete),
    bind(&[&[key!('y')]], Action::Copy),
    bind(&[&[key!('x')]], Action::Cut),
    bind(&[&[key!('p')]], Action::Paste),
    bind(&[&[key!('e')]], Action::EditDirectory),
    bind(&[&[key!('E')]], Action::EditTree),
    bind(&[&[key!('/')]], Action::Search),
    bind(&[&[key!('n')]], Action::NextMatch),
    bind(&[&[key!('N')]], Action::PreviousMatch),
    bind(&[&[key!('+')]], Action::Grow),
    bind(&[&[key!('-')]], Action::Shrink),
    bind(&[&[key!('=')]], Action::Fit),
    bind(&[&[key!('|')]], Action::ToggleWidth),
    bind(&[&[key!('?')]], Action::Help),
    bind(&[&[key!(Esc)]], Action::Unfocus),
]);

pub type Lookup = panel_keys::Lookup<Action>;

pub fn lookup(sequence: &[KeyEvent]) -> Lookup {
    BINDINGS.lookup(sequence)
}

/// The keys of the file tree, or the ones continuing `prefix`, as an infobox.
pub fn info(prefix: &[KeyEvent]) -> Info {
    BINDINGS.info(prefix, "File tree")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_resolve() {
        assert!(matches!(lookup(&[key!('j')]), Lookup::Action(Action::Down)));
        assert!(matches!(lookup(&[key!('z')]), Lookup::Prefix));
        assert!(matches!(
            lookup(&[key!('z'), key!('c')]),
            Lookup::Action(Action::AlignCenter)
        ));
        assert!(matches!(lookup(&[key!('z'), key!('x')]), Lookup::Unbound));
        assert!(matches!(lookup(&[key!(' ')]), Lookup::Unbound));
    }

    #[test]
    fn every_sequence_is_bound_once() {
        assert!(BINDINGS.are_unique());
    }

    #[test]
    fn help_lists_the_keys() {
        let info = info(&[]);
        let first = info.text.lines().next().unwrap();
        assert!(first.starts_with("j, down "), "{first}");
        assert!(first.ends_with("  Move down"), "{first}");
        let info = super::info(&[key!('z')]);
        assert_eq!(info.title, "View");
        assert!(info.text.starts_with("z, c"), "{}", info.text);
    }
}
