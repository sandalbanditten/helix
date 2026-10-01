//! Functions for working with [Path].

pub use etcetera::home_dir;
use once_cell::sync::Lazy;
use regex_cursor::{engines::meta::Regex, Input};
use ropey::RopeSlice;

use std::{
    borrow::Cow,
    ffi::OsString,
    ops::Range,
    path::{Component, Path, PathBuf, MAIN_SEPARATOR_STR},
};

use crate::env::current_working_dir;

/// Replaces users home directory from `path` with tilde `~` if the directory
/// is available, otherwise returns the path unchanged.
pub fn fold_home_dir<'a, P>(path: P) -> Cow<'a, Path>
where
    P: Into<Cow<'a, Path>>,
{
    let path = path.into();
    if let Ok(home) = home_dir() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            let mut path = OsString::with_capacity(2 + stripped.as_os_str().len());
            path.push("~");
            path.push(MAIN_SEPARATOR_STR);
            path.push(stripped);
            return Cow::Owned(PathBuf::from(path));
        }
    }

    path
}

/// Expands tilde `~` into users home directory if available, otherwise returns the path
/// unchanged.
///
/// The tilde will only be expanded when present as the first component of the path
/// and only slash follows it.
pub fn expand_tilde<'a, P>(path: P) -> Cow<'a, Path>
where
    P: Into<Cow<'a, Path>>,
{
    let path = path.into();
    let mut components = path.components();
    if let Some(Component::Normal(c)) = components.next() {
        if c == "~" {
            if let Ok(mut buf) = home_dir() {
                buf.push(components);
                return Cow::Owned(buf);
            }
        }
    }

    path
}

/// Normalize a path without resolving symlinks.
// Strategy: start from the first component and move up. Canonicalize previous path,
// join component, canonicalize new path, strip prefix and join to the final result.
pub fn normalize(path: impl AsRef<Path>) -> PathBuf {
    let mut components = path.as_ref().components().peekable();
    let mut ret = if let Some(c @ Component::Prefix(..)) = components.peek().copied() {
        components.next();
        PathBuf::from(c.as_os_str())
    } else {
        PathBuf::new()
    };

    for component in components {
        match component {
            Component::Prefix(..) => unreachable!(),
            Component::RootDir => {
                ret.push(component.as_os_str());
            }
            Component::CurDir => {}
            #[cfg(not(windows))]
            Component::ParentDir => {
                ret.pop();
            }
            #[cfg(windows)]
            Component::ParentDir => {
                if let Some(head) = ret.components().next_back() {
                    match head {
                        Component::Prefix(_) | Component::RootDir => {}
                        Component::CurDir => unreachable!(),
                        // If we left previous component as ".." it means we met a symlink before and we can't pop path.
                        Component::ParentDir => {
                            ret.push("..");
                        }
                        Component::Normal(_) => {
                            if ret.is_symlink() {
                                ret.push("..");
                            } else {
                                ret.pop();
                            }
                        }
                    }
                }
            }
            #[cfg(not(windows))]
            Component::Normal(c) => {
                ret.push(c);
            }
            #[cfg(windows)]
            Component::Normal(c) => 'normal: {
                use std::fs::canonicalize;

                let new_path = ret.join(c);
                if new_path.is_symlink() {
                    ret = new_path;
                    break 'normal;
                }
                let (can_new, can_old) = (canonicalize(&new_path), canonicalize(&ret));
                match (can_new, can_old) {
                    (Ok(can_new), Ok(can_old)) => {
                        let striped = can_new.strip_prefix(can_old);
                        ret.push(striped.unwrap_or_else(|_| c.as_ref()));
                    }
                    _ => ret.push(c),
                }
            }
        }
    }
    dunce::simplified(&ret).to_path_buf()
}

/// Returns the canonical, absolute form of a path with all intermediate components normalized.
///
/// This function is used instead of [`std::fs::canonicalize`] because we don't want to verify
/// here if the path exists, just normalize it's components.
pub fn canonicalize(path: impl AsRef<Path>) -> PathBuf {
    let path = expand_tilde(path.as_ref());
    let path = if path.is_relative() {
        Cow::Owned(current_working_dir().join(path))
    } else {
        path
    };

    normalize(path)
}

/// Convert path into a relative path
pub fn get_relative_path<'a, P>(path: P) -> Cow<'a, Path>
where
    P: Into<Cow<'a, Path>>,
{
    let path = path.into();
    if path.is_absolute() {
        let cwdir = normalize(current_working_dir());
        if let Ok(stripped) = normalize(&path).strip_prefix(cwdir) {
            return Cow::Owned(PathBuf::from(stripped));
        }

        return fold_home_dir(path);
    }

    path
}

/// Returns a truncated filepath where the basepart of the path is reduced to the first
/// char of the folder and the whole filename appended.
///
/// Also strip the current working directory from the beginning of the path.
/// Note that this function does not check if the truncated path is unambiguous.
///
/// ```
///    use helix_stdx::path::get_truncated_path;
///    use std::path::Path;
///
///    assert_eq!(
///         get_truncated_path("/home/cnorris/documents/jokes.txt").as_path(),
///         Path::new("/h/c/d/jokes.txt")
///     );
///     assert_eq!(
///         get_truncated_path("jokes.txt").as_path(),
///         Path::new("jokes.txt")
///     );
///     assert_eq!(
///         get_truncated_path("/jokes.txt").as_path(),
///         Path::new("/jokes.txt")
///     );
///     assert_eq!(
///         get_truncated_path("/h/c/d/jokes.txt").as_path(),
///         Path::new("/h/c/d/jokes.txt")
///     );
///     assert_eq!(get_truncated_path("").as_path(), Path::new(""));
/// ```
///
pub fn get_truncated_path(path: impl AsRef<Path>) -> PathBuf {
    let cwd = current_working_dir();
    let path = path.as_ref();
    let path = path.strip_prefix(cwd).unwrap_or(path);
    let file = path.file_name().unwrap_or_default();
    let base = path.parent().unwrap_or_else(|| Path::new(""));
    let mut ret = PathBuf::with_capacity(file.len());
    // A char can't be directly pushed to a PathBuf
    let mut first_char_buffer = String::new();
    for d in base {
        let Some(first_char) = d.to_string_lossy().chars().next() else {
            break;
        };
        first_char_buffer.push(first_char);
        ret.push(&first_char_buffer);
        first_char_buffer.clear();
    }
    ret.push(file);
    ret
}

fn path_component_regex(windows: bool) -> String {
    // TODO: support backslash path escape on windows (when using git bash for example)
    let space_escape = if windows { r"[\^`]\s" } else { r"[\\]\s" };
    // partially baesd on what's allowed in an url but with some care to avoid
    // false positives (like any kind of brackets or quotes)
    r"[\w@.\-+#$%?!,;~&]|".to_owned() + space_escape
}

/// Regex for delimited environment captures like `${HOME}`.
fn braced_env_regex(windows: bool) -> String {
    r"\$\{(?:".to_owned() + &path_component_regex(windows) + r"|[/:=])+\}"
}

fn compile_path_regex(
    prefix: &str,
    postfix: &str,
    match_single_file: bool,
    windows: bool,
) -> Regex {
    let first_component = format!(
        "(?:{}|(?:{}))",
        braced_env_regex(windows),
        path_component_regex(windows)
    );
    // For all components except the first we allow an equals so that `foo=/
    // bar/baz` does not include foo. This is primarily intended for url queries
    // (where an equals is never in the first component)
    let component = format!("(?:{first_component}|=)");
    let sep = if windows { r"[/\\]" } else { "/" };
    let url_prefix = r"[\w+\-.]+://??";
    let path_prefix = if windows {
        // single slash handles most windows prefixes (like\\server\...) but `\
        // \?\C:\..` (and C:\) needs special handling, since we don't allow : in path
        // components (so that colon separated paths and <path>:<line> work)
        r"\\\\\?\\\w:|\w:|\\|"
    } else {
        ""
    };
    let path_start = format!("(?:{first_component}+|~|{path_prefix}{url_prefix})");
    let optional = if match_single_file {
        format!("|{path_start}")
    } else {
        String::new()
    };
    let path_regex = format!(
        "{prefix}(?:{path_start}?(?:(?:{sep}{component}+)+{sep}?|{sep}){optional}){postfix}"
    );
    Regex::new(&path_regex).unwrap()
}

/// If `src` ends with a path then this function returns the part of the slice.
pub fn get_path_suffix(src: RopeSlice<'_>, match_single_file: bool) -> Option<RopeSlice<'_>> {
    let regex = if match_single_file {
        static REGEX: Lazy<Regex> = Lazy::new(|| compile_path_regex("", "$", true, cfg!(windows)));
        &*REGEX
    } else {
        static REGEX: Lazy<Regex> = Lazy::new(|| compile_path_regex("", "$", false, cfg!(windows)));
        &*REGEX
    };

    regex
        .find(Input::new(src))
        .map(|mat| src.byte_slice(mat.range()))
}

/// Returns an iterator of the **byte** ranges in src that contain a path.
pub fn find_paths(
    src: RopeSlice<'_>,
    match_single_file: bool,
) -> impl Iterator<Item = Range<usize>> + '_ {
    let regex = if match_single_file {
        static REGEX: Lazy<Regex> = Lazy::new(|| compile_path_regex("", "", true, cfg!(windows)));
        &*REGEX
    } else {
        static REGEX: Lazy<Regex> = Lazy::new(|| compile_path_regex("", "", false, cfg!(windows)));
        &*REGEX
    };
    regex.find_iter(Input::new(src)).map(|mat| mat.range())
}

/// A path followed by a position in it, like `src/lib.rs:7:5` in a compiler message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPosition {
    /// The byte range of the path and its position.
    pub range: Range<usize>,
    /// The byte range of the path alone, without the `file://` of a file URL.
    pub path: Range<usize>,
    /// The line, as written: counting from 1.
    pub line: usize,
    /// The column, as written: counting from 1.
    pub column: Option<usize>,
}

/// Returns an iterator of the paths in `src` that a position follows: `path:7`, `path:7:5`,
/// `path(7)`, `path(7,5)`, Maven's `path:[7,5]` or GHC's `path:(7,5)-(9,1)`. Paths need not exist,
/// so `12:30:45` reads as line 30 of `12`.
pub fn find_path_positions(src: RopeSlice<'_>) -> impl Iterator<Item = PathPosition> + '_ {
    const FILE_URL: &str = "file://";
    // The end of the last position found, as the numbers in it read like file names too.
    let mut found_end = 0;
    find_paths(src, true).filter_map(move |mut path| {
        if path.start < found_end {
            return None;
        }
        // The path regex finds a URL whole, or its scheme apart from the path after it. Only
        // file URLs name files.
        let text = Cow::from(src.byte_slice(path.clone()));
        let from = src.char_to_byte(src.byte_to_char(path.start.saturating_sub(FILE_URL.len())));
        let before = Cow::from(src.byte_slice(from..path.start));
        let mut start = path.start;
        if text.starts_with(FILE_URL) {
            path.start += FILE_URL.len();
        } else if before.ends_with(FILE_URL) {
            start -= FILE_URL.len();
        } else if text.contains("://") || before.ends_with("://") {
            return None;
        }
        let rest: String = src.byte_slice(path.end..).chars().take(48).collect();
        let (line, column, len) = position_suffix(&rest)?;
        found_end = path.end + len;
        Some(PathPosition {
            range: start..found_end,
            path,
            line,
            column,
        })
    })
}

/// Reads the position that starts `rest`, if any: its line, its column and its length in bytes.
fn position_suffix(rest: &str) -> Option<(usize, Option<usize>, usize)> {
    /// The number `text` starts with, and the bytes it takes.
    fn number(text: &str) -> Option<(usize, usize)> {
        let digits = text.bytes().take_while(u8::is_ascii_digit).count();
        Some((text[..digits].parse().ok()?, digits))
    }
    /// The `(7)` or `(7,5)` that `text` starts with, also in brackets, and the bytes it takes.
    fn parenthesized(text: &str) -> Option<(usize, Option<usize>, usize)> {
        let close = match text.chars().next()? {
            '(' => ')',
            '[' => ']',
            _ => return None,
        };
        let (line, digits) = number(&text[1..])?;
        let mut end = 1 + digits;
        let mut column = None;
        if let Some((col, digits)) = text[end..].strip_prefix(',').and_then(number) {
            column = Some(col);
            end += 1 + digits;
        }
        text[end..]
            .starts_with(close)
            .then_some((line, column, end + 1))
    }

    if let Some(range) = rest
        .strip_prefix(':')
        .filter(|range| range.starts_with(['(', '[']))
    {
        let (line, column, len) = parenthesized(range)?;
        let end_len = range[len..]
            .strip_prefix('-')
            .and_then(parenthesized)
            .map_or(0, |(.., len)| 1 + len);
        return Some((line, column, 1 + len + end_len));
    }
    if rest.starts_with('(') {
        return parenthesized(rest);
    }
    let (line, digits) = number(rest.strip_prefix(':')?)?;
    let end = 1 + digits;
    match rest[end..].strip_prefix(':').and_then(number) {
        Some((column, digits)) => Some((line, Some(column), end + 1 + digits)),
        None => Some((line, None, end)),
    }
}

/// Performs substitution of `~` and environment variables, see [`env::expand`](crate::env::expand) and [`expand_tilde`]
pub fn expand<T: AsRef<Path> + ?Sized>(path: &T) -> Cow<'_, Path> {
    let path = path.as_ref();
    let path = expand_tilde(path);
    match crate::env::expand(&*path) {
        Cow::Borrowed(_) => path,
        Cow::Owned(path) => PathBuf::from(path).into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        path::{Component, Path},
    };

    use regex_cursor::Input;
    use ropey::RopeSlice;

    use crate::path::{self, compile_path_regex};

    #[test]
    fn expand_tilde() {
        for path in ["~", "~/foo"] {
            let expanded = path::expand_tilde(Path::new(path));

            let tilde = Component::Normal(OsStr::new("~"));

            let mut component_count = 0;
            for component in expanded.components() {
                // No tilde left.
                assert_ne!(component, tilde);
                component_count += 1;
            }

            // The path was at least expanded to something.
            assert_ne!(component_count, 0);
        }
    }

    macro_rules! assert_match {
        ($regex: expr, $haystack: expr) => {
            let haystack = Input::new(RopeSlice::from($haystack));
            assert!(
                $regex.is_match(haystack),
                "regex should match {}",
                $haystack
            );
        };
    }
    macro_rules! assert_no_match {
        ($regex: expr, $haystack: expr) => {
            let haystack = Input::new(RopeSlice::from($haystack));
            assert!(
                !$regex.is_match(haystack),
                "regex should not match {}",
                $haystack
            );
        };
    }

    macro_rules! assert_matches {
        ($regex: expr, $haystack: expr, [$($matches: expr),*]) => {
            let src = $haystack;
            let matches: Vec<_> = $regex
                .find_iter(Input::new(RopeSlice::from(src)))
                .map(|it| &src[it.range()])
                .collect();
            assert_eq!(matches, vec![$($matches),*]);
        };
    }

    /// Linux-only path
    #[test]
    fn path_regex_unix() {
        // due to ambiguity with the `\` path separator we can't support space escapes `\ ` on windows
        let regex = compile_path_regex("^", "$", false, false);
        assert_match!(regex, "${FOO}/hello\\ world");
        assert_match!(regex, "${FOO}/\\ ");
    }

    /// Windows-only paths
    #[test]
    fn path_regex_windows() {
        let regex = compile_path_regex("^", "$", false, true);
        assert_match!(regex, "${FOO}/hello^ world");
        assert_match!(regex, "${FOO}/hello` world");
        assert_match!(regex, "${FOO}/^ ");
        assert_match!(regex, "${FOO}/` ");
        assert_match!(regex, r"foo\bar");
        assert_match!(regex, r"foo\bar");
        assert_match!(regex, r"..\bar");
        assert_match!(regex, r"..\");
        assert_match!(regex, r"C:\");
        assert_match!(regex, r"\\?\C:\foo");
        assert_match!(regex, r"\\server\foo");
    }

    /// Paths that should work on all platforms
    #[test]
    fn path_regex() {
        for windows in [false, true] {
            let regex = compile_path_regex("^", "$", false, windows);
            assert_no_match!(regex, "foo");
            assert_no_match!(regex, "");
            assert_match!(regex, "https://github.com/notifications/query=foo");
            assert_match!(regex, "file:///foo/bar");
            assert_match!(regex, "foo/bar");
            assert_match!(regex, "$HOME/foo");
            assert_match!(regex, "${FOO:-bar}/baz");
            assert_match!(regex, "foo/bar_");
            assert_match!(regex, "/home/bar");
            assert_match!(regex, "foo/");
            assert_match!(regex, "./");
            assert_match!(regex, "../");
            assert_match!(regex, "../..");
            assert_match!(regex, "./foo");
            assert_match!(regex, "./foo.rs");
            assert_match!(regex, "/");
            assert_match!(regex, "~/");
            assert_match!(regex, "~/foo");
            assert_match!(regex, "~/foo");
            assert_match!(regex, "~/foo/../baz");
            assert_match!(regex, "${HOME}/foo");
            assert_match!(regex, "$HOME/foo");
            assert_match!(regex, "/$FOO");
            assert_match!(regex, "/${FOO}");
            assert_match!(regex, "/${FOO}/${BAR}");
            assert_match!(regex, "/${FOO}/${BAR}/foo");
            assert_match!(regex, "/${FOO}/${BAR}");
            assert_match!(regex, "${FOO}/hello_$WORLD");
            assert_match!(regex, "${FOO}/hello_${WORLD}");
            let regex = compile_path_regex("", "", false, windows);
            assert_no_match!(regex, "");
            assert_matches!(
                regex,
                r#"${FOO}/hello_${WORLD}  ${FOO}/hello_${WORLD} foo("./bar", "/home/foo")""#,
                [
                    "${FOO}/hello_${WORLD}",
                    "${FOO}/hello_${WORLD}",
                    "./bar",
                    "/home/foo"
                ]
            );
            assert_matches!(
                regex,
                r#"--> helix-stdx/src/path.rs:427:13"#,
                ["helix-stdx/src/path.rs"]
            );
            assert_matches!(
                regex,
                r#"PATH=/foo/bar:/bar/baz:${foo:-/foo}/bar:${PATH}"#,
                ["/foo/bar", "/bar/baz", "${foo:-/foo}/bar"]
            );
            let regex = compile_path_regex("^", "$", true, windows);
            assert_no_match!(regex, "");
            assert_match!(regex, "foo");
            assert_match!(regex, "foo/");
            assert_match!(regex, "$FOO");
            assert_match!(regex, "${BAR}");
        }
    }

    /// The paths and positions found in `line`, as `(path, line, column)`.
    fn positions(line: &str) -> Vec<(&str, usize, Option<usize>)> {
        path::find_path_positions(RopeSlice::from(line))
            .map(|found| (&line[found.path], found.line, found.column))
            .collect()
    }

    /// Lines that compilers, test runners and stack traces print, as they print them.
    #[test]
    fn path_positions_in_compiler_messages() {
        for (line, expected) in [
            // cargo
            (" --> src/lib.rs:3:9", vec![("src/lib.rs", 3, Some(9))]),
            (
                "thread 'tests::it_fails' (1258450) panicked at src/lib.rs:10:9:",
                vec![("src/lib.rs", 10, Some(9))],
            ),
            // GHC, also with a span
            (
                "src/Main.hs:6:19: error: [GHC-88464] Variable not in scope: foo",
                vec![("src/Main.hs", 6, Some(19))],
            ),
            (
                "src/Main.hs:(12,5)-(14,10): error:",
                vec![("src/Main.hs", 12, Some(5))],
            ),
            // gcc and clang
            (
                "main.cpp:4:24: error: conversion from ‘int’ to non-scalar type ‘std::vector<int>’",
                vec![("main.cpp", 4, Some(24))],
            ),
            (
                "In file included from /usr/lib/gcc/x86_64-pc-linux-gnu/16/include/g++-v16/vector:68,",
                vec![("/usr/lib/gcc/x86_64-pc-linux-gnu/16/include/g++-v16/vector", 68, None)],
            ),
            ("                 from main.cpp:1:", vec![("main.cpp", 1, None)]),
            // javac through Gradle, and a failed Gradle test
            (
                "/home/me/gr/src/main/java/demo/App.java:4: error: incompatible types",
                vec![("/home/me/gr/src/main/java/demo/App.java", 4, None)],
            ),
            (
                "    org.opentest4j.AssertionFailedError at AppTest.java:6",
                vec![("AppTest.java", 6, None)],
            ),
            (
                "\tat demo.App.main(App.java:5)",
                vec![("App.java", 5, None)],
            ),
            // Kotlin, Typst and tsc
            (
                "e: file:///home/me/kt/Main.kt:12:5 Unresolved reference: foo",
                vec![("/home/me/kt/Main.kt", 12, Some(5))],
            ),
            ("  ┌─ main.typ:5:12", vec![("main.typ", 5, Some(12))]),
            // Maven
            (
                "[ERROR] /home/me/mv/src/main/java/demo/App.java:[4,13] cannot find symbol",
                vec![("/home/me/mv/src/main/java/demo/App.java", 4, Some(13))],
            ),
            (
                "a.ts(1,5): error TS2322: Type 'string' is not assignable to type 'number'.",
                vec![("a.ts", 1, Some(5))],
            ),
            // Paths without a position, and other URLs.
            (
                "     Running unittests src/lib.rs (target/debug/build/demo/a66/out/demo-a66)",
                vec![],
            ),
            ("   Compiling demo v0.1.0 (/home/me/demo)", vec![]),
            ("see https://example.com:8080/docs", vec![]),
            ("error: could not compile `demo` (lib)", vec![]),
            // Paths need not exist.
            ("Finished at 14:03:12", vec![("14", 3, Some(12))]),
        ] {
            assert_eq!(positions(line), expected, "{line}");
        }
    }

    #[test]
    fn path_position_ranges() {
        let line = "at src/Main.hs:(12,5)-(14,10): error";
        let found = path::find_path_positions(RopeSlice::from(line))
            .next()
            .unwrap();
        assert_eq!(&line[found.range], "src/Main.hs:(12,5)-(14,10)");
        let line = "e: file:///a/Main.kt:12:5 x";
        let found = path::find_path_positions(RopeSlice::from(line))
            .next()
            .unwrap();
        assert_eq!(&line[found.range], "file:///a/Main.kt:12:5");
        assert_eq!(&line[found.path], "/a/Main.kt");
        for line in [
            "a.rs:",
            "a.rs:x",
            "a.rs(1",
            "a.rs(1,)",
            "a.rs:(1,2",
            "a.rs:99999999999999999999999",
        ] {
            assert_eq!(positions(line), vec![], "{line}");
        }
        assert_eq!(positions("a.rs:7:"), vec![("a.rs", 7, None)]);
        assert_eq!(positions("a.rs:(1,2)-"), vec![("a.rs", 1, Some(2))]);
    }
}
