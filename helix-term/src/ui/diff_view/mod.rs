//! The diff view: two read-only panes side by side, zoomed over the editor, one per text of a
//! diff. Their rows line up, and they scroll together.
//!
//! [`DiffView`] is owned by the [`EditorView`]. It works out how the texts line up in the
//! background, opens the panes once that is known, and keeps them in step while they are shown.

mod run;
pub(crate) mod styles;

use std::{ops::Range, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{anyhow, bail};
use helix_core::{Position, Rope, Selection};
use helix_loader::workspace_trust::TrustQuery;
use helix_view::{
    align_view, current_ref,
    diff_view::{Alignment, LineChange, Pane, Side},
    doc, doc_mut,
    document::from_reader,
    editor::Action,
    graphics::{Rect, Style},
    view::ViewPosition,
    Align, Document, DocumentId, Editor, Theme, ViewId,
};
use tokio::task::JoinHandle;

use self::{run::Outcome, styles::Styles};
use crate::{
    job,
    ui::{
        document::{LinePos, TextRenderer},
        text_decorations::Decoration,
        EditorView,
    },
};

/// How long a buffer rests after a change before its diff is worked out again.
const REDIFF_DELAY: Duration = Duration::from_millis(250);

/// Two texts to compare, and what they are.
#[derive(Debug, Clone)]
pub struct Request {
    /// The path of the file the texts are of, relative where it can be, which picks the
    /// language.
    path: PathBuf,
    old: Rope,
    new: Rope,
    /// The names of the panes, old and new.
    names: [String; 2],
    /// The buffer whose text the new one is, and its version then.
    buffer: Option<(DocumentId, i32)>,
    /// The file `gf` opens.
    file: Option<PathBuf>,
    /// The view focused when the diff was asked for.
    origin: Option<ViewId>,
}

impl Request {
    /// The diff of the focused buffer against the version committed at HEAD.
    pub fn buffer_against_head(editor: &Editor) -> anyhow::Result<Self> {
        let (view, doc) = current_ref!(editor);
        if doc.diff_view.is_some() {
            bail!("This is a diff already");
        }
        let path = doc
            .path()
            .ok_or_else(|| anyhow!("{} has no file", doc.display_name()))?;
        let name = doc.display_name().into_owned();
        let trust = editor
            .workspace_trust
            .query(doc.workspace_root(), TrustQuery::Git)
            .is_trusted();
        let base = editor
            .diff_providers
            .get_diff_base(path, trust)
            .ok_or_else(|| anyhow!("{name} has no committed version"))?;
        let (base, ..) = from_reader(&mut base.as_slice(), Some(doc.encoding()))?;
        Ok(Self {
            path: doc.relative_path().unwrap_or(path).to_path_buf(),
            old: base,
            new: doc.text().clone(),
            names: [format!("{name} (HEAD)"), name],
            buffer: Some((doc.id(), doc.version())),
            file: Some(path.to_path_buf()),
            origin: Some(view.id),
        })
    }

    /// The same diff of the texts as they are now, if its buffer changed since.
    fn refreshed(&self, editor: &Editor) -> Option<Self> {
        let (doc_id, version) = self.buffer?;
        let doc = editor.document(doc_id)?;
        if doc.version() == version {
            return None;
        }
        let trust = editor
            .workspace_trust
            .query(doc.workspace_root(), TrustQuery::Git)
            .is_trusted();
        let base = editor.diff_providers.get_diff_base(doc.path()?, trust)?;
        let (base, ..) = from_reader(&mut base.as_slice(), Some(doc.encoding())).ok()?;
        Some(Self {
            old: base,
            new: doc.text().clone(),
            buffer: Some((doc_id, doc.version())),
            ..self.clone()
        })
    }
}

/// The two panes shown.
struct Pair {
    /// The documents of the old and the new side.
    panes: [DocumentId; 2],
    views: [ViewId; 2],
    request: Request,
}

#[derive(Default)]
pub struct DiffView {
    /// Counts the diffs asked for, so that the late result of an earlier one is dropped.
    generation: u64,
    task: Option<JoinHandle<()>>,
    /// Whether the task works out the diff of the pair shown anew.
    refreshing: bool,
    pair: Option<Pair>,
}

impl DiffView {
    /// Works out the diff of `request` in the background and shows it once known.
    pub fn open(&mut self, request: Request, editor: &mut Editor) {
        let tool = editor.config().diff.tool;
        if tool == helix_view::editor::DiffTool::Difftastic && run::has_difft() {
            editor.set_status(format!(
                "Diffing {} with difftastic…",
                request.path.display()
            ));
        }
        self.start(request, false, Duration::ZERO, editor);
    }

    fn start(&mut self, request: Request, refresh: bool, delay: Duration, editor: &Editor) {
        if let Some(task) = self.task.take() {
            // Dropping its `difft` stops it.
            task.abort();
        }
        self.generation += 1;
        self.refreshing = refresh;
        let generation = self.generation;
        let tool = editor.config().diff.tool;
        self.task = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let path = request.path.to_string_lossy().into_owned();
            let outcome = run::align(tool, path, request.old.clone(), request.new.clone()).await;
            job::dispatch(move |editor, compositor| {
                if let Some(view) = compositor.find::<EditorView>() {
                    view.diff_view.ready(generation, request, outcome, editor);
                }
            })
            .await;
        }));
    }

    /// Shows `outcome`, the diff of `request` asked for as the `generation`th, unless another
    /// one was asked for since.
    fn ready(&mut self, generation: u64, request: Request, outcome: Outcome, editor: &mut Editor) {
        if generation != self.generation {
            return;
        }
        self.task = None;
        match &outcome.fallback {
            Some(reason) => editor.set_status(format!("{reason}: showing Helix's own diff")),
            None if !self.refreshing => editor.clear_status(),
            None => {}
        }
        if self.refreshing {
            if let Some(pair) = &mut self.pair {
                refresh(pair, request, outcome.alignment, editor);
                return;
            }
        }
        if let Some(pair) = self.pair.take() {
            editor.close_diff_panes(pair.panes, None);
        }
        self.pair = Some(show(request, outcome.alignment, editor));
    }

    /// Keeps the panes in step: the one without the focus scrolls along with the focused one,
    /// and its cursor goes to the same row. Also notices the panes closing, and works the diff
    /// out anew once its buffer changed.
    pub fn follow(&mut self, editor: &mut Editor) {
        let Some(pair) = &self.pair else {
            return;
        };
        let intact = pair.views.iter().zip(&pair.panes).all(|(&view, &doc)| {
            editor
                .tree
                .try_get(view)
                .is_some_and(|view| view.doc == doc)
        });
        if !intact {
            let pair = self.pair.take().expect("checked above");
            editor.close_diff_panes(pair.panes, None);
            if let Some(task) = self.task.take() {
                task.abort();
            }
            return;
        }
        if let Some(index) = pair
            .views
            .iter()
            .position(|&view| view == editor.tree.focus)
        {
            sync(
                editor,
                [pair.views[index], pair.views[1 - index]],
                [pair.panes[index], pair.panes[1 - index]],
            );
        }
        if self.task.is_none() {
            if let Some(request) = pair.request.refreshed(editor) {
                self.start(request, true, REDIFF_DELAY, editor);
            }
        }
    }
}

/// Opens the panes of the diff of `request`, zoomed, with the cursor on the first hunk.
fn show(request: Request, alignment: Arc<Alignment>, editor: &mut Editor) -> Pair {
    let loader = editor.syn_loader.load();
    let language = loader
        .language_for_filename(&request.path)
        .map(|language| loader.language(language).config().clone());
    let mut panes = [Side::Old, Side::New].map(|side| {
        let text = match side {
            Side::Old => request.old.clone(),
            Side::New => request.new.clone(),
        };
        let mut doc = Document::from(text, None, editor.config.clone(), editor.syn_loader.clone());
        doc.set_language(language.clone(), &loader);
        doc.detect_indent_and_line_ending();
        doc.set_spelling_language_override(Some(Vec::new()));
        doc.detect_spelling();
        Some(doc)
    });
    drop(loader);

    let mut ids = [DocumentId::default(); 2];
    let mut views = [ViewId::default(); 2];
    for index in 0..2 {
        let doc = panes[index].take().expect("each pane once");
        ids[index] = editor.new_file_from_document(Action::VerticalSplit, doc);
        views[index] = editor.tree.focus;
    }
    for (index, side) in [Side::Old, Side::New].into_iter().enumerate() {
        doc_mut!(editor, &ids[index]).diff_view = Some(Box::new(pane(
            &request,
            side,
            alignment.clone(),
            ids[1 - index],
        )));
    }
    editor.tree.set_zoom(&views);

    // The cursors go to the first hunk, the focus to the new side.
    let first = alignment.hunks().first().map_or(0, |hunk| hunk.start);
    for index in 0..2 {
        let doc = doc_mut!(editor, &ids[index]);
        let pane = doc.diff_view.as_ref().expect("set above");
        let line = line_at_or_after(&alignment, pane.side, first);
        let pos = doc.text().line_to_char(line as usize);
        doc.set_selection(views[index], Selection::point(pos));
        let view = editor.tree.get(views[index]);
        align_view(doc, view, Align::Center);
    }
    Pair {
        panes: ids,
        views,
        request,
    }
}

/// Shows the panes of `pair` with the texts of `request`, lined up by `alignment`.
fn refresh(pair: &mut Pair, request: Request, alignment: Arc<Alignment>, editor: &mut Editor) {
    for (index, side) in [Side::Old, Side::New].into_iter().enumerate() {
        let text = match side {
            Side::Old => &request.old,
            Side::New => &request.new,
        };
        let pane = pane(&request, side, alignment.clone(), pair.panes[1 - index]);
        doc_mut!(editor, &pair.panes[index]).replace_diff_text(text, pane, pair.views[index]);
    }
    pair.request = request;
}

fn pane(request: &Request, side: Side, alignment: Arc<Alignment>, partner: DocumentId) -> Pane {
    let index = match side {
        Side::Old => 0,
        Side::New => 1,
    };
    Pane {
        side,
        alignment,
        name: request.names[index].clone(),
        partner,
        file: request.file.clone(),
        origin: request.origin,
    }
}

/// The first line of `side` on `row` or after it, or the last line.
fn line_at_or_after(alignment: &Alignment, side: Side, row: u32) -> u32 {
    let rows = alignment.rows();
    rows[(row as usize).min(rows.len())..]
        .iter()
        .find_map(|row| row.line(side))
        .unwrap_or_else(|| alignment.lines(side).saturating_sub(1))
}

/// Scrolls the pane `docs[1]` in `views[1]` to the row the focused pane `docs[0]` in `views[0]`
/// shows at its top, and puts its cursor on the row of the focused pane's cursor.
fn sync(editor: &mut Editor, views: [ViewId; 2], docs: [DocumentId; 2]) {
    let doc = doc!(editor, &docs[0]);
    let Some(pane) = doc.diff_view.as_ref() else {
        return;
    };
    let text = doc.text().slice(..);
    let offset = doc.view_offset(views[0]);
    let top = top_row(pane, text.char_to_line(offset.anchor), offset);
    let cursor = text.char_to_line(doc.selection(views[0]).primary().cursor(text));
    let cursor_row = pane.row_of_line(cursor);

    let partner = doc_mut!(editor, &docs[1]);
    let Some(pane) = partner.diff_view.as_ref() else {
        return;
    };
    let text = partner.text().slice(..);
    let current = partner.view_offset(views[1]);
    let offset = offset_at_row(pane, text, top, current.horizontal_offset);
    let cursor_line = text.char_to_line(partner.selection(views[1]).primary().cursor(text));
    let cursor = cursor_row
        .map(|row| pane.line_at_or_before(row).unwrap_or(0) as usize)
        .filter(|&line| line != cursor_line)
        .map(|line| text.line_to_char(line));
    if offset != current {
        partner.set_view_offset(views[1], offset);
    }
    if let Some(pos) = cursor {
        partner.set_selection(views[1], Selection::point(pos));
    }
}

/// The row of the alignment at the top of a pane scrolled to `offset`, anchored on `line`.
fn top_row(pane: &Pane, line: usize, offset: ViewPosition) -> u32 {
    // A pane anchored at its start counts from the top, above its first line.
    let anchor_row = if offset.anchor == 0 {
        0
    } else {
        pane.row_of_line(line).unwrap_or(0)
    };
    anchor_row + offset.vertical_offset as u32
}

/// The offset showing `row` of the alignment at the top of `pane`.
fn offset_at_row(
    pane: &Pane,
    text: helix_core::RopeSlice,
    row: u32,
    horizontal_offset: usize,
) -> ViewPosition {
    let (anchor, vertical_offset) = match pane.line_at_or_before(row) {
        Some(line) if line > 0 => {
            let line_row = pane.row_of_line(line as usize).unwrap_or(row);
            (text.line_to_char(line as usize), row - line_row)
        }
        _ => (0, row),
    };
    ViewPosition {
        anchor,
        horizontal_offset,
        vertical_offset: vertical_offset as usize,
    }
}

/// The styles of the text that changed on the lines `lines` of the pane `doc`, by char ranges.
pub fn changed_text(
    doc: &Document,
    pane: &Pane,
    lines: Range<usize>,
    theme: &Theme,
) -> Vec<(Range<usize>, Style)> {
    let style = Styles::new(theme).text(pane.side);
    let text = doc.text().slice(..);
    let lines = lines.start as u32..lines.end.min(text.len_lines()) as u32;
    let mut spans = Vec::new();
    for (line, change) in pane.alignment.changes(pane.side, lines) {
        let LineChange::Parts(parts) = change else {
            continue;
        };
        let start = text.line_to_byte(*line as usize);
        for part in parts {
            let range = text.byte_to_char(start + part.start as usize)
                ..text.byte_to_char(start + part.end as usize);
            spans.push((range, style));
        }
    }
    spans
}

/// Paints the rows of a pane: the background of its changed lines and of its fillers.
pub struct Rows<'a> {
    pane: &'a Pane,
    styles: Styles,
    /// The text area of the pane.
    area: Rect,
    /// The first visual row not painted yet.
    next_row: u16,
}

impl<'a> Rows<'a> {
    pub fn new(pane: &'a Pane, theme: &Theme, area: Rect) -> Self {
        Self {
            pane,
            styles: Styles::new(theme),
            area,
            next_row: 0,
        }
    }

    fn paint(&mut self, renderer: &mut TextRenderer, rows: Range<u16>, style: Style) {
        if rows.is_empty() {
            return;
        }
        let area = Rect::new(
            self.area.x,
            rows.start,
            self.area.width,
            rows.end - rows.start,
        );
        renderer.set_style(area, style);
        self.next_row = self.next_row.max(rows.end);
    }
}

impl Decoration for Rows<'_> {
    fn decorate_line(&mut self, renderer: &mut TextRenderer, pos: LinePos) {
        // Rows skipped since the last line are fillers: those above the first line, or those
        // after a line above the view.
        self.paint(renderer, self.next_row..pos.visual_line, self.styles.filler);
        let changed = self
            .pane
            .alignment
            .change(self.pane.side, pos.doc_line as u32);
        let row = pos.visual_line..pos.visual_line + 1;
        if pos.first_visual_line && changed.is_some() {
            self.paint(renderer, row, self.styles.line(self.pane.side));
        } else {
            self.next_row = self.next_row.max(row.end);
        }
    }

    fn render_virt_lines(
        &mut self,
        renderer: &mut TextRenderer,
        pos: LinePos,
        virt_off: Position,
    ) -> Position {
        let fillers = &self.pane.alignment.fillers(self.pane.side).after;
        let count = fillers
            .binary_search_by_key(&(pos.doc_line as u32), |(line, _)| *line)
            .map_or(0, |index| fillers[index].1) as u16;
        let start = pos.visual_line + virt_off.row as u16;
        self.paint(renderer, start..start + count, self.styles.filler);
        Position::new(count as usize, 0)
    }
}

#[cfg(test)]
mod tests {
    use helix_view::diff_view::{builtin, Row};

    use super::*;

    fn pane(side: Side, alignment: &Arc<Alignment>) -> Pane {
        Pane {
            side,
            alignment: alignment.clone(),
            name: String::new(),
            partner: DocumentId::default(),
            file: None,
            origin: None,
        }
    }

    #[test]
    fn panes_scroll_to_the_same_row() {
        // old: a b c       new: x y a b z c
        let old = Rope::from("a\nb\nc\n");
        let new = Rope::from("x\ny\na\nb\nz\nc\n");
        let alignment = Arc::new(builtin::align(old.slice(..), new.slice(..)));
        assert_eq!(
            alignment.rows()[..3],
            [
                Row {
                    old: None,
                    new: Some(0)
                },
                Row {
                    old: None,
                    new: Some(1)
                },
                Row {
                    old: Some(0),
                    new: Some(2)
                },
            ]
        );
        let (old_pane, new_pane) = (pane(Side::Old, &alignment), pane(Side::New, &alignment));
        let at = |anchor, vertical_offset| ViewPosition {
            anchor,
            horizontal_offset: 0,
            vertical_offset,
        };
        // The new side at its top shows the fillers above the old side's first line.
        assert_eq!(top_row(&new_pane, 0, at(0, 0)), 0);
        assert_eq!(offset_at_row(&old_pane, old.slice(..), 0, 0), at(0, 0));
        assert_eq!(offset_at_row(&old_pane, old.slice(..), 1, 0), at(0, 1));
        assert_eq!(offset_at_row(&old_pane, old.slice(..), 2, 0), at(0, 2));
        // Line 1 of the old side, `b`, is on row 3.
        assert_eq!(offset_at_row(&old_pane, old.slice(..), 3, 0), at(2, 0));
        // Row 4 is a filler after `b` on the old side.
        assert_eq!(
            offset_at_row(&old_pane, old.slice(..), 4, 7),
            ViewPosition {
                anchor: 2,
                horizontal_offset: 7,
                vertical_offset: 1
            }
        );
        assert_eq!(top_row(&old_pane, 1, at(2, 1)), 4);
        assert_eq!(top_row(&new_pane, 4, at(8, 0)), 4);
        assert_eq!(line_at_or_after(&alignment, Side::Old, 0), 0);
        assert_eq!(line_at_or_after(&alignment, Side::Old, 4), 2);
    }
}
