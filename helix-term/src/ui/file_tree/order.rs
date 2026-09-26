//! The order the file tree lists entries in, shared by the tree and its search.

use std::{
    cmp::Ordering,
    path::{Component, Path},
};

use helix_view::editor::FileTreeSort;

/// Where an entry sorts among its siblings when directories come first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Directory,
    File,
    /// Broken links, fifos, sockets and devices.
    Other,
}

/// Orders two entries of one directory.
pub fn entry_cmp(sort: FileTreeSort, a: (&str, Group), b: (&str, Group)) -> Ordering {
    let group = match sort {
        FileTreeSort::DirectoriesFirst => a.1.cmp(&b.1),
        FileTreeSort::Alphabetical => Ordering::Equal,
    };
    group.then_with(|| natural_cmp(a.0, b.0))
}

#[cfg_attr(not(test), expect(dead_code, reason = "used by the search"))]
/// Orders two workspace-relative paths the way the tree lists them, where `a_is_dir` and
/// `b_is_dir` tell whether the last component is a directory. An ancestor comes before its
/// descendants.
pub fn path_cmp(
    sort: FileTreeSort,
    a: &Path,
    a_is_dir: bool,
    b: &Path,
    b_is_dir: bool,
) -> Ordering {
    let mut a = a.components().peekable();
    let mut b = b.components().peekable();
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x == y => {}
            (Some(x), Some(y)) => {
                let group = |is_dir| {
                    if is_dir {
                        Group::Directory
                    } else {
                        Group::File
                    }
                };
                let x_group = group(a.peek().is_some() || a_is_dir);
                let y_group = group(b.peek().is_some() || b_is_dir);
                return entry_cmp(
                    sort,
                    (&component_name(x), x_group),
                    (&component_name(y), y_group),
                );
            }
        }
    }
}

fn component_name(component: Component<'_>) -> std::borrow::Cow<'_, str> {
    component.as_os_str().to_string_lossy()
}

/// Compares names like `eza` sorts them: ignoring case, with runs of digits compared by their
/// value, so `file2` comes before `file10`. Equal values put the shorter run first (`2` before
/// `02`), and names that still tie are ordered by their bytes.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_cmp_folded(a, b).then_with(|| a.cmp(b))
}

fn natural_cmp_folded(mut a: &str, mut b: &str) -> Ordering {
    loop {
        let (x, y) = match (a.chars().next(), b.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => (x, y),
        };
        match (x.is_ascii_digit(), y.is_ascii_digit()) {
            (true, true) => {
                let (x_digits, x_rest) = split_digits(a);
                let (y_digits, y_rest) = split_digits(b);
                let ordering = number_cmp(x_digits, y_digits);
                if ordering.is_ne() {
                    return ordering;
                }
                (a, b) = (x_rest, y_rest);
            }
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (false, false) => {
                let ordering = x.to_lowercase().cmp(y.to_lowercase());
                if ordering.is_ne() {
                    return ordering;
                }
                (a, b) = (&a[x.len_utf8()..], &b[y.len_utf8()..]);
            }
        }
    }
}

fn split_digits(text: &str) -> (&str, &str) {
    let end = text
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(text.len());
    text.split_at(end)
}

/// Compares two runs of ASCII digits by value without parsing them, so any length works.
fn number_cmp(a: &str, b: &str) -> Ordering {
    let a_value = a.trim_start_matches('0');
    let b_value = b.trim_start_matches('0');
    a_value
        .len()
        .cmp(&b_value.len())
        .then_with(|| a_value.cmp(b_value))
        .then_with(|| a.len().cmp(&b.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted<'a>(names: &[&'a str]) -> Vec<&'a str> {
        let mut names = names.to_vec();
        names.sort_by(|a, b| natural_cmp(a, b));
        names
    }

    #[test]
    fn names_sort_naturally() {
        assert_eq!(
            sorted(&[
                "file10.txt",
                "file2.txt",
                "file02.txt",
                ".env",
                "file-link",
                "File1"
            ]),
            [
                ".env",
                "File1",
                "file2.txt",
                "file02.txt",
                "file10.txt",
                "file-link"
            ]
        );
        assert_eq!(sorted(&["b", "B", "a"]), ["a", "B", "b"]);
        assert_eq!(
            sorted(&["v99999999999999999999999", "v100000000000000000000000"]),
            ["v99999999999999999999999", "v100000000000000000000000"]
        );
        assert_eq!(
            sorted(&["Ärger", "zebra", "apfel"]),
            ["apfel", "zebra", "Ärger"]
        );
    }

    #[test]
    fn directories_come_first_unless_alphabetical() {
        let entries = [
            ("gamma.txt", Group::File),
            ("broken", Group::Other),
            ("delta", Group::Directory),
            ("alpha.txt", Group::File),
            ("beta", Group::Directory),
        ];
        let order = |sort| {
            let mut entries = entries.to_vec();
            entries.sort_by(|a, b| entry_cmp(sort, *a, *b));
            entries
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(FileTreeSort::DirectoriesFirst),
            ["beta", "delta", "alpha.txt", "gamma.txt", "broken"]
        );
        assert_eq!(
            order(FileTreeSort::Alphabetical),
            ["alpha.txt", "beta", "broken", "delta", "gamma.txt"]
        );
    }

    #[test]
    fn paths_sort_in_tree_order() {
        let sort = FileTreeSort::DirectoriesFirst;
        let cmp =
            |a: &str, a_dir, b: &str, b_dir| path_cmp(sort, a.as_ref(), a_dir, b.as_ref(), b_dir);
        assert_eq!(cmp("src", true, "src/main.rs", false), Ordering::Less);
        // `z/a.rs` is inside a directory, so it comes before the file `a.rs`.
        assert_eq!(cmp("z/a.rs", false, "a.rs", false), Ordering::Less);
        assert_eq!(cmp("src/b.rs", false, "src/a", true), Ordering::Greater);
        assert_eq!(cmp("src/a.rs", false, "src/a.rs", false), Ordering::Equal);
        assert_eq!(
            path_cmp(
                FileTreeSort::Alphabetical,
                "z/a.rs".as_ref(),
                false,
                "a.rs".as_ref(),
                false
            ),
            Ordering::Greater
        );
    }
}
