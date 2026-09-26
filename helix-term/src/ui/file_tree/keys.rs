//! The keys of a focused file tree. The one table below drives both the key handling and the
//! `?` help, so the two cannot disagree.

use helix_view::{info::Info, input::KeyEvent};

use crate::{ctrl, key};

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
    Search,
    NextMatch,
    PreviousMatch,
    Grow,
    Shrink,
    Fit,
    Help,
    Unfocus,
}

impl Action {
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
            Self::Search => "Search for a file",
            Self::NextMatch => "Go to the next match",
            Self::PreviousMatch => "Go to the previous match",
            Self::Grow => "Widen the file tree",
            Self::Shrink => "Narrow the file tree",
            Self::Fit => "Fit the width to the widest row",
            Self::Help => "Show these keys",
            Self::Unfocus => "Return focus to the editor",
        }
    }
}

struct Binding {
    /// Key sequences that trigger `action`, each one or two keys long.
    keys: &'static [&'static [KeyEvent]],
    action: Action,
}

const fn bind(keys: &'static [&'static [KeyEvent]], action: Action) -> Binding {
    Binding { keys, action }
}

const BINDINGS: &[Binding] = &[
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
    bind(&[&[key!('/')]], Action::Search),
    bind(&[&[key!('n')]], Action::NextMatch),
    bind(&[&[key!('N')]], Action::PreviousMatch),
    bind(&[&[key!('+')]], Action::Grow),
    bind(&[&[key!('-')]], Action::Shrink),
    bind(&[&[key!('=')]], Action::Fit),
    bind(&[&[key!('?')]], Action::Help),
    bind(&[&[key!(Esc)]], Action::Unfocus),
];

pub enum Lookup {
    Action(Action),
    /// The start of a longer sequence.
    Prefix,
    Unbound,
}

pub fn lookup(sequence: &[KeyEvent]) -> Lookup {
    let mut prefix = false;
    for binding in BINDINGS {
        for keys in binding.keys {
            if *keys == sequence {
                return Lookup::Action(binding.action);
            }
            prefix |= keys.starts_with(sequence);
        }
    }
    if prefix {
        Lookup::Prefix
    } else {
        Lookup::Unbound
    }
}

/// The keys of the file tree, or the ones continuing `prefix`, as an infobox.
pub fn info(prefix: &[KeyEvent]) -> Info {
    let body: Vec<_> = BINDINGS
        .iter()
        .filter_map(|binding| {
            let keys: Vec<_> = binding
                .keys
                .iter()
                .filter(|keys| keys.len() > prefix.len() && keys.starts_with(prefix))
                .map(|keys| sequence(&keys[prefix.len()..]))
                .collect();
            (!keys.is_empty()).then(|| (keys.join(", "), binding.action.doc()))
        })
        .collect();
    // The sequences share their prefixes, and so their names, with the editor's.
    let title = match prefix {
        [] => "File tree",
        [key!('g')] => "Goto",
        [key!('z')] => "View",
        _ => "",
    };
    Info::new(title, &body)
}

fn sequence(keys: &[KeyEvent]) -> String {
    keys.iter().map(ToString::to_string).collect()
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
        let sequences: Vec<_> = BINDINGS.iter().flat_map(|binding| binding.keys).collect();
        for (i, keys) in sequences.iter().enumerate() {
            assert!(
                !sequences[i + 1..].contains(keys),
                "{keys:?} is bound twice"
            );
        }
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
