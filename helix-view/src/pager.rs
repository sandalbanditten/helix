//! Documents shown as a pager shows terminal output, like `less -R`: their escape sequences and
//! overstrikes become styles, and nothing edits them. Man pages get colors for their parts too,
//! like bat and Neovim give them.

use std::{
    ffi::OsString,
    ops::Range,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
};

use helix_core::regex::Regex;

use crate::{
    graphics::{Modifier, Style, UnderlineStyle},
    sgr,
};

/// What a document shown in a pager has besides its text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// The styles of char ranges of the text, in order and apart; other chars have none.
    pub styles: Vec<(Range<usize>, Style)>,
    /// The parts of a man page with colors of their own, in order and apart, if the text is one.
    pub man: Vec<(Range<usize>, ManPart)>,
    /// The command that showed the text in the pager, which formats it again to fit the view.
    pub source: Option<Source>,
}

/// A command that showed its output in the pager, formatted for the width of the view showing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub producer: Producer,
    /// The width the text is formatted for, once the view's is known.
    pub width: Option<u16>,
    /// The width the text is being formatted for.
    pub formatting: Option<u16>,
}

impl Source {
    /// The command showing its output in the pager it started, if it can format it again: `man`
    /// showing a man page, or git showing diffs.
    pub fn from_env(man_page: bool) -> Option<Self> {
        let producer = if man_page {
            Producer::man()?
        } else {
            Producer::git()?
        };
        Some(Self {
            producer,
            width: None,
            formatting: None,
        })
    }
}

/// A command whose output a pager shows, which lays it out for a width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Producer {
    /// `man` showing a page, as it names it to its pager in `MAN_PN`: like `ls(1)`, or a file.
    Man(String),
    /// git showing diffs: its arguments after `git`, without `-C`, and the directory it ran in.
    Git { args: Vec<OsString>, dir: PathBuf },
}

/// The options of git itself that take the argument after them, besides `-C`.
const GIT_OPTIONS_WITH_VALUES: [&str; 7] = [
    "-c",
    "--config-env",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--shallow-file",
    "--attr-source",
];

impl Producer {
    fn man() -> Option<Self> {
        let name = std::env::var("MAN_PN")
            .ok()
            .filter(|name| !name.is_empty())?;
        Some(Self::Man(name))
    }

    /// git, if it started the pager with a command that shows diffs. git runs its pager in the
    /// top directory of the work tree, and tells it in `GIT_PREFIX` where the command ran.
    fn git() -> Option<Self> {
        let prefix = std::env::var_os("GIT_PREFIX")?;
        let dir = helix_stdx::env::current_working_dir().join(prefix);
        let mut ancestors = helix_stdx::env::ancestor_args();
        let mut args = ancestors.next()?;
        // git runs the pager with `sh -c`, which starts it rather than becoming it when the
        // pager's command is more than a command, like `hx -p 2>/dev/null`.
        if Path::new(args.first()?).file_name()? == "sh" && args.get(1)? == "-c" {
            args = ancestors.next()?;
        }
        Self::git_command(args, dir)
    }

    /// git running `args` in `dir`, if they are a command that shows diffs: `diff`, `show`, `log`,
    /// `whatchanged`, `range-diff`, `stash show` or `stash list`. Other commands may change the
    /// repository, so they don't run again.
    fn git_command(args: Vec<OsString>, dir: PathBuf) -> Option<Self> {
        let mut args = args.into_iter();
        if Path::new(&args.next()?).file_name()? != "git" {
            return None;
        }
        let mut kept = Vec::new();
        // The options of git itself, up to the command; `-C` changed to `dir` already.
        let command = loop {
            let arg = args.next()?;
            if arg == "-C" {
                args.next()?;
                continue;
            }
            if !arg.as_encoded_bytes().starts_with(b"-") {
                break arg;
            }
            let takes_value = GIT_OPTIONS_WITH_VALUES.iter().any(|option| arg == *option);
            kept.push(arg);
            if takes_value {
                kept.push(args.next()?);
            }
        };
        let rest: Vec<OsString> = args.collect();
        let shows_diffs = match command.to_str()? {
            "diff" | "show" | "log" | "whatchanged" | "range-diff" => true,
            "stash" => rest
                .first()
                .is_some_and(|action| action == "show" || action == "list"),
            _ => false,
        };
        if !shows_diffs {
            return None;
        }
        kept.push(command);
        kept.extend(rest);
        Some(Self::Git { args: kept, dir })
    }

    /// The widest the text gets: for man pages `MANWIDTH`, if set.
    pub fn max_width(&self) -> Option<u16> {
        match self {
            Self::Man(_) => std::env::var("MANWIDTH")
                .ok()
                .and_then(|width| width.parse().ok()),
            Self::Git { .. } => None,
        }
    }

    /// The command that lays the text out again for `width` columns.
    pub fn command(&self, width: u16) -> Command {
        let mut command = match self {
            Self::Man(name) => {
                let mut command = Command::new("man");
                // `MANPAGER=cat` keeps `man` from starting the pager again, and
                // `MAN_KEEP_FORMATTING` keeps its overstrikes in output to a pipe.
                command
                    .args(man_args(name))
                    .env("MANPAGER", "cat")
                    .env("MANWIDTH", width.to_string())
                    .env("MAN_KEEP_FORMATTING", "1");
                command
            }
            Self::Git { args, dir } => {
                let mut command = Command::new("git");
                // Colored as output to a pager is, and laid out for `COLUMNS`, which difftastic
                // and `--stat` follow where no terminal tells them a width.
                command
                    .args(args)
                    .current_dir(dir)
                    .env("COLUMNS", width.to_string())
                    .env("GIT_PAGER_IN_USE", "true");
                command
            }
        };
        command.stdin(Stdio::null()).stderr(Stdio::null());
        command
    }
}

/// The arguments for `man` to show the page `name`.
fn man_args(name: &str) -> Vec<String> {
    if name.contains('/') {
        return vec!["-l".to_owned(), name.to_owned()];
    }
    match name
        .strip_suffix(')')
        .and_then(|name| name.rsplit_once('('))
    {
        Some((page, section)) => vec![section.to_owned(), page.to_owned()],
        None => vec![name.to_owned()],
    }
}

/// A part of a man page with a color of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManPart {
    /// The title and footer lines.
    Title,
    /// The heading of a section.
    Heading,
    /// A command-line option, like `-a` or `--all`.
    Option,
    /// What to write in place of a word: the text man underlines or italicizes.
    Argument,
    /// The page a reference like `stat(2)` names.
    Reference,
    /// The section of a reference.
    Section,
    Url,
    /// An environment variable, like `$HOME`.
    Variable,
}

impl ManPart {
    /// The theme key of the part, like `man.heading`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Title => "man.title",
            Self::Heading => "man.heading",
            Self::Option => "man.option",
            Self::Argument => "man.argument",
            Self::Reference => "man.reference",
            Self::Section => "man.section",
            Self::Url => "man.link",
            Self::Variable => "man.variable",
        }
    }

    /// The theme scope the part falls back to without a [key](Self::key) in the theme.
    pub fn scope(self) -> &'static str {
        match self {
            Self::Title | Self::Heading => "markup.heading",
            Self::Option => "constant",
            Self::Argument => "variable.parameter",
            Self::Reference => "function",
            Self::Section => "constant.numeric",
            Self::Url => "markup.link.url",
            Self::Variable => "variable.builtin",
        }
    }
}

/// The title of a man page, like `LS(1)`, at both ends of its first line.
static TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\S+\([^)\s]+\))\s.*\s(\S+\([^)\s]+\))\s*$").unwrap());
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s<>()\[\]]+[^\s<>()\[\].,;:]").unwrap());
static REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([A-Za-z0-9_][A-Za-z0-9_.:+-]*)\((\d[a-z]*)\)").unwrap());
static OPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s\[|,(])(--?[A-Za-z0-9][A-Za-z0-9_-]*)").unwrap());
static VARIABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{?[A-Za-z_][A-Za-z0-9_]*\}?").unwrap());

/// Whether `text` is a man page: whether its first line has its title at both ends.
pub fn is_man_page(text: &str) -> bool {
    let first = text.lines().find(|line| !line.trim().is_empty());
    first
        .and_then(|line| TITLE.captures(line))
        .is_some_and(|title| title[1] == title[2])
}

/// The parts of the man page `text` with colors of their own. Arguments are the text that
/// `styles` underlines or italicizes, as man writes them.
pub fn man_parts(text: &str, styles: &[(Range<usize>, Style)]) -> Vec<(Range<usize>, ManPart)> {
    let title = text
        .lines()
        .find_map(|line| TITLE.captures(line))
        .map(|title| title[1].to_owned());
    let last = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, _)| index)
        .last();
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, line) in text.lines().enumerate() {
        let chars = line.chars().count();
        let content = line.trim_end();
        let is_title_line = parts.is_empty() && TITLE.is_match(content)
            || Some(index) == last
                && title
                    .as_deref()
                    .is_some_and(|title| content.ends_with(title));
        let indent = content.chars().take_while(|c| c.is_whitespace()).count();
        if !content.is_empty() && (is_title_line || indent == 0 || indent == 3) {
            let heading = start + indent..start + content.chars().count();
            let part = if is_title_line {
                ManPart::Title
            } else {
                ManPart::Heading
            };
            parts.push((heading, part));
        } else {
            parts.extend(line_parts(line, start, styles));
        }
        // The line break, which `lines` leaves out.
        start += chars + 1;
    }
    parts
}

/// The parts of the line `line` of a man page, which starts at the char `start`.
fn line_parts(
    line: &str,
    start: usize,
    styles: &[(Range<usize>, Style)],
) -> Vec<(Range<usize>, ManPart)> {
    let char_at = |byte: usize| start + line[..byte].chars().count();
    let mut found = Vec::new();
    // In order of precedence, where they overlap.
    found.extend(URL.find_iter(line).map(|url| (url.range(), ManPart::Url)));
    for reference in REFERENCE.captures_iter(line) {
        let (name, section) = (reference.get(1).unwrap(), reference.get(2).unwrap());
        found.push((name.range(), ManPart::Reference));
        found.push((section.range(), ManPart::Section));
    }
    found.extend(
        OPTION
            .captures_iter(line)
            .map(|option| (option.get(1).unwrap().range(), ManPart::Option)),
    );
    found.extend(
        VARIABLE
            .find_iter(line)
            .map(|variable| (variable.range(), ManPart::Variable)),
    );
    let mut parts: Vec<(Range<usize>, ManPart)> = Vec::new();
    for (bytes, part) in found {
        let range = char_at(bytes.start)..char_at(bytes.end);
        if !parts.iter().any(|(kept, _)| overlap(kept, &range)) {
            parts.push((range, part));
        }
    }
    // Underlined or italic text, as far as the other parts leave it.
    let end = start + line.chars().count();
    let arguments = styles
        .iter()
        .skip_while(|(range, _)| range.end <= start)
        .take_while(|(range, _)| range.start < end)
        .filter(|(_, style)| {
            style.underline_style.is_some() || style.add_modifier.contains(Modifier::ITALIC)
        })
        .map(|(range, _)| range.start.max(start)..range.end.min(end));
    for range in arguments {
        if !parts.iter().any(|(kept, _)| overlap(kept, &range)) {
            parts.push((range, ManPart::Argument));
        }
    }
    parts.sort_by_key(|(range, _)| range.start);
    parts
}

fn overlap(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// The text terminal output `output` shows, and its page: the styles of its chars, as [`format`]
/// reads them, and the parts of a man page.
pub fn page(output: &str) -> (String, Page) {
    let (text, mut page) = if is_formatted(output) {
        format(output)
    } else {
        (output.to_owned(), Page::default())
    };
    if is_man_page(&text) {
        page.man = man_parts(&text, &page.styles);
    }
    (text, page)
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
        ..Page::default()
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

    #[test]
    fn man_pages_have_parts() {
        let output = "LS(1)       User Commands       LS(1)\n\nNAME\n       ls - list directory contents\n\nSYNOPSIS\n       ls [_\x08O_\x08P_\x08T]... [_\x08F]...\n\n       -a, --all\n              see stat(2), https://gnu.org/ls, or $LS_COLORS.\n   Exit status:\n       0      if OK,\n\nGNU coreutils 9.5     2024-03-28     LS(1)\n";
        let (text, page) = format(output);
        assert!(is_man_page(&text));
        let parts: Vec<_> = man_parts(&text, &page.styles)
            .into_iter()
            .map(|(range, part)| {
                let part_text: String = text.chars().skip(range.start).take(range.len()).collect();
                (part_text, part)
            })
            .collect();
        let expected = [
            ("LS(1)       User Commands       LS(1)", ManPart::Title),
            ("NAME", ManPart::Heading),
            ("SYNOPSIS", ManPart::Heading),
            ("OPT", ManPart::Argument),
            ("F", ManPart::Argument),
            ("-a", ManPart::Option),
            ("--all", ManPart::Option),
            ("stat", ManPart::Reference),
            ("2", ManPart::Section),
            ("https://gnu.org/ls", ManPart::Url),
            ("$LS_COLORS", ManPart::Variable),
            ("Exit status:", ManPart::Heading),
            ("GNU coreutils 9.5     2024-03-28     LS(1)", ManPart::Title),
        ];
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(text, part)| (text.to_owned(), part))
            .collect();
        assert_eq!(parts, expected);
        assert!(!is_man_page("commit 74c2ff7\nAuthor: someone\n"));
    }

    #[test]
    fn man_pages_name_their_arguments() {
        assert_eq!(man_args("ls(1)"), ["1", "ls"]);
        assert_eq!(man_args("git-log(1)"), ["1", "git-log"]);
        assert_eq!(man_args("./ls.1"), ["-l", "./ls.1"]);
        assert_eq!(man_args("ls"), ["ls"]);
    }

    #[test]
    fn only_git_commands_showing_diffs_run_again() {
        let git = |args: &[&str]| {
            let args = args.iter().map(OsString::from).collect();
            match Producer::git_command(args, PathBuf::from("/repo/sub"))? {
                Producer::Git { args, dir } => {
                    assert_eq!(dir, Path::new("/repo/sub"));
                    Some(args)
                }
                Producer::Man(_) => None,
            }
        };
        assert_eq!(git(&["git", "diff", "HEAD~1"]).unwrap(), ["diff", "HEAD~1"]);
        // `-C` took git to the directory already; other options of git itself stay.
        let args = [
            "/usr/bin/git",
            "-C",
            "sub",
            "-c",
            "a.b=c",
            "--no-pager",
            "log",
            "-p",
        ];
        assert_eq!(
            git(&args).unwrap(),
            ["-c", "a.b=c", "--no-pager", "log", "-p"]
        );
        assert!(git(&["git", "--git-dir=.git", "show"]).is_some());
        assert!(git(&["git", "stash", "show", "-p"]).is_some());
        assert!(git(&["git", "stash", "list"]).is_some());
        assert!(git(&["git", "-p", "stash", "pop"]).is_none());
        assert!(git(&["git", "stash"]).is_none());
        assert!(git(&["git", "-p", "commit", "-m", "diff"]).is_none());
        // The value of an option isn't the command.
        assert!(git(&["git", "--git-dir", "diff", "status"]).is_none());
        assert!(git(&["git", "-c"]).is_none());
        assert!(git(&["git"]).is_none());
        assert!(git(&["fish", "-c", "git diff"]).is_none());
    }

    #[test]
    fn commands_lay_their_output_out_for_the_width() {
        let env = |command: &Command, name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| value?.to_str())
                .map(str::to_owned)
        };
        let git = Producer::Git {
            args: vec!["diff".into()],
            dir: PathBuf::from("/repo"),
        };
        let command = git.command(80);
        assert_eq!(command.get_program(), "git");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["diff"]);
        assert_eq!(command.get_current_dir(), Some(Path::new("/repo")));
        assert_eq!(env(&command, "COLUMNS").as_deref(), Some("80"));
        assert_eq!(env(&command, "GIT_PAGER_IN_USE").as_deref(), Some("true"));
        let command = Producer::Man("ls(1)".to_owned()).command(80);
        assert_eq!(command.get_program(), "man");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["1", "ls"]);
        assert_eq!(env(&command, "MANWIDTH").as_deref(), Some("80"));
    }
}
