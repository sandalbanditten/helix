//! The text of a compilation buffer: the output of its command as a terminal shows it, between a
//! header and a footer.

use std::{ops::Range, path::Path, time::Duration};

use helix_view::graphics::Style;
use jiff::Zoned;

/// Text as a terminal shows it, with the styles of its chars.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shown {
    pub text: String,
    /// The chars of `text`.
    pub chars: usize,
    /// The styles of char ranges of `text`, in order and apart; other chars have none.
    pub styles: Vec<(Range<usize>, Style)>,
}

impl Shown {
    /// Adds `c` in `style`.
    pub fn push(&mut self, c: char, style: Style) {
        let at = self.chars;
        match self.styles.last_mut() {
            Some((range, last)) if range.end == at && *last == style => range.end += 1,
            _ if style == Style::default() => {}
            _ => self.styles.push((at..at + 1, style)),
        }
        self.text.push(c);
        self.chars += 1;
    }

    /// Gives the chars from `start` on `style`.
    pub fn style(&mut self, start: usize, style: Style) {
        let end = self.chars;
        match self.styles.last_mut() {
            _ if start == end || style == Style::default() => {}
            Some((range, last)) if range.end == start && *last == style => range.end = end,
            _ => self.styles.push((start..end, style)),
        }
    }

    /// Adds `other` at the end.
    pub fn append(&mut self, other: &Shown) {
        let start = self.chars;
        let styles = other.styles.iter();
        self.styles
            .extend(styles.map(|(range, style)| (range.start + start..range.end + start, *style)));
        self.text.push_str(&other.text);
        self.chars += other.chars;
    }

    /// The chars and the bytes of the whole lines that `self` and `other` start with, alike in
    /// their text and styles.
    pub fn common_lines(&self, other: &Shown) -> (usize, usize) {
        let (mut chars, mut bytes) = (0, 0);
        let (mut styles, mut other_styles) = (&self.styles[..], &other.styles[..]);
        let lines = self.text.split_inclusive('\n');
        for (line, other_line) in lines.zip(other.text.split_inclusive('\n')) {
            if line != other_line || !line.ends_with('\n') {
                break;
            }
            let end = chars + line.chars().count();
            let (own, rest) =
                styles.split_at(styles.partition_point(|(range, _)| range.start < end));
            let (others, other_rest) =
                other_styles.split_at(other_styles.partition_point(|(range, _)| range.start < end));
            if own != others {
                break;
            }
            (styles, other_styles) = (rest, other_rest);
            chars = end;
            bytes += line.len();
        }
        (chars, bytes)
    }

    /// Splits off the text from the char `chars`, at the byte `bytes`, which starts a line.
    pub fn split_off(&mut self, chars: usize, bytes: usize) -> Shown {
        let text = self.text.split_off(bytes);
        let at = self
            .styles
            .partition_point(|(range, _)| range.start < chars);
        let styles = self.styles.split_off(at);
        let rest = Shown {
            text,
            chars: self.chars - chars,
            styles: styles
                .into_iter()
                .map(|(range, style)| (range.start - chars..range.end - chars, style))
                .collect(),
        };
        self.chars = chars;
        rest
    }
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

    use helix_view::graphics::Color;

    fn plain(text: &str) -> Shown {
        Shown {
            text: text.to_owned(),
            chars: text.chars().count(),
            styles: Vec::new(),
        }
    }

    #[test]
    fn lines_alike_and_split_off() {
        let red = Style::default().fg(Color::Indexed(1));
        let mut old = plain("one\ntwö\nthree");
        old.styles = vec![(4..7, red)];
        let mut new = plain("one\ntwö\nthree\nfour");
        assert_eq!(old.common_lines(&new), (4, 4));
        new.styles = vec![(4..7, red)];
        // The last line has no line break yet, so it may still change.
        assert_eq!(old.common_lines(&new), (8, 9));
        new.push('!', red);
        assert_eq!(new.styles, [(4..7, red), (18..19, red)]);
        let rest = new.split_off(8, 9);
        assert_eq!((new.text.as_str(), new.chars), ("one\ntwö\n", 8));
        assert_eq!((rest.text.as_str(), rest.chars), ("three\nfour!", 11));
        assert_eq!(rest.styles, [(10..11, red)]);
        new.append(&rest);
        assert_eq!(new.styles, [(4..7, red), (18..19, red)]);
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
