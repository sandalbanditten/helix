//! Dired buffers: directory listings like `eza --git -aolg`, opened over the whole editor, whose
//! lines are edited to change the files.
//!
//! A dired buffer is a pathless [`Document`] carrying the [`Listing`] it shows. Listings are read
//! off the main thread; the text is drawn in `eza`'s colors (or the theme's) by styling the
//! visible lines as they are, edited or not.

mod apply;
mod colors;
mod format;
mod git;
mod listing;
mod paste;
mod plan;

use std::{
    cell::OnceCell,
    collections::HashSet,
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail};
use helix_core::{
    diagnostic::{DiagnosticProvider, Severity},
    ChangeSet, Diagnostic, Rope, Selection,
};
use helix_loader::workspace_trust::TrustQuery;
use helix_view::{
    current,
    dired::{Listing, Source, Yanked},
    doc, doc_mut,
    editor::Action,
    graphics::Style,
    view_mut, Document, DocumentId, Editor, ViewId,
};

use self::{
    colors::{EzaColors, Palette, You},
    format::Clock,
};
use crate::{
    job,
    ui::{file_tree::watch::Watcher, EditorView},
};

/// What dired keeps beside its buffers: the colors of the environment and who the editor runs
/// as, both looked up once, and what watches the listed directories.
#[derive(Default)]
pub struct Dired {
    eza: OnceCell<Option<EzaColors>>,
    you: OnceCell<You>,
    watcher: Option<Watcher>,
    /// The listings the watched directories are those of, by their buffer and address.
    watched: Vec<(DocumentId, usize)>,
}

impl Dired {
    /// Watches the directories the dired buffers list, when they changed since the last call.
    pub fn follow(&mut self, editor: &Editor) {
        let listings: Vec<_> = editor
            .documents()
            .filter_map(|doc| {
                let listing: *const Listing = doc.dired.as_deref()?;
                Some((doc.id(), listing as usize))
            })
            .collect();
        if listings == self.watched {
            return;
        }
        self.watched = listings;
        let dirs: HashSet<PathBuf> = editor
            .documents()
            .filter_map(|doc| doc.dired.as_deref())
            .flat_map(watched_dirs)
            .collect();
        if self.watcher.is_none() {
            self.watcher = Watcher::new(|paths, editor, compositor| {
                if let Some(editor_view) = compositor.find::<EditorView>() {
                    editor_view.dired.changed(&paths, editor);
                }
            })
            .inspect_err(|err| log::warn!("dired cannot watch for changes: {err}"))
            .ok();
        }
        if let Some(watcher) = &mut self.watcher {
            watcher.watch(dirs);
        }
    }

    /// Lists the unedited dired buffers anew that list something of `paths`. A buffer edited
    /// meanwhile keeps its text; `:reload` lists it anew.
    fn changed(&mut self, paths: &HashSet<PathBuf>, editor: &mut Editor) {
        let affected: Vec<_> = editor
            .documents()
            .filter(|doc| !doc.is_modified())
            .filter_map(|doc| {
                let listing = doc.dired.as_deref()?;
                let dirs: HashSet<PathBuf> = watched_dirs(listing).collect();
                paths
                    .iter()
                    .any(|path| {
                        dirs.contains(path) || path.parent().is_some_and(|dir| dirs.contains(dir))
                    })
                    .then(|| (doc.id(), doc.version(), listing.source.clone()))
            })
            .collect();
        for (doc_id, version, source) in affected {
            let options = listing_options(editor, source.root());
            in_background(
                move || read(&source, &options),
                move |editor, listing| {
                    let unchanged = editor
                        .document(doc_id)
                        .is_some_and(|doc| doc.version() == version && !doc.is_modified());
                    if unchanged {
                        let view = editor.get_synced_view_id(doc_id);
                        relist(editor, doc_id, view, listing);
                    }
                },
            );
        }
    }

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

/// The directories whose changes can change `listing`: the listed ones and the repository's
/// `.git`, which changes with the git status.
fn watched_dirs(listing: &Listing) -> impl Iterator<Item = PathBuf> + '_ {
    let root = listing.source.root();
    let expanded = match &listing.source {
        Source::Directory(_) => None,
        Source::Tree { expanded, .. } => Some(expanded.iter().map(|dir| root.join(dir))),
    };
    std::iter::once(root.to_path_buf())
        .chain(expanded.into_iter().flatten())
        .chain(listing.repo.as_ref().map(|repo| repo.join(".git")))
}

/// How listings of `root` are read.
fn listing_options(editor: &Editor, root: &Path) -> listing::Options {
    let config = editor.config();
    listing::Options {
        sort: config.file_tree.sort,
        icons: config.dired.icons,
        providers: editor.diff_providers.clone(),
        trust_git: editor
            .workspace_trust
            .query(&helix_loader::find_workspace_in(root).0, TrustQuery::Git)
            .is_trusted(),
    }
}

/// Lists `source` in the background and shows it over the whole editor, with the cursor on the
/// entry at `select` (relative to the listing's root) if there is one.
pub fn open(editor: &Editor, source: Source, select: Option<PathBuf>) {
    let options = listing_options(editor, source.root());
    in_background(
        move || read(&source, &options),
        move |editor, listing| show(editor, listing, select),
    );
}

/// Reads what `source` lists, and the text that shows it.
fn read(source: &Source, options: &listing::Options) -> Listing {
    let mut listing = listing::read(source, options);
    listing.text = Rope::from(format::text(&listing, &Clock::system()));
    listing
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
fn show(editor: &mut Editor, listing: Listing, select: Option<PathBuf>) {
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
            if !doc!(editor, &doc_id).is_modified() {
                let view = editor.tree.focus;
                relist(editor, doc_id, view, listing);
            }
        }
        None => {
            let mut doc = Document::from(
                listing.text.clone(),
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
fn relist(editor: &mut Editor, doc_id: DocumentId, view_id: ViewId, mut listing: Listing) {
    let view = view_mut!(editor, view_id);
    let doc = doc_mut!(editor, &doc_id);
    let transaction = helix_core::diff::compare_ropes(doc.text(), &listing.text);
    doc.apply(&transaction, view.id);
    doc.append_changes_to_history(view);
    listing.yanked = doc.dired.take().and_then(|listed| listed.yanked);
    doc.dired = Some(Box::new(listing));
    publish(doc, &[]);
    editor.reset_history(doc_id);
}

/// Applies the edits of the dired buffer `doc_id` to the files: all of them, or none if they
/// have problems, which become diagnostics. Only `force` (`:w!`) applies what deletes or creates.
/// The buffer is listed anew after.
pub fn write(editor: &mut Editor, doc_id: DocumentId, force: bool) -> anyhow::Result<()> {
    let view_id = editor.get_synced_view_id(doc_id);
    let view = view_mut!(editor, view_id);
    let doc = doc_mut!(editor, &doc_id);
    doc.append_changes_to_history(view);
    let Some(listing) = doc.dired.as_deref().cloned() else {
        bail!("{} is no dired buffer", doc.display_name());
    };
    let history = doc.history.get_mut();
    let transactions = history.transactions_since(0);
    let changes = history
        .changes_since(0)
        .map(|transaction| transaction.changes().clone())
        .unwrap_or_else(|| ChangeSet::new(doc.text().slice(..)));
    let open = editor.documents().filter_map(|doc| doc.dired.as_deref());
    let listings =
        paste::Origins::new(open.chain(editor.closed_listings.iter().map(AsRef::as_ref)));
    let pasted = paste::pasted(&listing.text, &transactions, &listings);
    let trusted = listing_options(editor, listing.source.root()).trust_git;
    let doc = doc_mut!(editor, &doc_id);
    let (plan, problems) = plan::plan(
        &listing,
        doc.text().slice(..),
        &changes,
        &pasted,
        &Clock::system(),
        trusted,
    );
    publish(doc, &problems);
    let blocking: Vec<_> = problems
        .iter()
        .filter(|problem| !(force && problem.forced))
        .collect();
    match blocking.as_slice() {
        [] => {}
        [problem] => bail!("{}", problem.message),
        problems if problems.iter().all(|problem| problem.forced) => {
            bail!("{} changes need :w!", problems.len())
        }
        problems => bail!("{} problems, nothing applied", problems.len()),
    }

    let (applied, result) = apply::apply(editor, &plan);
    let source = moved_source(&listing.source, &applied);
    let listing = read(&source, &listing_options(editor, source.root()));
    relist(editor, doc_id, view_id, listing);
    result.map_err(|err| {
        anyhow!(
            "Applied {} of {} changes: {err:#}",
            applied.done,
            plan.len()
        )
    })?;
    editor.set_status(match plan.len() {
        _ if plan.is_empty() => "No changes".to_owned(),
        1 => "Applied 1 change".to_owned(),
        changes => format!("Applied {changes} changes"),
    });
    Ok(())
}

/// Notes which entries of the dired buffer `doc_id` a command there yanked, when it wrote a
/// register holding their lines, so that their pasted lines copy them and not others alike.
pub fn yanked(editor: &mut Editor, doc_id: DocumentId) {
    let (write, Some(register)) = editor.registers.written() else {
        return;
    };
    let Some(listing) = editor.document(doc_id).and_then(|doc| doc.dired.as_deref()) else {
        return;
    };
    let values = editor
        .registers
        .read(register, editor)
        .into_iter()
        .flatten();
    let lines: HashSet<String> = values
        .flat_map(|value| {
            let lines = value.split_inclusive('\n');
            lines
                .filter_map(|line| line.strip_suffix('\n'))
                .map(|line| line.trim_end_matches('\r').to_owned())
                .collect::<Vec<_>>()
        })
        .collect();
    if lines.is_empty() {
        return;
    }
    let paths: HashSet<PathBuf> = (0..listing.entries.len())
        .filter(|&index| lines.contains(paste::listed_line(listing, index).as_ref()))
        .map(|index| listing.entries[index].path.clone())
        .collect();
    // Other writes, like that of a change to a name, leave what was yanked to be pasted.
    if paths.is_empty() {
        return;
    }
    if let Some(listing) = doc_mut!(editor, &doc_id).dired.as_deref_mut() {
        listing.yanked = Some(Yanked { write, paths });
    }
}

/// What `source` lists once the moves of a write are done: the same, with a tree's expanded
/// directories where they moved to.
fn moved_source(source: &Source, applied: &apply::Applied) -> Source {
    match source {
        Source::Directory(_) => source.clone(),
        Source::Tree { root, expanded } => Source::Tree {
            root: root.clone(),
            expanded: expanded
                .iter()
                .filter_map(|dir| {
                    let dir = applied.path(&root.join(dir));
                    Some(dir.strip_prefix(root).ok()?.to_path_buf())
                })
                .collect(),
        },
    }
}

/// Lists the dired buffer `doc_id` anew, dropping its edits.
pub fn reload(editor: &mut Editor, doc_id: DocumentId) {
    let Some(source) = editor
        .document(doc_id)
        .and_then(|doc| Some(doc.dired.as_ref()?.source.clone()))
    else {
        return;
    };
    let listing = read(&source, &listing_options(editor, source.root()));
    let view_id = editor.get_synced_view_id(doc_id);
    relist(editor, doc_id, view_id, listing);
}

/// Shows `problems` as the diagnostics of `doc`, replacing the ones a write found before.
fn publish(doc: &mut Document, problems: &[plan::Problem]) {
    let text = doc.text().slice(..);
    let diagnostics: Vec<_> = problems
        .iter()
        .map(|problem| Diagnostic {
            range: helix_core::diagnostic::Range {
                start: problem.range.start,
                end: problem.range.end,
            },
            ends_at_word: false,
            starts_at_word: false,
            zero_width: problem.range.is_empty(),
            line: text.char_to_line(problem.range.start.min(text.len_chars())),
            message: problem.message.clone(),
            severity: Some(if problem.forced {
                Severity::Warning
            } else {
                Severity::Error
            }),
            code: None,
            provider: DiagnosticProvider::Dired,
            tags: Vec::new(),
            source: Some("dired".into()),
            data: None,
        })
        .collect();
    doc.replace_diagnostics(diagnostics, &[], &DiagnosticProvider::Dired);
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

#[cfg(test)]
mod tests {
    use std::{fs, time::Instant};

    use helix_core::Transaction;
    use helix_view::editor::FileTreeSort;

    use super::*;

    /// Times what a dired buffer costs on large listings. Run it in release:
    /// `cargo test --release -p helix-term --lib dired::tests::measure -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure() {
        let dir = tempfile::tempdir().unwrap();
        let root = helix_stdx::path::canonicalize(dir.path());
        // A directory of 10,000 files, and a tree of 50 directories of 200 files each.
        fs::create_dir(root.join("flat")).unwrap();
        for i in 0..10_000 {
            fs::write(root.join(format!("flat/file-{i:05}.txt")), "x").unwrap();
        }
        let mut expanded = std::collections::BTreeSet::new();
        for d in 0..50 {
            let sub = PathBuf::from(format!("tree/dir-{d:02}"));
            fs::create_dir_all(root.join(&sub)).unwrap();
            for i in 0..200 {
                let name = format!("file-{d:02}-{i:03}.rs");
                fs::write(root.join(&sub).join(name), "x").unwrap();
            }
            expanded.insert(sub);
        }
        expanded.insert(PathBuf::from("tree"));
        let options = listing::Options {
            sort: FileTreeSort::DirectoriesFirst,
            icons: true,
            providers: helix_vcs::DiffProviderRegistry::default(),
            trust_git: true,
        };
        let clock = Clock::system();
        let time = |what: &str, f: &mut dyn FnMut()| {
            let start = Instant::now();
            f();
            println!("{what}: {:?}", start.elapsed());
        };

        for (name, source) in [
            ("flat 10k", Source::Directory(root.join("flat"))),
            (
                "tree 10k",
                Source::Tree {
                    root: root.clone(),
                    expanded: expanded.clone(),
                },
            ),
        ] {
            let mut listing = None;
            time(&format!("{name}: read"), &mut || {
                listing = Some(listing::read(&source, &options));
            });
            let mut listing = listing.unwrap();
            let mut text = String::new();
            time(&format!("{name}: format"), &mut || {
                text = format::text(&listing, &clock)
            });
            listing.text = Rope::from(text.as_str());

            let colors = EzaColors::from_environment();
            let palette = match &colors {
                Some(colors) => Palette::eza(colors),
                None => Palette::theme(&helix_view::Theme::default()),
            };
            let you = you();
            let tree = matches!(source, Source::Tree { .. });
            time(&format!("{name}: spans of 60 lines"), &mut || {
                for line in 5000..5060 {
                    let line = listing.text.line(line).to_string();
                    let parsed = format::parse(&line, listing.columns, tree).unwrap();
                    std::hint::black_box(palette.spans(&line, &parsed, &you, false));
                }
            });

            // Every name renamed: with a cursor on each, as `%s\.` and a change does, and by
            // replacing the whole text.
            let ends: Vec<_> = (0..listing.entries.len())
                .map(|line| listing.text.line_to_char(line + 1) - 1)
                .collect();
            let renamed = Transaction::change(
                &listing.text,
                ends.into_iter().map(|end| (end, end, Some(".bak".into()))),
            );
            let edited = text.replace(".txt", ".md").replace(".rs\n", ".rs.bak\n");
            let rewritten = Transaction::change(
                &listing.text,
                std::iter::once((0, listing.text.len_chars(), Some(edited.into()))),
            );
            let end = listing.text.line_to_char(5001) - 1;
            let one = Transaction::change(
                &listing.text,
                std::iter::once((end, end, Some(".bak".into()))),
            );
            // Lines pasted at the end, as `yp` does, then renamed; with `guides`, a line
            // yanked with its guides edited.
            let paste = |lines: Range<usize>, guides: bool| {
                let start = listing.text.line_to_char(lines.start);
                let end = listing.text.line_to_char(lines.end);
                let mut yanked = listing.text.slice(start..end).to_string();
                if guides {
                    yanked = yanked.replacen("── ", "─ ", 1);
                }
                let end = listing.text.len_chars();
                let pasted = Transaction::change(
                    &listing.text,
                    std::iter::once((end, end, Some(yanked.into()))),
                );
                let mut text = listing.text.clone();
                pasted.apply(&mut text);
                let ends: Vec<_> = (listing.entries.len()..text.len_lines() - 1)
                    .map(|line| text.line_to_char(line + 1) - 1)
                    .collect();
                let renamed = Transaction::change(
                    &text,
                    ends.into_iter().map(|end| (end, end, Some(".bak".into()))),
                );
                vec![pasted, renamed]
            };
            for (how, transactions) in [
                ("rename one name", vec![one]),
                ("rename each name", vec![renamed]),
                ("rename the whole text", vec![rewritten]),
                ("copy one line", paste(5000..5001, false)),
                ("copy 1000 lines", paste(5000..6000, false)),
                (
                    "copy one line yanked with other guides",
                    paste(5000..5001, true),
                ),
            ] {
                let mut planned = None;
                time(&format!("{name}: plan to {how}"), &mut || {
                    let mut text = listing.text.clone();
                    let mut changes = ChangeSet::new(text.slice(..));
                    for transaction in &transactions {
                        transaction.apply(&mut text);
                        changes = changes.compose(transaction.changes().clone());
                    }
                    let origins = paste::Origins::new(std::iter::once(&listing));
                    let pasted = paste::pasted(&listing.text, &transactions, &origins);
                    planned = Some(plan::plan(
                        &listing,
                        text.slice(..),
                        &changes,
                        &pasted,
                        &clock,
                        true,
                    ));
                });
                let (plan, problems) = planned.unwrap();
                println!(
                    "  {} moves, {} copies, {} problems",
                    plan.moves.len(),
                    plan.copies.len(),
                    problems.len()
                );
            }
        }
    }
}
