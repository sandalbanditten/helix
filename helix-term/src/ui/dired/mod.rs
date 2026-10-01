//! Dired buffers: directory listings like `eza --git -aolg`, opened over the whole editor, whose
//! lines are edited to change the files.
//!
//! A dired buffer is a pathless [`Document`] carrying the [`Listing`] it shows. Listings are read
//! off the main thread; the text is drawn in `eza`'s colors (or the theme's) by styling the
//! visible lines as they are, edited or not.

mod colors;
mod format;
mod listing;

use std::{cell::OnceCell, ops::Range, path::PathBuf};

use helix_core::{Rope, Selection};
use helix_loader::workspace_trust::TrustQuery;
use helix_view::{
    dired::{Listing, Source},
    editor::Action,
    graphics::Style,
    Document, Editor, View, ViewId,
};

use self::{
    colors::{EzaColors, Palette, You},
    format::Clock,
};
use crate::job;

/// What dired keeps beside its buffers: the colors of the environment and who the editor runs
/// as, both looked up once.
#[derive(Default)]
pub struct Dired {
    eza: OnceCell<Option<EzaColors>>,
    you: OnceCell<You>,
}

impl Dired {
    /// The styled char ranges of the lines `lines` of `doc`, in order, if it is a dired buffer.
    pub fn spans(
        &self,
        doc: &Document,
        lines: Range<usize>,
        editor: &Editor,
    ) -> Vec<(Range<usize>, Style)> {
        let Some(listing) = &doc.dired else {
            return Vec::new();
        };
        let eza = editor
            .config()
            .dired
            .colors
            .then(|| self.eza.get_or_init(EzaColors::from_environment).as_ref())
            .flatten();
        let palette = match eza {
            Some(eza) => Palette::eza(eza),
            None => Palette::theme(&editor.theme),
        };
        let you = self.you.get_or_init(you);
        let tree = matches!(listing.source, Source::Tree { .. });
        // Which entry a line shows is only known for sure while nothing is edited.
        let unmodified = !doc.is_modified();

        let text = doc.text().slice(..);
        let mut spans = Vec::new();
        for line in lines.start..lines.end.min(text.len_lines()) {
            let line_text = text.line(line).to_string();
            let Ok(parsed) = format::parse(&line_text, listing.columns, tree) else {
                continue;
            };
            let broken = unmodified
                && listing.entries.get(line).is_some_and(|entry| {
                    entry
                        .link
                        .as_ref()
                        .is_some_and(|link| link.target_kind.is_none())
                });
            let start = text.line_to_char(line);
            let char_of = |byte: usize| start + line_text[..byte].chars().count();
            spans.extend(
                palette
                    .spans(&line_text, &parsed, you, broken)
                    .into_iter()
                    .map(|(range, style)| (char_of(range.start)..char_of(range.end), style)),
            );
        }
        spans
    }
}

#[cfg(unix)]
fn you() -> You {
    use helix_stdx::users;
    let uid = users::current_user();
    You {
        user: users::user_name(uid).unwrap_or_else(|| uid.to_string()),
        groups: users::current_groups()
            .into_iter()
            .map(|gid| users::group_name(gid).unwrap_or_else(|| gid.to_string()))
            .collect(),
    }
}

#[cfg(not(unix))]
fn you() -> You {
    You::default()
}

/// Lists `source` in the background and shows it over the whole editor, with the cursor on the
/// entry at `select` (relative to the listing's root) if there is one.
pub fn open(editor: &Editor, source: Source, select: Option<PathBuf>) {
    let config = editor.config();
    let root = source.root();
    let options = listing::Options {
        sort: config.file_tree.sort,
        icons: config.dired.icons,
        providers: editor.diff_providers.clone(),
        trust_git: editor
            .workspace_trust
            .query(&helix_loader::find_workspace_in(root).0, TrustQuery::Git)
            .is_trusted(),
    };
    in_background(
        move || {
            let listing = listing::read(&source, &options);
            let text = format::text(&listing, &Clock::system());
            (listing, text)
        },
        move |editor, (listing, text)| show(editor, listing, text, select),
    );
}

/// Runs `work` on a thread of its own, then `apply` with its result on the main thread.
fn in_background<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    apply: impl FnOnce(&mut Editor, T) + Send + 'static,
) {
    let (sender, result) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    tokio::spawn(async move {
        let Ok(result) = result.await else {
            return;
        };
        job::dispatch(move |editor, _compositor| apply(editor, result)).await;
    });
}

/// Shows a listing in a zoomed split: in the buffer already showing the same source (listed
/// anew unless edited), or in a new one.
fn show(editor: &mut Editor, listing: Listing, text: String, select: Option<PathBuf>) {
    let shown = editor
        .documents()
        .find(|doc| {
            doc.dired
                .as_ref()
                .is_some_and(|shown| shown.source == listing.source)
        })
        .map(Document::id);
    match shown {
        Some(doc_id) => {
            let view = editor
                .tree
                .views()
                .find(|(view, _)| view.doc == doc_id)
                .map(|(view, _)| view.id);
            match view {
                Some(view) => editor.focus(view),
                None => editor.switch(doc_id, Action::VerticalSplit),
            }
            let (view, doc) = current!(editor);
            if !doc.is_modified() {
                relist(doc, view, listing, &text);
            }
        }
        None => {
            let mut doc = Document::from(
                Rope::from(text),
                None,
                editor.config.clone(),
                editor.syn_loader.clone(),
            );
            doc.dired = Some(Box::new(listing));
            doc.set_spelling_language_override(Some(Vec::new()));
            doc.detect_spelling();
            editor.new_file_from_document(Action::VerticalSplit, doc);
        }
    }
    let focus = editor.tree.focus;
    editor.tree.set_zoom(Some(focus));

    let scrolloff = editor.config().scrolloff;
    let (view, doc) = current!(editor);
    if let Some(pos) = select.and_then(|select| name_position(doc, &select)) {
        doc.set_selection(view.id, Selection::point(pos));
        view.ensure_cursor_in_view(doc, scrolloff);
    }
}

/// Replaces the text of a dired buffer with a new listing, keeping the cursors where they were
/// as far as the text allows, and starts its history afresh so undo cannot go back across it.
fn relist(doc: &mut Document, view: &mut View, listing: Listing, text: &str) {
    let transaction = helix_core::diff::compare_ropes(doc.text(), &Rope::from(text));
    doc.apply(&transaction, view.id);
    doc.append_changes_to_history(view);
    doc.reset_history();
    doc.dired = Some(Box::new(listing));
}

/// Where the name of the entry at `path` (relative to the root) starts in an unedited buffer.
fn name_position(doc: &Document, path: &std::path::Path) -> Option<usize> {
    let listing = doc.dired.as_ref()?;
    let line = listing
        .entries
        .iter()
        .position(|entry| entry.path == path)?;
    let text = doc.text().slice(..);
    let line_text = text.get_line(line)?.to_string();
    let tree = matches!(listing.source, Source::Tree { .. });
    let parsed = format::parse(&line_text, listing.columns, tree).ok()?;
    Some(text.line_to_char(line) + line_text[..parsed.name.start].chars().count())
}

/// The absolute path of the entry on the line of the primary cursor of `view`, while the buffer
/// is unedited.
pub fn cursor_path(doc: &Document, view: ViewId) -> Option<PathBuf> {
    let listing = doc.dired.as_ref()?;
    if doc.is_modified() {
        return None;
    }
    let text = doc.text().slice(..);
    let line = text.char_to_line(doc.selection(view).primary().cursor(text));
    let entry = listing.entries.get(line)?;
    Some(listing.source.root().join(&entry.path))
}
