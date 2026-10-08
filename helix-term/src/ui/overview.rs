//! What the scrollbars of splits mark besides their thumbs: diagnostics, changes and the matches
//! of the search being made, by the document lines they are on.

use std::ops::Range;

use helix_core::{diagnostic::Severity, syntax::config::LanguageServerFeature};
use helix_stdx::rope::RopeSliceExt;
use helix_view::{
    annotations::diagnostics::DiagnosticFilter,
    editor::{LiveSearch, ScrollbarConfig},
    graphics::Color,
    Document, DocumentId, Editor, Theme,
};

/// A mark. The greater of two marks on a row is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    Change(Change),
    Diagnostic(Severity),
    /// A match of the search being made.
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Deleted,
    Modified,
    Added,
}

/// Which marks are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filter {
    /// The least severe diagnostics marked.
    pub diagnostics: DiagnosticFilter,
    pub changes: bool,
    pub search: bool,
}

impl From<&ScrollbarConfig> for Filter {
    fn from(config: &ScrollbarConfig) -> Self {
        Self {
            diagnostics: config.diagnostics,
            changes: config.diff,
            search: config.search,
        }
    }
}

/// The colors of marks: those of the diagnostics and diff gutters, and `ui.scrollbar.search`.
pub struct Colors {
    error: Color,
    warning: Color,
    info: Color,
    hint: Color,
    added: Color,
    modified: Color,
    deleted: Color,
    search: Color,
}

impl Colors {
    pub fn new(theme: &Theme) -> Self {
        let color = |scope| theme.get(scope).fg.unwrap_or(Color::Reset);
        Self {
            error: color("error"),
            warning: color("warning"),
            info: color("info"),
            hint: color("hint"),
            added: color("diff.plus.gutter"),
            modified: color("diff.delta.gutter"),
            deleted: color("diff.minus.gutter"),
            // Like the matches in the file tree, which fall back to `special` too.
            search: theme
                .try_get_exact("ui.scrollbar.search")
                .unwrap_or_else(|| theme.get("special"))
                .fg
                .unwrap_or(Color::Reset),
        }
    }

    pub fn of(&self, mark: Mark) -> Color {
        match mark {
            Mark::Change(Change::Added) => self.added,
            Mark::Change(Change::Modified) => self.modified,
            Mark::Change(Change::Deleted) => self.deleted,
            Mark::Diagnostic(Severity::Error) => self.error,
            Mark::Diagnostic(Severity::Warning) => self.warning,
            Mark::Diagnostic(Severity::Info) => self.info,
            Mark::Diagnostic(Severity::Hint) => self.hint,
            Mark::Search => self.search,
        }
    }
}

/// The lines with matches of the search being made, kept for the version of the document they
/// were found in.
struct SearchLines {
    doc: DocumentId,
    version: i32,
    query: String,
    lines: Vec<usize>,
}

/// What the scrollbars mark, worked out once for all of them.
#[derive(Default)]
pub struct Overview {
    search: Option<SearchLines>,
}

impl Overview {
    /// Finds the matches of the search being made, unless they are known.
    pub fn update(&mut self, editor: &Editor) {
        let config = &editor.config().scrollbar;
        let live_search = editor
            .live_search
            .as_ref()
            .filter(|_| config.enable && config.search);
        let Some(LiveSearch { doc, query, regex }) = live_search else {
            self.search = None;
            return;
        };
        let Some(document) = editor.document(*doc) else {
            self.search = None;
            return;
        };
        let version = document.version();
        if self.search.as_ref().is_some_and(|search| {
            search.doc == *doc && search.version == version && &search.query == query
        }) {
            return;
        }
        let text = document.text().slice(..);
        let mut lines: Vec<usize> = regex
            .find_iter(text.regex_input())
            .map(|found| text.byte_to_line(found.start()))
            .collect();
        lines.dedup();
        self.search = Some(SearchLines {
            doc: *doc,
            version,
            query: query.clone(),
            lines,
        });
    }

    /// Calls `mark` with each of `doc`'s marks that `filter` lets through and the lines it is on.
    pub fn marks(&self, doc: &Document, filter: Filter, mut mark: impl FnMut(Range<usize>, Mark)) {
        if let DiagnosticFilter::Enable(least) = filter.diagnostics {
            for diagnostic in doc.diagnostics() {
                // Like the diagnostics gutter.
                let shown = diagnostic.provider.language_server_id().is_none_or(|id| {
                    doc.language_servers_with_feature(LanguageServerFeature::Diagnostics)
                        .any(|server| server.id() == id)
                }) && doc.shows_diagnostic(diagnostic);
                let severity = diagnostic.severity.unwrap_or(Severity::Warning);
                if shown && severity >= least {
                    let line = diagnostic.line;
                    mark(line..line + 1, Mark::Diagnostic(severity));
                }
            }
        }
        if filter.changes {
            if let Some(handle) = doc.diff_handle() {
                let hunks = handle.load();
                for i in 0..hunks.len() {
                    let hunk = hunks.nth_hunk(i);
                    let (start, end) = (hunk.after.start as usize, hunk.after.end as usize);
                    let change = if hunk.is_pure_insertion() {
                        Change::Added
                    } else if hunk.is_pure_removal() {
                        Change::Deleted
                    } else {
                        Change::Modified
                    };
                    mark(start..end.max(start + 1), Mark::Change(change));
                }
            }
        }
        if filter.search {
            let search = self
                .search
                .as_ref()
                .filter(|search| search.doc == doc.id() && search.version == doc.version());
            for &line in search.into_iter().flat_map(|search| &search.lines) {
                mark(line..line + 1, Mark::Search);
            }
        }
    }

    /// The greatest of `doc`'s marks on each row of a rail `height` rows tall beside it.
    pub fn rows(&self, doc: &Document, filter: Filter, height: usize) -> Vec<Option<Mark>> {
        let mut rows = vec![None; height];
        let len = doc.text().len_lines().max(1);
        self.marks(doc, filter, |lines, mark| {
            let first = lines.start * height / len;
            let last = (lines.end - 1) * height / len;
            for row in rows.iter_mut().take(last + 1).skip(first) {
                *row = (*row).max(Some(mark));
            }
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_are_ranked() {
        let marks = [
            Mark::Change(Change::Added),
            Mark::Diagnostic(Severity::Hint),
            Mark::Diagnostic(Severity::Error),
            Mark::Search,
        ];
        assert!(marks.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(Some(Mark::Change(Change::Deleted)) > None);
    }
}
