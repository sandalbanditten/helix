//! Finding workspace files by fuzzy matching their paths, going through them in the order the
//! tree lists them. Queries use the file picker's syntax.

use std::path::{Path, PathBuf};

use helix_view::editor::{FilePickerConfig, FileTreeSort};
use nucleo::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32String,
};

use super::order::path_cmp;

/// The workspace files the file picker lists, relative to the root and in tree order.
pub struct Candidates {
    sort: FileTreeSort,
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
        let haystacks = paths
            .iter()
            .map(|path| Utf32String::from(path.to_string_lossy().as_ref()))
            .collect();
        Self {
            sort,
            paths,
            haystacks,
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths
            .binary_search_by(|candidate| path_cmp(self.sort, candidate, false, path, false))
            .is_ok()
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
        let (index, wrapped) = match direction {
            Direction::Forward => {
                let start = self
                    .paths
                    .partition_point(|path| path_cmp(self.sort, path, false, from, is_dir).is_le());
                (start..len)
                    .find(&mut matches)
                    .map(|i| (i, false))
                    .or_else(|| (0..start).find(&mut matches).map(|i| (i, true)))
            }
            Direction::Backward => {
                let start = self
                    .paths
                    .partition_point(|path| path_cmp(self.sort, path, false, from, is_dir).is_lt());
                (0..start)
                    .rev()
                    .find(&mut matches)
                    .map(|i| (i, false))
                    .or_else(|| (start..len).rev().find(&mut matches).map(|i| (i, true)))
            }
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
