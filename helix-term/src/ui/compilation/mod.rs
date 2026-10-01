//! The compilation buffer: the output of a command like `cargo build`, shown over the whole
//! editor as it arrives.
//!
//! It is a pathless [`Document`] carrying the [`Compilation`] it shows. The command runs in a
//! process group of its own, read off the main thread; dropping the document stops it.

mod locus;
mod output;
mod run;

use std::{path::PathBuf, time::Instant};

use anyhow::bail;
use helix_core::{
    command_line::Token,
    diagnostic::{DiagnosticProvider, Severity},
    movement::Direction,
    Diagnostic, Rope, RopeSlice, Selection, Transaction,
};
use helix_view::{
    compilation::{Compilation, Kind},
    current_ref, doc, doc_mut,
    editor::Action,
    expansion, view, view_mut, Document, DocumentId, Editor, ViewId,
};
use jiff::Zoned;

use self::{
    locus::{Finder, Locus, Resolver},
    output::End,
};

/// A command to run in the compilation buffer.
#[derive(Debug, Clone)]
pub struct Run {
    pub kind: Kind,
    pub command: String,
    /// The directory it runs in.
    pub dir: PathBuf,
    /// The language it was run for.
    pub language: Option<String>,
}

/// A run of `command` for the focused buffer: in the root of its language, or where the
/// compilation buffer ran when it is the focused one.
pub fn run_for(editor: &Editor, kind: Kind, command: String) -> Run {
    let doc = doc!(editor);
    if let Some(compilation) = &doc.compilation {
        return Run {
            kind,
            command,
            dir: compilation.dir.clone(),
            language: compilation.language.clone(),
        };
    }
    Run {
        kind,
        command,
        dir: language_root(editor, doc),
        language: doc.language_name().map(ToOwned::to_owned),
    }
}

/// The directory a build for `doc` runs in, the one its language server gets: the top-most one in
/// its workspace with one of its language's root markers, like `Cargo.toml`, else the workspace.
/// That is the workspace of Helix's working directory when `doc` lies in it, else `doc`'s own.
fn language_root(editor: &Editor, doc: &Document) -> PathBuf {
    let (cwd_workspace, _) = helix_loader::find_workspace();
    let workspace = match doc.path() {
        Some(path) if path.starts_with(&cwd_workspace) => cwd_workspace,
        _ => doc.workspace_root().to_path_buf(),
    };
    let root = doc.language_config().and_then(|config| {
        let dir = doc.path()?.parent()?.to_str()?;
        let config_roots = &editor.config().workspace_lsp_roots;
        let root_dirs = config
            .workspace_lsp_roots
            .as_deref()
            .unwrap_or(config_roots);
        helix_lsp::find_lsp_workspace(dir, &config.roots, root_dirs, &workspace, false)
    });
    root.unwrap_or(workspace)
}

/// The configured command of `kind` for the focused buffer, with its expansions done: its
/// language's, or that of the compilation buffer's language when it is the focused one. There it
/// is the command it ran, when that was of `kind`.
pub fn configured(editor: &Editor, kind: Kind) -> anyhow::Result<String> {
    let doc = doc!(editor);
    let language = match &doc.compilation {
        Some(compilation) if compilation.kind == kind => return Ok(compilation.command.clone()),
        Some(compilation) => compilation.language.clone(),
        None => doc.language_name().map(ToOwned::to_owned),
    };
    let key = match kind {
        Kind::Compile => "compile-command",
        Kind::Test => "test-command",
        Kind::Any => bail!("Commands given by hand are not configured"),
    };
    let Some(language) = language else {
        bail!("No {key} without a language");
    };
    let loader = editor.syn_loader.load();
    let command = loader
        .language_for_name(language.clone())
        .map(|lang| loader.language(lang).config())
        .and_then(|config| match kind {
            Kind::Compile => config.compile_command.clone(),
            Kind::Test => config.test_command.clone(),
            Kind::Any => None,
        });
    let Some(command) = command else {
        bail!("No {key} for {language}");
    };
    Ok(expansion::expand(editor, Token::expand(command.as_str()))?.into_owned())
}

/// The compilation buffer, if there is one.
fn buffer(editor: &Editor) -> Option<DocumentId> {
    editor
        .documents()
        .find(|doc| doc.compilation.is_some())
        .map(Document::id)
}

/// Runs `run` in the compilation buffer, shown over the whole editor, stopping the run it showed.
pub fn start(editor: &mut Editor, run: Run) {
    // Loci open in the split the command is run from, or in that of the run before when it is
    // run from the compilation buffer.
    let focused = editor.tree.focus;
    let from_buffer = doc!(editor).compilation.is_some();
    let doc_id = show(editor);
    let shell = editor.config().shell.clone();
    let header = output::header(&run.command, &run.dir, &Zoned::now());
    let finder = Finder::new(Resolver::new(
        run.dir.clone(),
        editor.config().file_picker.clone(),
    ));

    let doc = doc_mut!(editor, &doc_id);
    // Stop the old run first, so that the new one doesn't wait for its locks.
    let (id, origin) = match doc.compilation.take() {
        Some(old) if from_buffer => (old.run + 1, old.origin),
        Some(old) => (old.run + 1, Some(focused)),
        None => (0, Some(focused)),
    };
    let spawned = run::spawn(&shell, &run.command, &run.dir, finder, doc_id, id);
    let (process, text) = match spawned {
        Ok(process) => (Some(process), header),
        Err(err) => {
            editor.set_error(format!("Failed to run '{}': {err}", run.command));
            (None, format!("{header}Failed to run: {err}\n"))
        }
    };
    doc_mut!(editor, &doc_id).compilation = Some(Box::new(Compilation {
        kind: run.kind,
        command: run.command,
        dir: run.dir,
        language: run.language,
        run: id,
        started: Instant::now(),
        process,
        origin,
        visited: None,
    }));
    replace(editor, doc_id, &text);
}

/// Shows the compilation buffer over the whole editor, made if there is none, and returns it.
fn show(editor: &mut Editor) -> DocumentId {
    let doc_id = match buffer(editor) {
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
            doc_id
        }
        None => {
            let mut doc = Document::from(
                Rope::new(),
                None,
                editor.config.clone(),
                editor.syn_loader.clone(),
            );
            doc.set_spelling_language_override(Some(Vec::new()));
            doc.detect_spelling();
            editor.new_file_from_document(Action::VerticalSplit, doc)
        }
    };
    let focus = editor.tree.focus;
    editor.tree.set_zoom(Some(focus));
    doc_id
}

/// Replaces the text of the compilation buffer with `text`, without loci, the cursors at its
/// start.
fn replace(editor: &mut Editor, doc_id: DocumentId, text: &str) {
    let doc = doc!(editor, &doc_id);
    let end = doc.text().len_chars();
    let transaction = Transaction::change(doc.text(), [(0, end, Some(text.into()))].into_iter());
    apply(editor, doc_id, &transaction);
    let doc = doc_mut!(editor, &doc_id);
    doc.replace_diagnostics([], &[], &DiagnosticProvider::Compilation);
    let views: Vec<ViewId> = doc.selections().keys().copied().collect();
    for view in views {
        doc.set_selection(view, Selection::point(0));
    }
}

/// Applies `transaction` to the compilation buffer as output, which undo doesn't go back across.
fn apply(editor: &mut Editor, doc_id: DocumentId, transaction: &Transaction) {
    let view_id = editor.get_synced_view_id(doc_id);
    let view = view_mut!(editor, view_id);
    let doc = doc_mut!(editor, &doc_id);
    doc.apply(transaction, view.id);
    doc.append_changes_to_history(view);
    editor.reset_history(doc_id);
}

/// The line `ge` goes to: the last one, unless it is the empty one after a final line break.
fn last_line(text: RopeSlice) -> usize {
    if text.line(text.len_lines() - 1).len_chars() == 0 {
        text.len_lines().saturating_sub(2)
    } else {
        text.len_lines() - 1
    }
}

/// Appends `output` of run `run` to the compilation buffer `doc_id`, unless it shows another run
/// by now. The cursors on the last line stay on it.
fn append(editor: &mut Editor, doc_id: DocumentId, run: u64, output: run::Output) {
    let Some(doc) = editor.document(doc_id) else {
        return;
    };
    let Some(compilation) = doc.compilation.as_ref().filter(|shown| shown.run == run) else {
        return;
    };
    let mut text = output.text;
    if let Some(end) = output.end {
        text.push_str(&output::footer(
            end,
            &Zoned::now(),
            compilation.started.elapsed(),
        ));
    }

    let followers: Vec<ViewId> = {
        let text = doc.text().slice(..);
        let last = last_line(text);
        editor
            .tree
            .views()
            .filter(|(view, _)| view.doc == doc_id)
            .filter(|(view, _)| {
                text.char_to_line(doc.selection(view.id).primary().cursor(text)) >= last
            })
            .map(|(view, _)| view.id)
            .collect()
    };
    let end = doc.text().len_chars();
    let transaction = Transaction::change(doc.text(), [(end, end, Some(text.into()))].into_iter());
    apply(editor, doc_id, &transaction);
    let doc = doc_mut!(editor, &doc_id);
    let diagnostics = diagnostics(doc.text().slice(..), end, output.loci);
    let appended = end..doc.text().len_chars();
    doc.splice_diagnostics(diagnostics, &[appended], &DiagnosticProvider::Compilation);

    let scrolloff = editor.config().scrolloff;
    for view_id in followers {
        let view = view_mut!(editor, view_id);
        let doc = doc_mut!(editor, &doc_id);
        let text = doc.text().slice(..);
        let pos = text.line_to_char(last_line(text));
        doc.set_selection(view_id, Selection::point(pos));
        view.ensure_cursor_in_view(doc, scrolloff);
    }

    if let Some(end) = output.end {
        finish(editor, doc_id, end);
    }
}

/// The diagnostics of `loci` in output appended at `start` to `text`.
fn diagnostics(text: RopeSlice, start: usize, loci: Vec<Locus>) -> Vec<Diagnostic> {
    let diagnostic = |locus: Locus| {
        let from = start + locus.start;
        Diagnostic {
            range: helix_core::diagnostic::Range {
                start: from,
                end: from + locus.len,
            },
            ends_at_word: false,
            starts_at_word: false,
            zero_width: false,
            line: text.char_to_line(from),
            message: locus.message,
            severity: Some(locus.severity),
            code: None,
            provider: DiagnosticProvider::Compilation,
            tags: Vec::new(),
            source: None,
            data: Some(locus::target_data(&locus.path, locus.position)),
        }
    };
    loci.into_iter().map(diagnostic).collect()
}

/// Ends the run of the compilation buffer, telling how it ended and what it found.
fn finish(editor: &mut Editor, doc_id: DocumentId, end: End) {
    let doc = doc_mut!(editor, &doc_id);
    if let Some(compilation) = doc.compilation.as_mut() {
        compilation.process = None;
    }
    let count = |severity| {
        doc.diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == Some(severity))
            .count()
    };
    let counts: Vec<_> = [
        (count(Severity::Error), "error"),
        (count(Severity::Warning), "warning"),
    ]
    .into_iter()
    .filter(|&(count, _)| count > 0)
    .map(|(count, what)| {
        let plural = if count == 1 { "" } else { "s" };
        format!("{count} {what}{plural}")
    })
    .collect();
    let mut status = end.status();
    if !counts.is_empty() {
        status = format!("{status} ({})", counts.join(", "));
    }
    if end.failed() {
        editor.set_error(status);
    } else {
        editor.set_status(status);
    }
}

/// The loci of the compilation buffer, in order.
fn loci(doc: &Document) -> impl DoubleEndedIterator<Item = &Diagnostic> {
    doc.diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.provider == DiagnosticProvider::Compilation)
}

/// Opens the locus on the line of the cursor, if the focused buffer is the compilation buffer:
/// the locus under the cursor, else the first on the line. Tells whether there is one.
pub fn open_on_cursor_line(editor: &mut Editor) -> bool {
    let (view, doc) = current_ref!(editor);
    if doc.compilation.is_none() {
        return false;
    }
    let text = doc.text().slice(..);
    let cursor = doc.selection(view.id).primary().cursor(text);
    let line = text.char_to_line(cursor);
    let on_line: Vec<_> = loci(doc).filter(|locus| locus.line == line).collect();
    let locus = on_line
        .iter()
        .find(|locus| locus.range.start <= cursor && cursor < locus.range.end)
        .or(on_line.first());
    let Some(start) = locus.map(|locus| locus.range.start) else {
        return false;
    };
    let doc_id = doc.id();
    open(editor, doc_id, start);
    true
}

/// Visits the next or previous locus of the compilation buffer: after the cursor when the buffer
/// has the focus, else after the locus visited last. Its cursors move to it, and it opens.
pub fn visit(editor: &mut Editor, direction: Direction) -> anyhow::Result<()> {
    let Some(doc_id) = buffer(editor) else {
        bail!("No compilation buffer");
    };
    let doc = doc!(editor, &doc_id);
    let focused = view!(editor);
    let from = if focused.doc == doc_id {
        Some(
            doc.selection(focused.id)
                .primary()
                .cursor(doc.text().slice(..)),
        )
    } else {
        doc.compilation
            .as_ref()
            .and_then(|compilation| compilation.visited)
    };
    let range = {
        let mut loci = loci(doc);
        let locus = match direction {
            Direction::Forward => {
                loci.find(|locus| from.is_none_or(|from| locus.range.start > from))
            }
            Direction::Backward => loci
                .rev()
                .find(|locus| from.is_none_or(|from| locus.range.start < from)),
        };
        locus.map(|locus| locus.range)
    };
    let Some(range) = range else {
        match direction {
            Direction::Forward => bail!("No next locus"),
            Direction::Backward => bail!("No previous locus"),
        }
    };

    let scrolloff = editor.config().scrolloff;
    let views: Vec<ViewId> = editor
        .tree
        .views()
        .filter(|(view, _)| view.doc == doc_id)
        .map(|(view, _)| view.id)
        .collect();
    for view_id in views {
        let view = view_mut!(editor, view_id);
        let doc = doc_mut!(editor, &doc_id);
        doc.set_selection(view_id, Selection::single(range.start, range.end));
        view.ensure_cursor_in_view(doc, scrolloff);
    }
    open(editor, doc_id, range.start);
    Ok(())
}

/// Opens the locus starting at `start` in the compilation buffer `doc_id`. When the buffer has
/// the focus, the file opens beside it: in the split the command was run from, else in another,
/// else in a new one. Otherwise it opens in the focused split.
fn open(editor: &mut Editor, doc_id: DocumentId, start: usize) {
    let doc = doc_mut!(editor, &doc_id);
    let target = loci(doc)
        .find(|locus| locus.range.start == start)
        .and_then(|locus| locus::target(locus.data.as_ref()?));
    let (Some((path, position)), Some(compilation)) = (target, doc.compilation.as_mut()) else {
        return;
    };
    compilation.visited = Some(start);
    let origin = compilation.origin;

    let focused = editor.tree.focus;
    let mut action = Action::Replace;
    if view!(editor).doc == doc_id {
        let beside = origin
            .filter(|&origin| origin != focused && editor.tree.contains(origin))
            .or_else(|| {
                let mut views = editor.tree.views().map(|(view, _)| view.id);
                views.find(|&view| view != focused)
            });
        match beside {
            Some(view) => editor.focus(view),
            None => action = Action::VerticalSplit,
        }
    }
    match editor.open(&path, action) {
        Ok(_) => crate::commands::goto_position(editor, position),
        Err(err) => editor.set_error(format!("Open file failed: {err}")),
    }
}

/// Stops the running compilation, keeping its output.
pub fn kill(editor: &mut Editor) -> anyhow::Result<()> {
    let process = editor
        .documents()
        .find_map(|doc| doc.compilation.as_ref()?.process.as_ref());
    match process {
        Some(process) => Ok(process.kill()?),
        None => bail!("No compilation is running"),
    }
}

/// Runs the command of the compilation buffer `doc_id` again, refusing while file buffers are
/// unsaved.
pub fn rerun(editor: &mut Editor, doc_id: DocumentId) -> anyhow::Result<()> {
    let Some(compilation) = editor
        .document(doc_id)
        .and_then(|doc| doc.compilation.as_deref())
    else {
        bail!("Not the compilation buffer");
    };
    let run = Run {
        kind: compilation.kind,
        command: compilation.command.clone(),
        dir: compilation.dir.clone(),
        language: compilation.language.clone(),
    };
    ensure_saved(editor, &forced(run.kind, &run.command))?;
    start(editor, run);
    Ok(())
}

/// The command that runs a compilation of `kind` despite unsaved buffers.
pub fn forced(kind: Kind, command: &str) -> String {
    match kind {
        Kind::Compile => ":compile!".to_owned(),
        Kind::Test => ":compile-test!".to_owned(),
        Kind::Any => format!(":compile-any! {command}"),
    }
}

/// Refuses to compile while file buffers have unsaved changes, which the build would miss;
/// `forced` names the command that compiles anyway.
pub fn ensure_saved(editor: &Editor, forced: &str) -> anyhow::Result<()> {
    let unsaved: Vec<_> = editor
        .documents()
        .filter(|doc| doc.path().is_some() && doc.is_modified())
        .map(|doc| doc.display_name())
        .collect();
    if unsaved.is_empty() {
        return Ok(());
    }
    bail!(
        "{} unsaved buffer{}: {:?}; {forced} runs anyway",
        unsaved.len(),
        if unsaved.len() == 1 { "" } else { "s" },
        unsaved,
    )
}
