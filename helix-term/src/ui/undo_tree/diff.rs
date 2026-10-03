//! The diff below the undo tree: what the revision under the cursor changed, as `difft` shows it
//! or as a line diff of Helix's own. It is worked out in the background once the cursor rests.

use std::{ops::Range, process::Stdio, time::Duration};

use helix_core::{unicode::width::UnicodeWidthChar, Rope};
use helix_view::smooth_scroll::SmoothOffset;
use helix_view::{
    editor::UndoDiff,
    graphics::{Color, Rect, Style},
    DocumentId, Editor, Theme,
};
use imara_diff::{Algorithm, BasicLineDiffPrinter, Diff, InternedInput, UnifiedDiffConfig};
use tokio::task::JoinHandle;
use tui::buffer::Buffer as Surface;

use crate::{
    job,
    ui::{
        compilation::{output::Shown, screen::Screen},
        EditorView,
    },
};

/// How long the cursor rests before the diff is worked out, like vim-mundo's preview.
const DELAY: Duration = Duration::from_millis(250);

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
    pub width: u16,
    pub tool: UndoDiff,
}

/// What a diff came out as, before it gets the theme's styles.
pub enum Output {
    /// What `difft` printed, colored by escape sequences.
    Ansi(Vec<u8>),
    /// The lines of a unified diff.
    Unified(String),
}

/// A line of the diff, with the styles of its char ranges.
#[derive(Debug, Default, Clone, PartialEq)]
struct Line {
    text: String,
    styles: Vec<(Range<usize>, Style)>,
}

impl Line {
    /// The columns the line takes.
    fn width(&self) -> usize {
        self.text.chars().map(|c| c.width().unwrap_or(0)).sum()
    }
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
    /// Whether `difft` is installed, once looked for.
    difft: Option<bool>,
}

impl DiffPane {
    /// Whether `request` is the diff asked for last.
    pub fn is_asked(&self, request: &Request) -> bool {
        self.asked.as_ref() == Some(request)
    }

    /// Asks for the diff of `request`, from the text `old` to `new` of the file shown as `path`.
    /// The diff of the root shows nothing.
    pub fn ask(
        &mut self,
        request: Request,
        texts: Option<(Rope, Rope)>,
        path: String,
        editor: &mut Editor,
    ) {
        if let Some(task) = self.task.take() {
            // Dropping its `difft` stops it.
            task.abort();
        }
        self.generation += 1;
        self.asked = Some(request.clone());
        let Some((old, new)) = texts.filter(|_| request.against.is_some()) else {
            self.show(request, Vec::new());
            return;
        };
        let difft = request.tool == UndoDiff::Difftastic && self.find_difft(editor);
        let (generation, width, dark) = (self.generation, request.width, is_dark(&editor.theme));
        self.task = Some(tokio::spawn(async move {
            tokio::time::sleep(DELAY).await;
            let ansi = if difft {
                difftastic(&path, &old, &new, width, dark).await.ok()
            } else {
                None
            };
            let output = match ansi {
                Some(ansi) => Output::Ansi(ansi),
                None => {
                    let unified = tokio::task::spawn_blocking(move || unified(&old, &new));
                    Output::Unified(unified.await.unwrap_or_default())
                }
            };
            job::dispatch(move |editor, compositor| {
                if let Some(view) = compositor.find::<EditorView>() {
                    view.undo_tree.diff_ready(generation, output, &editor.theme);
                }
            })
            .await;
        }));
    }

    /// Whether `difft` is installed. Says so once when it isn't.
    fn find_difft(&mut self, editor: &mut Editor) -> bool {
        *self.difft.get_or_insert_with(|| {
            let found = helix_stdx::env::which("difft").is_ok();
            if !found {
                editor.set_status("difft is not installed: the undo tree shows its own diff");
            }
            found
        })
    }

    /// Takes `output`, the diff of the request of `generation` if it is still the one asked for.
    pub fn ready(&mut self, generation: u64, output: Output, theme: &Theme) {
        if generation != self.generation {
            return;
        }
        let Some(request) = self.asked.clone() else {
            return;
        };
        let lines = match output {
            Output::Ansi(ansi) => {
                // Wide enough for any line: lines are cut, not wrapped, where they are drawn.
                let mut screen = Screen::new(u16::MAX, 1, true);
                screen.push(&ansi);
                let mut shown = screen.take_scrolled();
                shown.append(&screen.finish());
                lines_of(&shown)
            }
            Output::Unified(diff) => styled(&diff, theme),
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
                let style = line
                    .styles
                    .iter()
                    .find(|(range, _)| range.contains(&index))
                    .map_or(base, |(_, style)| base.patch(*style));
                let mut bytes = [0; 4];
                let c = c.encode_utf8(&mut bytes);
                x = surface
                    .set_stringn(x, y, c, lines.right().saturating_sub(x) as usize, style)
                    .0;
            }
        }
    }
}

/// Whether the theme's background is dark, which `difft` picks its colors for.
fn is_dark(theme: &Theme) -> bool {
    match theme.get("ui.background").bg {
        Some(Color::Rgb(r, g, b)) => {
            (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 < 128
        }
        Some(Color::White | Color::LightGray | Color::LightYellow | Color::LightCyan) => false,
        _ => true,
    }
}

/// Runs `difft` on `old` and `new`, the texts of the file shown as `path`: the path picks the
/// language, as when git runs it.
async fn difftastic(
    path: &str,
    old: &Rope,
    new: &Rope,
    width: u16,
    dark: bool,
) -> std::io::Result<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let (old_file, new_file) = (dir.path().join("old"), dir.path().join("new"));
    tokio::fs::write(&old_file, old.to_string()).await?;
    tokio::fs::write(&new_file, new.to_string()).await?;
    let background = if dark { "dark" } else { "light" };
    let output = tokio::process::Command::new("difft")
        .arg("--color=always")
        .arg("--display=inline")
        .arg(format!("--width={width}"))
        .arg(format!("--background={background}"))
        .arg(path)
        .arg(&old_file)
        .args(["0", "100644"])
        .arg(&new_file)
        .args(["0", "100644"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    Ok(output.stdout)
}

/// A unified diff of the lines of `old` and `new`.
fn unified(old: &Rope, new: &Rope) -> String {
    let (old, new) = (old.to_string(), new.to_string());
    let input = InternedInput::new(old.as_str(), new.as_str());
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    diff.unified_diff(
        &BasicLineDiffPrinter(&input.interner),
        UnifiedDiffConfig::default(),
        &input,
    )
    .to_string()
}

/// The lines of a unified diff in the theme's diff colors.
fn styled(diff: &str, theme: &Theme) -> Vec<Line> {
    diff.lines()
        .map(|text| {
            let style = match text.chars().next() {
                Some('+') => theme.get("diff.plus"),
                Some('-') => theme.get("diff.minus"),
                Some('@') => theme.get("diff.delta"),
                _ => Style::default(),
            };
            Line {
                text: text.to_owned(),
                styles: vec![(0..text.chars().count(), style)],
            }
        })
        .collect()
}

/// The lines of `shown`, each with its styles.
fn lines_of(shown: &Shown) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    for text in shown.text.split('\n') {
        let end = start + text.chars().count();
        let styles = shown
            .styles
            .iter()
            .filter(|(range, _)| range.start < end && range.end > start)
            .map(|(range, style)| {
                (
                    range.start.max(start) - start..range.end.min(end) - start,
                    *style,
                )
            })
            .collect();
        lines.push(Line {
            text: text.to_owned(),
            styles,
        });
        start = end + 1;
    }
    // The text ends in a line break, after which there is no line.
    if lines.last().is_some_and(|line| line.text.is_empty()) {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_diffs_show_the_lines_changed() {
        let diff = unified(&Rope::from("a\nb\nc\n"), &Rope::from("a\nB\nc\n"));
        assert_eq!(diff, "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
        let lines = styled(&diff, &Theme::default());
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[2].text, "-b");
    }

    #[test]
    fn motions_scroll_within_the_diff() {
        let line = |text: &str| Line {
            text: text.into(),
            styles: Vec::new(),
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

    #[test]
    fn colored_output_keeps_its_colors() {
        let mut screen = Screen::new(20, 1, true);
        screen.push(b"\x1b[91mold\x1b[0m\nplain\n\x1b[92mnew\x1b[0m\n");
        let mut shown = screen.take_scrolled();
        shown.append(&screen.finish());
        let lines = lines_of(&shown);
        let texts: Vec<_> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["old", "plain", "new"]);
        assert_eq!(lines[0].styles.len(), 1);
        assert_eq!(lines[0].styles[0].0, 0..3);
        assert!(lines[1].styles.is_empty());
        assert_eq!(lines[2].styles[0].0, 0..3);
    }
}
