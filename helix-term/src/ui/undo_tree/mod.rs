//! The undo tree docked beside the editor.
//!
//! [`UndoTree`] is owned by the [`EditorView`](crate::ui::EditorView), which reserves its columns
//! on the right, draws it and hands it keys while it is focused. Unfocused, it shows the history
//! of the focused buffer. Focused, it browses it: moving through the tree takes the buffer to the
//! revisions on the way, the diff gutter showing the changes against the revision browsing
//! started from. `Enter` keeps the revision reached, `Esc` goes back.

mod diff;
mod graph;
mod keys;
mod render;
mod rows;

use std::time::SystemTime;

use std::{cell::RefCell, rc::Rc};

use helix_core::{LineEnding, Position, Rope};
use helix_stdx::{
    path::get_relative_path,
    rope::{self, RegexInput},
};
use helix_view::{
    editor::UndoDiff,
    graphics::{CursorKind, Rect},
    input::{KeyEvent, MouseButton, MouseEvent, MouseEventKind},
    smooth_scroll::SmoothOffset,
    DocumentId, Editor, ViewId,
};
use tui::buffer::Buffer as Surface;

use self::{
    diff::{Compare, DiffPane, Motion, Output, Request},
    keys::{Action, Lookup},
    render::{Columns, Scene, Styles},
    rows::Rows,
};
use crate::{
    compositor::{Component, Context, Event, EventResult},
    ui::{
        dock::{self, Side},
        search, Prompt, PromptEvent,
    },
};

/// When the panel is shown.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    /// Only while the tree is focused.
    #[default]
    WhileFocused,
    Always,
}

/// Browsing the history of a buffer from the tree.
struct Browse {
    doc: DocumentId,
    view: ViewId,
    /// The revision the buffer was at, which `Esc` goes back to.
    start: usize,
    /// The text at `start`.
    start_text: Rope,
}

/// A search of the text revisions changed.
#[derive(Default)]
struct Search {
    /// The line a search is typed in.
    prompt: Option<Prompt>,
    /// What the prompt last told its callback: the line and the event. The prompt is no layer of
    /// the compositor, so the tree acts on what it told after handing it each key.
    reported: Rc<RefCell<Option<(String, PromptEvent)>>>,
    /// The revision the cursor was on when the search was typed.
    origin: Option<usize>,
    /// The query the matches are of: the one being typed, or the last search.
    query: String,
    /// The revisions whose changes match `query`.
    matches: Vec<usize>,
}

#[derive(Default)]
pub struct UndoTree {
    visibility: Visibility,
    browse: Option<Browse>,
    /// The width the panel is drawn at.
    width: u16,
    /// Whether the width was set by hand, rather than fitted to the rows.
    set_by_hand: bool,
    /// The widest the panel may get in the current screen.
    max_width: u16,
    /// Where the panel was laid out last.
    area: Option<Rect>,
    /// The keys of an unfinished sequence like `g`.
    pending: Vec<KeyEvent>,
    rows: Option<Rows>,
    /// The first row shown, and the row drawn first while scrolling there.
    start: usize,
    drawn_start: usize,
    smooth_scroll: SmoothOffset,
    /// The buffer and revision of the cursor when it was last brought into view.
    revealed: Option<(DocumentId, usize)>,
    search: Search,
    diff: DiffPane,
    /// Whether the diff part of the panel has the keys, rather than the tree part.
    diff_focused: bool,
}

impl UndoTree {
    pub fn is_focused(&self) -> bool {
        self.browse.is_some()
    }

    fn is_presented(&self) -> bool {
        self.visibility == Visibility::Always || self.is_focused()
    }

    /// Switches between always showing the panel and showing it only while focused.
    pub fn toggle(&mut self, editor: &mut Editor) {
        match self.visibility {
            Visibility::Always => {
                self.visibility = Visibility::WhileFocused;
                self.unfocus(editor);
            }
            Visibility::WhileFocused => self.visibility = Visibility::Always,
        }
    }

    /// Focuses the tree on the focused buffer's history, or gives focus back to the editor,
    /// keeping the revision the buffer is at.
    pub fn toggle_focus(&mut self, editor: &mut Editor) {
        if self.is_focused() {
            self.unfocus(editor);
        } else {
            self.focus(editor);
        }
    }

    fn focus(&mut self, editor: &mut Editor) {
        let (view, doc) = helix_view::current!(editor);
        doc.append_changes_to_history(view);
        let start = doc.get_current_revision();
        let start_text = doc.text().clone();
        doc.set_diff_override(Some(start_text.clone()));
        self.browse = Some(Browse {
            doc: doc.id(),
            view: view.id,
            start,
            start_text,
        });
        self.revealed = None;
    }

    /// Gives focus back to the editor, keeping the revision the buffer is at.
    pub fn unfocus(&mut self, editor: &mut Editor) {
        let Some(browse) = self.browse.take() else {
            return;
        };
        self.pending.clear();
        self.search.prompt = None;
        self.diff.compare = Compare::Parent;
        self.diff_focused = false;
        if let Some(doc) = editor.document_mut(browse.doc) {
            doc.set_diff_override(None);
        }
    }

    /// The buffer whose history is shown, and the view it is shown in.
    fn target(&self, editor: &Editor) -> Option<(ViewId, DocumentId)> {
        match &self.browse {
            Some(browse) => editor
                .tree
                .contains(browse.view)
                .then_some((browse.view, browse.doc))
                .filter(|_| editor.document(browse.doc).is_some()),
            None => {
                let view = editor.tree.get(editor.tree.focus);
                Some((view.id, view.doc))
            }
        }
    }

    /// Makes the rows those of the shown buffer's history as it is. Returns the revision the
    /// buffer is at.
    fn refresh(&mut self, editor: &mut Editor) -> Option<usize> {
        let (_, doc_id) = self.target(editor)?;
        let doc = editor.document_mut(doc_id)?;
        let history = doc.history.get_mut();
        if !self
            .rows
            .as_ref()
            .is_some_and(|rows| rows.are_of(doc_id, history))
        {
            self.rows = Some(Rows::new(doc_id, history));
            self.search.matches = match self.regex(&self.search.query, editor) {
                Ok(regex) if !self.search.query.is_empty() => self.matching(&regex, editor),
                _ => Vec::new(),
            };
        }
        Some(doc_mut(editor, doc_id).history.get_mut().current_revision())
    }

    /// Lays the panel out on the right of `main`, if it is shown and fits.
    pub fn layout(&mut self, main: Rect, editor: &mut Editor) -> Option<Rect> {
        let appearing = self.area.is_none();
        self.area = self.place(main, appearing, editor);
        self.area
    }

    fn place(&mut self, main: Rect, appearing: bool, editor: &mut Editor) -> Option<Rect> {
        // A zoomed view covers the panel.
        if !self.is_presented() || editor.tree.zoomed().is_some() {
            self.unfocus(editor);
            return None;
        }
        self.refresh(editor)?;
        self.max_width = dock::max_width(main);
        if appearing && !self.set_by_hand || self.width == 0 {
            self.width = self.fitted_width();
        }
        self.width = dock::clamp_width(self.width, self.max_width);
        if main.height < 2 || main.width < self.width + dock::MIN_EDITOR_WIDTH {
            self.unfocus(editor);
            return None;
        }
        // The statusline below keeps the full width.
        Some(Rect::new(
            main.right() - self.width,
            main.y,
            self.width,
            main.height - 1,
        ))
    }

    /// The width that shows the widest row whole, within the limits.
    fn fitted_width(&self) -> u16 {
        let Some(rows) = &self.rows else {
            return dock::MIN_WIDTH;
        };
        let columns = Columns::new(rows, SystemTime::now());
        let widest = (0..rows.parents.len())
            .map(|revision| columns.width(rows, revision))
            .max()
            .unwrap_or_default();
        dock::fitted_width(widest.saturating_sub(1), self.max_width)
    }

    pub fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let Some(current) = self.refresh(cx.editor) else {
            return;
        };
        let Some(rows) = &self.rows else {
            return;
        };
        let (area, diff_area) = self.split(area, cx.editor);
        let height = area.height as usize;
        let cursor_row = rows.graph.row_of(current);
        // The cursor comes into view when it moves, or the buffer changes; scrolling keeps it
        // out of view otherwise.
        if self.revealed != Some((rows.doc, current)) {
            self.revealed = Some((rows.doc, current));
            self.start = reveal(self.start, height, cursor_row, cx.editor.config().scrolloff);
        }
        self.start = self.start.min(rows.len().saturating_sub(height));
        let start = self.smooth_scroll.frame(self.start, area.height, cx.editor);
        self.drawn_start = start;
        let now = SystemTime::now();
        let marked = self.browse.as_ref().map_or(current, |browse| browse.start);
        Scene {
            rows,
            styles: &Styles::new(&cx.editor.theme),
            columns: &Columns::new(rows, now),
            marked,
            cursor: self.is_focused().then_some(cursor_row),
            marked_cursor: !self.diff_focused,
            start,
            now,
            matches: &self.search.matches,
        }
        .render(area, surface);
        match diff_area {
            Some(diff_area) => self.render_diff(diff_area, current, surface, cx.editor),
            None => self.diff_focused = false,
        }
    }

    /// The area of the graph and the one of the diff below it, if one is shown.
    fn split(&self, area: Rect, editor: &Editor) -> (Rect, Option<Rect>) {
        let config = &editor.config().undo;
        if config.diff == UndoDiff::None {
            return (area, None);
        }
        // The graph keeps at least half the panel; the diff has a row naming it.
        let height = config.diff_height.min(area.height / 2).saturating_add(1);
        let diff = area.clip_top(area.height.saturating_sub(height));
        (area.clip_bottom(diff.height), Some(diff))
    }

    /// Draws the diff of `current`, the revision under the cursor, asking for it first if it
    /// hasn't been.
    fn render_diff(
        &mut self,
        area: Rect,
        current: usize,
        surface: &mut Surface,
        editor: &mut Editor,
    ) {
        let (content, _) = dock::split(area, Side::Right);
        let Some((_, doc_id)) = self.target(editor) else {
            return;
        };
        let doc = doc_mut(editor, doc_id);
        // Changes still to commit make the text no revision's.
        if doc.changes().is_empty() {
            let compare = self.diff.compare;
            let history = doc.history.get_mut();
            let against = match (&self.browse, compare) {
                (Some(browse), Compare::Start) => Some(browse.start),
                _ => (current != 0).then(|| history.parent(current)),
            };
            let request = Request {
                doc: doc_id,
                revision: current,
                against,
                width: content.width,
                tool: editor.config().undo.diff,
            };
            if !self.diff.is_asked(&request) {
                let doc = doc_mut(editor, doc_id);
                let new = doc.text().clone();
                let old = match (&self.browse, compare) {
                    (Some(browse), Compare::Start) => browse.start_text.clone(),
                    _ => {
                        let mut old = new.clone();
                        let (_, inversion) = doc.history.get_mut().changes(current);
                        inversion.apply(&mut old);
                        old
                    }
                };
                let path = doc.path().map_or_else(
                    || "buffer".to_owned(),
                    |path| get_relative_path(path).to_string_lossy().into_owned(),
                );
                self.diff.ask(request, Some((old, new)), path, editor);
            }
        }
        let styles = Styles::new(&editor.theme);
        let mut header = styles.base.patch(styles.guide);
        if self.diff_focused {
            header = header.patch(styles.selected);
        }
        self.diff
            .render(content, surface, styles.base, header, editor);
        dock::render_rail(
            surface,
            area,
            Side::Right,
            0..0,
            styles.base,
            styles.track,
            styles.thumb,
        );
    }

    /// Runs the editor's command `name`, `count` times, if the panel has the keys and the command
    /// is one of the panel's: moving between the views moves between the tree and the diff
    /// part of the panel, and in the diff part the editor's motions move through the diff.
    /// Returns whether it ran.
    pub fn run_editor_command(&mut self, name: &str, count: usize, editor: &Editor) -> bool {
        if !self.is_focused() {
            return false;
        }
        match name {
            "jump_view_down" => {
                self.diff_focused = editor.config().undo.diff != UndoDiff::None;
                true
            }
            "jump_view_up" => {
                self.diff_focused = false;
                true
            }
            _ if self.diff_focused => match Motion::of_command(name) {
                Some(motion) => {
                    self.diff.go(motion, count);
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// Takes the diff that the request of `generation` came out as.
    pub(crate) fn diff_ready(
        &mut self,
        generation: u64,
        output: Output,
        theme: &helix_view::Theme,
    ) {
        self.diff.ready(generation, output, theme);
    }

    /// Draws the line of a search over the command line of `area`, the screen.
    pub fn render_command_line(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        if let Some(prompt) = &mut self.search.prompt {
            prompt.render(area, surface, cx);
        }
    }

    /// Where the terminal cursor goes while the tree is focused: in the line of a search.
    pub fn cursor(&self, area: Rect, editor: &Editor) -> (Option<Position>, CursorKind) {
        match &self.search.prompt {
            Some(prompt) => prompt.cursor(area, editor),
            None => (None, CursorKind::Hidden),
        }
    }

    /// Handles `key` while the tree is focused. A key it does not bind is ignored, for the
    /// editor to handle.
    pub fn handle_key(&mut self, key: KeyEvent, cx: &mut Context) -> EventResult {
        cx.editor.autoinfo = None;
        if self.search.prompt.is_some() {
            self.handle_search(&Event::Key(key), cx);
            return EventResult::Consumed(None);
        }
        let mut sequence = std::mem::take(&mut self.pending);
        sequence.push(key);
        // The diff part has a few keys of its own; the editor's motions move through it.
        let lookup = if self.diff_focused {
            keys::diff_lookup(&sequence)
        } else {
            keys::lookup(&sequence)
        };
        match lookup {
            Lookup::Action(action) => self.run(action, cx.editor),
            Lookup::Prefix => {
                cx.editor.autoinfo = Some(keys::info(&sequence));
                self.pending = sequence;
            }
            Lookup::Unbound if sequence.len() > 1 => {}
            Lookup::Unbound => return EventResult::Ignored(None),
        }
        EventResult::Consumed(None)
    }

    fn run(&mut self, action: Action, editor: &mut Editor) {
        let Some(current) = self.refresh(editor) else {
            return;
        };
        let Some(rows) = &self.rows else {
            return;
        };
        let height = self.area.map_or(1, |area| area.height as usize).max(1);
        let row = rows.graph.row_of(current);
        // The revision of the first row from `row` on in `direction` that has one.
        let revision_from = |row: usize, down: bool| {
            if down {
                (row..rows.len()).find_map(|row| rows.revision(row))
            } else {
                (0..=row.min(rows.len() - 1))
                    .rev()
                    .find_map(|row| rows.revision(row))
            }
        };
        let siblings = &rows.children[rows.parents[current]];
        let target = match action {
            Action::Older => revision_from(row + 1, true),
            Action::Newer => row.checked_sub(1).and_then(|row| revision_from(row, false)),
            Action::NewerSibling if current != 0 => {
                siblings.iter().copied().find(|&sibling| sibling > current)
            }
            Action::OlderSibling if current != 0 => siblings
                .iter()
                .rev()
                .copied()
                .find(|&sibling| sibling < current),
            Action::NewerSibling | Action::OlderSibling => None,
            Action::Undo => (current != 0).then(|| rows.parents[current]),
            Action::Redo => doc_mut(editor, rows.doc)
                .history
                .get_mut()
                .last_child(current),
            Action::OlderWritten => rows.saves.iter().copied().filter(|&r| r < current).max(),
            Action::NewerWritten => rows.saves.iter().copied().filter(|&r| r > current).min(),
            Action::HalfPageDown => revision_from(row + height / 2, true),
            Action::HalfPageUp => revision_from(row.saturating_sub(height / 2), false),
            Action::PageDown => revision_from(row + height, true),
            Action::PageUp => revision_from(row.saturating_sub(height), false),
            Action::Newest => Some(rows.parents.len() - 1),
            Action::Oldest => Some(0),
            Action::AlignCenter => {
                self.start = row.saturating_sub(height / 2);
                None
            }
            Action::AlignTop => {
                self.start = row;
                None
            }
            Action::AlignBottom => {
                self.start = (row + 1).saturating_sub(height);
                None
            }
            Action::Search => {
                let reported = Rc::clone(&self.search.reported);
                self.search.prompt = Some(search::regex_prompt(
                    "search:".into(),
                    Some(search::REGISTER),
                    search::completion(editor, search::REGISTER),
                    move |_, line, event| *reported.borrow_mut() = Some((line.to_owned(), event)),
                    editor,
                ));
                self.search.origin = Some(current);
                None
            }
            Action::ToggleDiff => {
                self.diff.compare = match self.diff.compare {
                    Compare::Parent => Compare::Start,
                    Compare::Start => Compare::Parent,
                };
                None
            }
            Action::DiffDown => {
                self.diff.go(Motion::HalfPageDown, 1);
                None
            }
            Action::DiffUp => {
                self.diff.go(Motion::HalfPageUp, 1);
                None
            }
            Action::NextMatch | Action::PreviousMatch => {
                self.find_next(current, action == Action::NextMatch, editor)
            }
            Action::Grow | Action::Shrink => {
                let width = if action == Action::Grow {
                    self.width.saturating_add(1)
                } else {
                    self.width.saturating_sub(1)
                };
                self.width = dock::clamp_width(width, self.max_width);
                self.set_by_hand = true;
                None
            }
            Action::Fit => {
                self.width = self.fitted_width();
                self.set_by_hand = false;
                None
            }
            Action::ToggleWidth => {
                self.width = dock::toggled_width(self.width, self.max_width);
                self.set_by_hand = true;
                None
            }
            Action::Help => {
                editor.autoinfo = Some(if self.diff_focused {
                    keys::diff_info()
                } else {
                    keys::info(&[])
                });
                None
            }
            Action::Keep => {
                self.unfocus(editor);
                None
            }
            Action::GoBack => {
                if let Some(start) = self.browse.as_ref().map(|browse| browse.start) {
                    self.browse_to(start, editor);
                }
                self.unfocus(editor);
                None
            }
        };
        if let Some(target) = target {
            self.browse_to(target, editor);
        }
    }

    /// Takes the buffer to `revision`.
    fn browse_to(&mut self, revision: usize, editor: &mut Editor) {
        let Some((view_id, doc_id)) = self.target(editor) else {
            return;
        };
        if revision >= doc_mut(editor, doc_id).history.get_mut().len() {
            return;
        }
        let scrolloff = editor.config().scrolloff;
        let view = editor.tree.get_mut(view_id);
        let doc = editor
            .documents
            .get_mut(&doc_id)
            .expect("the target exists");
        doc.append_changes_to_history(view);
        let from = doc.get_current_revision();
        doc.jump_to_revision(view, revision);
        // The view glides to where the revision changed the text rather than jumping there.
        if let Some(changes) = doc.history.get_mut().changes_since(from) {
            view.scroll_smoothly_across(doc, changes.changes());
        }
        view.ensure_cursor_in_view(doc, scrolloff);
    }

    /// The next revision below (`down`) or above `from` whose changes match the search, and
    /// whether the search wrapped around to find it, which it does if `wrap`.
    fn find(&self, from: usize, down: bool, wrap: bool) -> Option<(usize, bool)> {
        let rows = self.rows.as_ref()?;
        let matches = self.search.matches.iter().copied();
        let row = |revision: usize| rows.graph.row_of(revision);
        let from = row(from);
        let (next, wrapped) = if down {
            let after = matches
                .clone()
                .filter(|&r| row(r) > from)
                .min_by_key(|&r| row(r));
            (after, matches.min_by_key(|&r| row(r)))
        } else {
            let before = matches
                .clone()
                .filter(|&r| row(r) < from)
                .max_by_key(|&r| row(r));
            (before, matches.max_by_key(|&r| row(r)))
        };
        match next {
            Some(revision) => Some((revision, false)),
            None => wrapped.filter(|_| wrap).map(|revision| (revision, true)),
        }
    }

    /// The next revision below (`down`) or above `current` whose changes match the last search,
    /// the editor's or the tree's, telling as the editor's `n` and `N` do when it wrapped around
    /// or there is none.
    fn find_next(&mut self, current: usize, down: bool, editor: &mut Editor) -> Option<usize> {
        let register = editor.registers.last_search_register;
        let query = editor.registers.first(register, editor)?.into_owned();
        if query != self.search.query {
            match self.regex(&query, editor) {
                Ok(regex) => {
                    self.search.matches = self.matching(&regex, editor);
                    self.search.query = query;
                }
                Err(_) => {
                    editor.set_error(format!("Invalid regex: {query}"));
                    return None;
                }
            }
        }
        match self.find(current, down, editor.config().search.wrap_around) {
            Some((revision, wrapped)) => {
                if wrapped {
                    editor.set_status("Wrapped around the undo tree");
                }
                Some(revision)
            }
            None => {
                editor.set_error("No more matches");
                None
            }
        }
    }

    /// The regex of a search for `query` in the history shown.
    fn regex(&self, query: &str, editor: &Editor) -> Result<rope::Regex, search::RegexError> {
        let crlf = self
            .rows
            .as_ref()
            .and_then(|rows| editor.document(rows.doc))
            .is_some_and(|doc| doc.line_ending == LineEnding::Crlf);
        search::regex(query, &editor.config().search, crlf)
    }

    /// The revisions whose changes match `regex`: the text they inserted or deleted.
    fn matching(&self, regex: &rope::Regex, editor: &mut Editor) -> Vec<usize> {
        let Some(rows) = &self.rows else {
            return Vec::new();
        };
        let history = doc_mut(editor, rows.doc).history.get_mut();
        (1..history.len())
            .filter(|&revision| {
                let text = rows::changed_text(history, revision);
                regex.is_match(RegexInput::new(text.as_str()))
            })
            .collect()
    }

    /// Hands `event` to the line of the search, then follows what it told: like the editor's
    /// search, the buffer goes to the first match below where the search started as the query is
    /// typed, and back there while it matches nothing; `Enter` keeps the search for `n` and `N`,
    /// `Esc` goes back.
    fn handle_search(&mut self, event: &Event, cx: &mut Context) {
        let Some(prompt) = &mut self.search.prompt else {
            return;
        };
        // The prompt asks to be closed as a layer would be.
        if let EventResult::Consumed(Some(_)) = prompt.handle_event(event, cx) {
            self.search.prompt = None;
        }
        let Some(origin) = self.search.origin else {
            return;
        };
        let reported = self.search.reported.borrow_mut().take();
        match reported {
            Some((line, PromptEvent::Update)) => self.search_from(origin, &line, false, cx),
            Some((line, PromptEvent::Validate)) => {
                cx.editor.registers.last_search_register = search::REGISTER;
                self.search_from(origin, &line, true, cx);
            }
            Some((_, PromptEvent::Abort)) if self.search.prompt.is_none() => {
                self.browse_to(origin, cx.editor);
            }
            _ => {}
        }
    }

    /// Takes the buffer to the first revision below `origin` whose changes match `query`, or
    /// back to `origin` while none does or `query` is empty or no regex. A query that is no
    /// regex when it is `entered` says why.
    fn search_from(&mut self, origin: usize, query: &str, entered: bool, cx: &mut Context) {
        let regex = self.regex(query, cx.editor);
        let found = match regex {
            Ok(regex) if !query.is_empty() => {
                self.search.matches = self.matching(&regex, cx.editor);
                let wrap = cx.editor.config().search.wrap_around;
                self.find(origin, true, wrap)
            }
            Ok(_) => {
                self.search.matches.clear();
                None
            }
            Err(error) => {
                self.search.matches.clear();
                if entered {
                    search::show_invalid(cx, error);
                }
                None
            }
        };
        self.search.query = query.to_owned();
        let target = found.map_or(origin, |(revision, _)| revision);
        self.browse_to(target, cx.editor);
    }

    /// Handles a mouse event over the panel: a click browses to the revision of its row, the
    /// wheel scrolls. Returns `None` for events elsewhere.
    pub fn handle_mouse(&mut self, event: &MouseEvent, editor: &mut Editor) -> Option<EventResult> {
        let area = self.area?;
        let inside = (area.left()..area.right()).contains(&event.column)
            && (area.top()..area.bottom()).contains(&event.row);
        if !inside {
            return None;
        }
        match event.kind {
            MouseEventKind::ScrollDown => self.start = self.start.saturating_add(3),
            MouseEventKind::ScrollUp => self.start = self.start.saturating_sub(3),
            MouseEventKind::Down(MouseButton::Left) => {
                let row = self.drawn_start + (event.row - area.top()) as usize;
                let revision = self.rows.as_ref().and_then(|rows| rows.revision(row));
                if let Some(revision) = revision {
                    if !self.is_focused() {
                        self.focus(editor);
                    }
                    self.browse_to(revision, editor);
                }
            }
            _ => {}
        }
        Some(EventResult::Consumed(None))
    }
}

fn doc_mut(editor: &mut Editor, doc: DocumentId) -> &mut helix_view::Document {
    editor
        .documents
        .get_mut(&doc)
        .expect("the history shown is of an open buffer")
}

/// The first row to show so that row `cursor` is in view with `scrolloff` rows around it,
/// moving as little as possible from `start`.
fn reveal(start: usize, height: usize, cursor: usize, scrolloff: usize) -> usize {
    let margin = scrolloff.min(height.saturating_sub(1) / 2);
    if cursor < start + margin {
        cursor.saturating_sub(margin)
    } else if cursor + margin >= start + height {
        (cursor + margin + 1).saturating_sub(height)
    } else {
        start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revealing_moves_as_little_as_possible() {
        assert_eq!(reveal(0, 10, 3, 2), 0);
        assert_eq!(reveal(0, 10, 9, 2), 2);
        assert_eq!(reveal(5, 10, 6, 2), 4);
        assert_eq!(reveal(5, 10, 0, 2), 0);
    }
}
