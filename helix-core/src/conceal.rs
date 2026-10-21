//! Concealing: document text shown as the symbol it stands for, like `α` for `alpha` in Typst
//! math, or Markdown drawn rendered.
//!
//! A language's `conceals.scm` query captures the text to conceal, and a [`SymbolTable`] resolves
//! it. [`SyntaxConceals`] hands the conceals of a syntax tree to the document formatter, except
//! the ones the cursors reveal.

mod markdown;
mod typst;

pub use markdown::MarkdownRender;

use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::ops;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::chars::char_is_line_ending;
use crate::syntax::{merge_regions, Loader, Syntax};
use crate::text_annotations::{Conceal, ConcealSource, Replacement};
use crate::{RopeSlice, Selection};

/// The longest text, in bytes, that is looked up in a [`SymbolTable`] of symbols.
const MAX_CONCEALED_BYTES: usize = 64;

/// The longest wikilink, in bytes, that is concealed.
const MAX_WIKILINK_BYTES: usize = 512;

/// The most lines whose conceals are computed at once.
const FETCHED_LINES: usize = 128;

/// The most lines whose conceals are handed to the document formatter at once.
const SERVED_LINES: usize = 16;

/// A table of symbols that the text a `conceals.scm` query captures is looked up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolTable {
    /// Typst's symbols as math refers to them, like `alpha` or `arrow.r.long`.
    TypstMath,
    /// Typst's symbols and emoji as code refers to them, like `sym.qed` or `#emoji.face`.
    TypstCode,
    /// Typst's shorthands of more than one character, like `->` or `--`.
    TypstShorthand,
    /// Obsidian-style wikilinks in Typst markup, like `[[Note\#Heading|alias]]`, shown without
    /// their brackets and the backslashes of their escapes.
    TypstWikilink,
    /// Lines across the page, `#line(length: 100%)`, shown as a rule filling the row.
    TypstRule,
    /// Markdown's markup, drawn rendered from the captured node, like `**` hidden around bold
    /// text.
    Markdown(markdown::Element),
}

impl SymbolTable {
    /// What `text` is shown as, if the table looks up text rather than rendering Markdown.
    pub fn resolve(self, text: &str) -> Option<Replacement> {
        match self {
            Self::TypstMath => typst::math_symbol(text).map(widened),
            Self::TypstCode => typst::code_symbol(text).map(widened),
            Self::TypstShorthand => typst::shorthand(text).map(widened),
            Self::TypstWikilink => typst::wikilink(text).map(Replacement::from),
            Self::TypstRule => typst::is_full_width_line(text).then_some(Replacement::Fill("─")),
            Self::Markdown(_) => None,
        }
    }

    /// Whether the table renders Markdown.
    pub(crate) fn renders_markdown(self) -> bool {
        matches!(self, Self::Markdown(_))
    }

    /// The longest text, in bytes, that is looked up in the table.
    fn max_bytes(self) -> usize {
        match self {
            Self::TypstWikilink => MAX_WIKILINK_BYTES,
            _ => MAX_CONCEALED_BYTES,
        }
    }
}

/// `symbol` as a replacement, taking three columns if it is a long arrow like `⟹`, which fonts draw
/// about as wide as `==>`.
fn widened(symbol: &'static str) -> Replacement {
    const LONG_ARROWS: &[(&str, &str)] = &[
        ("⟵", "⟵  "),
        ("⟶", "⟶  "),
        ("⟷", "⟷  "),
        ("⟸", "⟸  "),
        ("⟹", "⟹  "),
        ("⟺", "⟺  "),
        ("⟻", "⟻  "),
        ("⟼", "⟼  "),
        ("⟽", "⟽  "),
        ("⟾", "⟾  "),
        ("⟿", "⟿  "),
        ("⬳", "⬳  "),
    ];
    let wide = LONG_ARROWS.iter().find(|&&(arrow, _)| arrow == symbol);
    Replacement::from(wide.map_or(symbol, |&(_, wide)| wide))
}

impl FromStr for SymbolTable {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "typst-math" => Ok(Self::TypstMath),
            "typst-code" => Ok(Self::TypstCode),
            "typst-shorthand" => Ok(Self::TypstShorthand),
            "typst-wikilink" => Ok(Self::TypstWikilink),
            "typst-rule" => Ok(Self::TypstRule),
            _ => name
                .strip_prefix("markdown-")
                .and_then(|element| element.parse().ok())
                .map(Self::Markdown)
                .ok_or_else(|| format!("unknown symbol table '{name}'")),
        }
    }
}

/// When concealed text is shown as it is, at the cursors of the focused view.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConcealReveal {
    /// While a cursor is on the text or next to it.
    #[default]
    Adjacent,
    /// While a cursor is on the text.
    Symbol,
    /// While a cursor is on the text's line.
    Line,
}

impl ConcealReveal {
    /// Whether `selection` shows the concealed chars `range` of `text` as they are.
    pub fn reveals(self, text: RopeSlice, selection: &Selection, range: ops::Range<usize>) -> bool {
        let line_start = || text.line_to_char(text.char_to_line(range.start));
        // the cursor positions that reveal `range`
        let cursors = match self {
            Self::Adjacent if range.start > line_start() => range.start - 1..=range.end,
            Self::Adjacent => range.start..=range.end,
            Self::Symbol => range.start..=range.end - 1,
            Self::Line => {
                let line = text.char_to_line(range.start);
                // the last line has no line break, so its last position is the end of the text
                let last = if line + 1 < text.len_lines() {
                    text.line_to_char(line + 1) - 1
                } else {
                    text.len_chars()
                };
                text.line_to_char(line)..=last
            }
        };
        let cuts = |pos: usize| range.start < pos && pos < range.end;

        let ranges = selection.ranges();
        // ranges are sorted and disjoint, so their ends are sorted too
        let first = ranges.partition_point(|sel| sel.to() < *cursors.start().min(&range.start));
        ranges[first..]
            .iter()
            .take_while(|sel| sel.from() <= *cursors.end().max(&range.end))
            .any(|sel| cursors.contains(&sel.cursor(text)) || cuts(sel.from()) || cuts(sel.to()))
    }
}

/// What is concealed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConcealOptions {
    /// Whether text is shown as the symbol it stands for, like Typst's `alpha` as `α`.
    pub symbols: bool,
    /// How Markdown is drawn rendered, or `None` to leave it as it is.
    pub markdown: Option<MarkdownRender>,
}

/// When the cursors of the focused view reveal a conceal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reveal {
    /// As `[editor.conceal] reveal` says.
    Configured,
    /// While a cursor is on its line, like rendered Markdown.
    Line,
}

/// A conceal and when it is revealed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cached {
    conceal: Conceal,
    reveal: Reveal,
}

/// Rendered Markdown drawn over the text of a line rather than instead of it, unless a cursor
/// reveals the line: the chars `range` drawn as `style` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcealDecoration {
    pub range: ops::Range<usize>,
    pub style: DecorationStyle,
}

/// How a [`ConcealDecoration::Text`] is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationStyle {
    /// Dimmed and struck through, like the text of a checked task.
    Done,
    /// In the color of a theme scope, like the title of a callout.
    Scope(&'static str),
}

/// What the Markdown of a window of lines is rendered as.
#[derive(Debug, Default)]
struct Rendered {
    conceals: Vec<Cached>,
    /// The decorations and their lines.
    decorations: Vec<(usize, ConcealDecoration)>,
}

/// The conceals of the lines of a syntax tree, computed as they are needed. It has to be cleared
/// whenever the tree changes.
#[derive(Debug, Default)]
pub struct ConcealCache {
    /// The conceals of each line computed so far, sorted and disjoint.
    lines: HashMap<usize, Vec<Cached>>,
    /// The decorations of the lines in `lines` that have any.
    decorations: HashMap<usize, Vec<ConcealDecoration>>,
    /// What was concealed in `lines`.
    options: ConcealOptions,
}

impl ConcealCache {
    pub fn clear(&mut self) {
        self.lines.clear();
        self.decorations.clear();
    }

    /// The window of lines around `line` whose conceals are computed together, leaving out lines
    /// already known.
    fn window(&self, line: usize, len_lines: usize) -> ops::Range<usize> {
        let unknown = |line: &usize| !self.lines.contains_key(line);
        let before = (line.saturating_sub(FETCHED_LINES / 4)..line).rev();
        let first = before.take_while(unknown).last().unwrap_or(line);
        let after = (line + 1..len_lines).take(FETCHED_LINES - (line + 1 - first));
        let end = after.take_while(unknown).last().unwrap_or(line) + 1;
        first..end
    }
}

/// The conceals that a syntax tree's `conceals.scm` queries capture, fetched as the document
/// formatter reaches them.
pub struct SyntaxConceals<'a> {
    text: RopeSlice<'a>,
    syntax: &'a Syntax,
    loader: Arc<Loader>,
    /// The selection whose cursors reveal conceals and how, or `None` to reveal nothing.
    reveal: Option<(&'a Selection, ConcealReveal)>,
    options: ConcealOptions,
    cache: &'a RefCell<ConcealCache>,
}

impl<'a> SyntaxConceals<'a> {
    /// The conceals of `syntax`, a tree of `text`, that `options` asks for, kept in `cache`, or
    /// `None` if nothing in it can be concealed.
    pub fn new(
        text: RopeSlice<'a>,
        syntax: &'a Syntax,
        loader: Arc<Loader>,
        reveal: Option<(&'a Selection, ConcealReveal)>,
        mut options: ConcealOptions,
        cache: &'a RefCell<ConcealCache>,
    ) -> Option<Self> {
        let root = syntax.root_language();
        // Markdown injected into other languages, like their doc comments, is rendered only if
        // `injected` is set
        options.markdown = options
            .markdown
            .filter(|markdown| markdown.injected || loader.conceals_markdown(root));
        let conceals_root =
            loader.conceal_query(root).is_some() && (options.symbols || options.markdown.is_some());
        let renders_injected = options.markdown.is_some_and(|markdown| markdown.injected);
        if !conceals_root && !renders_injected {
            return None;
        }
        let mut cache_mut = cache.borrow_mut();
        if cache_mut.options != options {
            cache_mut.clear();
            cache_mut.options = options;
        }
        drop(cache_mut);
        Some(Self {
            text,
            syntax,
            loader,
            reveal,
            options,
            cache,
        })
    }

    /// Computes the conceals and decorations of the lines `lines` into `cache`.
    fn fetch(&self, cache: &mut ConcealCache, lines: ops::Range<usize>) {
        let text = self.text;
        let end = if lines.end < text.len_lines() {
            text.line_to_char(lines.end)
        } else {
            text.len_chars()
        };
        for line in lines.clone() {
            cache.lines.insert(line, Vec::new());
            cache.decorations.remove(&line);
        }
        // what a match renders on lines it reaches outside the window, like the title of a
        // callout above it, is left to the window of those lines
        let rendered = self.render(text.line_to_char(lines.start)..end);
        for cached in rendered.conceals {
            let line = text.char_to_line(cached.conceal.start);
            if lines.contains(&line) {
                cache.lines.entry(line).or_default().push(cached);
            }
        }
        for (line, decoration) in rendered.decorations {
            if lines.contains(&line) {
                cache.decorations.entry(line).or_default().push(decoration);
            }
        }
    }

    /// The conceals of the chars `range`, which covers whole lines, sorted and disjoint, and the
    /// decorations of rendered Markdown.
    fn render(&self, range: ops::Range<usize>) -> Rendered {
        let text = self.text;
        let bytes = text.char_to_byte(range.start) as u32..text.char_to_byte(range.end) as u32;
        let mut noconceal = Vec::new();
        let mut conceals = Vec::new();
        let mut decorations = Vec::new();
        for mat in self.syntax.conceal_matches(text, &self.loader, bytes) {
            let bytes = mat.bytes.start as usize..mat.bytes.end as usize;
            let Some(table) = mat.table else {
                noconceal.push(text.byte_to_char(bytes.start)..text.byte_to_char(bytes.end));
                continue;
            };
            if let SymbolTable::Markdown(element) = table {
                if let Some(options) = self.options.markdown {
                    let mut renderer =
                        markdown::Renderer::new(text, options, &mut conceals, &mut decorations);
                    renderer.render(element, &mat.node, bytes);
                }
                continue;
            }
            if !self.options.symbols {
                continue;
            }
            if bytes.is_empty() || bytes.len() > table.max_bytes() {
                continue;
            }
            let concealed: Cow<str> = text.byte_slice(bytes.clone()).into();
            if concealed.contains(char_is_line_ending) {
                continue;
            }
            let Some(replacement) =
                table
                    .resolve(&concealed)
                    .filter(|replacement| match replacement {
                        Replacement::Text(text) => is_visible(text),
                        Replacement::Fill(g) => is_visible(g),
                        Replacement::Hidden => true,
                    })
            else {
                continue;
            };
            let start = text.byte_to_char(bytes.start);
            conceals.push(Cached {
                conceal: Conceal::new(start, text.byte_to_char(bytes.end), replacement),
                reveal: Reveal::Configured,
            });
        }

        merge_regions(&mut noconceal);
        let in_noconceal = |conceal: &Conceal| {
            let i = noconceal.partition_point(|region| region.start <= conceal.start);
            i.checked_sub(1)
                .is_some_and(|i| conceal.end <= noconceal[i].end)
        };
        conceals.retain(|cached| !in_noconceal(&cached.conceal));

        conceals.sort_unstable_by_key(|cached| (cached.conceal.start, Reverse(cached.conceal.end)));
        let mut outermost: Vec<Cached> = Vec::with_capacity(conceals.len());
        for cached in conceals {
            if outermost
                .last()
                .is_none_or(|last| last.conceal.end <= cached.conceal.start)
            {
                outermost.push(cached);
            }
        }
        Rendered {
            conceals: outermost,
            decorations,
        }
    }

    /// The decorations of the lines `lines` that the cursors don't reveal, and their lines.
    pub fn decorations(&self, lines: ops::Range<usize>) -> Vec<(usize, ConcealDecoration)> {
        let text = self.text;
        let lines = lines.start.min(text.len_lines())..lines.end.min(text.len_lines());
        let mut cache = self.cache.borrow_mut();
        let mut decorations = Vec::new();
        for line in lines {
            if !cache.lines.contains_key(&line) {
                let window = cache.window(line, text.len_lines());
                self.fetch(&mut cache, window);
            }
            let Some(line_decorations) = cache.decorations.get(&line) else {
                continue;
            };
            let line_start = text.line_to_char(line);
            let revealed = self.reveal.is_some_and(|(selection, _)| {
                ConcealReveal::Line.reveals(text, selection, line_start..line_start + 1)
            });
            if !revealed {
                decorations.extend(
                    line_decorations
                        .iter()
                        .map(|decoration| (line, decoration.clone())),
                );
            }
        }
        decorations
    }

    /// Whether the cursors show the concealed text of `cached` as it is.
    fn reveals(&self, cached: &Cached) -> bool {
        let Some((selection, configured)) = self.reveal else {
            return false;
        };
        let text = self.text;
        let range = cached.conceal.start..cached.conceal.end;
        match cached.reveal {
            Reveal::Configured => configured.reveals(text, selection, range),
            Reveal::Line => ConcealReveal::Line.reveals(text, selection, range),
        }
    }
}

impl ConcealSource for SyntaxConceals<'_> {
    fn conceals_from(&self, char_idx: usize, conceals: &mut Vec<Conceal>) -> usize {
        let text = self.text;
        let first_line = text.char_to_line(char_idx.min(text.len_chars()));
        let mut cache = self.cache.borrow_mut();
        if !cache.lines.contains_key(&first_line) {
            let lines = cache.window(first_line, text.len_lines());
            self.fetch(&mut cache, lines);
        }

        // hand out the known lines that follow, except what the cursors reveal
        let mut line = first_line;
        while let Some(line_conceals) = cache.lines.get(&line) {
            let shown = line_conceals
                .iter()
                .filter(|cached| cached.conceal.start >= char_idx && !self.reveals(cached))
                .map(|cached| cached.conceal.clone());
            conceals.extend(shown);
            line += 1;
            if line == first_line + SERVED_LINES {
                break;
            }
        }
        // the last line has no line break, so its end is past its last char
        if line < text.len_lines() {
            text.line_to_char(line)
        } else {
            text.len_chars() + 1
        }
    }
}

/// Whether `symbol` draws something: it is neither whitespace nor without width.
fn is_visible(symbol: &str) -> bool {
    !symbol.chars().all(char::is_whitespace) && symbol.width() > 0
}

#[cfg(test)]
mod test {
    use once_cell::sync::Lazy;

    use super::*;
    use crate::{Range, Rope};

    static LOADER: Lazy<Arc<Loader>> = Lazy::new(|| Arc::new(crate::config::default_lang_loader()));

    const OPTIONS: ConcealOptions = ConcealOptions {
        symbols: true,
        markdown: Some(MarkdownRender {
            icons: true,
            injected: true,
        }),
    };

    fn parse(language: &str, text: &Rope) -> Syntax {
        let language = LOADER.language_for_name(language).unwrap();
        Syntax::new(text.slice(..), language, &LOADER).unwrap()
    }

    /// Renders `source` with its conceals, except those `selection` reveals.
    fn render(language: &str, source: &str, reveal: Option<(&Selection, ConcealReveal)>) -> String {
        render_with(language, source, reveal, OPTIONS)
    }

    /// Renders `source` with the conceals `options` asks for, except those `selection` reveals.
    fn render_with(
        language: &str,
        source: &str,
        reveal: Option<(&Selection, ConcealReveal)>,
        options: ConcealOptions,
    ) -> String {
        let text = Rope::from(source);
        let slice = text.slice(..);
        let syntax = parse(language, &text);
        let cache = RefCell::default();
        let conceals =
            SyntaxConceals::new(slice, &syntax, LOADER.clone(), reveal, options, &cache).unwrap();
        let mut rendered = String::new();
        let mut pos = 0;
        let mut fetched = 0;
        while fetched <= text.len_chars() {
            let mut batch = Vec::new();
            let end = conceals.conceals_from(fetched, &mut batch);
            for conceal in batch {
                rendered.extend(slice.slice(pos..conceal.start).chars());
                match conceal.replacement {
                    Replacement::Text(text) => rendered.push_str(&text),
                    Replacement::Hidden => {}
                    Replacement::Fill(g) => rendered.push_str(&format!("{g}…")),
                }
                pos = conceal.end;
            }
            fetched = end;
        }
        rendered.extend(slice.slice(pos..).chars());
        rendered
    }

    fn typst(source: &str) -> String {
        render("typst", source, None)
    }

    fn markdown(source: &str) -> String {
        render("markdown", source, None)
    }

    #[test]
    fn markdown_inline_markup() {
        assert_eq!(
            markdown("A **bold**, *italic*, _under_ and ~~gone~~ `code` or ``a`b``\n"),
            "A bold, italic, under and gone code or a`b\n"
        );
        assert_eq!(
            markdown("&copy; 2026 &mdash; A &rarr; B &#8594; &#x2192; &nope; &Tab;\n"),
            "© 2026 — A → B → → &nope; &Tab;\n"
        );
        // only numeric footnote references
        assert_eq!(
            markdown("As shown[^1] before[^note] and[^12].\n"),
            "As shown¹ before[^note] and¹².\n"
        );
    }

    #[test]
    fn markdown_links() {
        assert_eq!(
            markdown(
                "See [the docs](https://example.com \"Docs\") and [ref][example], [it][], \
                 [top](#top), <https://x.org>, <me@x.org> and ![Diagram](img/arch.png).\n\n\
                 [example]: https://example.com\n"
            ),
            "See \u{f059f} the docs and \u{f0339} ref, \u{f0339} it, \u{f0339} top, \
             \u{f059f} https://x.org, \u{f0013} me@x.org and \u{f0976} Diagram.\n\n\
             [example]: https://example.com\n"
        );
        // the text of a link is rendered too, and links without text are left as they are
        assert_eq!(
            markdown("[**bold** link](https://x.org) [](https://x.org)\n"),
            "\u{f059f} bold link [](https://x.org)\n"
        );
        assert_eq!(
            markdown("See [[Kampagne]] and [[004 - K|the cast]] or [[S#B]], not [a].\n"),
            "See \u{f15d6} Kampagne and \u{f15d6} the cast or \u{f15d6} S#B, not [a].\n"
        );
        // without icons
        let options = ConcealOptions {
            markdown: Some(MarkdownRender {
                icons: false,
                injected: true,
            }),
            ..OPTIONS
        };
        assert_eq!(
            render_with(
                "markdown",
                "[docs](https://x.org) [[Note|alias]]\n",
                None,
                options
            ),
            "docs alias\n"
        );
    }

    #[test]
    fn markdown_rules() {
        assert_eq!(markdown("a\n\n---\n\n***\nb\n"), "a\n\n─…\n\n─…\nb\n");
        // frontmatter and setext underlines are no rules
        let source = "---\ntitle: x\n---\n\nTitle\n---\n";
        assert_eq!(markdown(source), source);
    }

    #[test]
    fn markdown_headings() {
        // icons right-aligned over the `#`s, closing sequences hidden
        assert_eq!(
            markdown("# H1\n### H3\n###### Six\n## Closed ##\n#\n"),
            "\u{f0ca1} H1\n  \u{f0ca5} H3\n     \u{f0cab} Six\n \u{f0ca3} Closed\n\u{f0ca1}\n"
        );
        // a `#` in the text stays
        assert_eq!(markdown("# C# rocks\n"), "\u{f0ca1} C# rocks\n");
    }

    #[test]
    fn markdown_lists() {
        assert_eq!(markdown("- a\n* b\n  + c\n"), "• a\n• b\n  • c\n");
        // a marker indented more than its list needs
        assert_eq!(markdown("* a\n    * b\n"), "• a\n    • b\n");
        // numbers counted along the list unless written out
        assert_eq!(
            markdown("1. a\n1. b\n1. c\n\ntext\n\n5. x\n7. y\n\ntext\n\n1) z\n"),
            "1. a\n2. b\n3. c\n\ntext\n\n5. x\n7. y\n\ntext\n\n1. z\n"
        );
        assert_eq!(
            markdown("- [ ] todo\n- [x] done\n- [-] doing\n- plain\n"),
            "\u{f0131}  todo\n\u{f0c52}  done\n\u{f0954}  doing\n• plain\n"
        );
    }

    #[test]
    fn markdown_quotes() {
        assert_eq!(
            markdown("> a\n> b\n> > c\n> - d\n>   e\n"),
            "▎ a\n▎ b\n▎ ▎ c\n▎ • d\n▎   e\n"
        );
        assert_eq!(
            markdown("> [!NOTE]\n> body\n\n> [!warning] Custom\n> x\n\n> [!nope]\n"),
            "▎ \u{f02fd} Note\n▎ body\n\n▎ \u{f002a} Custom\n▎ x\n\n▎ [!nope]\n"
        );
    }

    #[test]
    fn markdown_decorations() {
        let source = "# Title\n- [x] done\n  more\n> [!TIP] Hint\n";
        let text = Rope::from(source);
        let syntax = parse("markdown", &text);
        let cache = RefCell::default();
        let conceals = SyntaxConceals::new(
            text.slice(..),
            &syntax,
            LOADER.clone(),
            None,
            OPTIONS,
            &cache,
        )
        .unwrap();
        let decoration = |range, style| ConcealDecoration { range, style };
        assert_eq!(
            conceals.decorations(0..text.len_lines()),
            [
                (1, decoration(14..18, DecorationStyle::Done)),
                (2, decoration(21..25, DecorationStyle::Done)),
                (3, decoration(35..39, DecorationStyle::Scope("diff.plus"))),
            ]
        );
        // the cursor's line has none
        let selection = Selection::point(9);
        let reveal = Some((&selection, ConcealReveal::Adjacent));
        let conceals = SyntaxConceals::new(
            text.slice(..),
            &syntax,
            LOADER.clone(),
            reveal,
            OPTIONS,
            &cache,
        )
        .unwrap();
        assert_eq!(conceals.decorations(1..2), []);
    }

    #[test]
    fn markdown_reveal() {
        let source = "A **bold** word\nand *more*\n";
        let selection = Selection::point(3);
        let reveal = Some((&selection, ConcealReveal::Adjacent));
        // the cursor's line is revealed, whatever `reveal` is
        assert_eq!(
            render("markdown", source, reveal),
            "A **bold** word\nand more\n"
        );
    }

    #[test]
    fn markdown_injected() {
        let source = "/// See [docs](https://x.org).\nfn f() {}\n";
        assert_eq!(
            render("rust", source, None),
            "/// See \u{f059f} docs.\nfn f() {}\n"
        );
        // only Markdown documents are rendered unless injected Markdown is
        let options = ConcealOptions {
            markdown: Some(MarkdownRender {
                icons: true,
                injected: false,
            }),
            ..OPTIONS
        };
        let text = Rope::from(source);
        let syntax = parse("rust", &text);
        let cache = RefCell::default();
        let conceals = SyntaxConceals::new(
            text.slice(..),
            &syntax,
            LOADER.clone(),
            None,
            options,
            &cache,
        );
        assert!(conceals.is_none());
        assert_eq!(
            render_with("markdown", "[docs](https://x.org)\n", None, options),
            "\u{f059f} docs\n"
        );
    }

    #[test]
    fn typst_math_symbols() {
        assert_eq!(typst("$2 alpha^2$"), "$2 α^2$");
        // modifiers in any order, like Typst
        assert_eq!(
            typst("$arrow.r.long arrow.long.r eq.not RR dot$"),
            "$⟶   ⟶   ≠ ℝ ⋅$"
        );
        // symbols in attachments, fractions, roots, primes, factorials and arguments
        assert_eq!(
            typst("$x_alpha^beta 1/gamma √delta pi' theta! vec(1, tau)$"),
            "$x_α^β 1/γ √δ π' θ! vec(1, τ)$"
        );
        // prose around math, called symbols like accents, functions and ops stay
        assert_eq!(
            typst("$A dot B$ and $x in RR$ at $arrow(n)_alpha dot(x) sqrt(beta) sin(theta)$"),
            "$A ⋅ B$ and $x ∈ ℝ$ at $arrow(n)_α dot(x) sqrt(β) sin(θ)$"
        );
        assert_eq!(
            typst("$f'(x) = (dif f)/(dif x) quad lim_(n -> oo)$"),
            "$f'(x) = (dif f)/(dif x) quad lim_(n → ∞)$"
        );
        // submodules
        assert_eq!(typst("$gender.female control.nul$"), "$♀\u{FE0E} ␀$");
        // a period without a modifier after it is punctuation
        assert_eq!(typst("$alpha.$"), "$α.$");
        // named arguments are not symbols
        assert_eq!(
            typst("$mat(delim: \"[\", alpha)$"),
            "$mat(delim: \"[\", α)$"
        );
    }

    #[test]
    fn typst_unconcealed() {
        // unknown modifiers, invisible symbols, strings, comments and raw text
        for source in [
            "$alpha.foo arrow.foo$",
            "$space.quad zws$",
            "$\"alpha\"$ // alpha\n`alpha`",
            // code after `#` in math
            "$#alpha #x$",
            // after an unclosed `$` the rest is a syntax error
            "Text $alpha beta\n\nSome prose in and dot here.\n",
        ] {
            assert_eq!(typst(source), source);
        }
    }

    #[test]
    fn typst_shorthands() {
        assert_eq!(
            typst("$a -> b != c <= d ... e := f$"),
            "$a → b ≠ c ≤ d … e ≔ f$"
        );
        assert_eq!(typst("$||x|| [|x|]$"), "$‖x‖ ⟦x⟧$");
        // shorthands the grammar splits, like `|=>` into `|` and `=>`, but not across spaces
        assert_eq!(
            typst("$a ==> b |=> c <~~ d >-> e <-< f$"),
            "$a ⟹   b ⤇ c ⬳   d ↣ e ↢ f$"
        );
        assert_eq!(typst("$a = => b$"), "$a = ⇒ b$");
        // markup, where `~` is a non-breaking space
        assert_eq!(typst("a -- b --- c... d~e"), "a – b — c… d~e");
    }

    #[test]
    fn typst_long_arrows() {
        // long arrows take three columns, however they are written
        assert_eq!(
            typst("$arrow.r.double.long$ $a --> b$ #sym.arrow.l.long"),
            "$⟹  $ $a ⟶   b$ ⟵  "
        );
        assert_eq!(typst("$arrow.r$ $a |-> b$"), "$→$ $a ↦ b$");
    }

    #[test]
    fn typst_code_symbols() {
        // the `#` is hidden with the symbol
        assert_eq!(typst("Done. #sym.qed #emoji.face"), "Done. ∎ 😀");
        assert_eq!(typst("#let a = sym.arrow.r.long"), "#let a = ⟶  ");
        assert_eq!(typst("$#sym.alpha + 1$"), "$α + 1$");
        // other fields are not symbols
        assert_eq!(typst("#calc.pi #page.width"), "#calc.pi #page.width");
    }

    #[test]
    fn typst_wikilinks() {
        assert_eq!(
            typst("See [[Functor\\#Laws|functors]] and [[Natural Isomorphism]]."),
            "See Functor#Laws|functors and Natural Isomorphism."
        );
        // the `!` of an embed belongs to the text before it
        assert_eq!(typst("![[img.png|300]]"), "!img.png|300");
        assert_eq!(typst("- a [[x]]\n= Head [[y]]"), "- a x\n= Head y");
        // not in raw text or math, and not single or empty brackets
        assert_eq!(typst("`[[raw]]` $[[m]]$"), "`[[raw]]` $[[m]]$");
        assert_eq!(typst("[x] [[]]"), "[x] [[]]");
    }

    #[test]
    fn typst_rules() {
        assert_eq!(typst("a\n#line(length: 100%)\nb"), "a\n─…\nb");
        assert_eq!(typst("#line(length:100%)"), "─…");
        // other lines are left as they are
        assert_eq!(typst("#line(length: 50%)"), "#line(length: 50%)");
        assert_eq!(typst("#line()"), "#line()");
    }

    fn cursors(ranges: &[(usize, usize)]) -> Selection {
        let ranges = ranges
            .iter()
            .map(|&(anchor, head)| Range::new(anchor, head));
        Selection::new(ranges.collect(), 0)
    }

    #[test]
    fn reveal() {
        let text = Rope::from("x $2 alpha^2$\n$beta$\n");
        let text = text.slice(..);
        // `alpha` is concealed from 5 to 10
        let revealed_at = |reveal: ConcealReveal, cursor| {
            let selection = cursors(&[(cursor, cursor + 1)]);
            reveal.reveals(text, &selection, 5..10)
        };
        let revealing = |reveal| {
            (0..text.len_chars())
                .filter(|&cursor| revealed_at(reveal, cursor))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            revealing(ConcealReveal::Adjacent),
            (4..=10).collect::<Vec<_>>()
        );
        assert_eq!(
            revealing(ConcealReveal::Symbol),
            (5..=9).collect::<Vec<_>>()
        );
        assert_eq!(revealing(ConcealReveal::Line), (0..=13).collect::<Vec<_>>());

        // a cursor on the previous line's line break is not next to a conceal starting a line
        assert!(!ConcealReveal::Adjacent.reveals(text, &cursors(&[(13, 14)]), 14..15));
        assert!(ConcealReveal::Adjacent.reveals(text, &cursors(&[(14, 15)]), 15..19));

        // a selection that covers a conceal leaves it, one that cuts it reveals it
        let select = |anchor, head| cursors(&[(anchor, head)]);
        assert!(!ConcealReveal::Symbol.reveals(text, &select(2, 13), 5..10));
        assert!(ConcealReveal::Symbol.reveals(text, &select(7, 13), 5..10));
        assert!(ConcealReveal::Symbol.reveals(text, &select(13, 7), 5..10));
        // every cursor counts
        let many = cursors(&[(0, 1), (6, 7), (15, 16)]);
        assert!(ConcealReveal::Symbol.reveals(text, &many, 5..10));
        assert!(ConcealReveal::Symbol.reveals(text, &many, 15..19));
        assert!(!ConcealReveal::Symbol.reveals(text, &many, 2..3));
    }

    #[test]
    fn cache_windows() {
        let mut cache = ConcealCache::default();
        // a quarter before the line and the rest after it, within the text
        assert_eq!(cache.window(100, 1000), 68..196);
        assert_eq!(cache.window(10, 1000), 0..128);
        assert_eq!(cache.window(990, 1000), 958..1000);
        // leaving out the lines already known
        cache.lines.insert(90, Vec::new());
        cache.lines.insert(150, Vec::new());
        assert_eq!(cache.window(100, 1000), 91..150);
    }

    #[test]
    fn cache_is_shared() {
        let text = Rope::from("$alpha beta$");
        let slice = text.slice(..);
        let syntax = parse("typst", &text);
        let cache = RefCell::default();
        // the first cursor reveals `alpha`, the second `beta`, from the same cache
        for (cursor, shown) in [(2, [(7, "β")]), (8, [(1, "α")])] {
            let selection = Selection::point(cursor);
            let reveal = Some((&selection, ConcealReveal::Symbol));
            let conceals =
                SyntaxConceals::new(slice, &syntax, LOADER.clone(), reveal, OPTIONS, &cache)
                    .unwrap();
            let mut fetched = Vec::new();
            conceals.conceals_from(0, &mut fetched);
            let fetched: Vec<_> = fetched
                .into_iter()
                .map(|c| (c.start, c.replacement))
                .collect();
            let shown = shown.map(|(start, symbol)| (start, Replacement::from(symbol)));
            assert_eq!(fetched, shown);
        }
        assert_eq!(cache.borrow().lines.len(), 1);
    }

    #[test]
    fn typst_reveal() {
        let source = "$2 alpha^2 beta$";
        let at = |cursor: usize, reveal| {
            let selection = cursors(&[(cursor, cursor + 1)]);
            render("typst", source, Some((&selection, reveal)))
        };
        assert_eq!(at(0, ConcealReveal::Adjacent), "$2 α^2 β$");
        assert_eq!(at(2, ConcealReveal::Adjacent), "$2 alpha^2 β$");
        assert_eq!(at(8, ConcealReveal::Adjacent), "$2 alpha^2 β$");
        assert_eq!(at(9, ConcealReveal::Adjacent), "$2 α^2 β$");
        assert_eq!(at(8, ConcealReveal::Symbol), "$2 α^2 β$");
        assert_eq!(at(0, ConcealReveal::Line), source);
    }
}
