//! The run a compilation buffer shows.
//!
//! A compilation buffer is a document holding the output of a command, like `cargo build`, as it
//! arrives. The file positions in it, its loci, are diagnostics that open their files.

use std::{path::PathBuf, time::Instant};

use helix_stdx::process::ProcessGroup;

use crate::ViewId;

/// Which command a run is, so that running it again knows what to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The language's `compile-command`.
    Compile,
    /// The language's `test-command`.
    Test,
    /// A command given by hand.
    Any,
}

/// The run a compilation buffer shows.
#[derive(Debug)]
pub struct Compilation {
    pub kind: Kind,
    /// The command line the shell runs.
    pub command: String,
    /// The directory it runs in, which relative loci are looked up from.
    pub dir: PathBuf,
    /// The language whose commands the buffer's `:compile` and `:compile-test` run.
    pub language: Option<String>,
    /// Counts the runs of the buffer, so that output of an earlier run is dropped.
    pub run: u64,
    pub started: Instant,
    /// The running command, stopped when dropped; `None` once it ended.
    pub process: Option<ProcessGroup>,
    /// The split the command was run from, which loci open in.
    pub origin: Option<ViewId>,
    /// Where the locus selected or opened last starts, which `]q` and `[q` go on from while the
    /// buffer is hidden.
    pub visited: Option<usize>,
}

impl Compilation {
    /// How the buffer is called, like `[compilation] cargo build`.
    pub fn display_name(&self) -> String {
        format!("[compilation] {}", self.command)
    }
}
