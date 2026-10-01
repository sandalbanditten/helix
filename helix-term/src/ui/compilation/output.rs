//! The text of a compilation buffer: the output of its command, cleaned up line by line, between
//! a header and a footer.

use std::{path::Path, time::Duration};

use jiff::Zoned;

/// Splits output into lines as it arrives, holding back a line until it ends.
#[derive(Debug, Default)]
pub struct Lines {
    partial: Vec<u8>,
}

impl Lines {
    /// The lines that `bytes` ends, cleaned up and each ending in a line break.
    pub fn push(&mut self, bytes: &[u8]) -> String {
        let mut text = String::new();
        let mut rest = bytes;
        while let Some(end) = rest.iter().position(|&byte| byte == b'\n') {
            self.partial.extend_from_slice(&rest[..end]);
            clean_into(&self.partial, &mut text);
            self.partial.clear();
            rest = &rest[end + 1..];
        }
        self.partial.extend_from_slice(rest);
        text
    }

    /// The line the output ended in without a line break, cleaned up, if there is one.
    pub fn finish(&mut self) -> String {
        let mut text = String::new();
        if !self.partial.is_empty() {
            clean_into(&self.partial, &mut text);
            self.partial.clear();
        }
        text
    }
}

/// Where a line is in an escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// After `ESC`, or after it and intermediate bytes like `(`.
    Start,
    /// In a control sequence, `ESC [ … final`.
    Control,
    /// In a string, like an OSC title `ESC ] … BEL`, until `BEL` or `ESC \`.
    String,
    /// After an `ESC` in a string.
    StringEnd,
}

/// Appends `line` to `text` the way a terminal shows it, with a line break: decoded, without
/// escape sequences and other control characters, and with what a carriage return let the rest
/// of the line overwrite.
fn clean_into(line: &[u8], text: &mut String) {
    let mut shown: Vec<char> = Vec::new();
    let mut column = 0;
    let mut escape = Escape::None;
    for c in String::from_utf8_lossy(line).chars() {
        escape = match (escape, c) {
            (Escape::None, '\x1b') => Escape::Start,
            (Escape::None, '\r') => {
                column = 0;
                Escape::None
            }
            (Escape::None, c) => {
                if c == '\t' || !c.is_control() {
                    if let Some(old) = shown.get_mut(column) {
                        *old = c;
                    } else {
                        shown.push(c);
                    }
                    column += 1;
                }
                Escape::None
            }
            (Escape::Start, '[') => Escape::Control,
            (Escape::Start, ']' | 'P' | 'X' | '^' | '_') => Escape::String,
            (Escape::Start, ' '..='/') => Escape::Start,
            (Escape::Start, _) => Escape::None,
            (Escape::Control, '@'..='~') => Escape::None,
            (Escape::Control, _) => Escape::Control,
            (Escape::String, '\x07') => Escape::None,
            (Escape::String, '\x1b') => Escape::StringEnd,
            (Escape::String, _) => Escape::String,
            (Escape::StringEnd, '\\') => Escape::None,
            (Escape::StringEnd, _) => Escape::String,
        };
    }
    text.extend(shown);
    text.push('\n');
}

/// The lines before the output: the command, then where and when it started.
pub fn header(command: &str, dir: &Path, started: &Zoned) -> String {
    let dir = helix_stdx::path::fold_home_dir(dir);
    format!(
        "{command}\nin {}, started {}\n\n",
        dir.display(),
        started.strftime("%H:%M:%S")
    )
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The command exited with a code, 0 when it succeeded.
    Exited(i32),
    /// A signal from elsewhere ended the command.
    Signal(i32),
    /// It was stopped from the editor.
    Killed,
}

impl End {
    pub fn failed(self) -> bool {
        self != Self::Exited(0)
    }

    /// Tells how the compilation ended, for the status line.
    pub fn status(self) -> String {
        match self {
            Self::Exited(0) => "Compilation finished".to_owned(),
            Self::Exited(code) => format!("Compilation exited with code {code}"),
            Self::Signal(signal) => format!("Compilation killed by signal {signal}"),
            Self::Killed => "Compilation killed".to_owned(),
        }
    }
}

/// The lines after the output: how and when the run ended, and after how long.
pub fn footer(end: End, ended: &Zoned, elapsed: Duration) -> String {
    let how = match end {
        End::Exited(0) => "Finished".to_owned(),
        End::Exited(code) => format!("Exited with code {code}"),
        End::Signal(signal) => format!("Killed by signal {signal}"),
        End::Killed => "Killed".to_owned(),
    };
    format!(
        "\n{how} at {} after {}\n",
        ended.strftime("%H:%M:%S"),
        duration(elapsed)
    )
}

/// A duration as precise as it is worth: `2.81 s`, `12.3 s` or `1:02:03`.
fn duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.2} s")
    } else if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let secs = elapsed.as_secs();
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(line: &str) -> String {
        let mut text = String::new();
        clean_into(line.as_bytes(), &mut text);
        text
    }

    #[test]
    fn lines_are_held_back_until_they_end() {
        let mut lines = Lines::default();
        assert_eq!(
            lines.push(b"   Compiling demo\nerror[E0425]: can"),
            "   Compiling demo\n"
        );
        assert_eq!(lines.push(b"not find"), "");
        assert_eq!(
            lines.push(b" value\r\n\nwarn"),
            "error[E0425]: cannot find value\n\n"
        );
        assert_eq!(lines.finish(), "warn\n");
        assert_eq!(lines.finish(), "");
        // A character split between reads is decoded whole.
        let arrow = "┌─ main.typ:5:12\n".as_bytes();
        assert_eq!(lines.push(&arrow[..2]), "");
        assert_eq!(lines.push(&arrow[2..]), "┌─ main.typ:5:12\n");
    }

    #[test]
    fn lines_are_shown_as_a_terminal_shows_them() {
        // Colors, a title, a charset switch and the cursor hidden.
        assert_eq!(
            clean("\x1b[0m\x1b[1m\x1b[38;5;9merror[E0425]\x1b[0m: cannot find"),
            "error[E0425]: cannot find\n"
        );
        assert_eq!(
            clean("\x1b]0;cargo\x07done \x1b]8;;file:///a\x1b\\a\x1b]8;;\x1b\\"),
            "done a\n"
        );
        assert_eq!(clean("\x1b(Bplain\x1b[?25l"), "plain\n");
        // Progress drawn over itself, and other control characters.
        assert_eq!(
            clean("Downloading 10%\rDownloading 100%"),
            "Downloading 100%\n"
        );
        assert_eq!(clean("abcdef\rXY"), "XYcdef\n");
        assert_eq!(clean("crlf\r"), "crlf\n");
        assert_eq!(clean("a\tb\x07\x08c"), "a\tbc\n");
        // Invalid UTF-8 shows replaced.
        let mut text = String::new();
        clean_into(b"bad \xff byte", &mut text);
        assert_eq!(text, "bad \u{fffd} byte\n");
    }

    #[test]
    fn header_and_footer() {
        let at: Zoned = "2026-10-01T14:03:12+02:00[Europe/Copenhagen]"
            .parse()
            .unwrap();
        let dir = std::env::temp_dir().join("demo");
        assert_eq!(
            header("cargo build", &dir, &at),
            format!("cargo build\nin {}, started 14:03:12\n\n", dir.display())
        );
        for (end, elapsed, footer_text) in [
            (
                End::Exited(101),
                2.81,
                "\nExited with code 101 at 14:03:12 after 2.81 s\n",
            ),
            (
                End::Exited(0),
                12.34,
                "\nFinished at 14:03:12 after 12.3 s\n",
            ),
            (End::Killed, 3723.0, "\nKilled at 14:03:12 after 1:02:03\n"),
            (
                End::Signal(9),
                0.5,
                "\nKilled by signal 9 at 14:03:12 after 0.50 s\n",
            ),
        ] {
            assert_eq!(
                footer(end, &at, Duration::from_secs_f64(elapsed)),
                footer_text
            );
        }
        assert_eq!(
            End::Exited(101).status(),
            "Compilation exited with code 101"
        );
        assert!(!End::Exited(0).failed());
        assert!(End::Killed.failed());
    }
}
