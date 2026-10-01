//! The text of dired listings, the columns of `eza --git -aolg`, and reading those columns back
//! from edited lines.

use std::{fmt::Write as _, ops::Range, time::SystemTime};

use helix_core::unicode::width::UnicodeWidthStr;
use helix_view::dired::{Entry, GitStatus, Kind, Listing, Size};
use jiff::{civil, tz::TimeZone, Timestamp, Zoned};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The time zone dates are shown in, and the year whose dates show their time.
#[derive(Debug, Clone)]
pub struct Clock {
    tz: TimeZone,
    year: i16,
}

impl Clock {
    pub fn system() -> Self {
        let tz = TimeZone::system();
        let year = Timestamp::now().to_zoned(tz.clone()).year();
        Self { tz, year }
    }

    #[cfg(test)]
    pub fn new(tz: TimeZone, year: i16) -> Self {
        Self { tz, year }
    }

    fn zoned(&self, time: SystemTime) -> Option<Zoned> {
        Some(Timestamp::try_from(time).ok()?.to_zoned(self.tz.clone()))
    }
}

/// The text of `listing`: one line per entry, in order.
pub fn text(listing: &Listing, clock: &Clock) -> String {
    let sizes: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| size(entry.size))
        .collect();
    let width = |widths: &mut dyn Iterator<Item = usize>| widths.max().unwrap_or(0);
    let size_width = width(&mut sizes.iter().map(|size| size.width()));
    let user_width = width(&mut listing.entries.iter().map(|entry| entry.user.width()));
    let group_width = width(&mut listing.entries.iter().map(|entry| entry.group.width()));

    let mut text = String::new();
    for (entry, size) in listing.entries.iter().zip(&sizes) {
        if listing.columns.unix {
            let _ = write!(
                text,
                "{:04o} {} ",
                entry.mode & 0o7777,
                permissions(entry.kind, entry.mode)
            );
        }
        pad(&mut text, size, size_width, true);
        text.push(' ');
        if listing.columns.unix {
            pad(&mut text, &entry.user, user_width, false);
            text.push(' ');
            pad(&mut text, &entry.group, group_width, false);
            text.push(' ');
        }
        text.push_str(&date(entry.modified, clock));
        text.push(' ');
        if listing.columns.git {
            let git = entry.git.unwrap_or(GitStatus {
                index: '-',
                worktree: '-',
            });
            text.push(git.index);
            text.push(git.worktree);
            text.push(' ');
        }
        text.push_str(&entry.guides);
        if let Some(icon) = entry.icon.filter(|_| listing.columns.icons) {
            text.push_str(icon);
            text.push(' ');
        }
        text.push_str(&quote(&name(entry)));
        if let Some(link) = &entry.link {
            text.push_str(" -> ");
            text.push_str(&quote(&link.target.to_string_lossy()));
        }
        text.push('\n');
    }
    text
}

/// The name an entry's line shows: its file name, or `.` for the root of a tree listing.
pub fn name(entry: &Entry) -> String {
    entry.path.file_name().map_or_else(
        || ".".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn pad(text: &mut String, column: &str, width: usize, right: bool) {
    let padding = width.saturating_sub(column.width());
    if right {
        text.extend(std::iter::repeat_n(' ', padding));
        text.push_str(column);
    } else {
        text.push_str(column);
        text.extend(std::iter::repeat_n(' ', padding));
    }
}

/// The letter `eza` starts permissions with for `kind`.
pub fn kind_letter(kind: Kind) -> char {
    match kind {
        Kind::Directory => 'd',
        Kind::File => '.',
        Kind::Link => 'l',
        Kind::Fifo => '|',
        Kind::Socket => 's',
        Kind::BlockDevice => 'b',
        Kind::CharDevice => 'c',
    }
}

/// The read, write and execute bits of user, group and others, with the special bit that shares
/// the execute position of each.
const TRIPLES: [(u32, u32, u32, u32, char); 3] = [
    (0o400, 0o200, 0o100, 0o4000, 's'),
    (0o040, 0o020, 0o010, 0o2000, 's'),
    (0o004, 0o002, 0o001, 0o1000, 't'),
];

/// Permissions like `drwxr-xr-x`, with `s`/`S` for setuid and setgid and `t`/`T` for sticky.
pub fn permissions(kind: Kind, mode: u32) -> String {
    let mut text = String::with_capacity(10);
    text.push(kind_letter(kind));
    for (read, write, execute, special, letter) in TRIPLES {
        text.push(if mode & read != 0 { 'r' } else { '-' });
        text.push(if mode & write != 0 { 'w' } else { '-' });
        text.push(match (mode & execute != 0, mode & special != 0) {
            (true, true) => letter,
            (false, true) => letter.to_ascii_uppercase(),
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    text
}

/// The kind letter and the mode bits of permissions like `drwxr-xr-x`.
pub fn parse_permissions(text: &str) -> Option<(char, u32)> {
    let chars: Vec<char> = text.chars().collect();
    let [kind, bits @ ..] = chars.as_slice() else {
        return None;
    };
    if bits.len() != 9 {
        return None;
    }
    let mut mode = 0;
    for ((read, write, execute, special, letter), bits) in TRIPLES.into_iter().zip(bits.chunks(3)) {
        mode |= match bits[0] {
            'r' => read,
            '-' => 0,
            _ => return None,
        };
        mode |= match bits[1] {
            'w' => write,
            '-' => 0,
            _ => return None,
        };
        mode |= match bits[2] {
            'x' => execute,
            '-' => 0,
            c if c == letter => execute | special,
            c if c == letter.to_ascii_uppercase() => special,
            _ => return None,
        };
    }
    Some((*kind, mode))
}

/// Octal permissions like `0755` or `755`.
pub fn parse_octal(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 4 {
        return None;
    }
    u32::from_str_radix(text, 8).ok()
}

/// A size like `eza` shows it: bytes below 1000, else with a decimal prefix and one decimal
/// below 10, like `1.9k`, `87k` or `1.2M`.
pub fn size(size: Size) -> String {
    match size {
        Size::None => "-".to_owned(),
        Size::Device { major, minor } => format!("{major},{minor}"),
        Size::Bytes(bytes) if bytes < 1000 => bytes.to_string(),
        Size::Bytes(bytes) => {
            let mut value = bytes as f64 / 1000.0;
            let mut prefixes = ["k", "M", "G", "T", "P", "E"].into_iter().peekable();
            let mut prefix = prefixes.next().unwrap_or_default();
            while value >= 1000.0 && prefixes.peek().is_some() {
                value /= 1000.0;
                prefix = prefixes.next().unwrap_or_default();
            }
            if value < 10.0 {
                format!("{value:.1}{prefix}")
            } else {
                format!("{value:.0}{prefix}")
            }
        }
    }
}

/// A date like `eza` shows it, ` 1 Oct 10:59` this year and ` 4 Mar  2023` otherwise, in the
/// local time of that date.
pub fn date(time: SystemTime, clock: &Clock) -> String {
    let Some(zoned) = clock.zoned(time) else {
        return "-".to_owned();
    };
    let month = MONTHS[zoned.month() as usize - 1];
    if zoned.year() == clock.year {
        format!(
            "{:>2} {month} {:02}:{:02}",
            zoned.day(),
            zoned.hour(),
            zoned.minute()
        )
    } else {
        format!("{:>2} {month}  {}", zoned.day(), zoned.year())
    }
}

/// The time an edited date stands for: one of the forms dates are shown in, or
/// `YYYY-MM-DD[ HH:MM[:SS]]`. Whatever the form leaves out is taken from `original`.
pub fn parse_date(text: &str, original: SystemTime, clock: &Clock) -> Option<SystemTime> {
    let original = clock.zoned(original)?.datetime();
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let (date, time) = match tokens.as_slice() {
        [day, month, last] => {
            let day = day.parse().ok()?;
            let month = MONTHS
                .iter()
                .position(|name| name.eq_ignore_ascii_case(month))? as i8
                + 1;
            match parse_time(last, original.time()) {
                Some(time) => (civil::Date::new(clock.year, month, day).ok()?, time),
                None if last.len() == 4 => {
                    let year = last.parse().ok()?;
                    (civil::Date::new(year, month, day).ok()?, original.time())
                }
                None => return None,
            }
        }
        [date] => (date.parse().ok()?, original.time()),
        [date, time] => (date.parse().ok()?, parse_time(time, original.time())?),
        _ => return None,
    };
    let zoned = date.to_datetime(time).to_zoned(clock.tz.clone()).ok()?;
    Some(zoned.timestamp().into())
}

/// `HH:MM` or `HH:MM:SS`, keeping what is left out from `original`.
fn parse_time(text: &str, original: civil::Time) -> Option<civil::Time> {
    let mut parts = text.split(':');
    let hour = parts.next()?.parse().ok()?;
    let minute = parts.next()?.parse().ok()?;
    let (second, subsec) = match parts.next() {
        Some(second) => (second.parse().ok()?, 0),
        None => (original.second(), original.subsec_nanosecond()),
    };
    if parts.next().is_some() {
        return None;
    }
    civil::Time::new(hour, minute, second, subsec).ok()
}

/// A name like `eza` shows it: quoted with `'` when it holds a space, with `"` when it holds a
/// `'`, and with control characters escaped. Unlike `eza`, backslashes are escaped too, so that
/// [`unquote`] gives back the name.
pub fn quote(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            c if c < ' ' || c == '\x7f' => escaped.extend(c.escape_default()),
            c => escaped.push(c),
        }
    }
    match (name.contains('\''), name.contains(' ')) {
        (true, _) => format!("\"{escaped}\""),
        (false, true) => format!("'{escaped}'"),
        (false, false) => escaped,
    }
}

/// The name a quoted name like `'my file'` or `new\nline` stands for.
pub fn unquote(text: &str) -> String {
    let text = match text.as_bytes() {
        [first @ (b'\'' | b'"'), .., last] if first == last => &text[1..text.len() - 1],
        _ => text,
    };
    let mut name = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            name.push(c);
            continue;
        }
        match chars.peek() {
            Some('\\') => name.push('\\'),
            Some('n') => name.push('\n'),
            Some('t') => name.push('\t'),
            Some('r') => name.push('\r'),
            Some('0') => name.push('\0'),
            Some('\'') => name.push('\''),
            Some('"') => name.push('"'),
            Some('u') => {
                let rest: String = chars.clone().skip(1).take_while(|&c| c != '}').collect();
                let code = rest
                    .strip_prefix('{')
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .and_then(char::from_u32);
                match code {
                    Some(code) => {
                        name.push(code);
                        // `u`, the digits in braces and the closing brace
                        for _ in 0..rest.chars().count() + 2 {
                            chars.next();
                        }
                        continue;
                    }
                    None => {
                        name.push('\\');
                        continue;
                    }
                }
            }
            _ => {
                name.push('\\');
                continue;
            }
        }
        chars.next();
    }
    name
}

/// Where the columns of a listing line are, as byte ranges of it. Without the unix columns
/// their ranges are empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub octal: Range<usize>,
    pub permissions: Range<usize>,
    pub size: Range<usize>,
    pub user: Range<usize>,
    pub group: Range<usize>,
    pub date: Range<usize>,
    pub git: Option<Range<usize>>,
    pub guides: Range<usize>,
    pub icon: Option<Range<usize>>,
    pub name: Range<usize>,
    /// The `->` before a link's target.
    pub arrow: Option<Range<usize>>,
    pub target: Option<Range<usize>>,
}

/// The whitespace-separated tokens of a line, as byte ranges.
struct Tokens<'a> {
    line: &'a str,
    pos: usize,
}

impl Tokens<'_> {
    fn peek(&self) -> Option<Range<usize>> {
        let rest = &self.line[self.pos..];
        let start = self.pos + rest.find(|c: char| c != ' ')?;
        let end = self.line[start..]
            .find(' ')
            .map_or(self.line.len(), |end| start + end);
        Some(start..end)
    }

    fn next(&mut self, missing: &'static str) -> Result<Range<usize>, &'static str> {
        let token = self.peek().ok_or(missing)?;
        self.pos = token.end;
        Ok(token)
    }
}

/// The parts of a line of a listing with `columns`. With `tree`, tree guides precede the name.
pub fn parse(
    line: &str,
    columns: helix_view::dired::Columns,
    tree: bool,
) -> Result<Line, &'static str> {
    let line = line.trim_end_matches(['\n', '\r']);
    let mut tokens = Tokens { line, pos: 0 };
    let mut parsed = Line::default();
    if columns.unix {
        parsed.octal = tokens.next("Missing the octal permissions")?;
        parsed.permissions = tokens.next("Missing the permissions")?;
    }
    parsed.size = tokens.next("Missing the size")?;
    if columns.unix {
        parsed.user = tokens.next("Missing the user")?;
        parsed.group = tokens.next("Missing the group")?;
    }

    let first = tokens.next("Missing the date")?;
    let is_digits = |range: &Range<usize>| line[range.clone()].bytes().all(|b| b.is_ascii_digit());
    let last = if is_digits(&first) {
        tokens.next("Missing the date")?;
        tokens.next("Missing the date")?
    } else if line[first.clone()].contains('-') {
        match tokens.peek() {
            Some(time) if line[time.clone()].contains(':') => tokens.next("")?,
            _ => first.clone(),
        }
    } else {
        return Err("Unreadable date");
    };
    parsed.date = first.start..last.end;
    if columns.git {
        parsed.git = Some(tokens.next("Missing the git status")?);
    }

    let mut pos = tokens.pos + line[tokens.pos..].len() - line[tokens.pos..].trim_start().len();
    let guides_start = pos;
    if tree {
        pos += line[pos..].len()
            - line[pos..]
                .trim_start_matches(['│', '├', '└', '─', ' '])
                .len();
    }
    parsed.guides = guides_start..pos;
    if columns.icons {
        let mut chars = line[pos..].chars();
        if let (Some(icon), Some(' ')) = (chars.next(), chars.next()) {
            if is_private_use(icon) {
                parsed.icon = Some(pos..pos + icon.len_utf8());
                pos += icon.len_utf8() + 1;
            }
        }
    }
    let rest = line[pos..].trim_end();
    let link = if columns.unix {
        line[parsed.permissions.clone()].starts_with('l')
    } else {
        rest.contains(" -> ")
    };
    let name_end = match link {
        true => arrow(rest).unwrap_or(rest.len()),
        false => rest.len(),
    };
    parsed.name = pos..pos + name_end;
    if name_end < rest.len() {
        parsed.arrow = Some(pos + name_end + 1..pos + name_end + 3);
        parsed.target = Some(pos + name_end + 4..pos + rest.len());
    }
    Ok(parsed)
}

/// Where the ` -> ` of a link's line starts in `rest`, the name and target: after the name's
/// closing quote if it is quoted.
fn arrow(rest: &str) -> Option<usize> {
    let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'));
    let from = match quote {
        Some(quote) => rest[1..]
            .match_indices(quote)
            .map(|(i, _)| i + 2)
            .find(|&end| rest[end..].starts_with(" -> "))?,
        None => 0,
    };
    rest[from..].find(" -> ").map(|i| from + i)
}

/// Whether `c` is from a Private Use Area, where the Nerd Font icons live.
fn is_private_use(c: char) -> bool {
    matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}' | '\u{100000}'..='\u{10fffd}')
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use helix_view::dired::{Columns, Link, Source};

    use super::*;

    const UNIX_GIT: Columns = Columns {
        unix: true,
        git: true,
        icons: false,
    };

    fn clock() -> Clock {
        Clock::new(TimeZone::get("Europe/Copenhagen").unwrap(), 2026)
    }

    fn time(text: &str) -> SystemTime {
        let zoned: Zoned = text.parse().unwrap();
        zoned.timestamp().into()
    }

    fn entry(path: &str, kind: Kind, mode: u32, size: Size, modified: &str) -> Entry {
        Entry {
            path: path.into(),
            kind,
            mode,
            uid: 1000,
            gid: 1000,
            user: "notroot".into(),
            group: "notroot".into(),
            size,
            modified: time(modified),
            id: (0, 0),
            link: None,
            git: Some(GitStatus {
                index: '-',
                worktree: '-',
            }),
            guides: String::new(),
            icon: None,
        }
    }

    fn listing(entries: Vec<Entry>, columns: Columns) -> Listing {
        Listing {
            source: Source::Directory("/repo".into()),
            columns,
            repo: Some("/repo".into()),
            entries,
            text: helix_core::Rope::new(),
        }
    }

    /// Lines `eza --git -aolg` printed for the same files.
    #[test]
    fn lines_look_like_eza() {
        let summer = "2026-10-01T10:59+02[Europe/Copenhagen]";
        let mut gitignore = entry(".gitignore", Kind::File, 0o644, Size::Bytes(19), summer);
        gitignore.git = Some(GitStatus {
            index: '-',
            worktree: '-',
        });
        let mut docs = entry("docs", Kind::Directory, 0o755, Size::None, summer);
        docs.git = Some(GitStatus {
            index: '-',
            worktree: 'M',
        });
        let mut link = entry("typechange.txt", Kind::Link, 0o777, Size::None, summer);
        link.link = Some(Link {
            target: PathBuf::from("tracked.txt"),
            target_kind: Some(Kind::File),
        });
        let old = entry(
            "guide.md",
            Kind::File,
            0o755,
            Size::Bytes(2),
            "2023-03-04T05:06+01[Europe/Copenhagen]",
        );
        let text = text(
            &listing(vec![gitignore, docs, link, old], UNIX_GIT),
            &clock(),
        );
        assert_eq!(
            text,
            "0644 .rw-r--r-- 19 notroot notroot  1 Oct 10:59 -- .gitignore\n\
             0755 drwxr-xr-x  - notroot notroot  1 Oct 10:59 -M docs\n\
             0777 lrwxrwxrwx  - notroot notroot  1 Oct 10:59 -- typechange.txt -> tracked.txt\n\
             0755 .rwxr-xr-x  2 notroot notroot  4 Mar  2023 -- guide.md\n"
        );
    }

    #[test]
    fn tree_lines_put_guides_before_the_icon() {
        let summer = "2026-10-01T10:59+02[Europe/Copenhagen]";
        let mut root = entry("", Kind::Directory, 0o755, Size::None, summer);
        root.icon = Some("\u{e5ff}");
        let mut file = entry("lib.rs", Kind::File, 0o644, Size::Bytes(4), summer);
        file.guides = "│   └── ".into();
        file.icon = Some("\u{e7a8}");
        let columns = Columns {
            icons: true,
            ..UNIX_GIT
        };
        let text = text(&listing(vec![root, file], columns), &clock());
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "0755 drwxr-xr-x - notroot notroot  1 Oct 10:59 -- \u{e5ff} .",
                "0644 .rw-r--r-- 4 notroot notroot  1 Oct 10:59 -- │   └── \u{e7a8} lib.rs",
            ]
        );
        let line = parse(lines[1], columns, true).unwrap();
        assert_eq!(&lines[1][line.guides.clone()], "│   └── ");
        assert_eq!(&lines[1][line.icon.unwrap()], "\u{e7a8}");
        assert_eq!(&lines[1][line.name], "lib.rs");
    }

    #[test]
    fn lines_parse_back_into_their_columns() {
        let line = "2640 .rw-r-S--- 1.9k notroot wheel  4 Mar  2023 MM 'link 1' -> \"it's\"";
        let parsed = parse(line, UNIX_GIT, false).unwrap();
        let field = |range: Range<usize>| &line[range];
        assert_eq!(field(parsed.octal), "2640");
        assert_eq!(field(parsed.permissions.clone()), ".rw-r-S---");
        assert_eq!(field(parsed.size), "1.9k");
        assert_eq!(field(parsed.user), "notroot");
        assert_eq!(field(parsed.group), "wheel");
        assert_eq!(field(parsed.date), "4 Mar  2023");
        assert_eq!(field(parsed.git.unwrap()), "MM");
        // Not a link: ` -> ` is part of the name.
        assert_eq!(field(parsed.name), "'link 1' -> \"it's\"");

        let line = line.replacen(".rw-r-S---", "lrwxrwxrwx", 1);
        let parsed = parse(&line, UNIX_GIT, false).unwrap();
        assert_eq!(&line[parsed.name], "'link 1'");
        assert_eq!(&line[parsed.arrow.unwrap()], "->");
        assert_eq!(&line[parsed.target.unwrap()], "\"it's\"");

        // An edited ISO date, and a name with an arrow inside its quotes.
        let line = "0777 lrwxrwxrwx - notroot notroot 2024-01-02 03:04 -- 'a -> b' -> c";
        let parsed = parse(line, UNIX_GIT, false).unwrap();
        assert_eq!(&line[parsed.date], "2024-01-02 03:04");
        assert_eq!(&line[parsed.name], "'a -> b'");
        assert_eq!(&line[parsed.target.unwrap()], "c");

        assert_eq!(
            parse("0644 .rw-r--r-- 4 notroot", UNIX_GIT, false),
            Err("Missing the group")
        );
        assert_eq!(
            parse("0644 .rw-r--r-- 4 a b Oct 1 10:59 -- x", UNIX_GIT, false),
            Err("Unreadable date")
        );
    }

    #[test]
    fn permissions_round_trip_with_special_bits() {
        for (kind, mode, text) in [
            (Kind::Directory, 0o1777, "drwxrwxrwt"),
            (Kind::File, 0o4755, ".rwsr-xr-x"),
            (Kind::File, 0o2640, ".rw-r-S---"),
            (Kind::Fifo, 0o644, "|rw-r--r--"),
            (Kind::Link, 0o777, "lrwxrwxrwx"),
        ] {
            assert_eq!(permissions(kind, mode), text);
            assert_eq!(parse_permissions(text), Some((kind_letter(kind), mode)));
        }
        assert_eq!(parse_permissions(".rwxr-xr-"), None);
        assert_eq!(parse_permissions(".rwqr-xr-x"), None);
        assert_eq!(parse_octal("0755"), Some(0o755));
        assert_eq!(parse_octal("4755"), Some(0o4755));
        assert_eq!(parse_octal("755"), Some(0o755));
        assert_eq!(parse_octal("0758"), None);
        assert_eq!(parse_octal("07555"), None);
    }

    #[test]
    fn sizes_use_decimal_prefixes() {
        let sizes = [
            0,
            999,
            1000,
            1500,
            1884,
            12345,
            87_000,
            1_234_567,
            3_000_000_000,
        ];
        let shown: Vec<_> = sizes.map(|bytes| size(Size::Bytes(bytes))).to_vec();
        assert_eq!(
            shown,
            ["0", "999", "1.0k", "1.5k", "1.9k", "12k", "87k", "1.2M", "3.0G"]
        );
        assert_eq!(size(Size::None), "-");
        assert_eq!(size(Size::Device { major: 8, minor: 1 }), "8,1");
    }

    #[test]
    fn dates_are_shown_in_their_own_offset() {
        let clock = clock();
        // Winter time, which `eza` shows an hour off while it is summer.
        let winter = time("2026-03-04T05:06:07+01[Europe/Copenhagen]");
        assert_eq!(date(winter, &clock), " 4 Mar 05:06");
        let last_year = time("2025-12-31T23:59+01[Europe/Copenhagen]");
        assert_eq!(date(last_year, &clock), "31 Dec  2025");
    }

    #[test]
    fn edited_dates_keep_what_they_leave_out() {
        let clock = clock();
        let original = time("2026-03-04T05:06:07.5+01[Europe/Copenhagen]");
        let parsed = |text| parse_date(text, original, &clock);
        assert_eq!(
            parsed("5 Mar 06:07"),
            Some(time("2026-03-05T06:07:07.5+01[Europe/Copenhagen]"))
        );
        // Across the change to summer time.
        assert_eq!(
            parsed("1 Jul 12:00"),
            Some(time("2026-07-01T12:00:07.5+02[Europe/Copenhagen]"))
        );
        assert_eq!(
            parsed("4 Mar  2020"),
            Some(time("2020-03-04T05:06:07.5+01[Europe/Copenhagen]"))
        );
        assert_eq!(
            parsed("2024-02-29 13:14:15"),
            Some(time("2024-02-29T13:14:15+01[Europe/Copenhagen]"))
        );
        assert_eq!(
            parsed("2024-02-29"),
            Some(time("2024-02-29T05:06:07.5+01[Europe/Copenhagen]"))
        );
        assert_eq!(parsed("30 Feb 10:00"), None);
        assert_eq!(parsed("4 Mars 10:00"), None);
        assert_eq!(parsed("tomorrow"), None);
        assert!(original.duration_since(SystemTime::UNIX_EPOCH).unwrap() > Duration::ZERO);
    }

    #[test]
    fn names_are_quoted_like_eza_and_back() {
        for (name, quoted) in [
            ("plain.txt", "plain.txt"),
            ("my file.txt", "'my file.txt'"),
            ("it's.txt", "\"it's.txt\""),
            (" lead", "' lead'"),
            ("new\nline", "new\\nline"),
            ("tab\tname", "tab\\tname"),
            ("esc\u{1b}", "esc\\u{1b}"),
            ("back\\slash", "back\\\\slash"),
            ("ünïcødé.md", "ünïcødé.md"),
        ] {
            assert_eq!(quote(name), quoted);
            assert_eq!(unquote(quoted), name, "{quoted}");
        }
        // Typed without quotes, or with a stray backslash.
        assert_eq!(unquote("my new file"), "my new file");
        assert_eq!(unquote("a\\qb"), "a\\qb");
        assert_eq!(unquote("'"), "'");
    }
}
