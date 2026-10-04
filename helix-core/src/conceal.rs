//! Concealing: document text shown as the symbol it stands for, like `α` for `alpha` in Typst
//! math.
//!
//! A language's `conceals.scm` query captures the text to conceal, and a [`SymbolTable`] resolves
//! it. [`SyntaxConceals`] hands the conceals of a syntax tree to the document formatter, except
//! the ones the cursors reveal.

mod typst;

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
use crate::text_annotations::{Conceal, ConcealSource};
use crate::{RopeSlice, Selection};

/// The longest text, in bytes, that is looked up in a [`SymbolTable`].
const MAX_CONCEALED_BYTES: usize = 64;

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
}

impl SymbolTable {
    /// The symbol that `text` stands for.
    pub fn resolve(self, text: &str) -> Option<&'static str> {
        match self {
            Self::TypstMath => typst::math_symbol(text),
            Self::TypstCode => typst::code_symbol(text),
            Self::TypstShorthand => typst::shorthand(text),
        }
    }
}

impl FromStr for SymbolTable {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "typst-math" => Ok(Self::TypstMath),
            "typst-code" => Ok(Self::TypstCode),
            "typst-shorthand" => Ok(Self::TypstShorthand),
            _ => Err(format!("unknown symbol table '{name}'")),
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

/// The conceals of the lines of a syntax tree, computed as they are needed. It has to be cleared
/// whenever the tree changes.
#[derive(Debug, Default)]
pub struct ConcealCache {
    /// The conceals of each line computed so far, sorted and disjoint.
    lines: HashMap<usize, Vec<Conceal>>,
}

impl ConcealCache {
    pub fn clear(&mut self) {
        self.lines.clear();
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
    cache: &'a RefCell<ConcealCache>,
}

impl<'a> SyntaxConceals<'a> {
    /// The conceals of `syntax`, a tree of `text`, kept in `cache`, or `None` if its language has no
    /// conceals.
    pub fn new(
        text: RopeSlice<'a>,
        syntax: &'a Syntax,
        loader: Arc<Loader>,
        reveal: Option<(&'a Selection, ConcealReveal)>,
        cache: &'a RefCell<ConcealCache>,
    ) -> Option<Self> {
        loader.conceal_query(syntax.root_language())?;
        Some(Self {
            text,
            syntax,
            loader,
            reveal,
            cache,
        })
    }

    /// Computes the conceals of the lines `lines` into `cache`.
    fn fetch(&self, cache: &mut ConcealCache, lines: ops::Range<usize>) {
        let text = self.text;
        let end = if lines.end < text.len_lines() {
            text.line_to_char(lines.end)
        } else {
            text.len_chars()
        };
        for line in lines.clone() {
            cache.lines.insert(line, Vec::new());
        }
        for conceal in self.conceals(text.line_to_char(lines.start)..end) {
            let line = text.char_to_line(conceal.start);
            cache.lines.entry(line).or_default().push(conceal);
        }
    }

    /// The conceals of the chars `range`, which covers whole lines, sorted and disjoint.
    fn conceals(&self, range: ops::Range<usize>) -> Vec<Conceal> {
        let text = self.text;
        let bytes = text.char_to_byte(range.start) as u32..text.char_to_byte(range.end) as u32;
        let mut noconceal = Vec::new();
        let mut conceals = Vec::new();
        for mat in self.syntax.conceal_matches(text, &self.loader, bytes) {
            let bytes = mat.bytes.start as usize..mat.bytes.end as usize;
            let Some(table) = mat.table else {
                noconceal.push(text.byte_to_char(bytes.start)..text.byte_to_char(bytes.end));
                continue;
            };
            if bytes.is_empty() || bytes.len() > MAX_CONCEALED_BYTES {
                continue;
            }
            let concealed: Cow<str> = text.byte_slice(bytes.clone()).into();
            if concealed.contains(char_is_line_ending) {
                continue;
            }
            let Some(replacement) = table
                .resolve(&concealed)
                .filter(|symbol| is_visible(symbol))
            else {
                continue;
            };
            let start = text.byte_to_char(bytes.start);
            conceals.push(Conceal::new(
                start,
                text.byte_to_char(bytes.end),
                replacement,
            ));
        }

        merge_regions(&mut noconceal);
        let in_noconceal = |conceal: &Conceal| {
            let i = noconceal.partition_point(|region| region.start <= conceal.start);
            i.checked_sub(1)
                .is_some_and(|i| conceal.end <= noconceal[i].end)
        };
        conceals.retain(|conceal| !in_noconceal(conceal));

        conceals.sort_unstable_by_key(|conceal| (conceal.start, Reverse(conceal.end)));
        let mut outermost: Vec<Conceal> = Vec::with_capacity(conceals.len());
        for conceal in conceals {
            if outermost
                .last()
                .is_none_or(|last| last.end <= conceal.start)
            {
                outermost.push(conceal);
            }
        }
        outermost
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
        let revealed = |conceal: &Conceal| {
            self.reveal.is_some_and(|(selection, reveal)| {
                reveal.reveals(text, selection, conceal.start..conceal.end)
            })
        };
        let mut line = first_line;
        while let Some(line_conceals) = cache.lines.get(&line) {
            let shown = line_conceals
                .iter()
                .filter(|conceal| conceal.start >= char_idx && !revealed(conceal));
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

    fn parse(language: &str, text: &Rope) -> Syntax {
        let language = LOADER.language_for_name(language).unwrap();
        Syntax::new(text.slice(..), language, &LOADER).unwrap()
    }

    /// Renders `source` with its conceals, except those `selection` reveals.
    fn render(language: &str, source: &str, reveal: Option<(&Selection, ConcealReveal)>) -> String {
        let text = Rope::from(source);
        let slice = text.slice(..);
        let syntax = parse(language, &text);
        let cache = RefCell::default();
        let conceals = SyntaxConceals::new(slice, &syntax, LOADER.clone(), reveal, &cache).unwrap();
        let mut rendered = String::new();
        let mut pos = 0;
        let mut fetched = 0;
        while fetched <= text.len_chars() {
            let mut batch = Vec::new();
            let end = conceals.conceals_from(fetched, &mut batch);
            for conceal in batch {
                rendered.extend(slice.slice(pos..conceal.start).chars());
                rendered.push_str(conceal.replacement);
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

    #[test]
    fn typst_math_symbols() {
        assert_eq!(typst("$2 alpha^2$"), "$2 α^2$");
        // modifiers in any order, like Typst
        assert_eq!(
            typst("$arrow.r.long arrow.long.r eq.not RR dot$"),
            "$⟶ ⟶ ≠ ℝ ⋅$"
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
        // markup, where `~` is a non-breaking space
        assert_eq!(typst("a -- b --- c... d~e"), "a – b — c… d~e");
    }

    #[test]
    fn typst_code_symbols() {
        // the `#` is hidden with the symbol
        assert_eq!(typst("Done. #sym.qed #emoji.face"), "Done. ∎ 😀");
        assert_eq!(typst("#let a = sym.arrow.r.long"), "#let a = ⟶");
        assert_eq!(typst("$#sym.alpha + 1$"), "$α + 1$");
        // other fields are not symbols
        assert_eq!(typst("#calc.pi #page.width"), "#calc.pi #page.width");
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
                SyntaxConceals::new(slice, &syntax, LOADER.clone(), reveal, &cache).unwrap();
            let mut fetched = Vec::new();
            conceals.conceals_from(0, &mut fetched);
            let fetched: Vec<_> = fetched.iter().map(|c| (c.start, c.replacement)).collect();
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
