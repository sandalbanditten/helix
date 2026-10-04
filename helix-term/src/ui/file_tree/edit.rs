//! Typing a name or path for a file operation, in a row of the tree or in the command line.

use std::path::PathBuf;

use helix_view::Editor;

use super::tree::NodeId;
use crate::{
    compositor::{Component, Context, Event},
    ctrl, key,
    ui::{completers, Prompt},
};

pub enum EditKind {
    /// Renaming `node`, at `path`, within its directory; the line holds its name.
    Rename { node: NodeId, path: PathBuf },
    /// Moving the entry at `path` to the path the line holds: relative to the workspace root,
    /// or `absolute`.
    Move { path: PathBuf, absolute: bool },
    /// Creating a file, or a `directory`, in `dir` (at `dir_path`), typed in an input row.
    Create {
        dir: NodeId,
        dir_path: PathBuf,
        directory: bool,
    },
    /// Deleting the entry at `path`, a `directory` or not, once the line says `y`.
    Delete { path: PathBuf, directory: bool },
    /// Searching for a file, moving the cursor to the matches while the line is typed.
    Search,
    /// Pasting `clip` into `dir` (at `dir_path`) under the name typed in an input row.
    Paste {
        clip: Clip,
        dir: NodeId,
        dir_path: PathBuf,
        directory: bool,
    },
}

impl EditKind {
    /// The directory of the input row a new entry is named in, and whether the entry is a
    /// directory.
    pub fn input(&self) -> Option<(NodeId, bool)> {
        match self {
            Self::Create { dir, directory, .. } | Self::Paste { dir, directory, .. } => {
                Some((*dir, *directory))
            }
            Self::Rename { .. } | Self::Move { .. } | Self::Delete { .. } | Self::Search => None,
        }
    }
}

/// An entry copied or cut, for pasting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    pub path: PathBuf,
    /// Whether pasting moves the entry rather than copying it.
    pub cut: bool,
}

/// Where the line of an edit is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// In place of the label of a row.
    Row,
    /// In the command line, below the statusline.
    CommandLine,
}

pub struct Edit {
    pub kind: EditKind,
    pub prompt: Prompt,
}

pub enum EditEvent {
    Submit,
    Cancel,
    Continue,
}

impl Edit {
    /// Starts editing `line`, with the cursor at its end.
    pub fn new(kind: EditKind, line: String, editor: &Editor) -> Self {
        let prompt = match &kind {
            EditKind::Move { .. } => {
                Prompt::new("move-to:".into(), None, completers::filename, |_, _, _| {})
            }
            EditKind::Delete { path, directory } => Prompt::new(
                format!(
                    "Delete {}{}? (y/n):",
                    path.display(),
                    if *directory { "/" } else { "" }
                )
                .into(),
                None,
                |_, _| Vec::new(),
                |_, _, _| {},
            ),
            EditKind::Search => {
                Prompt::new("file search:".into(), None, |_, _| Vec::new(), |_, _, _| {})
            }
            EditKind::Rename { .. } | EditKind::Create { .. } | EditKind::Paste { .. } => {
                Prompt::new("".into(), None, |_, _| Vec::new(), |_, _, _| {})
            }
        };
        Self {
            kind,
            prompt: prompt.with_line(line, editor),
        }
    }

    pub fn placement(&self) -> Placement {
        match self.kind {
            EditKind::Rename { .. } | EditKind::Create { .. } | EditKind::Paste { .. } => {
                Placement::Row
            }
            EditKind::Move { .. } | EditKind::Delete { .. } | EditKind::Search => {
                Placement::CommandLine
            }
        }
    }

    pub fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EditEvent {
        match event {
            Event::Key(key!(Enter)) => EditEvent::Submit,
            Event::Key(key!(Esc) | ctrl!('c')) => EditEvent::Cancel,
            // The prompt's own Enter and Esc would close a layer; everything else is editing.
            _ => {
                self.prompt.handle_event(event, cx);
                EditEvent::Continue
            }
        }
    }
}
