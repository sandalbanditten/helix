//! The diff view: two texts side by side, lined up by rows, with what changed highlighted.
//!
//! [`Alignment`] says which line of each text is shown on each row and what changed about the
//! lines. It comes from difftastic's structural diff ([`difftastic`]) or from Helix's own line
//! diff ([`builtin`]).

pub mod alignment;
pub mod builtin;
pub mod difftastic;

pub use alignment::{Alignment, Fillers, LineChange, Row, Side};
