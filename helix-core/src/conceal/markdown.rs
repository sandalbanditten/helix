//! Markdown drawn rendered, after render-markdown.nvim: markup hidden or shown as icons.
//!
//! The conceals of Markdown are revealed while a cursor is on their line.

mod entities;

use std::borrow::Cow;
use std::ops::Range;
use std::str::FromStr;

use tree_house::tree_sitter::Node;
use unicode_width::UnicodeWidthStr;

use super::{latex, widened, Cached, ConcealDecoration, DecorationStyle, Reveal};
use crate::chars::char_is_line_ending;
use crate::icons;
use crate::line_ending::line_end_char_index;
use crate::syntax::config::FileType;
use crate::syntax::Loader;
use crate::text_annotations::{Conceal, ConcealStyle, Replacement};
use crate::RopeSlice;

/// How Markdown is drawn rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownRender {
    /// Whether icons are drawn with Nerd Font glyphs, or else left out or drawn in plain Unicode.
    pub icons: bool,
    /// Whether Markdown injected into other languages, like their doc comments, is rendered too.
    pub injected: bool,
}

/// A part of Markdown's markup, captured by a `conceals.scm` query as `markdown-…`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Element {
    /// Hidden delimiters, like the `**` around bold text or the backticks of inline code.
    Hide,
    /// A character reference shown as its character, like `&copy;` as `©`.
    Entity,
    /// A link, image or autolink shown as an icon and its text, without its brackets and
    /// destination.
    Link,
    /// An Obsidian-style wikilink, `[[note|alias]]`, shown as an icon and its alias or note.
    Wikilink,
    /// A numeric footnote reference, `[^12]`, shown in superscript as `¹²`.
    Footnote,
    /// A thematic break, `---`, shown as a rule across the row.
    Rule,
    /// An ATX heading, `## Title`, with its `#`s shown as one in its level's color.
    Heading,
    /// The marker of a list item: a bullet, a number counted along the list, or a task's
    /// checkbox as an icon.
    ListMarker,
    /// The `>`s of block quotes, shown as bars in the color of their quote or callout.
    QuoteMarker,
    /// A block quote that may be a callout, `> [!NOTE]`, whose kind is shown as an icon and title.
    Callout,
    /// A fenced code block, whose opening fence is shown as the icon of its language, its info
    /// and a rule, and whose closing fence as a rule.
    CodeFence,
    /// A command of LaTeX math in Markdown shown as the symbol it stands for, like `\alpha` as
    /// `α`, with the script after it, like `\sum_{i=1}` as `∑ᵢ₌₁`.
    LatexCommand,
    /// The scripts in a word of LaTeX math in Markdown shown as superscripts and subscripts, like
    /// `x^2` as `x²`.
    LatexScripts,
}

impl Element {
    /// Whether the element is in a language injected into Markdown, which renders only there.
    pub(super) fn is_injected(self) -> bool {
        matches!(self, Self::LatexCommand | Self::LatexScripts)
    }
}

impl FromStr for Element {
    type Err = ();

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "hide" => Ok(Self::Hide),
            "entity" => Ok(Self::Entity),
            "link" => Ok(Self::Link),
            "wikilink" => Ok(Self::Wikilink),
            "footnote" => Ok(Self::Footnote),
            "rule" => Ok(Self::Rule),
            "heading" => Ok(Self::Heading),
            "list-marker" => Ok(Self::ListMarker),
            "quote-marker" => Ok(Self::QuoteMarker),
            "callout" => Ok(Self::Callout),
            "code-fence" => Ok(Self::CodeFence),
            "latex-command" => Ok(Self::LatexCommand),
            "latex-scripts" => Ok(Self::LatexScripts),
            _ => Err(()),
        }
    }
}

/// Nerd Font icons, from render-markdown.nvim.
mod icon {
    pub const WEB: &str = "\u{f059f} ";
    pub const LINK: &str = "\u{f0339} ";
    pub const IMAGE: &str = "\u{f0976} ";
    pub const EMAIL: &str = "\u{f0013} ";
    pub const WIKILINK: &str = "\u{f15d6} ";
    pub const UNCHECKED: &str = "\u{f0131} ";
    pub const CHECKED: &str = "\u{f0c52} ";
    pub const TODO: &str = "\u{f0954} ";
}

/// Icons in plain Unicode, for fonts without Nerd Font glyphs.
mod plain {
    pub const UNCHECKED: &str = "☐ ";
    pub const CHECKED: &str = "☑ ";
    pub const TODO: &str = "◐ ";
}

/// The scopes headings are styled with, by level.
const HEADING_SCOPES: [&str; 6] = [
    "markup.heading.1",
    "markup.heading.2",
    "markup.heading.3",
    "markup.heading.4",
    "markup.heading.5",
    "markup.heading.6",
];

/// What a bullet is shown as.
const BULLET: &str = "•";

/// What the `>` of a block quote is shown as.
const QUOTE_BAR: &str = "▎";

/// The scope icons of links are styled with.
const LINK_SCOPE: &str = "markup.link.text";

/// Renders the captured Markdown of a query match into conceals and decorations.
pub(super) struct Renderer<'a> {
    text: RopeSlice<'a>,
    loader: &'a Loader,
    options: MarkdownRender,
    conceals: &'a mut Vec<Cached>,
    decorations: &'a mut Vec<(usize, ConcealDecoration)>,
}

impl<'a> Renderer<'a> {
    pub(super) fn new(
        text: RopeSlice<'a>,
        loader: &'a Loader,
        options: MarkdownRender,
        conceals: &'a mut Vec<Cached>,
        decorations: &'a mut Vec<(usize, ConcealDecoration)>,
    ) -> Self {
        Self {
            text,
            loader,
            options,
            conceals,
            decorations,
        }
    }

    /// Renders `element`, captured as `node` and the nodes after it up to the end of `bytes`.
    pub(super) fn render(&mut self, element: Element, node: &Node, bytes: Range<usize>) {
        match element {
            Element::Hide => self.conceal(bytes, Replacement::Hidden, ConcealStyle::Text),
            Element::Entity => self.entity(bytes),
            Element::Link => self.link(node),
            Element::Wikilink => self.wikilink(bytes),
            Element::Footnote => self.footnote(bytes),
            Element::Rule => {
                let rule = self.slice(bytes.clone());
                let end = bytes.start + rule.trim_end().len();
                self.conceal(
                    bytes.start..end,
                    Replacement::Fill("─"),
                    ConcealStyle::Guide,
                );
            }
            Element::Heading => self.heading(node),
            Element::ListMarker => self.list_marker(node),
            Element::QuoteMarker => self.quote_marker(node),
            Element::Callout => self.callout(node),
            Element::CodeFence => self.code_fence(node),
            Element::LatexCommand => self.latex_command(node),
            Element::LatexScripts => self.latex_scripts(node),
        }
    }

    /// Conceals the bytes `bytes` with `replacement` in `style`, unless they span lines.
    fn conceal(&mut self, bytes: Range<usize>, replacement: Replacement, style: ConcealStyle) {
        let text = self.text;
        if bytes.is_empty() || bytes.end > text.len_bytes() {
            return;
        }
        let range = text.byte_to_char(bytes.start)..text.byte_to_char(bytes.end);
        if text.slice(range.clone()).chars().any(char_is_line_ending) {
            return;
        }
        let conceal = Conceal::new(range.start, range.end, replacement).styled(style);
        self.conceals.push(Cached {
            conceal,
            reveal: Reveal::Line,
        });
    }

    /// Shows `replacement` before the byte `byte`, concealing nothing.
    fn insert(&mut self, byte: usize, replacement: Replacement, style: ConcealStyle) {
        let char_idx = self.text.byte_to_char(byte);
        let conceal = Conceal::new(char_idx, char_idx, replacement).styled(style);
        self.conceals.push(Cached {
            conceal,
            reveal: Reveal::Line,
        });
    }

    /// Shows the bytes `bytes` as an `icon`, or hides them if icons are off.
    fn icon(&mut self, bytes: Range<usize>, icon: &'static str, scope: &'static str) {
        let replacement = if self.options.icons {
            Replacement::from(icon)
        } else {
            Replacement::Hidden
        };
        self.conceal(bytes, replacement, ConcealStyle::Scope(scope));
    }

    fn slice(&self, bytes: Range<usize>) -> Cow<'a, str> {
        self.text.byte_slice(bytes).into()
    }

    /// The Nerd Font `icon`, or the `plain` one if icons are off.
    fn pick(&self, icon: &'static str, plain: &'static str) -> &'static str {
        if self.options.icons {
            icon
        } else {
            plain
        }
    }

    /// The end of the line that `byte` is on, before its line break.
    fn line_end_byte(&self, byte: usize) -> usize {
        let text = self.text;
        let line = text.byte_to_line(byte);
        text.char_to_byte(line_end_char_index(&text, line))
    }

    fn decorate(&mut self, byte: usize, decoration: ConcealDecoration) {
        let line = self.text.byte_to_line(byte);
        self.decorations.push((line, decoration));
    }

    /// Decorates the text of the bytes `bytes` line by line, without the whitespace and quote
    /// markers that lines start with.
    fn decorate_text(&mut self, bytes: Range<usize>, style: DecorationStyle) {
        let text = self.text;
        let mut start = bytes.start;
        while start < bytes.end {
            let line_end = self.line_end_byte(start).min(bytes.end);
            let line = self.slice(start..line_end);
            let content = line.trim_start_matches(|c: char| c.is_whitespace() || c == '>');
            let content_start = start + line.len() - content.len();
            let content_end = start + line.trim_end().len();
            if content_start < content_end {
                let range = text.byte_to_char(content_start)..text.byte_to_char(content_end);
                self.decorate(content_start, ConcealDecoration { range, style });
            }
            let next_line = text.byte_to_line(start) + 1;
            if next_line >= text.len_lines() {
                break;
            }
            start = text.line_to_byte(next_line);
        }
    }

    /// Shows the `#`s of an ATX heading as one, in its level's color, and hides its closing
    /// `#`s.
    fn heading(&mut self, heading: &Node) {
        let Some(marker) = heading.child(0) else {
            return;
        };
        let Some(level) = marker
            .kind()
            .strip_prefix("atx_h")
            .and_then(|kind| kind.strip_suffix("_marker"))
            .and_then(|level| level.parse::<u8>().ok())
            .filter(|level| (1..=6).contains(level))
        else {
            return;
        };
        let marker = bytes_of(&marker);
        let line_end = self.line_end_byte(marker.start);
        let after = self.slice(marker.end..line_end);
        // the `#`s of every level are one, in the level's color
        let written = self.slice(marker.clone());
        let hashes = marker.start..marker.start + written.trim_end().len();
        let scope = ConcealStyle::Scope(HEADING_SCOPES[usize::from(level - 1)]);
        self.conceal(hashes.clone(), Replacement::from("#"), scope);

        // a closing sequence after a space, like the `##` of `## Title ##`
        let content = after.trim_end();
        let without_closing = content.trim_end_matches('#');
        if without_closing.len() < content.len()
            && (without_closing.is_empty() || without_closing.ends_with([' ', '\t']))
        {
            let start = marker.end + without_closing.trim_end().len();
            let end = marker.end + content.len();
            self.conceal(
                start.max(hashes.end + 1)..end,
                Replacement::Hidden,
                ConcealStyle::Text,
            );
        }
    }

    /// Shows a list marker as a bullet, a number counted along the list or a checkbox.
    fn list_marker(&mut self, marker: &Node) {
        let Some(item) = marker.parent() else {
            return;
        };
        // a marker indented more than its list starts with the indentation
        let bytes = bytes_of(marker);
        let indentation = {
            let written = self.slice(bytes.clone());
            written.len() - written.trim_start().len()
        };
        let bytes = bytes.start + indentation..bytes.end;
        let task = marker.next_sibling().filter(|task| {
            matches!(
                task.kind(),
                "task_list_marker_checked" | "task_list_marker_unchecked"
            )
        });
        // `[-]`, a task in progress, which the grammar reads as text
        let is_todo = task.is_none() && {
            let after = self.slice(bytes.end..self.line_end_byte(bytes.end));
            after == "[-]" || after.starts_with("[-] ") || after.starts_with("[-]\t")
        };
        if let Some(task) = task {
            let checked = task.kind() == "task_list_marker_checked";
            let (icon, scope) = if checked {
                (
                    self.pick(icon::CHECKED, plain::CHECKED),
                    "markup.list.checked",
                )
            } else {
                let icon = self.pick(icon::UNCHECKED, plain::UNCHECKED);
                (icon, "markup.list.unchecked")
            };
            self.conceal(
                bytes.start..task.end_byte() as usize,
                Replacement::from(icon),
                ConcealStyle::Scope(scope),
            );
            let paragraph = task
                .next_sibling()
                .filter(|text| text.kind() == "paragraph");
            if let Some(inline) = paragraph.filter(|_| checked).and_then(|text| text.child(0)) {
                self.decorate_text(bytes_of(&inline), DecorationStyle::Done);
            }
        } else if is_todo {
            self.conceal(
                bytes.start..bytes.end + "[-]".len(),
                Replacement::from(self.pick(icon::TODO, plain::TODO)),
                ConcealStyle::Scope("markup.raw"),
            );
        } else if matches!(marker.kind(), "list_marker_dot" | "list_marker_parenthesis") {
            let written = self.slice(bytes.clone());
            let written = written.trim_end();
            let Ok(number) = written.trim_end_matches(['.', ')']).parse::<u64>() else {
                return;
            };
            // the position of the item in its list, unless a later number is written out
            let shown = if number > 1 {
                number
            } else {
                item.parent().map_or(1, |list| {
                    let before = list.children().take_while(|other| other != &item);
                    before.filter(|other| other.kind() == "list_item").count() as u64 + 1
                })
            };
            self.conceal(
                bytes.start..bytes.start + written.len(),
                Replacement::from(format!("{shown}.")),
                ConcealStyle::Scope("markup.list.numbered"),
            );
        } else {
            self.conceal(
                bytes.start..bytes.start + 1,
                Replacement::from(BULLET),
                ConcealStyle::Scope("markup.list.unnumbered"),
            );
        }
    }

    /// Shows the `>`s of a quote marker, or of the continuation of a line in a quote, as bars in
    /// the color of the quotes they belong to: the outermost quote has the first `>`.
    fn quote_marker(&mut self, marker: &Node) {
        let mut quotes = Vec::new();
        let mut ancestor = marker.parent();
        while let Some(node) = ancestor {
            if node.kind() == "block_quote" {
                quotes.push(node.clone());
            }
            ancestor = node.parent();
        }
        quotes.reverse();
        // a quote marker is the `>` of its own quote, the innermost
        let first = match marker.kind() {
            "block_quote_marker" => quotes.len().saturating_sub(1),
            _ => 0,
        };
        let bytes = bytes_of(marker);
        let markers = self.slice(bytes.clone());
        let bars: Vec<_> = markers
            .match_indices('>')
            .map(|(i, _)| bytes.start + i)
            .collect();
        for (k, bar) in bars.into_iter().enumerate() {
            let scope = quotes
                .get(first + k)
                .and_then(|quote| self.callout_kind(quote))
                .map_or("markup.quote", |callout| callout.scope);
            self.conceal(
                bar..bar + 1,
                Replacement::from(QUOTE_BAR),
                ConcealStyle::Scope(scope),
            );
        }
    }

    /// The kind of callout `quote` is, if its first line starts with `[!KIND]`.
    fn callout_kind(&self, quote: &Node) -> Option<&'static Callout> {
        let marker = quote
            .child(0)
            .filter(|marker| marker.kind() == "block_quote_marker")?;
        let start = marker.end_byte() as usize;
        let line = self.slice(start..self.line_end_byte(start));
        let kind = line.strip_prefix("[!")?.split_once(']')?.0;
        CALLOUTS
            .iter()
            .find(|callout| callout.kind.eq_ignore_ascii_case(kind))
    }

    /// Shows the opening fence of a code block as the icon of its language, its info string and a
    /// rule, or as a rule if it has no info string, and the closing fence as a rule.
    fn code_fence(&mut self, block: &Node) {
        let mut children = block.children();
        let Some(open) = children
            .next()
            .filter(|open| open.kind() == "fenced_code_block_delimiter")
        else {
            return;
        };
        let open = bytes_of(&open);
        let info = block.children().find(|child| child.kind() == "info_string");
        match info {
            Some(info) => {
                let language = info.children().find(|child| child.kind() == "language");
                let icon = match language {
                    Some(language) if self.options.icons => {
                        let language = self.slice(bytes_of(&language));
                        Replacement::from(format!("{} ", self.language_icon(&language)))
                    }
                    _ => Replacement::Hidden,
                };
                // the fence and the space before the info string
                let info_start = info.start_byte() as usize;
                self.conceal(open.start..info_start, icon, ConcealStyle::Scope("label"));
                let line_end = self.line_end_byte(open.start);
                let rule = ConcealStyle::Guide;
                self.insert(line_end, Replacement::from(" "), rule);
                self.insert(line_end, Replacement::Fill("─"), rule);
            }
            None => self.conceal(open, Replacement::Fill("─"), ConcealStyle::Guide),
        }
        let close = block
            .children()
            .skip(1)
            .filter(|child| child.kind() == "fenced_code_block_delimiter")
            .last();
        if let Some(close) = close {
            let close = bytes_of(&close);
            self.conceal(close, Replacement::Fill("─"), ConcealStyle::Guide);
        }
    }

    /// The icon of the files of the language a code block is written in, like `rust` or `py`.
    fn language_icon(&self, name: &str) -> &'static str {
        let language = self
            .loader
            .language_for_match(RopeSlice::from(name))
            .map(|language| self.loader.language(language).config());
        let extension = language.and_then(|config| {
            config
                .file_types
                .iter()
                .find_map(|file_type| match file_type {
                    FileType::Extension(extension) => Some(extension.as_str()),
                    FileType::Glob(_) => None,
                })
        });
        icons::file(&format!("code.{}", extension.unwrap_or(name)))
    }

    /// Shows a LaTeX command as the symbol it stands for, and the script it ends with, like the
    /// `_` of `\sum_{i=1}`, as a subscript.
    fn latex_command(&mut self, command: &Node) {
        let Some(name) = command
            .child(0)
            .filter(|name| name.kind() == "command_name")
        else {
            return;
        };
        let name_bytes = bytes_of(&name);
        let written = self.slice(name_bytes.clone());
        let base = written.trim_end_matches(['_', '^']);
        let group = name
            .next_sibling()
            .filter(|group| group.kind() == "curly_group");
        // a letter in an alphabet, like `\mathbb{R}`, as a whole
        if base.len() == written.len() && latex::ALPHABETS.contains(&base) {
            if let Some(group) = group {
                let letter = self.slice(bytes_of(&group));
                if let Some(symbol) = latex::symbol(&format!("{base}{letter}")) {
                    let bytes = name_bytes.start..group.end_byte() as usize;
                    self.conceal(bytes, Replacement::from(symbol), ConcealStyle::Text);
                }
            }
            return;
        }
        if let Some(symbol) = latex::symbol(base) {
            let bytes = name_bytes.start..name_bytes.start + base.len();
            self.conceal(bytes, widened(symbol), ConcealStyle::Text);
        }
        // the grammar keeps the `_` or `^` after a command in its name
        if let (Some(mark), Some(group)) = (written[base.len()..].chars().next(), group) {
            let group_bytes = bytes_of(&group);
            let script = self.slice(group_bytes.start + 1..group_bytes.end - 1);
            if let Some(script) = latex::script(mark, &script) {
                let bytes = name_bytes.start + base.len()..group_bytes.end;
                self.conceal(bytes, Replacement::from(script), ConcealStyle::Text);
            }
        }
    }

    /// Shows the scripts of a word of LaTeX math, like `x^2` or `a_{ij}`, as superscripts and
    /// subscripts, if Unicode has all their characters.
    fn latex_scripts(&mut self, word: &Node) {
        let bytes = bytes_of(word);
        let written = self.slice(bytes.clone());
        let marks: Vec<_> = written.match_indices(['^', '_']).map(|(i, _)| i).collect();
        for i in marks {
            let mark = written[i..].chars().next().unwrap_or('^');
            let start = bytes.start + i;
            match written[i + 1..].chars().next() {
                // one character, like `x^2`
                Some(c) if c != '^' && c != '_' => {
                    if let Some(script) = latex::script(mark, &c.to_string()) {
                        let end = start + 1 + c.len_utf8();
                        self.conceal(start..end, Replacement::from(script), ConcealStyle::Text);
                    }
                }
                // a group the grammar puts after the word, like `x^{10}`
                None => {
                    let group = word
                        .next_sibling()
                        .or_else(|| word.parent().and_then(|text| text.next_sibling()))
                        .filter(|group| group.kind() == "curly_group");
                    let Some(group) = group else {
                        continue;
                    };
                    let group_bytes = bytes_of(&group);
                    if group_bytes.start != bytes.end {
                        continue;
                    }
                    let script = self.slice(group_bytes.start + 1..group_bytes.end - 1);
                    if let Some(script) = latex::script(mark, &script) {
                        let bytes = start..group_bytes.end;
                        self.conceal(bytes, Replacement::from(script), ConcealStyle::Text);
                    }
                }
                Some(_) => {}
            }
        }
    }

    /// Shows the `[!KIND]` of a callout as its icon and title, or as its icon if it has a title
    /// of its own, which is drawn in the callout's color.
    fn callout(&mut self, quote: &Node) {
        let Some(callout) = self.callout_kind(quote) else {
            return;
        };
        let Some(marker) = quote.child(0) else {
            return;
        };
        let start = marker.end_byte() as usize;
        let line_end = self.line_end_byte(start);
        let kind_end = start + "[!]".len() + callout.kind.len();
        let title = self.slice(kind_end..line_end);
        let icon = self.pick(callout.icon, "");
        let style = ConcealStyle::Scope(callout.scope);
        if title.trim().is_empty() {
            let shown = format!("{icon}{}", callout.title);
            self.conceal(start..kind_end, Replacement::from(shown), style);
        } else {
            let title_start = kind_end + (title.len() - title.trim_start().len());
            let replacement = if icon.is_empty() {
                Replacement::Hidden
            } else {
                Replacement::from(icon)
            };
            self.conceal(start..title_start, replacement, style);
            let title_style = DecorationStyle::Scope(callout.scope);
            self.decorate_text(title_start..line_end, title_style);
        }
    }

    fn entity(&mut self, bytes: Range<usize>) {
        let reference = self.slice(bytes.clone());
        let Some(shown) = character_reference(&reference) else {
            return;
        };
        self.conceal(bytes, Replacement::Text(shown), ConcealStyle::Text);
    }

    /// Conceals the brackets and destination of a link or image, `[text](destination)`, its
    /// label, `[text][label]`, or the angle brackets of an autolink, `<https://…>`.
    fn link(&mut self, node: &Node) {
        let start = node.start_byte() as usize;
        let end = node.end_byte() as usize;
        match node.kind() {
            "uri_autolink" | "email_autolink" if end - start > 2 => {
                let icon = match node.kind() {
                    "email_autolink" => icon::EMAIL,
                    _ if is_web(&self.slice(start + 1..end - 1)) => icon::WEB,
                    _ => icon::LINK,
                };
                self.icon(start..start + 1, icon, LINK_SCOPE);
                self.conceal(end - 1..end, Replacement::Hidden, ConcealStyle::Text);
            }
            "inline_link" | "full_reference_link" | "collapsed_reference_link" | "image" => {
                let is_image = node.kind() == "image";
                let shown = if is_image {
                    "image_description"
                } else {
                    "link_text"
                };
                let Some(shown) = node
                    .children()
                    .find(|child| child.kind() == shown)
                    .filter(|shown| shown.start_byte() < shown.end_byte())
                else {
                    return;
                };
                let icon = if is_image {
                    icon::IMAGE
                } else {
                    match node
                        .children()
                        .find(|child| child.kind() == "link_destination")
                    {
                        Some(destination) if is_web(&self.slice(bytes_of(&destination))) => {
                            icon::WEB
                        }
                        _ => icon::LINK,
                    }
                };
                // the brackets are the bytes around the shown text: `[` or `![` and `](…)` or `][…]`
                let shown_end = shown.end_byte() as usize;
                self.icon(start..shown.start_byte() as usize, icon, LINK_SCOPE);
                self.conceal(shown_end..end, Replacement::Hidden, ConcealStyle::Text);
            }
            _ => {}
        }
    }

    /// Shows `[[note]]` and `[[note#heading]]` as an icon and the note, and `[[note|alias]]` as an
    /// icon and the alias.
    fn wikilink(&mut self, bytes: Range<usize>) {
        let link = self.slice(bytes.clone());
        let Some(inner) = link
            .strip_prefix("[[")
            .and_then(|link| link.strip_suffix("]]"))
            .filter(|inner| !inner.is_empty())
        else {
            return;
        };
        // the alias follows the last `|`, which may be escaped in a table cell
        let shown_start = match inner.rfind('|') {
            Some(bar) if bar + 1 < inner.len() => bar + 1,
            _ => 0,
        };
        let start = bytes.start;
        self.icon(start..start + 2 + shown_start, icon::WIKILINK, LINK_SCOPE);
        self.conceal(
            bytes.end - 2..bytes.end,
            Replacement::Hidden,
            ConcealStyle::Text,
        );
    }

    /// Shows a numeric footnote reference, `[^12]`, as `¹²`.
    fn footnote(&mut self, bytes: Range<usize>) {
        let reference = self.slice(bytes.clone());
        let Some(number) = reference
            .strip_prefix("[^")
            .and_then(|reference| reference.strip_suffix(']'))
            .filter(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        else {
            return;
        };
        let Some(superscript) = latex::script('^', number) else {
            return;
        };
        self.conceal(
            bytes,
            Replacement::from(superscript),
            ConcealStyle::Scope(LINK_SCOPE),
        );
    }
}

/// A kind of callout, `> [!KIND]`, shown as its icon and title in the color of a theme scope.
#[derive(Debug)]
struct Callout {
    kind: &'static str,
    title: &'static str,
    icon: &'static str,
    scope: &'static str,
}

const fn callout(
    kind: &'static str,
    title: &'static str,
    icon: &'static str,
    scope: &'static str,
) -> Callout {
    Callout {
        kind,
        title,
        icon,
        scope,
    }
}

/// The callouts of GitHub and Obsidian, as render-markdown.nvim shows them.
const CALLOUTS: &[Callout] = &[
    // GitHub
    callout("NOTE", "Note", "\u{f02fd} ", "info"),
    callout("TIP", "Tip", "\u{f0336} ", "diff.plus"),
    callout("IMPORTANT", "Important", "\u{f017e} ", "hint"),
    callout("WARNING", "Warning", "\u{f002a} ", "warning"),
    callout("CAUTION", "Caution", "\u{f0ce6} ", "error"),
    // Obsidian
    callout("ABSTRACT", "Abstract", "\u{f0a38} ", "info"),
    callout("SUMMARY", "Summary", "\u{f0a38} ", "info"),
    callout("TLDR", "Tldr", "\u{f0a38} ", "info"),
    callout("INFO", "Info", "\u{f02fd} ", "info"),
    callout("TODO", "Todo", "\u{f05e1} ", "info"),
    callout("HINT", "Hint", "\u{f0336} ", "diff.plus"),
    callout("SUCCESS", "Success", "\u{f012c} ", "diff.plus"),
    callout("CHECK", "Check", "\u{f012c} ", "diff.plus"),
    callout("DONE", "Done", "\u{f012c} ", "diff.plus"),
    callout("QUESTION", "Question", "\u{f0625} ", "warning"),
    callout("HELP", "Help", "\u{f0625} ", "warning"),
    callout("FAQ", "Faq", "\u{f0625} ", "warning"),
    callout("ATTENTION", "Attention", "\u{f002a} ", "warning"),
    callout("FAILURE", "Failure", "\u{f0156} ", "error"),
    callout("FAIL", "Fail", "\u{f0156} ", "error"),
    callout("MISSING", "Missing", "\u{f0156} ", "error"),
    callout("DANGER", "Danger", "\u{f140c} ", "error"),
    callout("ERROR", "Error", "\u{f140c} ", "error"),
    callout("BUG", "Bug", "\u{f0a30} ", "error"),
    callout("EXAMPLE", "Example", "\u{f0279} ", "hint"),
    callout("QUOTE", "Quote", "\u{f11a8} ", "markup.quote"),
    callout("CITE", "Cite", "\u{f11a8} ", "markup.quote"),
];

fn bytes_of(node: &Node) -> Range<usize> {
    node.start_byte() as usize..node.end_byte() as usize
}

/// Whether `destination` is on the web.
fn is_web(destination: &str) -> bool {
    let destination = destination.trim_start_matches('<');
    ["http://", "https://"].iter().any(|scheme| {
        destination
            .get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })
}

/// What a character reference, like `&copy;`, `&#8594;` or `&#x2192;`, stands for, if it shows
/// something: invisible characters and line breaks are left as they are written.
fn character_reference(reference: &str) -> Option<Cow<'static, str>> {
    let name = reference.strip_prefix('&')?.strip_suffix(';')?;
    let shown: Cow<str> = match name.strip_prefix('#') {
        Some(number) => {
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
                .filter(|&c| c != '\0')?
                .to_string()
                .into()
        }
        None => {
            let i = entities::ENTITIES
                .binary_search_by_key(&name, |&(name, _)| name)
                .ok()?;
            entities::ENTITIES[i].1.into()
        }
    };
    let shows = shown.width() > 0
        && !shown
            .chars()
            .any(|c| c.is_control() || char_is_line_ending(c));
    shows.then_some(shown)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn character_references() {
        assert_eq!(character_reference("&copy;").as_deref(), Some("©"));
        assert_eq!(character_reference("&rarr;").as_deref(), Some("→"));
        assert_eq!(character_reference("&#8594;").as_deref(), Some("→"));
        assert_eq!(character_reference("&#x2192;").as_deref(), Some("→"));
        assert_eq!(character_reference("&nbsp;").as_deref(), Some("\u{a0}"));
        // unknown, invisible and line-breaking references stay
        for reference in [
            "&nope;",
            "&#0;",
            "&NewLine;",
            "&Tab;",
            "&ZeroWidthSpace;",
            "&#xD800;",
        ] {
            assert_eq!(character_reference(reference), None, "{reference}");
        }
    }

    #[test]
    fn entities_are_sorted() {
        assert!(entities::ENTITIES
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0));
    }
}
