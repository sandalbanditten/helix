//! The keys of the panels docked beside the editor. One table of a panel drives both the key
//! handling and the `?` help, so the two cannot disagree.

use helix_view::{info::Info, input::KeyEvent};

use crate::key;

/// What a key of a panel does, with a short description for the help.
pub trait Action: Copy + 'static {
    fn doc(self) -> &'static str;
}

pub struct Binding<A: 'static> {
    /// Key sequences that trigger `action`, each one or two keys long.
    keys: &'static [&'static [KeyEvent]],
    action: A,
}

pub const fn bind<A>(keys: &'static [&'static [KeyEvent]], action: A) -> Binding<A> {
    Binding { keys, action }
}

/// The key table of a panel.
pub struct Bindings<A: 'static>(pub &'static [Binding<A>]);

pub enum Lookup<A> {
    Action(A),
    /// The start of a longer sequence.
    Prefix,
    Unbound,
}

impl<A: Action> Bindings<A> {
    pub fn lookup(&self, sequence: &[KeyEvent]) -> Lookup<A> {
        let mut prefix = false;
        for binding in self.0 {
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

    /// The keys of the panel called `title`, or the ones continuing `prefix`, as an infobox.
    pub fn info(&self, prefix: &[KeyEvent], title: &'static str) -> Info {
        let body = self.rows(prefix);
        // The sequences share their prefixes, and so their names, with the editor's.
        let title = match prefix {
            [] => title,
            [key!('g')] => "Goto",
            [key!('z')] => "View",
            _ => "",
        };
        Info::new(title, &body)
    }

    /// The rows of the help: the keys continuing `prefix`, and what they do.
    pub fn rows(&self, prefix: &[KeyEvent]) -> Vec<(String, &'static str)> {
        self.0
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
            .collect()
    }

    /// Whether every key sequence triggers one action only.
    #[cfg(test)]
    pub fn are_unique(&self) -> bool {
        let sequences: Vec<_> = self.0.iter().flat_map(|binding| binding.keys).collect();
        sequences
            .iter()
            .enumerate()
            .all(|(i, keys)| !sequences[i + 1..].contains(keys))
    }
}

fn sequence(keys: &[KeyEvent]) -> String {
    keys.iter().map(ToString::to_string).collect()
}
