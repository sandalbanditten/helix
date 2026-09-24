use helix_core::indent::IndentStyle;
use helix_core::{coords_at_pos, encoding, unicode::width::UnicodeWidthStr, Position};
use helix_lsp::lsp::DiagnosticSeverity;
use helix_view::document::DEFAULT_LANGUAGE_NAME;
use helix_view::{
    document::{Mode, SCRATCH_BUFFER_NAME},
    graphics::Rect,
    theme::Style,
    Document, Editor, View,
};

use crate::ui::ProgressSpinners;

use helix_view::editor::{BreadcrumbsConfig, StatusLineElement as StatusLineElementID};
use tui::buffer::Buffer as Surface;
use tui::text::{Span, Spans};

pub struct RenderContext<'a> {
    pub editor: &'a Editor,
    pub doc: &'a Document,
    pub view: &'a View,
    pub focused: bool,
    pub spinners: &'a ProgressSpinners,
    pub parts: RenderBuffer<'a>,
    /// Set by elements that could be rendered narrower, like the breadcrumbs.
    pub shrinkable: bool,
    /// How many columns the left and right side of the status line overlap by. Shrinkable elements
    /// make up for it when the status line is rendered again.
    pub overflow: usize,
}

impl<'a> RenderContext<'a> {
    pub fn new(
        editor: &'a Editor,
        doc: &'a Document,
        view: &'a View,
        focused: bool,
        spinners: &'a ProgressSpinners,
    ) -> Self {
        RenderContext {
            editor,
            doc,
            view,
            focused,
            spinners,
            parts: RenderBuffer::default(),
            shrinkable: false,
            overflow: 0,
        }
    }
}

#[derive(Default)]
pub struct RenderBuffer<'a> {
    pub left: Spans<'a>,
    pub center: Spans<'a>,
    pub right: Spans<'a>,
}

pub fn render(context: &mut RenderContext, viewport: Rect, surface: &mut Surface) {
    let base_style = if context.focused {
        context.editor.theme.get("ui.statusline")
    } else {
        context.editor.theme.get("ui.statusline.inactive")
    };

    surface.set_style(viewport.with_height(1), base_style);

    render_elements(context, base_style);

    // If the sides overlap, render again so shrinkable elements make room.
    let overflow = (context.parts.left.width() + context.parts.right.width())
        .saturating_sub(viewport.width as usize);
    if overflow > 0 && context.shrinkable {
        context.overflow = overflow;
        context.parts = RenderBuffer::default();
        render_elements(context, base_style);
    }

    // Left side of the status line.

    surface.set_spans(
        viewport.x,
        viewport.y,
        &context.parts.left,
        context.parts.left.width() as u16,
    );

    // Right side of the status line.

    surface.set_spans(
        viewport.x
            + viewport
                .width
                .saturating_sub(context.parts.right.width() as u16),
        viewport.y,
        &context.parts.right,
        context.parts.right.width() as u16,
    );

    // Center of the status line.

    // Width of the empty space between the left and center area and between the center and right area.
    let spacing = 1u16;

    let edge_width = context.parts.left.width().max(context.parts.right.width()) as u16;
    let center_max_width = viewport.width.saturating_sub(2 * edge_width + 2 * spacing);
    let center_width = center_max_width.min(context.parts.center.width() as u16);

    surface.set_spans(
        viewport.x + viewport.width / 2 - center_width / 2,
        viewport.y,
        &context.parts.center,
        center_width,
    );
}

fn render_elements(context: &mut RenderContext, base_style: Style) {
    let config = context.editor.config();

    for element_id in &config.statusline.left {
        let render = get_render_function(*element_id);
        (render)(context, |context, span| {
            append(&mut context.parts.left, span, base_style)
        });
    }

    for element_id in &config.statusline.right {
        let render = get_render_function(*element_id);
        (render)(context, |context, span| {
            append(&mut context.parts.right, span, base_style)
        })
    }

    for element_id in &config.statusline.center {
        let render = get_render_function(*element_id);
        (render)(context, |context, span| {
            append(&mut context.parts.center, span, base_style)
        })
    }
}

fn append<'a>(buffer: &mut Spans<'a>, mut span: Span<'a>, base_style: Style) {
    span.style = base_style.patch(span.style);
    buffer.0.push(span);
}

fn get_render_function<'a, F>(element_id: StatusLineElementID) -> impl Fn(&mut RenderContext<'a>, F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    match element_id {
        helix_view::editor::StatusLineElement::Mode => render_mode,
        helix_view::editor::StatusLineElement::Spinner => render_lsp_spinner,
        helix_view::editor::StatusLineElement::FileBaseName => render_file_base_name,
        helix_view::editor::StatusLineElement::FileName => render_file_name,
        helix_view::editor::StatusLineElement::Breadcrumbs => render_breadcrumbs,
        helix_view::editor::StatusLineElement::FileAbsolutePath => render_file_absolute_path,
        helix_view::editor::StatusLineElement::FileModificationIndicator => {
            render_file_modification_indicator
        }
        helix_view::editor::StatusLineElement::ReadOnlyIndicator => render_read_only_indicator,
        helix_view::editor::StatusLineElement::FileEncoding => render_file_encoding,
        helix_view::editor::StatusLineElement::FileLineEnding => render_file_line_ending,
        helix_view::editor::StatusLineElement::FileIndentStyle => render_file_indent_style,
        helix_view::editor::StatusLineElement::FileType => render_file_type,
        helix_view::editor::StatusLineElement::Diagnostics => render_diagnostics,
        helix_view::editor::StatusLineElement::WorkspaceDiagnostics => render_workspace_diagnostics,
        helix_view::editor::StatusLineElement::Selections => render_selections,
        helix_view::editor::StatusLineElement::PrimarySelectionLength => {
            render_primary_selection_length
        }
        helix_view::editor::StatusLineElement::Position => render_position,
        helix_view::editor::StatusLineElement::PositionPercentage => render_position_percentage,
        helix_view::editor::StatusLineElement::TotalLineNumbers => render_total_line_numbers,
        helix_view::editor::StatusLineElement::Separator => render_separator,
        helix_view::editor::StatusLineElement::Spacer => render_spacer,
        helix_view::editor::StatusLineElement::VersionControl => render_version_control,
        helix_view::editor::StatusLineElement::Register => render_register,
        helix_view::editor::StatusLineElement::CurrentWorkingDirectory => render_cwd,
        helix_view::editor::StatusLineElement::CodeActionHint => render_code_action_hint,
    }
}

fn render_mode<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let visible = context.focused;
    let config = context.editor.config();
    let modenames = &config.statusline.mode;
    let mode_str = match context.editor.mode() {
        Mode::Insert => &modenames.insert,
        Mode::Select => &modenames.select,
        Mode::Normal => &modenames.normal,
    };
    let content = if visible {
        format!(" {mode_str} ")
    } else {
        // If not focused, explicitly leave an empty space instead of returning None.
        " ".repeat(mode_str.width() + 2)
    };
    let style = if visible && config.color_modes {
        match context.editor.mode() {
            Mode::Insert => context.editor.theme.get("ui.statusline.insert"),
            Mode::Select => context.editor.theme.get("ui.statusline.select"),
            Mode::Normal => context.editor.theme.get("ui.statusline.normal"),
        }
    } else {
        Style::default()
    };
    write(context, Span::styled(content, style));
}

fn render_lsp_spinner<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    write(
        context,
        context
            .doc
            .language_servers()
            .find_map(|srv| {
                context
                    .spinners
                    .get(srv.id())
                    .and_then(|spinner| spinner.frame())
            })
            // Even if there's no spinner; reserve its space to avoid elements frequently shifting.
            .unwrap_or(" ")
            .into(),
    );
}

fn render_diagnostics<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    use helix_core::diagnostic::Severity;
    let (hints, info, warnings, errors) =
        context
            .doc
            .diagnostics()
            .iter()
            .fold((0, 0, 0, 0), |mut counts, diag| {
                match diag.severity {
                    Some(Severity::Hint) | None => counts.0 += 1,
                    Some(Severity::Info) => counts.1 += 1,
                    Some(Severity::Warning) => counts.2 += 1,
                    Some(Severity::Error) => counts.3 += 1,
                }
                counts
            });

    for sev in &context.editor.config().statusline.diagnostics {
        match sev {
            Severity::Hint if hints > 0 => {
                write(context, Span::styled("●", context.editor.theme.get("hint")));
                write(context, format!(" {} ", hints).into());
            }
            Severity::Info if info > 0 => {
                write(context, Span::styled("●", context.editor.theme.get("info")));
                write(context, format!(" {} ", info).into());
            }
            Severity::Warning if warnings > 0 => {
                write(
                    context,
                    Span::styled("●", context.editor.theme.get("warning")),
                );
                write(context, format!(" {} ", warnings).into());
            }
            Severity::Error if errors > 0 => {
                write(
                    context,
                    Span::styled("●", context.editor.theme.get("error")),
                );
                write(context, format!(" {} ", errors).into());
            }
            _ => {}
        }
    }
}

fn render_workspace_diagnostics<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    use helix_core::diagnostic::Severity;
    let (hints, info, warnings, errors) = context.editor.diagnostics.values().flatten().fold(
        (0u32, 0u32, 0u32, 0u32),
        |mut counts, (diag, _)| {
            match diag.severity {
                // PERF: For large workspace diagnostics, this loop can be very tight.
                //
                // Most often the diagnostics will be for warnings and errors.
                // Errors should tend to be fixed fast, leaving warnings as the most common.
                Some(DiagnosticSeverity::WARNING) => counts.2 += 1,
                Some(DiagnosticSeverity::ERROR) => counts.3 += 1,
                Some(DiagnosticSeverity::HINT) => counts.0 += 1,
                Some(DiagnosticSeverity::INFORMATION) => counts.1 += 1,
                // Fallback to `hint`.
                _ => counts.0 += 1,
            }
            counts
        },
    );

    let sevs_to_show = &context.editor.config().statusline.workspace_diagnostics;

    // Avoid showing the " W " if no diagnostic counts will be shown.
    if !sevs_to_show.iter().any(|sev| match sev {
        Severity::Hint => hints != 0,
        Severity::Info => info != 0,
        Severity::Warning => warnings != 0,
        Severity::Error => errors != 0,
    }) {
        return;
    }

    write(context, " W ".into());

    for sev in sevs_to_show {
        match sev {
            Severity::Hint if hints > 0 => {
                write(context, Span::styled("●", context.editor.theme.get("hint")));
                write(context, format!(" {} ", hints).into());
            }
            Severity::Info if info > 0 => {
                write(context, Span::styled("●", context.editor.theme.get("info")));
                write(context, format!(" {} ", info).into());
            }
            Severity::Warning if warnings > 0 => {
                write(
                    context,
                    Span::styled("●", context.editor.theme.get("warning")),
                );
                write(context, format!(" {} ", warnings).into());
            }
            Severity::Error if errors > 0 => {
                write(
                    context,
                    Span::styled("●", context.editor.theme.get("error")),
                );
                write(context, format!(" {} ", errors).into());
            }
            _ => {}
        }
    }
}

fn render_selections<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let selection = context.doc.selection(context.view.id);
    let count = selection.len();
    write(
        context,
        if count == 1 {
            " 1 sel ".into()
        } else {
            format!(" {}/{count} sels ", selection.primary_index() + 1).into()
        },
    );
}

fn render_primary_selection_length<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let tot_sel = context.doc.selection(context.view.id).primary().len();
    write(
        context,
        format!(" {} char{} ", tot_sel, if tot_sel == 1 { "" } else { "s" }).into(),
    );
}

fn get_position(context: &RenderContext) -> Position {
    coords_at_pos(
        context.doc.text().slice(..),
        context
            .doc
            .selection(context.view.id)
            .primary()
            .cursor(context.doc.text().slice(..)),
    )
}

fn render_position<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let position = get_position(context);
    write(
        context,
        format!(" {}:{} ", position.row + 1, position.col + 1).into(),
    );
}

fn render_total_line_numbers<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let total_line_numbers = context.doc.text().len_lines();

    write(context, format!(" {} ", total_line_numbers).into());
}

fn render_position_percentage<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let position = get_position(context);
    let maxrows = context.doc.text().len_lines();
    write(
        context,
        format!("{}%", (position.row + 1) * 100 / maxrows).into(),
    );
}

fn render_file_encoding<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let enc = context.doc.encoding();

    if enc != encoding::UTF_8 {
        write(context, format!(" {} ", enc.name()).into());
    }
}

fn render_file_line_ending<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    use helix_core::LineEnding::*;
    let line_ending = match context.doc.line_ending {
        Crlf => "CRLF",
        LF => "LF",
        #[cfg(feature = "unicode-lines")]
        VT => "VT", // U+000B -- VerticalTab
        #[cfg(feature = "unicode-lines")]
        FF => "FF", // U+000C -- FormFeed
        #[cfg(feature = "unicode-lines")]
        CR => "CR", // U+000D -- CarriageReturn
        #[cfg(feature = "unicode-lines")]
        Nel => "NEL", // U+0085 -- NextLine
        #[cfg(feature = "unicode-lines")]
        LS => "LS", // U+2028 -- Line Separator
        #[cfg(feature = "unicode-lines")]
        PS => "PS", // U+2029 -- ParagraphSeparator
    };

    write(context, format!(" {} ", line_ending).into());
}

fn render_file_type<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let file_type = context.doc.language_name().unwrap_or(DEFAULT_LANGUAGE_NAME);

    write(context, format!(" {} ", file_type).into());
}

fn render_file_name<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let title = {
        let rel_path = context.doc.relative_path();
        let path = rel_path
            .as_ref()
            .map(|p| p.to_string_lossy())
            .unwrap_or_else(|| SCRATCH_BUFFER_NAME.into());
        format!(" {} ", path)
    };

    write(context, title.into());
}

fn render_breadcrumbs<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let editor = context.editor;
    let Some(syntax) = context.doc.syntax() else {
        return;
    };
    let text = context.doc.text().slice(..);
    let cursor = context
        .doc
        .selection(context.view.id)
        .primary()
        .cursor(text);

    let breadcrumbs: Vec<_> = syntax
        .breadcrumbs(
            text,
            &editor.syn_loader.load(),
            text.char_to_byte(cursor) as u32,
        )
        .into_iter()
        .map(|breadcrumb| {
            let mut spans = Vec::new();
            for segment in breadcrumb.segments {
                if !spans.is_empty() {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(segment.text, editor.theme.get(segment.scope)));
            }
            Spans(spans)
        })
        .collect();

    let config = editor.config();
    let config = &config.statusline.breadcrumbs;
    // Truncating drops breadcrumbs but always keeps the innermost one.
    context.shrinkable |= config.truncate && breadcrumbs.len() > 1;

    let trail = breadcrumb_trail(
        &breadcrumbs,
        config,
        editor.theme.get("ui.statusline.separator"),
        context.overflow,
    );
    for span in trail {
        write(context, span);
    }
}

/// Lays out `breadcrumbs`, outermost first, each after a separator and followed by a space. If
/// `truncate` is enabled, the outermost breadcrumbs are replaced with an ellipsis until the trail
/// is `overflow` columns narrower, keeping at least the innermost one.
fn breadcrumb_trail<'a>(
    breadcrumbs: &[Spans<'a>],
    config: &BreadcrumbsConfig,
    separator_style: Style,
    overflow: usize,
) -> Vec<Span<'a>> {
    let layout = |skip: usize| {
        let mut trail = Vec::new();
        if skip > 0 {
            trail.push(Span::styled("…", separator_style));
            trail.push(Span::raw(" "));
        }
        for (i, breadcrumb) in breadcrumbs.iter().enumerate().skip(skip) {
            if i > 0 || config.leading_separator {
                trail.push(Span::styled(config.separator.clone(), separator_style));
                trail.push(Span::raw(" "));
            }
            trail.extend(breadcrumb.0.iter().cloned());
            trail.push(Span::raw(" "));
        }
        Spans(trail)
    };

    let mut trail = layout(0);
    if config.truncate && overflow > 0 {
        let max_width = trail.width().saturating_sub(overflow);
        for skip in 1..breadcrumbs.len() {
            trail = layout(skip);
            if trail.width() <= max_width {
                break;
            }
        }
    }
    trail.0
}

fn render_file_absolute_path<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let title = {
        let path = context
            .doc
            .path()
            .as_ref()
            .map_or_else(|| SCRATCH_BUFFER_NAME.into(), |p| p.to_string_lossy());
        format!(" {} ", path)
    };

    write(context, title.into());
}

fn render_file_modification_indicator<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let title = if context.doc.is_modified() {
        "[+]"
    } else {
        "   "
    };

    write(context, title.into());
}

fn render_read_only_indicator<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let title = if context.doc.readonly {
        " [readonly] "
    } else {
        ""
    };
    write(context, title.into());
}

fn render_file_base_name<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let title = {
        let rel_path = context.doc.relative_path();
        let path = rel_path
            .as_ref()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy()))
            .unwrap_or_else(|| SCRATCH_BUFFER_NAME.into());
        format!(" {} ", path)
    };

    write(context, title.into());
}

fn render_separator<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let sep = &context.editor.config().statusline.separator;
    let style = context.editor.theme.get("ui.statusline.separator");

    write(context, Span::styled(sep.to_string(), style));
}

fn render_spacer<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    write(context, " ".into());
}

fn render_version_control<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let head = context
        .doc
        .version_control_head()
        .unwrap_or_default()
        .to_string();

    write(context, head.into());
}

fn render_register<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    if let Some(reg) = context.editor.selected_register {
        write(context, format!(" reg={} ", reg).into())
    }
}

fn render_file_indent_style<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let style = context.doc.indent_style;

    write(
        context,
        match style {
            IndentStyle::Tabs => " tabs ".into(),
            IndentStyle::Spaces(indent) => {
                format!(" {} space{} ", indent, if indent == 1 { "" } else { "s" }).into()
            }
        },
    );
}

fn render_cwd<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    let cwd = helix_stdx::env::current_working_dir();
    let cwd = cwd
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    write(context, cwd.into())
}

fn render_code_action_hint<'a, F>(context: &mut RenderContext<'a>, write: F)
where
    F: Fn(&mut RenderContext<'a>, Span<'a>) + Copy,
{
    if context.focused && context.doc.code_action_hints(context.view.id) {
        write(context, " ⋮ ".into())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const BREADCRUMBS: [&str; 3] = ["mod a", "impl Editor", "fn render"];

    fn trail(breadcrumbs: &[&str], config: &BreadcrumbsConfig, overflow: usize) -> String {
        let breadcrumbs: Vec<_> = breadcrumbs.iter().copied().map(Spans::from).collect();
        breadcrumb_trail(&breadcrumbs, config, Style::default(), overflow)
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn breadcrumb_trail_separators() {
        let config = BreadcrumbsConfig::default();
        assert_eq!(
            trail(&BREADCRUMBS, &config, 0),
            "> mod a > impl Editor > fn render "
        );
        assert_eq!(trail(&[], &config, 0), "");

        let config = BreadcrumbsConfig {
            separator: "›".to_string(),
            leading_separator: false,
            ..Default::default()
        };
        assert_eq!(
            trail(&BREADCRUMBS, &config, 0),
            "mod a › impl Editor › fn render "
        );
    }

    #[test]
    fn breadcrumb_trail_truncation() {
        let config = BreadcrumbsConfig::default();
        // Replacing "> mod a " with "… " saves six columns.
        assert_eq!(
            trail(&BREADCRUMBS, &config, 1),
            "… > impl Editor > fn render "
        );
        assert_eq!(
            trail(&BREADCRUMBS, &config, 6),
            "… > impl Editor > fn render "
        );
        assert_eq!(trail(&BREADCRUMBS, &config, 7), "… > fn render ");
        // The innermost breadcrumb is always kept.
        assert_eq!(trail(&BREADCRUMBS, &config, 100), "… > fn render ");
        assert_eq!(trail(&BREADCRUMBS[2..], &config, 100), "> fn render ");

        let config = BreadcrumbsConfig {
            leading_separator: false,
            ..Default::default()
        };
        assert_eq!(
            trail(&BREADCRUMBS, &config, 1),
            "… > impl Editor > fn render "
        );

        let config = BreadcrumbsConfig {
            truncate: false,
            ..Default::default()
        };
        assert_eq!(
            trail(&BREADCRUMBS, &config, 100),
            "> mod a > impl Editor > fn render "
        );
    }
}
