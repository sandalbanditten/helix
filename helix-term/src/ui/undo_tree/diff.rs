//! The diff below the undo tree: what the revision under the cursor changed, as `difft` shows it
//! or as a line diff of Helix's own. It is worked out in the background once the cursor rests.

use std::{ops::Range, process::Stdio, time::Duration};

use helix_core::{unicode::width::UnicodeWidthChar, Rope};
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

#[derive(Default)]
pub struct DiffPane {
    pub compare: Compare,
    asked: Option<Request>,
    /// The request the lines are the diff of.
    shown: Option<Request>,
    lines: Vec<Line>,
    task: Option<JoinHandle<()>>,
    generation: u64,
    /// The first line shown.
    pub scroll: usize,
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
            self.shown = Some(request);
            self.lines.clear();
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
        self.lines = match output {
            Output::Ansi(ansi) => {
                // Lines too long are wrapped when drawn.
                let mut screen = Screen::new(u16::MAX, 1, true);
                screen.push(&ansi);
                let mut shown = screen.take_scrolled();
                shown.append(&screen.finish());
                lines_of(&shown)
            }
            Output::Unified(diff) => styled(&diff, theme),
        };
        if self.shown.as_ref() != Some(&request) {
            self.scroll = 0;
        }
        self.shown = Some(request);
        self.task = None;
    }

    /// Scrolls the diff `rows` down, or up when negative, its lines wrapped at `width`.
    pub fn scroll_by(&mut self, rows: isize, width: usize) {
        let max = self.wrapped(width).count().saturating_sub(1);
        self.scroll = self.scroll.saturating_add_signed(rows).min(max);
    }

    /// Draws the diff in `area`, under a row naming the revisions it compares.
    pub fn render(&self, area: Rect, surface: &mut Surface, base: Style, guide: Style) {
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
        let header = format!("{title:─<width$}", width = area.width as usize);
        surface.set_stringn(
            area.x,
            area.y,
            &header,
            area.width as usize,
            base.patch(guide),
        );
        let rows = self.wrapped(area.width as usize).skip(self.scroll);
        for (y, (line, chars)) in (area.y + 1..area.bottom()).zip(rows) {
            let mut x = area.x;
            for (index, c) in line
                .text
                .chars()
                .enumerate()
                .skip(chars.start)
                .take(chars.len())
            {
                let style = line
                    .styles
                    .iter()
                    .find(|(range, _)| range.contains(&index))
                    .map_or(base, |(_, style)| base.patch(*style));
                let mut bytes = [0; 4];
                let c = c.encode_utf8(&mut bytes);
                x = surface
                    .set_stringn(x, y, c, area.right().saturating_sub(x) as usize, style)
                    .0;
            }
        }
    }

    /// The rows of the lines wrapped at `width` columns: each a line and its chars on the row.
    fn wrapped(&self, width: usize) -> impl Iterator<Item = (&Line, Range<usize>)> {
        self.lines.iter().flat_map(move |line| {
            let mut rows = Vec::new();
            let (mut start, mut columns) = (0, 0);
            for (index, c) in line.text.chars().enumerate() {
                let c_width = c.width().unwrap_or(0);
                if columns + c_width > width && index > start {
                    rows.push((line, start..index));
                    (start, columns) = (index, 0);
                }
                columns += c_width;
            }
            rows.push((line, start..line.text.chars().count()));
            rows
        })
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
    fn long_lines_wrap() {
        let pane = DiffPane {
            lines: vec![
                Line {
                    text: "abcdefgh".into(),
                    styles: Vec::new(),
                },
                Line::default(),
            ],
            ..DiffPane::default()
        };
        let rows: Vec<_> = pane.wrapped(3).map(|(_, chars)| chars).collect();
        assert_eq!(rows, [0..3, 3..6, 6..8, 0..0]);
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
