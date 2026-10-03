//! The diff below the undo tree: what the revision under the cursor changed, inline and colored
//! like the diff view, lined up by `difft` or by Helix's own line diff. It is worked out in the
//! background once the cursor rests.

use std::time::Duration;

use helix_core::{unicode::width::UnicodeWidthChar, Rope, Syntax};
use helix_view::smooth_scroll::SmoothOffset;
use helix_view::{
    editor::{DiffTool, UndoDiff},
    graphics::{Rect, Style},
    DocumentId, Editor,
};
use tokio::task::JoinHandle;
use tui::buffer::Buffer as Surface;

use crate::{
    job,
    ui::{
        diff_view::{
            inline::{self, Line, Text},
            run,
        },
        EditorView,
    },
};

/// How long the cursor rests before the diff is worked out, like vim-mundo's preview.
const DELAY: Duration = Duration::from_millis(250);
/// The unchanged lines shown around each hunk.
const CONTEXT: u32 = 3;

/// What the revision under the cursor is compared with.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    /// Its parent: what the revision changed.
    #[default]
    Parent,
    /// The revision browsing started from.
    Start,
}

/// A diff asked for: of `revision` against the revision `against`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub doc: DocumentId,
    pub revision: usize,
    /// None for the root, which changed nothing.
    pub against: Option<usize>,
    pub tool: UndoDiff,
}

/// The texts a diff is of, and how to show them.
pub struct Texts {
    pub old: Rope,
    pub new: Rope,
    /// The path the file is shown as, which picks difftastic's language.
    pub path: String,
    /// The name of the buffer's language, whose syntax colors the texts.
    pub language: Option<String>,
    pub tab_width: usize,
}

/// A move through the diff, made by one of the editor's motions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Down,
    Up,
    Left,
    Right,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
    Leftmost,
    Rightmost,
}

impl Motion {
    /// The move the editor's command `name` makes through the diff, if it is a motion.
    pub fn of_command(name: &str) -> Option<Self> {
        Some(match name {
            "move_visual_line_down" | "move_line_down" | "scroll_down" => Self::Down,
            "move_visual_line_up" | "move_line_up" | "scroll_up" => Self::Up,
            "move_char_left" => Self::Left,
            "move_char_right" => Self::Right,
            "page_cursor_half_down" | "half_page_down" => Self::HalfPageDown,
            "page_cursor_half_up" | "half_page_up" => Self::HalfPageUp,
            "page_cursor_down" | "page_down" => Self::PageDown,
            "page_cursor_up" | "page_up" => Self::PageUp,
            "goto_file_start" => Self::Top,
            "goto_file_end" | "goto_last_line" => Self::Bottom,
            "goto_line_start" | "goto_first_nonwhitespace" => Self::Leftmost,
            "goto_line_end" | "goto_line_end_newline" => Self::Rightmost,
            _ => return None,
        })
    }
}

#[derive(Default)]
pub struct DiffPane {
    pub compare: Compare,
    asked: Option<Request>,
    /// The request the lines are the diff of.
    shown: Option<Request>,
    lines: Vec<Line>,
    task: Option<JoinHandle<()>>,
    generation: u64,
    /// The first line shown, and the column each line is shown from.
    scroll: usize,
    column: usize,
    /// The lines and columns shown last.
    height: usize,
    width: usize,
    smooth_scroll: SmoothOffset,
    /// Whether the missing `difft` was told of.
    told: bool,
}

impl DiffPane {
    /// Whether `request` is the diff asked for last.
    pub fn is_asked(&self, request: &Request) -> bool {
        self.asked.as_ref() == Some(request)
    }

    /// Asks for the diff of `request`, of `texts`. The diff of the root shows nothing.
    pub fn ask(&mut self, request: Request, texts: Option<Texts>, editor: &mut Editor) {
        if let Some(task) = self.task.take() {
            // Dropping its `difft` stops it.
            task.abort();
        }
        self.generation += 1;
        self.asked = Some(request.clone());
        let Some(texts) = texts.filter(|_| request.against.is_some()) else {
            self.show(request, Vec::new());
            return;
        };
        let tool = match request.tool {
            UndoDiff::Difftastic => DiffTool::Difftastic,
            UndoDiff::Builtin | UndoDiff::None => DiffTool::Builtin,
        };
        if tool == DiffTool::Difftastic
            && !run::has_difft()
            && !std::mem::replace(&mut self.told, true)
        {
            editor.set_status("difft is not installed: the undo tree shows its own diff");
        }
        let Texts {
            old,
            new,
            path,
            language,
            tab_width,
        } = texts;
        let (loader, theme) = (editor.syn_loader.load_full(), editor.theme.clone());
        let generation = self.generation;
        self.task = Some(tokio::spawn(async move {
            tokio::time::sleep(DELAY).await;
            let outcome = run::align(tool, path, old.clone(), new.clone()).await;
            let lines = tokio::task::spawn_blocking(move || {
                let language = language.and_then(|name| loader.language_for_name(name));
                let parse = |text: &Rope| {
                    language
                        .and_then(|language| Syntax::new(text.slice(..), language, &loader).ok())
                };
                let syntaxes = [parse(&old), parse(&new)];
                let sides =
                    [(&old, &syntaxes[0]), (&new, &syntaxes[1])].map(|(text, syntax)| Text {
                        text,
                        syntax: syntax.as_ref(),
                    });
                inline::lines(
                    &outcome.alignment,
                    sides,
                    &loader,
                    &theme,
                    CONTEXT,
                    tab_width,
                )
            })
            .await
            .unwrap_or_default();
            job::dispatch(move |_, compositor| {
                if let Some(view) = compositor.find::<EditorView>() {
                    view.undo_tree.diff_ready(generation, lines);
                }
            })
            .await;
        }));
    }

    /// Takes `lines`, the diff of the request of `generation` if it is still the one asked for.
    pub fn ready(&mut self, generation: u64, lines: Vec<Line>) {
        if generation != self.generation {
            return;
        }
        let Some(request) = self.asked.clone() else {
            return;
        };
        self.show(request, lines);
        self.task = None;
    }

    /// Shows `lines`, the diff of `request`: from the start if it is the diff of another one.
    fn show(&mut self, request: Request, lines: Vec<Line>) {
        if self.shown.as_ref() != Some(&request) {
            (self.scroll, self.column) = (0, 0);
            self.smooth_scroll.reset();
        }
        self.lines = lines;
        self.shown = Some(request);
    }

    /// Moves through the diff by `motion`, `count` times.
    pub fn go(&mut self, motion: Motion, count: usize) {
        let page = self.height.max(1);
        let last = self.lines.len().saturating_sub(page);
        let widest = self.lines.iter().map(Line::width).max().unwrap_or(0);
        let rightmost = widest.saturating_sub(self.width);
        // Columns go by half the width: the pane is narrow.
        let columns = (self.width / 2).max(1) * count;
        let down = |rows: usize| self.scroll.saturating_add(rows * count).min(last);
        let up = |rows: usize| self.scroll.saturating_sub(rows * count);
        match motion {
            Motion::Down => self.scroll = down(1),
            Motion::Up => self.scroll = up(1),
            Motion::HalfPageDown => self.scroll = down(page.div_ceil(2)),
            Motion::HalfPageUp => self.scroll = up(page.div_ceil(2)),
            Motion::PageDown => self.scroll = down(page),
            Motion::PageUp => self.scroll = up(page),
            Motion::Top => self.scroll = 0,
            Motion::Bottom => self.scroll = last,
            Motion::Left => self.column = self.column.saturating_sub(columns),
            Motion::Right => self.column = self.column.saturating_add(columns).min(rightmost),
            Motion::Leftmost => self.column = 0,
            Motion::Rightmost => self.column = rightmost,
        }
    }

    /// Draws the diff in `area`, under a row naming the revisions it compares in `header`'s
    /// style, its lines cut at the edge. Scrolling glides.
    pub fn render(
        &mut self,
        area: Rect,
        surface: &mut Surface,
        base: Style,
        header: Style,
        editor: &mut Editor,
    ) {
        surface.clear_with(area, base);
        if area.height == 0 {
            return;
        }
        let title = match &self.shown {
            Some(Request {
                revision,
                against: Some(against),
                ..
            }) => format!("─ {against} → {revision} "),
            Some(Request { revision, .. }) => format!("─ {revision} "),
            None => "─ ".to_owned(),
        };
        let title = format!("{title:─<width$}", width = area.width as usize);
        surface.set_stringn(area.x, area.y, &title, area.width as usize, header);

        let lines = area.clip_top(1);
        (self.height, self.width) = (lines.height as usize, lines.width as usize);
        self.scroll = self
            .scroll
            .min(self.lines.len().saturating_sub(self.height));
        let scroll = self.smooth_scroll.frame(self.scroll, lines.height, editor);
        for (y, line) in (lines.top()..lines.bottom()).zip(self.lines.iter().skip(scroll)) {
            let base = line
                .background
                .map_or(base, |background| base.patch(background));
            surface.set_style(Rect::new(lines.x, y, lines.width, 1), base);
            let (mut x, mut column) = (lines.x, 0);
            for (index, c) in line.text.chars().enumerate() {
                let width = c.width().unwrap_or(0);
                // Part of a wide char cut by the left edge is left out.
                let shown = column >= self.column;
                column += width;
                if !shown {
                    continue;
                }
                if x >= lines.right() {
                    break;
                }
                // Later styles, like those of the text that changed, go over earlier ones.
                let style = line
                    .styles
                    .iter()
                    .filter(|(range, _)| range.contains(&index))
                    .fold(base, |style, (_, patch)| style.patch(*patch));
                let mut bytes = [0; 4];
                let c = c.encode_utf8(&mut bytes);
                x = surface
                    .set_stringn(x, y, c, lines.right().saturating_sub(x) as usize, style)
                    .0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motions_scroll_within_the_diff() {
        let line = |text: &str| Line {
            text: text.into(),
            ..Line::default()
        };
        let mut pane = DiffPane {
            lines: (0..10)
                .map(|i| line(&format!("{i}{}", "-".repeat(i * 4))))
                .collect(),
            height: 4,
            width: 10,
            ..DiffPane::default()
        };
        pane.go(Motion::Down, 3);
        assert_eq!(pane.scroll, 3);
        pane.go(Motion::PageDown, 1);
        assert_eq!(pane.scroll, 6, "the last line ends the last page");
        pane.go(Motion::HalfPageUp, 1);
        assert_eq!(pane.scroll, 4);
        pane.go(Motion::Top, 1);
        assert_eq!(pane.scroll, 0);
        pane.go(Motion::Right, 1);
        assert_eq!(pane.column, 5);
        pane.go(Motion::Rightmost, 1);
        assert_eq!(
            pane.column, 27,
            "the widest line, 37 columns, ends at the edge"
        );
        pane.go(Motion::Left, 2);
        assert_eq!(pane.column, 17);
        assert_eq!(
            Motion::of_command("page_cursor_half_down"),
            Some(Motion::HalfPageDown)
        );
        assert_eq!(Motion::of_command("goto_line_end"), Some(Motion::Rightmost));
        assert_eq!(Motion::of_command("delete_selection"), None);
    }
}
