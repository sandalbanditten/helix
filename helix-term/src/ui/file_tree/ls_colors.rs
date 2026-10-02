//! Entry colors from an `LS_COLORS` specification, the format GNU `ls`, `eza` and `vivid`
//! share.
//!
//! Only the foreground and the modifiers that compose with the tree's row backgrounds (bold,
//! dim, italic, underline, crossed out) are kept, so a rule can never hide the cursor row.

use std::collections::HashMap;

use helix_view::graphics::{Modifier, Style};

/// What kind of entry a name belongs to, in the terms of `LS_COLORS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    Directory,
    File,
    Executable,
    Link,
    /// A link whose target is missing.
    Orphan,
    Fifo,
    Socket,
    BlockDevice,
    CharDevice,
}

impl EntryType {
    fn key(self) -> &'static str {
        match self {
            Self::Directory => "di",
            Self::File => "fi",
            Self::Executable => "ex",
            Self::Link => "ln",
            Self::Orphan => "or",
            Self::Fifo => "pi",
            Self::Socket => "so",
            Self::BlockDevice => "bd",
            Self::CharDevice => "cd",
        }
    }
}

/// A parsed `LS_COLORS` specification.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LsColors {
    /// Rules for entry types, keyed like `di`.
    types: HashMap<Box<str>, Style>,
    /// Rules for a single extension, keyed without the dot: `*.rs` is `rs`.
    extensions: HashMap<Box<str>, Style>,
    /// Every other `*suffix` rule, bucketed by its last character; later rules first.
    suffixes: HashMap<char, Vec<(Box<str>, Style)>>,
    /// `ln=target`: links take the color of what they point to.
    links_as_target: bool,
}

impl LsColors {
    /// Reads `LS_COLORS` and then `EZA_COLORS` (or `EXA_COLORS` when that is not set), whose
    /// rules win, like `eza` does. `None` if none is set.
    pub fn from_environment() -> Option<Self> {
        let specs: Vec<_> = [env("LS_COLORS"), eza_environment()]
            .into_iter()
            .flatten()
            .collect();
        (!specs.is_empty()).then(|| Self::parse(&specs.join(":")))
    }

    /// The colors GNU `ls` uses when `LS_COLORS` is not set.
    pub fn gnu() -> Self {
        Self::parse("di=01;34:ln=01;36:pi=33:so=01;35:bd=01;33:cd=01;33:ex=01;32")
    }

    /// Parses a specification such as `di=1;34:*.rs=38;5;208`. Unreadable rules are skipped:
    /// real specifications collect entries from many tools.
    pub fn parse(spec: &str) -> Self {
        let mut colors = Self::default();
        for rule in spec.split(':') {
            let Some((key, value)) = rule.trim().split_once('=') else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            if key == "ln" && value == "target" {
                colors.links_as_target = true;
                continue;
            }
            let style = parse_style(value);
            match key.strip_prefix('*') {
                Some(suffix) if !suffix.is_empty() => {
                    match suffix
                        .strip_prefix('.')
                        .filter(|ext| !ext.is_empty() && !ext.contains('.'))
                    {
                        Some(extension) => {
                            colors.extensions.insert(extension.into(), style);
                        }
                        None => {
                            let last = suffix.chars().next_back().expect("suffix is not empty");
                            colors
                                .suffixes
                                .entry(last)
                                .or_default()
                                .insert(0, (suffix.into(), style));
                        }
                    }
                }
                Some(_) => {}
                None => {
                    colors.types.insert(key.into(), style);
                }
            }
        }
        colors
    }

    /// Whether executables have a rule of their own, which is only worth a `stat` if so.
    pub fn colors_executables(&self) -> bool {
        self.types.contains_key(EntryType::Executable.key())
    }

    /// The style of an entry called `name`. `target` is what a link points to, used when the
    /// specification says `ln=target`. Only regular files are matched by name, and an
    /// executable only if there is no `ex` rule.
    pub fn style(
        &self,
        name: &str,
        entry_type: EntryType,
        target: Option<EntryType>,
    ) -> Option<Style> {
        let entry_type = match (entry_type, target) {
            (EntryType::Link, Some(target)) if self.links_as_target => target,
            _ => entry_type,
        };
        match entry_type {
            EntryType::File => self
                .pattern_style(name)
                .or_else(|| self.type_style(EntryType::File)),
            EntryType::Executable => self
                .type_style(EntryType::Executable)
                .or_else(|| self.pattern_style(name))
                .or_else(|| self.type_style(EntryType::File)),
            EntryType::Orphan => self
                .type_style(EntryType::Orphan)
                .or_else(|| self.type_style(EntryType::Link)),
            entry_type => self.type_style(entry_type),
        }
    }

    fn type_style(&self, entry_type: EntryType) -> Option<Style> {
        self.types.get(entry_type.key()).copied()
    }

    /// The rule with the longest suffix of `name`, so `*README.md` beats `*.md`.
    fn pattern_style(&self, name: &str) -> Option<Style> {
        let extension = name
            .rsplit_once('.')
            .and_then(|(_, extension)| Some((extension, *self.extensions.get(extension)?)));
        let mut best = extension.map(|(extension, style)| (extension.len() + 1, style));
        let last = name.chars().next_back()?;
        for (suffix, style) in self.suffixes.get(&last).into_iter().flatten() {
            if best.is_none_or(|(len, _)| suffix.len() > len) && name.ends_with(&**suffix) {
                best = Some((suffix.len(), *style));
            }
        }
        best.map(|(_, style)| style)
    }
}

/// The value of the environment variable `name` if it is set and not blank.
fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|spec| !spec.trim().is_empty())
}

/// `EZA_COLORS`, or `EXA_COLORS` like `eza` falls back to, if one is set.
pub fn eza_environment() -> Option<String> {
    env("EZA_COLORS").or_else(|| env("EXA_COLORS"))
}

/// Parses SGR parameters like `01;38;2;255;0;0`, keeping the foreground and the safe modifiers.
pub fn parse_style(value: &str) -> Style {
    let style = crate::ui::sgr::apply(Style::default(), value);
    let unsafe_modifiers =
        Modifier::REVERSED | Modifier::HIDDEN | Modifier::SLOW_BLINK | Modifier::RAPID_BLINK;
    Style {
        bg: None,
        underline_color: None,
        add_modifier: style.add_modifier - unsafe_modifiers,
        ..style
    }
}

#[cfg(test)]
mod tests {
    use helix_view::graphics::Color;

    use super::*;

    #[test]
    fn styles_keep_the_foreground_and_safe_modifiers() {
        assert_eq!(
            parse_style("01;38;2;69;133;136"),
            Style::default()
                .fg(Color::Rgb(69, 133, 136))
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            parse_style("0;38;2;235;219;178;48;2;40;40;40"),
            Style::default().fg(Color::Rgb(235, 219, 178))
        );
        assert_eq!(
            parse_style("7;8;5;34"),
            Style::default().fg(Color::Indexed(4))
        );
        assert_eq!(
            parse_style("38;5;208"),
            Style::default().fg(Color::Indexed(208))
        );
        assert_eq!(parse_style("92"), Style::default().fg(Color::Indexed(10)));
        assert_eq!(
            parse_style("1;0;3"),
            Style::default().add_modifier(Modifier::ITALIC)
        );
        assert_eq!(parse_style("38;2;1"), Style::default());
    }

    #[test]
    fn the_longest_pattern_wins_and_types_apply_by_kind() {
        let colors = LsColors::parse(
            "di=34:fi=37:ex=32:ln=36:*.md=33:*README.md=31:*.tar.gz=35:bogus:*.md=93",
        );
        let fg = |name, entry_type| {
            colors
                .style(name, entry_type, None)
                .and_then(|style| style.fg)
        };
        assert_eq!(fg("guide.md", EntryType::File), Some(Color::Indexed(11)));
        assert_eq!(fg("README.md", EntryType::File), Some(Color::Indexed(1)));
        assert_eq!(fg("x.tar.gz", EntryType::File), Some(Color::Indexed(5)));
        assert_eq!(fg("x.gz", EntryType::File), Some(Color::Indexed(7)));
        // Directories never match patterns, and patterns are case sensitive.
        assert_eq!(fg("docs.md", EntryType::Directory), Some(Color::Indexed(4)));
        assert_eq!(fg("GUIDE.MD", EntryType::File), Some(Color::Indexed(7)));
        assert_eq!(fg("run.md", EntryType::Executable), Some(Color::Indexed(2)));
        assert_eq!(fg("broken", EntryType::Orphan), Some(Color::Indexed(6)));
        assert_eq!(fg("pipe", EntryType::Fifo), None);
        assert!(colors.colors_executables());
    }

    #[test]
    fn links_can_take_their_targets_color() {
        let colors = LsColors::parse("ln=target:di=34");
        let style = colors.style("dir-link", EntryType::Link, Some(EntryType::Directory));
        assert_eq!(style.and_then(|style| style.fg), Some(Color::Indexed(4)));
    }
}
