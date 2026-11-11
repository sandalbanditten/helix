//! Terminal buffers: a program on a pseudo-terminal, shown as its screen at the end of the text,
//! below what scrolled away. Keys go to the program in insert mode; normal mode reads the text.

mod keys;
mod run;

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use helix_core::{Rope, Selection, Transaction};
use helix_view::{
    doc, doc_mut,
    document::Mode,
    editor::Action,
    graphics::Color,
    input::KeyEvent,
    keyboard::{KeyCode, KeyModifiers},
    terminal::Terminal,
    view::ViewPosition,
    Document, DocumentId, Editor, View, ViewId,
};
use jiff::Zoned;

use super::compilation::output;

/// Opens a terminal of `command` run by the shell, or of the shell, in `dir`, in the focused split
/// or a new one as `action` says, and types into it.
pub fn open(
    editor: &mut Editor,
    action: Action,
    command: Option<String>,
    dir: PathBuf,
) -> anyhow::Result<()> {
    let (command, name) = match command {
        Some(command) => {
            let mut shell = editor.config().shell.clone();
            shell.push(command.clone());
            (shell, command)
        }
        None => {
            let shell = shell(editor);
            let name = Path::new(&shell[0])
                .file_name()
                .map_or_else(|| shell[0].clone(), |name| name.to_string_lossy().into());
            (shell, name)
        }
    };
    let mut doc = Document::from(
        Rope::new(),
        None,
        editor.config.clone(),
        editor.syn_loader.clone(),
    );
    doc.set_spelling_language_override(Some(Vec::new()));
    doc.detect_spelling();
    doc.terminal = Some(Box::new(Terminal::new(command, name, dir)));
    let doc_id = editor.new_file_from_document(action, doc);
    start(editor, doc_id)?;
    input(editor);
    Ok(())
}

/// The shell of terminals: the configured one, else `$SHELL`, else `sh`.
fn shell(editor: &Editor) -> Vec<String> {
    let shell = &editor.config().terminal.shell;
    if !shell.is_empty() {
        return shell.clone();
    }
    vec![std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned())]
}

/// Runs the command of the terminal buffer `doc_id`, again if it ran before.
fn start(editor: &mut Editor, doc_id: DocumentId) -> anyhow::Result<()> {
    let view = editor
        .tree
        .views()
        .find(|(view, _)| view.doc == doc_id)
        .map_or(editor.tree.focus, |(view, _)| view.id);
    let size = size(editor.tree.get(view), doc!(editor, &doc_id));
    let rgb = |color| match color {
        Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
        _ => None,
    };
    let palette = [
        rgb(editor.theme.get("ui.text").fg),
        rgb(editor.theme.get("ui.background").bg),
    ];
    let doc = doc_mut!(editor, &doc_id);
    let end = doc.text().len_chars();
    let Some(terminal) = doc.terminal.as_mut() else {
        return Ok(());
    };
    terminal.run += 1;
    terminal.started = Instant::now();
    let (process, pty, input) = run::spawn(
        &terminal.command,
        &terminal.dir,
        size,
        palette,
        doc_id,
        terminal.run,
    )
    .map_err(|err| anyhow::anyhow!("Failed to run '{}': {err}", terminal.command.join(" ")))?;
    terminal.process = Some(process);
    terminal.pty = Some(pty);
    terminal.input = Some(input);
    (terminal.screen, terminal.cursor) = (end, end);
    Ok(())
}

/// The columns and rows of the screen of `doc` in `view`: a column less than the text, for the
/// line break after a full row.
fn size(view: &View, doc: &Document) -> (u16, u16) {
    let rows = u16::try_from(view.inner_height()).unwrap_or(u16::MAX);
    (view.inner_width(doc).saturating_sub(1).max(1), rows)
}

/// Gives terminals the size of the views showing them, the focused one's if there are more.
pub fn follow_size(editor: &Editor) {
    let mut views: Vec<_> = editor.tree.views().collect();
    views.sort_by_key(|&(_, focused)| !focused);
    let mut sized = Vec::new();
    for (view, _) in views {
        let doc = doc!(editor, &view.doc);
        let Some(pty) = doc
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.pty.as_ref())
        else {
            continue;
        };
        if sized.contains(&view.doc) {
            continue;
        }
        sized.push(view.doc);
        let size = size(view, doc);
        if pty.size().is_ok_and(|shown| shown != size) {
            // The kernel tells the program.
            let _ = pty.resize(size.0, size.1);
        }
    }
}

/// Types into the focused terminal at its cursor, running its command again if it ended.
/// Returns false if the focused buffer isn't a terminal.
pub fn input(editor: &mut Editor) -> bool {
    let doc = doc!(editor);
    let Some(terminal) = &doc.terminal else {
        return false;
    };
    if !terminal.is_running() {
        if let Err(err) = start(editor, doc.id()) {
            editor.set_error(err.to_string());
            return true;
        }
    }
    editor.mode = Mode::Insert;
    let view = editor.tree.focus;
    follow(doc_mut!(editor), view);
    true
}

/// Puts the cursor of `view` at the terminal's, and the screen at the top of the view.
fn follow(doc: &mut Document, view: ViewId) {
    let Some(terminal) = &doc.terminal else {
        return;
    };
    let (screen, cursor) = (terminal.screen, terminal.cursor);
    doc.set_selection(view, Selection::point(cursor));
    doc.set_view_offset(
        view,
        ViewPosition {
            anchor: screen,
            horizontal_offset: 0,
            vertical_offset: 0,
        },
    );
}

/// Sends `key` to the program of the focused terminal, but for `Esc`, which goes to normal mode.
/// After `C-\`, which terminals without the kitty keyboard protocol send as `C-4`, any key goes
/// as it is.
pub fn key(editor: &mut Editor, key: KeyEvent) {
    let doc = doc_mut!(editor);
    let Some(terminal) = doc.terminal.as_mut() else {
        return;
    };
    let verbatim = std::mem::take(&mut terminal.verbatim);
    match (key.code, key.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) if !verbatim => editor.enter_normal_mode(),
        (KeyCode::Char('\\' | '4'), KeyModifiers::CONTROL) if !verbatim => terminal.verbatim = true,
        _ => terminal.send(keys::encode(key, terminal.app_cursor)),
    }
}

/// Pastes `text` into the program of the focused terminal.
pub fn paste(editor: &mut Editor, text: &str) {
    let Some(terminal) = doc_mut!(editor).terminal.as_mut() else {
        return;
    };
    // Lines are entered, as by the return key.
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    let input = if terminal.bracketed_paste {
        format!("\x1b[200~{text}\x1b[201~")
    } else {
        text
    };
    terminal.send(input.into_bytes());
}

fn apply(editor: &mut Editor, doc_id: DocumentId, transaction: &Transaction) {
    let view = editor.get_synced_view_id(doc_id);
    doc_mut!(editor, &doc_id).apply_output(transaction, view);
}

/// Shows `output` of run `run` in the terminal buffer `doc_id`, unless it runs another by now.
fn show(editor: &mut Editor, doc_id: DocumentId, run: u64, output: run::Output) {
    let Some(doc) = editor.document(doc_id) else {
        return;
    };
    let Some(terminal) = doc.terminal.as_ref().filter(|shown| shown.run == run) else {
        return;
    };
    let mut text = output.text.text;
    if let Some(end) = output.end {
        text.push_str(&output::footer(
            end,
            &Zoned::now(),
            terminal.started.elapsed(),
        ));
    }
    // Views on the screen keep it in view.
    let followers: Vec<_> = editor
        .tree
        .views()
        .filter(|(view, _)| view.doc == doc_id)
        .filter(|(view, _)| doc.selection(view.id).primary().head >= terminal.screen)
        .map(|(view, _)| view.id)
        .collect();
    let end = doc.text().len_chars();
    let from = end - output.replace.min(end);
    let transaction = Transaction::change(doc.text(), [(from, end, Some(text.into()))].into_iter());
    apply(editor, doc_id, &transaction);

    let doc = doc_mut!(editor, &doc_id);
    let end = doc.text().len_chars();
    let screen = end - output.screen.min(end);
    // What scrolled away goes, as the program asked or past the lines kept, a tenth more at once.
    let scrollback = doc.config.load().terminal.scrollback;
    let lines = doc.text().char_to_line(screen);
    let gone = if output.cleared {
        screen
    } else if lines > scrollback + scrollback / 10 {
        doc.text().line_to_char(lines - scrollback)
    } else {
        0
    };
    if let Some(terminal) = doc.terminal.as_mut() {
        let kept = terminal
            .styles
            .partition_point(|(range, _)| range.end <= from);
        terminal.styles.truncate(kept);
        let styles = output.text.styles.into_iter();
        terminal
            .styles
            .extend(styles.map(|(range, style)| (range.start + from..range.end + from, style)));
    }
    if gone > 0 {
        let transaction = Transaction::change(doc.text(), [(0, gone, None)].into_iter());
        apply(editor, doc_id, &transaction);
    }

    let focused = editor.tree.focus;
    let typing = editor.mode == Mode::Insert && editor.tree.get(focused).doc == doc_id;
    let doc = doc_mut!(editor, &doc_id);
    let end = doc.text().len_chars();
    let screen = end - output.screen.min(end);
    let Some(terminal) = doc.terminal.as_mut() else {
        return;
    };
    terminal.screen = screen + output.top;
    terminal.cursor = (screen + output.cursor).min(end);
    terminal.title = output.title;
    terminal.app_cursor = output.modes.app_cursor;
    terminal.bracketed_paste = output.modes.bracketed_paste;
    if output.end.is_some() {
        terminal.process = None;
        terminal.pty = None;
        terminal.input = None;
    }
    let ended = output.end.is_some();
    let scrolloff = editor.config().scrolloff;
    for view_id in followers {
        let view = editor.tree.get(view_id);
        let doc = doc_mut!(editor, &doc_id);
        if ended {
            // The end of the text shows, with how the program ended.
            let text = doc.text().slice(..);
            let last = text.line_to_char(text.len_lines().saturating_sub(2));
            doc.set_selection(view_id, Selection::point(last));
            view.ensure_cursor_in_view(doc, scrolloff);
        } else if view_id == focused && typing {
            follow(doc, view_id);
        } else {
            let mut offset = doc.view_offset(view_id);
            offset.anchor = doc.terminal.as_ref().map_or(0, |terminal| terminal.screen);
            doc.set_view_offset(view_id, offset);
        }
    }
    if ended && typing {
        editor.enter_normal_mode();
    }
}
