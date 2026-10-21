//! Markdown drawn rendered, after render-markdown.nvim: markup hidden or shown as icons.
//!
//! The conceals of Markdown are revealed while a cursor is on their line.

mod entities;

use std::borrow::Cow;
use std::ops::Range;
use std::str::FromStr;

use tree_house::tree_sitter::Node;
use unicode_width::UnicodeWidthStr;

use super::{Cached, Reveal};
use crate::chars::char_is_line_ending;
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
}

/// The scope icons of links are styled with.
const LINK_SCOPE: &str = "markup.link.text";

/// Renders the captured Markdown of a query match into conceals.
pub(super) struct Renderer<'a> {
    text: RopeSlice<'a>,
    options: MarkdownRender,
    conceals: &'a mut Vec<Cached>,
}

impl<'a> Renderer<'a> {
    pub(super) fn new(
        text: RopeSlice<'a>,
        options: MarkdownRender,
        conceals: &'a mut Vec<Cached>,
    ) -> Self {
        Self {
            text,
            options,
            conceals,
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
        let superscript: String = number.chars().map(superscript_digit).collect();
        self.conceal(
            bytes,
            Replacement::from(superscript),
            ConcealStyle::Scope(LINK_SCOPE),
        );
    }
}

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

fn superscript_digit(digit: char) -> char {
    match digit {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        _ => '⁹',
    }
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
