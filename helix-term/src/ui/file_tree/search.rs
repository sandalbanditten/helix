//! Finding workspace files by fuzzy matching their paths, in the order the tree lists them.

use std::{
    cmp::Ordering,
    collections::HashMap,
    path::{Path, PathBuf},
};

use helix_view::editor::{FilePickerConfig, FileTreeSort};
use nucleo::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32String,
};

use super::order::path_cmp;

/// The workspace files the file picker lists, relative to the root and in tree order.
pub struct Candidates {
    order: Order,
    paths: Vec<PathBuf>,
    /// `paths` as the matcher reads them.
    haystacks: Vec<Utf32String>,
}

/// A match of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// Whether the search went past the end of the tree to find it.
    pub wrapped: bool,
}

/// A query ready to match paths.
pub struct Matching {
    pattern: Pattern,
    matcher: Matcher,
}

impl Matching {
    /// `None` for a query without anything to match.
    pub fn new(query: &str) -> Option<Self> {
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        (!pattern.atoms.is_empty()).then(|| Self {
            pattern,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
        })
    }

    fn matches(&mut self, haystack: &Utf32String) -> bool {
        self.pattern
            .score(haystack.slice(..), &mut self.matcher)
            .is_some()
    }

    /// The indices of the characters of `path` that match, in order, if it matches.
    pub fn indices(&mut self, path: &str) -> Option<Vec<u32>> {
        let haystack = Utf32String::from(path);
        let mut indices = Vec::new();
        self.pattern
            .indices(haystack.slice(..), &mut self.matcher, &mut indices)?;
        indices.sort_unstable();
        indices.dedup();
        Some(indices)
    }
}

/// How [`Candidates`] come in tree order.
enum Order {
    /// Sorted by [`path_cmp`].
    Sorted(FileTreeSort),
    /// In the order they were given in, by their index.
    Given(HashMap<PathBuf, usize>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

impl Candidates {
    /// Walks the files below `root` like the file picker does. This blocks.
    pub fn collect(root: &Path, config: &FilePickerConfig, sort: FileTreeSort) -> Self {
        let paths = crate::ui::workspace_files(root, config)
            .filter_map(|path| Some(path.strip_prefix(root).ok()?.to_path_buf()))
            .collect();
        Self::new(paths, sort)
    }

    pub fn new(mut paths: Vec<PathBuf>, sort: FileTreeSort) -> Self {
        paths.sort_by(|a, b| path_cmp(sort, a, false, b, false));
        Self::with_order(paths, Order::Sorted(sort))
    }

    /// Files already in tree order, like the diff tree's, which lists a renamed file below the
    /// directory its paths share.
    pub fn in_order(paths: Vec<PathBuf>) -> Self {
        let indices = paths
            .iter()
            .enumerate()
            .map(|(index, path)| (path.clone(), index))
            .collect();
        Self::with_order(paths, Order::Given(indices))
    }

    fn with_order(paths: Vec<PathBuf>, order: Order) -> Self {
        let haystacks = paths
            .iter()
            .map(|path| Utf32String::from(path.to_string_lossy().as_ref()))
            .collect();
        Self {
            order,
            paths,
            haystacks,
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        match &self.order {
            Order::Sorted(sort) => self
                .paths
                .binary_search_by(|candidate| path_cmp(*sort, candidate, false, path, false))
                .is_ok(),
            Order::Given(indices) => indices.contains_key(path),
        }
    }

    /// Where the files after the entry at `from` start, and where the files before it end.
    fn starts(&self, from: &Path, is_dir: bool) -> (usize, usize) {
        match &self.order {
            Order::Sorted(sort) => {
                let start = |cmp: fn(Ordering) -> bool| {
                    self.paths
                        .partition_point(|path| cmp(path_cmp(*sort, path, false, from, is_dir)))
                };
                (start(Ordering::is_le), start(Ordering::is_lt))
            }
            Order::Given(indices) => match indices.get(from) {
                Some(&index) => (index + 1, index),
                // A directory: its files come one after the other.
                None => {
                    let start = self
                        .paths
                        .iter()
                        .position(|path| path.starts_with(from))
                        .unwrap_or(self.paths.len());
                    (start, start)
                }
            },
        }
    }

    /// The first file matching `query` after the entry at `from` (a directory or not), going in
    /// `direction` and wrapping around.
    pub fn find(
        &self,
        query: &str,
        from: &Path,
        is_dir: bool,
        direction: Direction,
    ) -> Option<Hit> {
        let mut matching = Matching::new(query)?;
        let len = self.paths.len();
        let mut matches = |&i: &usize| matching.matches(&self.haystacks[i]);
        let (after, before) = self.starts(from, is_dir);
        let (index, wrapped) = match direction {
            Direction::Forward => (after..len)
                .find(&mut matches)
                .map(|i| (i, false))
                .or_else(|| (0..after).find(&mut matches).map(|i| (i, true))),
            Direction::Backward => (0..before)
                .rev()
                .find(&mut matches)
                .map(|i| (i, false))
                .or_else(|| (before..len).rev().find(&mut matches).map(|i| (i, true))),
        }?;
        Some(Hit {
            path: self.paths[index].clone(),
            wrapped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In tree order: docs/guide.md, helix-core/src/lib.rs, helix-core/src/movement.rs,
    /// helix-term/src/ui/markdown.rs, helix-term/src/main.rs, README.md
    fn candidates() -> Candidates {
        let paths = [
            "README.md",
            "helix-core/src/movement.rs",
            "helix-term/src/main.rs",
            "helix-term/src/ui/markdown.rs",
            "helix-core/src/lib.rs",
            "docs/guide.md",
        ];
        Candidates::new(
            paths.into_iter().map(PathBuf::from).collect(),
            FileTreeSort::DirectoriesFirst,
        )
    }

    /// Where searching `md` from `from` lands, and whether it wrapped.
    fn find(from: &str, is_dir: bool, direction: Direction) -> Option<(String, bool)> {
        candidates()
            .find("md", from.as_ref(), is_dir, direction)
            .map(|hit| (hit.path.to_string_lossy().into_owned(), hit.wrapped))
    }

    #[test]
    fn searching_goes_in_tree_order() {
        let hit = |path: &str, wrapped| Some((path.to_owned(), wrapped));
        // `md` matches guide.md, markdown.rs (m…d) and README.md.
        assert_eq!(
            find("helix-core/src/lib.rs", false, Direction::Forward),
            hit("helix-term/src/ui/markdown.rs", false)
        );
        assert_eq!(
            find("helix-term", true, Direction::Forward),
            hit("helix-term/src/ui/markdown.rs", false)
        );
        assert_eq!(
            find("README.md", false, Direction::Forward),
            hit("docs/guide.md", true)
        );
        assert_eq!(
            find("helix-term/src/ui/markdown.rs", false, Direction::Backward),
            hit("docs/guide.md", false)
        );
        assert_eq!(find("", true, Direction::Backward), hit("README.md", true));
    }

    #[test]
    fn candidates_in_order_are_searched_in_that_order() {
        let candidates = Candidates::in_order(
            ["src/ui/main.rs", "src/b.md", "src/{ => ui}/a.md", "c.md"]
                .map(PathBuf::from)
                .to_vec(),
        );
        let find = |from: &str, is_dir, direction| {
            let hit = candidates.find("md", from.as_ref(), is_dir, direction)?;
            Some((hit.path.to_string_lossy().into_owned(), hit.wrapped))
        };
        let hit = |path: &str, wrapped| Some((path.to_owned(), wrapped));
        assert_eq!(
            find("src/b.md", false, Direction::Forward),
            hit("src/{ => ui}/a.md", false)
        );
        assert_eq!(
            find("src", true, Direction::Forward),
            hit("src/b.md", false)
        );
        assert_eq!(
            find("src/{ => ui}/a.md", false, Direction::Backward),
            hit("src/b.md", false)
        );
        assert_eq!(
            find("c.md", false, Direction::Forward),
            hit("src/b.md", true)
        );
        assert!(candidates.contains("src/{ => ui}/a.md".as_ref()));
        assert!(!candidates.contains("src/ui/a.md".as_ref()));
    }

    #[test]
    fn matching_tells_the_matched_characters() {
        let mut matching = Matching::new("main").unwrap();
        assert_eq!(
            matching.indices("helix-term/src/main.rs"),
            Some(vec![15, 16, 17, 18])
        );
        assert_eq!(matching.indices("helix-term/src/lib.rs"), None);
        assert!(Matching::new(" ").is_none());
        assert!(candidates()
            .find("", "".as_ref(), true, Direction::Forward)
            .is_none());
    }
}
