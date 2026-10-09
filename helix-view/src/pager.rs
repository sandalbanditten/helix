//! Documents shown as a pager shows terminal output, like `less -R`: their escape sequences and
//! overstrikes become styles, and nothing edits them.

use std::ops::Range;

use crate::{
    graphics::{Modifier, Style, UnderlineStyle},
    sgr,
};

/// What a document shown in a pager has besides its text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// The styles of char ranges of the text, in order and apart; other chars have none.
    pub styles: Vec<(Range<usize>, Style)>,
}

/// Whether `output` has formatting for [`format`] to take out.
pub fn is_formatted(output: &str) -> bool {
    output.contains(['\x1b', '\x08'])
}

/// The text terminal output `output` shows, and the styles of its chars. SGR sequences like
/// `ESC [ 1;31 m` become styles and other escape sequences go. Overstrikes, which `man` and the
/// like use, become bold (`a BS a`) or underlined (`_ BS a`) text.
pub fn format(output: &str) -> (String, Page) {
    let mut text = Text::default();
    let mut style = Style::default();
    let mut input = output.chars().peekable();
    while let Some(c) = input.next() {
        match c {
            '\x1b' => match input.next() {
                // A control sequence: parameters and intermediates up to a final byte.
                Some('[') => {
                    let mut params = String::new();
                    for c in input.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            if c == 'm' {
                                style = sgr::apply(style, &params);
                            }
                            break;
                        }
                        params.push(c);
                    }
                }
                // An operating system command, like a hyperlink, up to BEL or ST.
                Some(']') => {
                    while let Some(c) = input.next() {
                        if c == '\x07' || (c == '\x1b' && input.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                // Designates a character set.
                Some('(' | ')' | '*' | '+') => {
                    input.next();
                }
                _ => {}
            },
            '\x08' => {
                let Some(&after) = input.peek().filter(|&&after| after != '\n') else {
                    continue;
                };
                let Some((before, before_style)) = text.pop() else {
                    continue;
                };
                input.next();
                let underlined = before_style.underline_style(UnderlineStyle::Line);
                let (c, style) = if before == after {
                    (after, before_style.add_modifier(Modifier::BOLD))
                } else if before == '_' {
                    (after, underlined)
                } else if after == '_' {
                    (before, underlined)
                } else {
                    (after, before_style)
                };
                text.push(c, style);
            }
            c => text.push(c, style),
        }
    }
    let page = Page {
        styles: text.styles,
    };
    (text.text, page)
}

/// Text being formatted, with the styles of its chars.
#[derive(Default)]
struct Text {
    text: String,
    chars: usize,
    styles: Vec<(Range<usize>, Style)>,
}

impl Text {
    fn push(&mut self, c: char, style: Style) {
        let at = self.chars;
        match self.styles.last_mut() {
            Some((range, last)) if range.end == at && *last == style => range.end += 1,
            _ if style == Style::default() => {}
            _ => self.styles.push((at..at + 1, style)),
        }
        self.text.push(c);
        self.chars += 1;
    }

    /// Takes the last char off again, with its style. A line break stays.
    fn pop(&mut self) -> Option<(char, Style)> {
        if self.text.ends_with('\n') {
            return None;
        }
        let c = self.text.pop()?;
        self.chars -= 1;
        let style = match self.styles.last_mut() {
            Some((range, style)) if range.end > self.chars => {
                let style = *style;
                range.end -= 1;
                if range.start == range.end {
                    self.styles.pop();
                }
                style
            }
            _ => Style::default(),
        };
        Some((c, style))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::Color;

    #[test]
    fn colors_become_styles() {
        let (text, page) = format("\x1b[33mcommit 74c2ff7\x1b[m\n+added\x1b[K\n");
        assert_eq!(text, "commit 74c2ff7\n+added\n");
        let yellow = Style::default().fg(Color::Indexed(3));
        assert_eq!(page.styles, [(0..14, yellow)]);
    }

    #[test]
    fn overstrikes_become_bold_and_underlined() {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let underlined = Style::default().underline_style(UnderlineStyle::Line);
        let (text, page) = format("N\x08NA\x08AM\x08ME\x08E\n_\x08f_\x08i x\n");
        assert_eq!(text, "NAME\nfi x\n");
        assert_eq!(page.styles, [(0..4, bold), (5..7, underlined)]);
        // Both, as `_ BS x BS x`, and a char struck with another one, which shows.
        let (text, page) = format("_\x08x\x08x +\x08o");
        assert_eq!(text, "x o");
        assert_eq!(
            page.styles,
            [(0..1, underlined.add_modifier(Modifier::BOLD))]
        );
        // A backspace with nothing to strike goes.
        assert_eq!(format("\x08a\n\x08b\x08").0, "a\nb");
    }

    #[test]
    fn other_sequences_go() {
        let hyperlink = "\x1b]8;;https://helix-editor.com\x1b\\helix\x1b]8;;\x07";
        assert_eq!(format(hyperlink).0, "helix");
        assert_eq!(format("\x1b(Bplain\x1b=").0, "plain");
        assert!(!is_formatted("plain\ttext\n"));
        assert!(is_formatted("a\x08a"));
    }
}
