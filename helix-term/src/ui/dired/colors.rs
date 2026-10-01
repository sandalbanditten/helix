//! The colors of dired listings: the ones `eza` paints its columns with, or the theme's.

use std::{collections::HashMap, ops::Range};

use helix_view::{
    graphics::{Color, Modifier, Style, UnderlineStyle},
    Theme,
};

use super::format::{unquote, Line};
use crate::ui::file_tree::ls_colors::{eza_environment, parse_style, EntryType, LsColors};

const RED: Color = Color::Indexed(1);
const GREEN: Color = Color::Indexed(2);
const YELLOW: Color = Color::Indexed(3);
const BLUE: Color = Color::Indexed(4);
const PURPLE: Color = Color::Indexed(5);
const CYAN: Color = Color::Indexed(6);
const DARK_GRAY: Color = Color::Indexed(8);

fn fg(color: Color) -> Style {
    Style::default().fg(color)
}

fn bold(color: Color) -> Style {
    fg(color).add_modifier(Modifier::BOLD)
}

/// The `EZA_COLORS` codes of the columns dired shows, with the styles of `eza`'s default theme
/// (`src/theme/default_theme.rs`, sizes in its default gradient).
fn eza_defaults() -> [(&'static str, Style); 43] {
    [
        ("oc", fg(PURPLE)),
        ("ur", bold(YELLOW)),
        ("uw", bold(RED)),
        ("ux", bold(GREEN).underline_style(UnderlineStyle::Line)),
        ("ue", bold(GREEN)),
        ("gr", fg(YELLOW)),
        ("gw", fg(RED)),
        ("gx", fg(GREEN)),
        ("tr", fg(YELLOW)),
        ("tw", fg(RED)),
        ("tx", fg(GREEN)),
        ("su", fg(PURPLE)),
        ("sf", fg(PURPLE)),
        ("nb", fg(GREEN)),
        ("nk", bold(GREEN)),
        ("nm", fg(YELLOW)),
        ("ng", fg(RED)),
        ("nt", fg(PURPLE)),
        ("ub", fg(GREEN)),
        ("uk", bold(GREEN)),
        ("um", fg(YELLOW)),
        ("ug", fg(RED)),
        ("ut", fg(PURPLE)),
        ("df", bold(GREEN)),
        ("ds", fg(GREEN)),
        ("uu", bold(YELLOW)),
        ("uR", Style::default()),
        ("un", Style::default()),
        ("gu", bold(YELLOW)),
        ("gR", Style::default()),
        ("gn", Style::default()),
        ("ga", fg(GREEN)),
        ("gm", fg(BLUE)),
        ("gd", fg(RED)),
        ("gv", fg(YELLOW)),
        ("gt", fg(PURPLE)),
        ("gi", Style::default().add_modifier(Modifier::DIM)),
        ("gc", fg(RED)),
        ("xx", bold(DARK_GRAY)),
        ("da", fg(BLUE)),
        ("lp", fg(CYAN)),
        ("cc", fg(RED)),
        ("bO", Style::default().underline_style(UnderlineStyle::Line)),
    ]
}

/// `eza`'s colors as the environment configures them.
#[derive(Debug)]
pub struct EzaColors {
    codes: HashMap<&'static str, Style>,
    /// The icon's style, which follows the name's unless `ic` is set.
    icon: Option<Style>,
    names: LsColors,
}

impl EzaColors {
    /// `eza`'s defaults with the codes of `EZA_COLORS` (or `EXA_COLORS`), and the name rules of
    /// those and `LS_COLORS`. `None` when none of them is set.
    pub fn from_environment() -> Option<Self> {
        let names = LsColors::from_environment()?;
        Some(Self::new(names, eza_environment().as_deref()))
    }

    fn new(names: LsColors, spec: Option<&str>) -> Self {
        let mut codes: HashMap<_, _> = eza_defaults().into_iter().collect();
        let mut icon = None;
        for rule in spec.unwrap_or_default().split(':') {
            let Some((key, value)) = rule.trim().split_once('=') else {
                continue;
            };
            let style = parse_style(value);
            // `sn` and `sb` set the numbers and units of every magnitude.
            let keys: &[&str] = match key {
                "sn" => &["nb", "nk", "nm", "ng", "nt"],
                "sb" => &["ub", "uk", "um", "ug", "ut"],
                "ic" => {
                    icon = Some(style);
                    continue;
                }
                key => &[key][..],
            };
            for key in keys {
                if let Some(code) = codes.get_mut(*key) {
                    *code = style;
                }
            }
        }
        Self { codes, icon, names }
    }
}

/// What a line's user and group are compared with: the editor's user and groups.
#[derive(Debug, Default, Clone)]
pub struct You {
    pub user: String,
    pub groups: Vec<String>,
}

/// The styles of one frame: `eza`'s or the theme's.
pub struct Palette<'a> {
    codes: HashMap<&'static str, Style>,
    icon: Option<Style>,
    names: Names<'a>,
}

enum Names<'a> {
    Eza(&'a LsColors),
    Theme { directory: Style, broken: Style },
}

impl<'a> Palette<'a> {
    pub fn eza(colors: &'a EzaColors) -> Self {
        Self {
            codes: colors.codes.clone(),
            icon: colors.icon,
            names: Names::Eza(&colors.names),
        }
    }

    pub fn theme(theme: &Theme) -> Self {
        let scope = |scope: &str, fallback: &str| {
            theme
                .try_get_exact(scope)
                .or_else(|| theme.try_get(fallback))
                .unwrap_or_default()
        };
        let octal = scope("ui.dired.octal", "constant.numeric");
        let read = scope("ui.dired.permission.read", "warning");
        let write = scope("ui.dired.permission.write", "error");
        let execute = scope("ui.dired.permission.execute", "diff.plus");
        let special = scope("ui.dired.permission.special", "constant");
        let none = scope("ui.dired.permission.none", "comment");
        let punctuation = scope("ui.dired.punctuation", "comment");
        let size = scope("ui.dired.size", "constant.numeric");
        let user = scope("ui.dired.user", "variable");
        let group = scope("ui.dired.group", "variable");
        let date = scope("ui.dired.date", "info");
        let link = scope("ui.dired.link", "ui.text");
        let error = theme.get("error");
        let git = |status: &str, fallback: &str| scope(&format!("ui.dired.git.{status}"), fallback);
        let mut codes: HashMap<_, _> = eza_defaults()
            .map(|(code, _)| (code, Style::default()))
            .into_iter()
            .collect();
        for (keys, style) in [
            (&["oc"][..], octal),
            (&["ur", "gr", "tr"], read),
            (&["uw", "gw", "tw"], write),
            (&["ux", "ue", "gx", "tx"], execute),
            (&["su", "sf"], special),
            (
                &[
                    "nb", "nk", "nm", "ng", "nt", "ub", "uk", "um", "ug", "ut", "df", "ds",
                ],
                size,
            ),
            (&["uu"], user),
            (&["gu"], group),
            (&["da"], date),
            (&["ga"], git("new", "diff.plus")),
            (&["gm"], git("modified", "diff.delta")),
            (&["gd"], git("deleted", "diff.minus")),
            (&["gv"], git("renamed", "diff.delta.moved")),
            (&["gt"], git("typechange", "diff.delta")),
            (&["gi"], git("ignored", "comment")),
            (&["gc"], git("conflict", "diff.delta.conflict")),
            (&["lp"], link),
            (&["cc", "bO"], error),
        ] {
            for key in keys {
                codes.insert(key, style);
            }
        }
        // The `-` of an unset permission is punctuation in `eza`, its own scope here.
        codes.insert("xx", punctuation);
        codes.insert("x-", none);
        Self {
            codes,
            icon: None,
            names: Names::Theme {
                directory: theme.get("ui.text.directory"),
                broken: error,
            },
        }
    }

    fn code(&self, code: &str) -> Style {
        self.codes.get(code).copied().unwrap_or_default()
    }

    /// The style of a name of `entry_type`, a link pointing to `target`.
    fn name(&self, name: &str, entry_type: EntryType, target: Option<EntryType>) -> Style {
        match &self.names {
            Names::Eza(names) => {
                names
                    .style(name, entry_type, target)
                    .unwrap_or_else(|| match entry_type {
                        EntryType::Directory => bold(BLUE),
                        EntryType::Link => fg(CYAN),
                        EntryType::Orphan => fg(RED),
                        EntryType::Fifo => fg(YELLOW),
                        EntryType::BlockDevice | EntryType::CharDevice => bold(YELLOW),
                        EntryType::Socket => bold(RED),
                        EntryType::Executable => bold(GREEN),
                        EntryType::File => Style::default(),
                    })
            }
            Names::Theme { directory, broken } => match entry_type {
                EntryType::Directory => *directory,
                EntryType::Orphan => *broken,
                _ => Style::default(),
            },
        }
    }

    /// The styled byte ranges of `line`, in order. `broken` tells whether the line's entry is a
    /// link whose target is missing, which the text does not show.
    pub fn spans(
        &self,
        text: &str,
        line: &Line,
        you: &You,
        broken: bool,
    ) -> Vec<(Range<usize>, Style)> {
        let mut spans = Vec::with_capacity(32);
        let mut push = |range: Range<usize>, style: Style| {
            if !range.is_empty() && style != Style::default() {
                spans.push((range, style));
            }
        };
        let permissions = &text[line.permissions.clone()];
        let kind = permissions.chars().next();
        let executable = permissions.contains(['x', 's', 't']);
        let name = unquote(&text[line.name.clone()]);
        let entry_type = match kind {
            Some('d') => EntryType::Directory,
            Some('l') if broken => EntryType::Orphan,
            Some('l') => EntryType::Link,
            Some('|') => EntryType::Fifo,
            Some('s') => EntryType::Socket,
            Some('b') => EntryType::BlockDevice,
            Some('c') => EntryType::CharDevice,
            _ if executable => EntryType::Executable,
            _ => EntryType::File,
        };
        let name_style = self.name(&name, entry_type, None);
        let is_file = matches!(kind, Some('.') | None);

        push(line.octal.clone(), self.code("oc"));
        for (i, (offset, c)) in permissions.char_indices().enumerate() {
            let start = line.permissions.start + offset;
            let triple = (i.saturating_sub(1)) / 3;
            let style = match (i, c) {
                (0, _) => Style {
                    underline_style: None,
                    ..name_style
                },
                (_, '-') => self
                    .codes
                    .get("x-")
                    .copied()
                    .unwrap_or_else(|| self.code("xx")),
                (_, 'r') => self.code(["ur", "gr", "tr"][triple]),
                (_, 'w') => self.code(["uw", "gw", "tw"][triple]),
                (_, 'x') if triple == 0 && is_file => self.code("ux"),
                (_, 'x') => self.code(["ue", "gx", "tx"][triple]),
                _ if is_file => self.code("su"),
                _ => self.code("sf"),
            };
            push(start..start + c.len_utf8(), style);
        }

        let size = &text[line.size.clone()];
        if size == "-" {
            push(line.size.clone(), self.code("xx"));
        } else if let Some(comma) = size.find(',') {
            let comma = line.size.start + comma;
            push(line.size.start..comma, self.code("df"));
            push(comma..comma + 1, self.code("xx"));
            push(comma + 1..line.size.end, self.code("ds"));
        } else {
            let unit = size
                .find(|c: char| c.is_ascii_alphabetic())
                .unwrap_or(size.len());
            let magnitude = match &size[unit..] {
                "" => 0,
                "k" => 1,
                "M" => 2,
                "G" => 3,
                _ => 4,
            };
            let number = line.size.start + unit;
            push(
                line.size.start..number,
                self.code(["nb", "nk", "nm", "ng", "nt"][magnitude]),
            );
            push(
                number..line.size.end,
                self.code(["ub", "uk", "um", "ug", "ut"][magnitude]),
            );
        }

        let user = &text[line.user.clone()];
        let user_code = match user {
            _ if user == you.user => "uu",
            "root" | "0" => "uR",
            _ => "un",
        };
        push(line.user.clone(), self.code(user_code));
        let group = &text[line.group.clone()];
        let group_code = match group {
            _ if you.groups.iter().any(|yours| yours == group) => "gu",
            "root" | "0" => "gR",
            _ => "gn",
        };
        push(line.group.clone(), self.code(group_code));
        push(line.date.clone(), self.code("da"));

        if let Some(git) = &line.git {
            for (offset, c) in text[git.clone()].char_indices() {
                let code = match c {
                    'N' => "ga",
                    'M' => "gm",
                    'D' => "gd",
                    'R' => "gv",
                    'T' => "gt",
                    'I' => "gi",
                    'U' => "gc",
                    _ => "xx",
                };
                let start = git.start + offset;
                push(start..start + c.len_utf8(), self.code(code));
            }
        }
        push(line.guides.clone(), self.code("xx"));
        if let Some(icon) = &line.icon {
            // Like `eza`, the icon takes the name's color but not its modifiers.
            let style = self.icon.unwrap_or(Style {
                fg: name_style.fg,
                ..Style::default()
            });
            push(icon.clone(), style);
        }

        // Escaped control characters stand out within the name.
        let mut start = line.name.start;
        let name_text = &text[line.name.clone()];
        let mut escapes = name_text.match_indices('\\').peekable();
        while let Some((offset, _)) = escapes.next() {
            let rest = &name_text[offset + 1..];
            let len = match rest.chars().next() {
                Some('n' | 't' | 'r' | '0') => 2,
                Some('u') => rest.find('}').map_or(0, |end| end + 2),
                Some('\\') => {
                    escapes.next();
                    0
                }
                _ => 0,
            };
            if len > 0 {
                let escape = line.name.start + offset;
                push(start..escape, name_style);
                push(escape..escape + len, self.code("cc"));
                start = escape + len;
            }
        }
        push(start..line.name.end, name_style);

        if let (Some(arrow), Some(target)) = (&line.arrow, &line.target) {
            push(arrow.clone(), self.code("xx"));
            let style = if broken {
                self.name("", EntryType::Orphan, None)
                    .patch(self.code("bO"))
            } else {
                self.code("lp")
            };
            push(target.clone(), style);
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use helix_view::dired::Columns;

    use super::*;
    use crate::ui::dired::format::parse;

    const COLUMNS: Columns = Columns {
        unix: true,
        git: true,
        icons: false,
    };

    fn styled(palette: &Palette, text: &str) -> Vec<(String, Style)> {
        let you = You {
            user: "notroot".into(),
            groups: vec!["notroot".into()],
        };
        let line = parse(text, COLUMNS, false).unwrap();
        palette
            .spans(text, &line, &you, false)
            .into_iter()
            .map(|(range, style)| (text[range].to_owned(), style))
            .collect()
    }

    /// The colors `eza` printed for this line, with vivid's `LS_COLORS` for the name.
    #[test]
    fn eza_colors_match_eza() {
        let names = LsColors::parse("di=0;38;2;69;133;136");
        let colors = EzaColors::new(names, None);
        let palette = Palette::eza(&colors);
        let spans = styled(
            &palette,
            "0755 drwxr-x--- 1.9k notroot wheel  1 Oct 10:59 -M src",
        );
        let dir = fg(Color::Rgb(69, 133, 136));
        let span = |text: &str, style| (text.to_owned(), style);
        assert_eq!(
            spans,
            [
                span("0755", fg(PURPLE)),
                span("d", dir),
                span("r", bold(YELLOW)),
                span("w", bold(RED)),
                span("x", bold(GREEN)),
                span("r", fg(YELLOW)),
                span("-", bold(DARK_GRAY)),
                span("x", fg(GREEN)),
                span("-", bold(DARK_GRAY)),
                span("-", bold(DARK_GRAY)),
                span("-", bold(DARK_GRAY)),
                span("1.9", bold(GREEN)),
                span("k", bold(GREEN)),
                span("notroot", bold(YELLOW)),
                span("1 Oct 10:59", fg(BLUE)),
                span("-", bold(DARK_GRAY)),
                span("M", fg(BLUE)),
                span("src", dir),
            ]
        );
    }

    #[test]
    fn eza_colors_take_the_codes_of_the_environment() {
        // vivid's `tw` (sticky, other-writable directories) is `eza`'s others-write bit.
        let colors = EzaColors::new(LsColors::default(), Some("da=32:tw=3;38;2;1;2;3:sn=35"));
        let palette = Palette::eza(&colors);
        assert_eq!(palette.code("da"), fg(Color::Indexed(2)));
        assert_eq!(
            palette.code("tw"),
            fg(Color::Rgb(1, 2, 3)).add_modifier(Modifier::ITALIC)
        );
        assert_eq!(palette.code("nk"), fg(Color::Indexed(5)));
        assert_eq!(palette.code("ub"), fg(GREEN));
    }

    #[test]
    fn escapes_in_names_are_colored_apart() {
        let colors = EzaColors::new(LsColors::parse("fi=37"), None);
        let palette = Palette::eza(&colors);
        let spans = styled(
            &palette,
            "0644 .rw-r--r-- 0 notroot notroot  1 Oct 10:59 -- a\\nb",
        );
        let tail: Vec<_> = spans
            .iter()
            .rev()
            .take(2)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(tail, ["b", "\\n"]);
    }
}
