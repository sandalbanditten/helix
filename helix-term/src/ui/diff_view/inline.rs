//! A diff shown inline, as `git diff` shows one: a few unchanged lines around each hunk, then the
//! hunk's removed lines and its added ones, colored by syntax and by what changed. The undo tree
//! shows the changes of a revision like this.

use std::ops::Range;

use helix_core::{
    syntax::{HighlightEvent, Loader},
    unicode::width::UnicodeWidthChar,
    Rope, RopeSlice, Syntax,
};
use helix_view::{
    diff_view::{Alignment, LineChange, Side},
    graphics::Style,
    Theme,
};

use super::styles::Styles;

/// A line of an inline diff, with the styles of its char ranges.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    pub styles: Vec<(Range<usize>, Style)>,
    /// The style of the whole row, as a changed line's background.
    pub background: Option<Style>,
}

impl Line {
    /// The columns the line takes.
    pub fn width(&self) -> usize {
        self.text.chars().map(|c| c.width().unwrap_or(0)).sum()
    }
}

/// One side of the diff: its text and its syntax tree, if its language has one.
pub struct Text<'a> {
    pub text: &'a Rope,
    pub syntax: Option<&'a Syntax>,
}

/// The lines of the inline diff of `old` and `new`, lined up by `alignment`, with `context`
/// unchanged lines around each hunk and tabs `tab_width` columns wide.
pub fn lines(
    alignment: &Alignment,
    [old, new]: [Text; 2],
    loader: &Loader,
    theme: &Theme,
    context: u32,
    tab_width: usize,
) -> Vec<Line> {
    let styles = Styles::new(theme);
    let rows = alignment.rows();
    let builder = Builder {
        alignment,
        sides: [old, new],
        loader,
        theme,
        styles,
        tab_width,
    };
    let mut lines = Vec::new();
    for (index, group) in groups(alignment.hunks(), rows.len() as u32, context)
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            lines.push(Line {
                text: "┄".repeat(3),
                styles: vec![(0..3, theme.get("ui.virtual.whitespace"))],
                background: None,
            });
        }
        builder.group(group, &mut lines);
    }
    lines
}

/// The runs of rows to show: each hunk with `context` rows around it, overlapping runs merged.
fn groups(hunks: &[Range<u32>], rows: u32, context: u32) -> Vec<Range<u32>> {
    let mut groups: Vec<Range<u32>> = Vec::new();
    for hunk in hunks {
        let group = hunk.start.saturating_sub(context)..(hunk.end + context).min(rows);
        match groups.last_mut() {
            Some(last) if last.end >= group.start => last.end = group.end,
            _ => groups.push(group),
        }
    }
    groups
}

struct Builder<'a> {
    alignment: &'a Alignment,
    sides: [Text<'a>; 2],
    loader: &'a Loader,
    theme: &'a Theme,
    styles: Styles,
    tab_width: usize,
}

impl Builder<'_> {
    /// Pushes the lines of the rows `group`: unchanged ones as they are, each hunk's removed lines
    /// before its added ones.
    fn group(&self, group: Range<u32>, lines: &mut Vec<Line>) {
        let rows = self.alignment.rows();
        let side_lines = |side: Side| -> Option<Range<usize>> {
            let first = self.alignment.line_at_or_after(side, group.start)?;
            let last = self.alignment.line_at_or_before(side, group.end - 1)?;
            (first <= last).then_some(first as usize..last as usize + 1)
        };
        // The syntax styles of the lines of each side the group shows, highlighted at once.
        let highlights = [Side::Old, Side::New]
            .map(|side| side_lines(side).map(|range| (range.start, self.highlights(side, range))));
        let highlight = |side: Side, line: u32| {
            let (first, styles) = highlights[index(side)].as_ref()?;
            styles.get(line as usize - first)
        };

        let mut row = group.start;
        while row < group.end {
            let hunk = self
                .alignment
                .hunks()
                .iter()
                .find(|hunk| hunk.contains(&row));
            let Some(hunk) = hunk else {
                if let Some(line) = rows[row as usize].new {
                    lines.push(self.line(Side::New, line, ' ', highlight(Side::New, line)));
                }
                row += 1;
                continue;
            };
            let end = hunk.end.min(group.end);
            for (side, sign) in [(Side::Old, '-'), (Side::New, '+')] {
                for line in rows[row as usize..end as usize]
                    .iter()
                    .filter_map(|row| row.line(side))
                {
                    lines.push(self.line(side, line, sign, highlight(side, line)));
                }
            }
            row = end;
        }
    }

    /// Line `line` of `side` behind `sign`, with tabs expanded.
    fn line(
        &self,
        side: Side,
        line: u32,
        sign: char,
        syntax: Option<&Vec<(Range<usize>, Style)>>,
    ) -> Line {
        let source = self.sides[index(side)].text.line(line as usize);
        let changed = sign != ' ';
        const SIGN: usize = 2;
        let mut text = String::from(sign);
        text.push(' ');
        // Where each char of the line starts in `text`, and where the line ends.
        let mut starts = Vec::with_capacity(source.len_chars() + 1);
        let mut column = 0;
        for c in source.chars() {
            starts.push(SIGN + column);
            match c {
                '\t' => {
                    let width = self.tab_width.max(1) - column % self.tab_width.max(1);
                    text.extend(std::iter::repeat_n(' ', width));
                    column += width;
                }
                '\n' | '\r' => {}
                c => {
                    text.push(c);
                    column += 1;
                }
            }
        }
        starts.push(SIGN + column);
        let map =
            |range: &Range<usize>| starts[range.start]..starts[range.end.min(starts.len() - 1)];

        let mut styles: Vec<(Range<usize>, Style)> = syntax
            .map(|syntax| {
                syntax
                    .iter()
                    .map(|(range, style)| (map(range), *style))
                    .collect()
            })
            .unwrap_or_default();
        let background = changed.then(|| self.styles.line(side));
        if let Some(LineChange::Parts(parts)) = self.alignment.change(side, line) {
            for part in parts {
                let chars = source.byte_to_char(part.start as usize)
                    ..source.byte_to_char(part.end as usize);
                styles.push((map(&chars), self.styles.text(side)));
            }
        }
        if changed {
            let sign_style = self.theme.get(match side {
                Side::Old => "diff.minus",
                Side::New => "diff.plus",
            });
            styles.push((0..1, sign_style));
        }
        Line {
            text,
            styles,
            background,
        }
    }

    /// The syntax styles of the lines `lines` of `side`, by char ranges of each line.
    fn highlights(&self, side: Side, lines: Range<usize>) -> Vec<Vec<(Range<usize>, Style)>> {
        let Text { text, syntax } = self.sides[index(side)];
        let mut styles = vec![Vec::new(); lines.len()];
        let Some(syntax) = syntax else {
            return styles;
        };
        syntax_styles(
            syntax,
            text.slice(..),
            self.loader,
            self.theme,
            lines,
            &mut styles,
        );
        styles
    }
}

/// Adds the styles the syntax tree gives the lines `lines` of `text` to `styles`, one list of
/// char ranges per line.
fn syntax_styles(
    syntax: &Syntax,
    text: RopeSlice,
    loader: &Loader,
    theme: &Theme,
    lines: Range<usize>,
    styles: &mut [Vec<(Range<usize>, Style)>],
) {
    let start = text.line_to_byte(lines.start) as u32;
    let end = text.line_to_byte(lines.end) as u32;
    let mut highlighter = syntax.highlighter(text, loader, start..end);
    let mut stack = Vec::new();
    let mut pos = start;
    while pos < end {
        if pos == highlighter.next_event_offset() {
            let (event, highlights) = highlighter.advance();
            if event == HighlightEvent::Refresh {
                stack.clear();
            }
            stack.extend(highlights);
            continue;
        }
        let next = highlighter.next_event_offset().min(end);
        let style = stack.iter().fold(Style::default(), |style, highlight| {
            style.patch(theme.highlight(*highlight))
        });
        // Split the span at line ends, as char ranges of each line.
        let (mut from, to) = (pos as usize, next as usize);
        while from < to {
            let line = text.byte_to_line(from);
            let line_start = text.line_to_byte(line);
            let line_end = text.line_to_byte(line + 1).min(to);
            if let Some(line_styles) = styles.get_mut(line - lines.start) {
                let chars = text.byte_to_char(from) - text.line_to_char(line)
                    ..text.byte_to_char(line_end) - text.line_to_char(line);
                if style != Style::default() && !chars.is_empty() {
                    line_styles.push((chars, style));
                }
            }
            debug_assert!(line_start <= from);
            from = line_end;
        }
        pos = next;
    }
}

fn index(side: Side) -> usize {
    match side {
        Side::Old => 0,
        Side::New => 1,
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use helix_view::diff_view::builtin;

    use super::*;

    fn texts(old: &str, new: &str) -> Vec<String> {
        let (old, new) = (Rope::from(old), Rope::from(new));
        let alignment = builtin::align(old.slice(..), new.slice(..));
        let loader = helix_core::config::default_lang_loader();
        let sides = [
            Text {
                text: &old,
                syntax: None,
            },
            Text {
                text: &new,
                syntax: None,
            },
        ];
        lines(&alignment, sides, &loader, &Theme::default(), 1, 4)
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    #[test]
    fn hunks_show_their_removed_lines_before_their_added_ones() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nB\nc\nd\ne\nf\nG\nX\nh\n";
        assert_eq!(
            texts(old, new),
            [
                "  a",
                "- b",
                "+ B",
                "  c",
                "┄┄┄",
                "  f",
                "- g",
                "+ G",
                "+ X",
                "  h"
            ]
        );
    }

    #[test]
    fn tabs_are_expanded() {
        assert_eq!(texts("\ta\n", "\tb\n"), ["-     a", "+     b"]);
    }

    #[test]
    fn lines_are_colored_by_syntax_and_by_what_changed() {
        let (old, new) = (Rope::from("fn a() {}\n"), Rope::from("fn b() {}\n"));
        let alignment = builtin::align(old.slice(..), new.slice(..));
        let theme: Theme = toml::from_str::<toml::Value>(r##""keyword" = "#ff0000""##)
            .unwrap()
            .into();
        let loader = helix_core::config::default_lang_loader();
        loader.set_scopes(theme.scopes().to_vec());
        let rust = loader.language_for_name("rust".to_owned()).unwrap();
        let syntax = |text: &Rope| Syntax::new(text.slice(..), rust, &loader).unwrap();
        let (old_syntax, new_syntax) = (syntax(&old), syntax(&new));
        let sides = [
            Text {
                text: &old,
                syntax: Some(&old_syntax),
            },
            Text {
                text: &new,
                syntax: Some(&new_syntax),
            },
        ];
        let lines = lines(&alignment, sides, &loader, &theme, 3, 4);
        let added = &lines[1];
        assert_eq!(added.text, "+ fn b() {}");
        let keyword = theme.get("keyword");
        assert!(
            added.styles.contains(&(2..4, keyword)),
            "`fn` is a keyword: {:?}",
            added.styles
        );
        assert!(
            added
                .styles
                .contains(&(5..6, Styles::new(&theme).text(Side::New))),
            "`b` changed"
        );
        assert_eq!(added.background, Some(Styles::new(&theme).line(Side::New)));
    }

    #[test]
    fn groups_merge_when_their_context_touches() {
        assert_eq!(groups(&[2..3, 5..6], 10, 1), [1..7]);
        assert_eq!(groups(&[2..3, 6..7], 10, 1), [1..4, 5..8]);
        assert_eq!(groups(&[0..1, 9..10], 10, 3), [0..4, 6..10]);
    }
}
