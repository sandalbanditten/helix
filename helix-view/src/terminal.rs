//! The terminal a terminal buffer shows: a command on a pseudo-terminal, which keys go to in
//! insert mode.

use std::{ops::Range, path::PathBuf, sync::mpsc::Sender, time::Instant};

use helix_core::ChangeSet;
use helix_stdx::{process::ProcessGroup, pty::Pty};

use crate::graphics::Style;

#[derive(Debug)]
pub struct Terminal {
    /// The program and its arguments.
    pub command: Vec<String>,
    /// What the buffer is called, unless the program gives a title.
    pub name: String,
    pub dir: PathBuf,
    pub title: Option<String>,
    /// Counts the runs of the buffer, so that output of an earlier run is dropped.
    pub run: u64,
    pub started: Instant,
    /// When keys last went to it.
    pub used: Instant,
    /// The running command, hung up on when dropped; `None` once it ended.
    pub process: Option<ProcessGroup>,
    pub pty: Option<Pty>,
    /// Where keys go, as the program reads them.
    pub input: Option<Sender<Vec<u8>>>,
    /// Whether cursor keys send `ESC O A` rather than `ESC [ A`.
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    /// Where the screen starts in the text, and where its cursor is.
    pub screen: usize,
    pub cursor: usize,
    /// Whether the next key goes to the program as it is.
    pub verbatim: bool,
    /// The colors of the output: the styles of char ranges, in order and apart.
    pub styles: Vec<(Range<usize>, Style)>,
}

impl Terminal {
    pub fn new(command: Vec<String>, name: String, dir: PathBuf) -> Self {
        let now = Instant::now();
        Self {
            command,
            name,
            dir,
            title: None,
            run: 0,
            started: now,
            used: now,
            process: None,
            pty: None,
            input: None,
            app_cursor: false,
            bracketed_paste: false,
            screen: 0,
            cursor: 0,
            verbatim: false,
            styles: Vec::new(),
        }
    }

    /// How the buffer is called, like `[term] fish`.
    pub fn display_name(&self) -> String {
        format!("[term] {}", self.title.as_deref().unwrap_or(&self.name))
    }

    pub fn is_running(&self) -> bool {
        self.process.is_some()
    }

    /// Sends `input` to the program, as if typed.
    pub fn send(&self, input: Vec<u8>) {
        if let Some(sender) = &self.input {
            let _ = sender.send(input);
        }
    }

    /// Maps the styles of the output over `changes` to the buffer.
    pub fn map(&mut self, changes: &ChangeSet) {
        crate::compilation::map_styles(&mut self.styles, changes);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(process) = &self.process {
            let _ = process.hang_up();
        }
    }
}
