//! Code folding: text hidden behind a placeholder at the end of its first line.
//!
//! A [`Fold`] hides everything from the line break of its first line, the *header*, up to the
//! point where the header's visual row continues. The
//! [`DocumentFormatter`](crate::doc_formatter::DocumentFormatter) draws the hidden text as a
//! single placeholder grapheme, the *fold cell*:
//!
//! ```text
//! fn new() -> Self {                fn new() -> Self {…}
//!     Self { pos: 0 }
//! }
//! ```
//!
//! A closing bracket that starts the last line and closes a bracket of the header line is pulled
//! up onto the row. Otherwise the fold hides the rest of the region, up to the end of its last
//! line's content. The hidden lines after the header form the fold's *interior*. Folds chain, so
//! one visual row can span several document lines: `if a {…} else {…}`.

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::ops;

use helix_stdx::rope::RopeSliceExt;

use crate::graphemes::next_grapheme_boundary;
use crate::line_ending::line_end_char_index;
use crate::syntax::{Loader, Syntax};
use crate::tree_sitter::Node;
use crate::{Assoc, ChangeSet, Range, RopeSlice, Selection};

/// A region of text hidden behind the fold cell at the end of its header line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    /// The char index of the header line's line break, where the fold cell is drawn.
    pub start: usize,
    /// The char index where the header's visual row continues after the hidden text.
    pub end: usize,
    /// Whether `end` is a closing bracket pulled up from the last hidden line. Edits keep such an
    /// end on the first non-whitespace char of its line.
    pub pulled_up: bool,
}

impl Fold {
    /// Returns the fold hiding the region `region` (char indices) behind its first line, or
    /// `None` if the region has no content after its first line.
    ///
    /// `closer` is the char index of a closing bracket that starts the region's last line and
    /// closes a bracket opened on its first line. It is pulled up onto the header's row.
    pub fn from_region(
        text: RopeSlice,
        region: ops::Range<usize>,
        closer: Option<usize>,
    ) -> Option<Fold> {
        let last = last_non_whitespace(text, region.clone())?;
        let header = text.char_to_line(region.start);
        if text.char_to_line(last) == header {
            return None;
        }
        let (end, pulled_up) = match closer {
            Some(closer) => (closer, true),
            None => (next_grapheme_boundary(text, last), false),
        };
        let fold = Fold {
            start: line_end_char_index(&text, header),
            end,
            pulled_up,
        };
        last_non_whitespace(text, fold.interior(text)).map(|_| fold)
    }

    /// The line the fold is drawn on.
    pub fn header_line(&self, text: RopeSlice) -> usize {
        text.char_to_line(self.start)
    }

    /// The line on which the header's row continues after the fold.
    pub fn last_line(&self, text: RopeSlice) -> usize {
        text.char_to_line(self.end)
    }

    /// The hidden text after the header's line break.
    pub fn interior(&self, text: RopeSlice) -> ops::Range<usize> {
        text.line_to_char(self.header_line(text) + 1)..self.end
    }

    /// Whether `range` covers part of the interior, but not all of it.
    fn cuts(&self, text: RopeSlice, range: &Range) -> bool {
        let interior = self.interior(text);
        let intersects = range.from() < interior.end && interior.start < range.to();
        let covers = range.from() <= interior.start && interior.end <= range.to();
        intersects && !covers
    }

    /// Returns the fold after an edit moved its positions, or `None` if it no longer hides a
    /// header line's line break and some text after it.
    fn revalidate(mut self, text: RopeSlice) -> Option<Fold> {
        if self.start >= self.end || self.end > text.len_chars() {
            return None;
        }
        let header = self.header_line(text);
        if header + 1 >= text.len_lines() || line_end_char_index(&text, header) != self.start {
            return None;
        }
        if self.pulled_up {
            let line = self.last_line(text);
            let first_char = text.line(line).first_non_whitespace_char();
            match first_char.map(|col| text.line_to_char(line) + col) {
                Some(closer) if is_closing_bracket(text.char(closer)) => self.end = closer,
                _ => self.pulled_up = false,
            }
        }
        (text.line_to_char(header + 1) < self.end).then_some(self)
    }

    fn key(&self) -> (usize, Reverse<usize>) {
        (self.start, Reverse(self.end))
    }
}

/// What toggling a fold at a selection does, see [`Folds::toggle_target`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Open(Fold),
    Close(Fold),
}

/// Whether `ch` closes a bracket that folds pull up onto the header's row.
pub fn is_closing_bracket(ch: char) -> bool {
    matches!(ch, ')' | ']' | '}')
}

fn last_non_whitespace(text: RopeSlice, range: ops::Range<usize>) -> Option<usize> {
    let start = range.start;
    text.slice(range)
        .last_non_whitespace_char()
        .map(|i| start + i)
}

/// Sorts `folds` outermost first and drops duplicates and folds that cross another fold (overlap
/// without nesting). Of two crossing folds the preferred one is kept, or else the earlier one.
fn nest(mut folds: Vec<(Fold, bool)>) -> Vec<Fold> {
    folds.sort_by_key(|&(fold, preferred)| (fold.key(), !preferred));
    folds.dedup_by_key(|(fold, _)| fold.key());

    let mut nested: Vec<Option<(Fold, bool)>> = Vec::with_capacity(folds.len());
    // indices into `nested` of the folds enclosing the current one, innermost last
    let mut enclosing: Vec<usize> = Vec::new();
    'folds: for (fold, preferred) in folds {
        while let Some(&parent) = enclosing.last() {
            let (parent_fold, parent_preferred) = nested[parent].expect("enclosing folds are kept");
            if parent_fold.end <= fold.start {
                enclosing.pop();
            } else if parent_fold.end < fold.end {
                // `fold` starts inside `parent_fold` but ends after it
                if !preferred || parent_preferred {
                    continue 'folds;
                }
                nested[parent] = None;
                enclosing.pop();
            } else {
                break;
            }
        }
        enclosing.push(nested.len());
        nested.push(Some((fold, preferred)));
    }
    nested.into_iter().flatten().map(|(fold, _)| fold).collect()
}

/// The folds of a document in one view.
///
/// Closed folds are nested or disjoint. A fold inside another closed fold stays closed when the
/// outer one is opened.
#[derive(Debug, Clone, Default)]
pub struct Folds {
    /// All closed folds, outermost first.
    closed: Vec<Fold>,
    /// The closed folds that are not inside another closed fold, which is what is hidden.
    outermost: Vec<Fold>,
    /// Changes whenever the folds do, so that views can tell that the layout changed.
    revision: usize,
    /// Rows of the outermost folds for converting between lines and rows, built on demand.
    rows: OnceCell<Vec<FoldRows>>,
}

#[derive(Debug, Clone, Copy)]
struct FoldRows {
    header: usize,
    last: usize,
    /// The number of lines merged into earlier rows by this fold and the ones before it.
    merged: usize,
}

impl Folds {
    /// All closed folds, sorted by start and outermost first.
    pub fn closed(&self) -> &[Fold] {
        &self.closed
    }

    /// The closed folds that are not inside another closed fold, sorted and disjoint.
    pub fn outermost(&self) -> &[Fold] {
        &self.outermost
    }

    pub fn is_empty(&self) -> bool {
        self.closed.is_empty()
    }

    /// A number that changes whenever the folds change.
    pub fn revision(&self) -> usize {
        self.revision
    }

    /// Closes `folds`. A closed fold that crosses a new one is opened.
    pub fn close(&mut self, folds: impl IntoIterator<Item = Fold>) {
        let closed = self.closed.iter().map(|&fold| (fold, false));
        let new = folds.into_iter().map(|fold| (fold, true));
        self.closed = nest(closed.chain(new).collect());
        self.changed();
    }

    /// Opens `folds`, leaving the closed folds inside them closed.
    pub fn open(&mut self, folds: &[Fold]) {
        self.closed.retain(|fold| {
            !folds
                .iter()
                .any(|open| (open.start, open.end) == (fold.start, fold.end))
        });
        self.changed();
    }

    /// Opens `folds` and every closed fold inside them.
    pub fn open_recursive(&mut self, folds: &[Fold]) {
        self.closed.retain(|fold| {
            !folds
                .iter()
                .any(|open| open.start <= fold.start && fold.end <= open.end)
        });
        self.changed();
    }

    /// Opens every fold.
    pub fn clear(&mut self) {
        self.closed.clear();
        self.changed();
    }

    fn changed(&mut self) {
        self.outermost.clear();
        for &fold in &self.closed {
            if self
                .outermost
                .last()
                .is_none_or(|last| last.end <= fold.start)
            {
                self.outermost.push(fold);
            }
        }
        self.rows.take();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Maps the folds through `changes`, which turned the text into `text`. Folds touched by a
    /// change that no longer hide text behind a header line are dropped.
    pub fn map(&mut self, text: RopeSlice, changes: &ChangeSet) {
        if self.closed.is_empty() || changes.is_empty() {
            return;
        }

        let changed: Vec<ops::RangeInclusive<usize>> = changes
            .changes_iter()
            .map(|(from, to, _)| from..=to)
            .collect();
        let touched = |pos: usize| {
            let i = changed.partition_point(|range| *range.end() < pos);
            changed.get(i).is_some_and(|range| range.contains(&pos))
        };
        let touched: Vec<bool> = self
            .closed
            .iter()
            .map(|fold| touched(fold.start) || touched(fold.end))
            .collect();

        changes.update_positions(
            self.closed
                .iter_mut()
                .map(|fold| (&mut fold.start, Assoc::After)),
        );
        changes.update_positions(
            self.closed
                .iter_mut()
                .map(|fold| (&mut fold.end, Assoc::Before)),
        );

        let any_touched = touched.contains(&true);
        let mapped = self
            .closed
            .drain(..)
            .zip(touched)
            .filter_map(|(fold, touched)| {
                if touched {
                    fold.revalidate(text)
                } else {
                    Some(fold)
                }
            });
        let mapped: Vec<Fold> = mapped.collect();
        // touched folds may have collapsed onto or across others
        self.closed = if any_touched {
            nest(mapped.into_iter().map(|fold| (fold, false)).collect())
        } else {
            mapped
        };
        self.changed();
    }

    /// Opens the folds that `selection` would otherwise partly hide: a range may not cover only a
    /// part of a fold's interior, and its cursor may not lie inside an interior. Returns whether
    /// any fold was opened.
    pub fn reveal(&mut self, text: RopeSlice, selection: &Selection) -> bool {
        let mut revealed = false;
        loop {
            let mut open = Vec::new();
            for range in selection {
                let cursor = range.cursor(text);
                let from = range.from().min(cursor);
                let to = range.to().max(cursor + 1);
                let first = self.outermost.partition_point(|fold| fold.end <= from);
                for fold in self.outermost[first..]
                    .iter()
                    .take_while(|fold| fold.start < to)
                {
                    if fold.cuts(text, range) || fold.interior(text).contains(&cursor) {
                        open.push(*fold);
                    }
                }
            }
            if open.is_empty() {
                return revealed;
            }
            self.open(&open);
            revealed = true;
        }
    }

    /// Moves the parts of `selection` that lie inside a fold's interior out of it: cursors go to
    /// the fold cell and anchors grow the range over the whole fold.
    pub fn snap(&self, text: RopeSlice, selection: Selection) -> Selection {
        selection.transform(|range| {
            let hidden_in = |pos: usize| {
                self.fold_at(pos)
                    .filter(|fold| fold.interior(text).contains(&pos))
            };
            if range.is_empty() {
                return hidden_in(range.head).map_or(range, |fold| Range::point(fold.start));
            }
            let mut anchor = range.anchor;
            if let Some(fold) = hidden_in(anchor) {
                anchor = if range.head < range.anchor {
                    fold.end
                } else {
                    fold.start
                };
            }
            let range = Range::new(anchor, range.head);
            match hidden_in(range.cursor(text)) {
                Some(fold) => range.put_cursor(text, fold.start, true),
                None => range,
            }
        })
    }

    /// Widens every range that covers exactly a fold cell to the text hidden behind it.
    pub fn widen_cells(&self, text: RopeSlice, selection: Selection) -> Selection {
        selection.transform(|range| match self.fold_at(range.from()) {
            Some(fold)
                if range.from() == fold.start
                    && range.to() == next_grapheme_boundary(text, fold.start) =>
            {
                Range::new(fold.start, fold.end).with_direction(range.direction())
            }
            _ => range,
        })
    }

    /// The outermost fold whose cell or hidden text covers `char_idx`.
    pub fn fold_at(&self, char_idx: usize) -> Option<&Fold> {
        fold_at(&self.outermost, char_idx)
    }

    /// What toggling a fold at `range` does, given the foldable regions around its lines:
    ///
    /// 1. Open the closed fold whose header is the cursor's line.
    /// 2. Else, for a range on a single line, close the innermost region starting on it.
    /// 3. Else open the closed fold whose row continues on the cursor's line.
    /// 4. Else close the innermost region containing the lines of the range.
    ///
    /// So on `} else {` of `if a {…} else {`, toggling folds the `else` block.
    pub fn toggle_target(
        &self,
        text: RopeSlice,
        range: &Range,
        regions: impl FnOnce() -> Vec<Fold>,
    ) -> Option<Toggle> {
        let line = range.cursor_line(text);
        let first_line = row_start_line(&self.outermost, text, line);
        let row = text.line_to_char(first_line)
            ..=line_end_char_index(&text, row_end_line(&self.outermost, text, first_line));
        let first = self
            .outermost
            .partition_point(|fold| fold.start < *row.start());
        let mut on_row = self.outermost[first..]
            .iter()
            .take_while(|fold| row.contains(&fold.start));
        if let Some(fold) = on_row.clone().find(|fold| fold.header_line(text) == line) {
            return Some(Toggle::Open(*fold));
        }

        let regions = regions();
        let innermost = |candidate: &dyn Fn(&Fold) -> bool| {
            regions
                .iter()
                .filter(|region| candidate(region))
                .min_by_key(|region| region.end - region.start)
                .copied()
        };
        let (from_line, to_line) = range.line_range(text);
        if from_line == to_line {
            if let Some(region) = innermost(&|region| region.header_line(text) == line) {
                return Some(Toggle::Close(region));
            }
        }
        if let Some(fold) = on_row.find(|fold| fold.last_line(text) == line) {
            return Some(Toggle::Open(*fold));
        }
        innermost(&|region| {
            region.header_line(text) <= from_line && to_line <= region.last_line(text)
        })
        .map(Toggle::Close)
    }

    /// Toggles a fold at every range of `selection`, see [`Folds::toggle_target`]. `recursive`
    /// also opens the closed folds inside an opened fold, or closes the regions inside a closed
    /// one. Returns `selection` moved out of the new folds.
    pub fn toggle(
        &mut self,
        text: RopeSlice,
        syntax: &Syntax,
        loader: &Loader,
        selection: Selection,
        recursive: bool,
    ) -> Selection {
        let mut open = Vec::new();
        let mut close = Vec::new();
        for range in &selection {
            let (from_line, to_line) = range.line_range(text);
            let lines = text.line_to_char(from_line)..text.line_to_char(to_line + 1);
            let region =
                match self.toggle_target(text, range, || regions(syntax, text, loader, lines)) {
                    Some(Toggle::Open(fold)) => {
                        open.push(fold);
                        continue;
                    }
                    Some(Toggle::Close(region)) => region,
                    None => continue,
                };
            close.push(region);
            if recursive {
                let inner = regions(syntax, text, loader, region.start..region.end);
                close.extend(
                    inner
                        .into_iter()
                        .filter(|inner| region.start <= inner.start && inner.end <= region.end),
                );
            }
        }
        if recursive {
            self.open_recursive(&open);
        } else {
            self.open(&open);
        }
        self.close(close);
        self.snap(text, selection)
    }

    /// Closes every foldable region of the text. Returns `selection` moved out of the folds.
    pub fn close_all(
        &mut self,
        text: RopeSlice,
        syntax: &Syntax,
        loader: &Loader,
        selection: Selection,
    ) -> Selection {
        self.close(regions(syntax, text, loader, 0..text.len_chars()));
        self.snap(text, selection)
    }

    /// The index of the row that `line` is on, where the lines a closed fold joins into one
    /// row count as one line.
    pub fn row(&self, text: RopeSlice, line: usize) -> usize {
        let rows = self.rows(text);
        let i = rows.partition_point(|fold| fold.last <= line);
        let merged = i.checked_sub(1).map_or(0, |i| rows[i].merged);
        match rows.get(i) {
            // `line` is hidden inside the fold, on the header's row
            Some(fold) if fold.header < line => fold.header - merged,
            _ => line - merged,
        }
    }

    /// The first line of the row with index `row`, see [`Folds::row`].
    pub fn row_start(&self, text: RopeSlice, row: usize) -> usize {
        let rows = self.rows(text);
        // folds whose header row comes before `row`
        let i = rows.partition_point(|fold| {
            let merged_before = fold.merged - (fold.last - fold.header);
            fold.header - merged_before < row
        });
        let merged = i.checked_sub(1).map_or(0, |i| rows[i].merged);
        (row + merged).min(text.len_lines() - 1)
    }

    fn rows(&self, text: RopeSlice) -> &[FoldRows] {
        self.rows.get_or_init(|| {
            let mut merged = 0;
            self.outermost
                .iter()
                .map(|fold| {
                    let header = fold.header_line(text);
                    let last = fold.last_line(text);
                    merged += last - header;
                    FoldRows {
                        header,
                        last,
                        merged,
                    }
                })
                .collect()
        })
    }
}

/// The foldable regions of `syntax` that intersect the chars `range`, outermost first.
pub fn regions(
    syntax: &Syntax,
    text: RopeSlice,
    loader: &Loader,
    range: ops::Range<usize>,
) -> Vec<Fold> {
    let bytes = text.char_to_byte(range.start) as u32..text.char_to_byte(range.end) as u32;
    let regions = syntax
        .fold_regions(text, loader, bytes)
        .filter_map(|(first, last)| {
            let region = text.byte_to_char(first.start_byte() as usize)
                ..text.byte_to_char(content_end(text, &last) as usize);
            let closer = pulled_up_closer(text, &last, region.clone());
            Fold::from_region(text, region, closer)
        });
    nest(regions.map(|fold| (fold, false)).collect())
}

/// The end of `node` without the comments that trail it, except those indented deeper than its
/// first line. Tree-sitter puts comments in the innermost node that is open before them, so a
/// block of an indentation-based language ends with the comments that precede the next, less
/// indented line.
fn content_end(text: RopeSlice, node: &Node) -> u32 {
    if node.is_extra() {
        return node.end_byte();
    }
    let column = |byte: u32| {
        let char_idx = text.byte_to_char(byte as usize);
        char_idx - text.line_to_char(text.char_to_line(char_idx))
    };
    let header = text.byte_to_line(node.start_byte() as usize);
    let indent = text.line(header).first_non_whitespace_char().unwrap_or(0);

    let mut node = node.clone();
    loop {
        let (mut last, mut last_content) = (None, None);
        for child in node.children() {
            if !child.is_extra() || column(child.start_byte()) > indent {
                last_content = Some(child.clone());
            }
            last = Some(child);
        }
        // a node can end with text of its own after its last child
        match (last, last_content) {
            (Some(last), Some(content)) if last.end_byte() == node.end_byte() => node = content,
            _ => return node.end_byte(),
        }
    }
}

/// The closing bracket that starts the last line of `region` and closes a bracket opened on its
/// first line, according to the tree of `last`, the region's last node.
fn pulled_up_closer(text: RopeSlice, last: &Node, region: ops::Range<usize>) -> Option<usize> {
    let last_char = last_non_whitespace(text, region.clone())?;
    let line = text.char_to_line(last_char);
    let closer = text.line_to_char(line) + text.line(line).first_non_whitespace_char()?;
    if !is_closing_bracket(text.char(closer)) {
        return None;
    }
    let byte = text.char_to_byte(closer) as u32;
    let opener = matching_opener(text, &last.descendant_for_byte_range(byte, byte + 1)?)?;
    let opener_line = text.byte_to_line(opener.start_byte() as usize);
    (opener_line == text.char_to_line(region.start)).then_some(closer)
}

/// The bracket that `closer`, a closing bracket token, closes among its siblings. Some grammars
/// wrap bracket tokens in named nodes like `block_end`, so brackets are recognized by their text.
fn matching_opener<'tree>(text: RopeSlice, closer: &Node<'tree>) -> Option<Node<'tree>> {
    let bracket =
        |node: &Node| (node.byte_range().len() == 1).then(|| text.byte(node.start_byte() as usize));
    let mut closer = closer.clone();
    while let Some(parent) = closer
        .parent()
        .filter(|parent| parent.byte_range() == closer.byte_range())
    {
        closer = parent;
    }
    let close = bracket(&closer)?;
    let open = match close {
        b')' => b'(',
        b']' => b'[',
        b'}' => b'{',
        _ => return None,
    };
    let mut open_brackets = Vec::new();
    for sibling in closer.parent()?.children() {
        if sibling.byte_range() == closer.byte_range() {
            return open_brackets.pop();
        }
        match bracket(&sibling) {
            Some(byte) if byte == open => open_brackets.push(sibling),
            Some(byte) if byte == close => {
                open_brackets.pop();
            }
            _ => (),
        }
    }
    None
}

/// The fold of the sorted, disjoint `folds` whose cell or hidden text covers `char_idx`.
pub fn fold_at(folds: &[Fold], char_idx: usize) -> Option<&Fold> {
    let i = folds.partition_point(|fold| fold.start <= char_idx);
    folds[..i].last().filter(|fold| char_idx < fold.end)
}

/// The first line of the visual row that `line` is on, given the outermost `folds`.
pub fn row_start_line(folds: &[Fold], text: RopeSlice, mut line: usize) -> usize {
    loop {
        let line_start = text.line_to_char(line);
        let i = folds.partition_point(|fold| fold.start < line_start);
        match folds[..i].last() {
            // the fold hides the start of the line or its row continues right at it
            Some(fold) if line_start <= fold.end => line = fold.header_line(text),
            _ => return line,
        }
    }
}

/// The last line of the visual row starting on `line`, given the outermost `folds`.
pub fn row_end_line(folds: &[Fold], text: RopeSlice, mut line: usize) -> usize {
    loop {
        let line_end = line_end_char_index(&text, line);
        match folds.binary_search_by_key(&line_end, |fold| fold.start) {
            Ok(i) => line = folds[i].last_line(text),
            Err(_) => return line,
        }
    }
}

/// The first and the last line of the visual row that `line` is on, given the outermost `folds`.
pub fn row_lines(folds: &[Fold], text: RopeSlice, line: usize) -> (usize, usize) {
    let first = row_start_line(folds, text, line);
    (first, row_end_line(folds, text, first))
}

/// The first and the last line of the visual rows that `range` covers, given the outermost
/// `folds`, like [`Range::line_range`] does for lines.
pub fn row_line_range(folds: &[Fold], text: RopeSlice, range: &Range) -> (usize, usize) {
    let (from, to) = range.line_range(text);
    (
        row_start_line(folds, text, from),
        row_lines(folds, text, to).1,
    )
}

/// The first line of the visual row `count` rows below the row of `line`, or of the last row.
pub fn next_row(folds: &[Fold], text: RopeSlice, line: usize, count: usize) -> usize {
    let last_line = text.len_lines() - 1;
    let mut line = row_start_line(folds, text, line);
    for _ in 0..count {
        let next = row_end_line(folds, text, line) + 1;
        if next > last_line {
            break;
        }
        line = next;
    }
    line
}

/// The first line of the visual row `count` rows above the row of `line`, or of the first row.
pub fn prev_row(folds: &[Fold], text: RopeSlice, line: usize, count: usize) -> usize {
    let mut line = row_start_line(folds, text, line);
    for _ in 0..count {
        if line == 0 {
            break;
        }
        line = row_start_line(folds, text, line - 1);
    }
    line
}

/// The parts of `range` that the outermost `folds` do not hide.
pub fn visible_ranges(
    folds: &[Fold],
    range: ops::Range<usize>,
) -> impl Iterator<Item = ops::Range<usize>> + '_ {
    let first = folds.partition_point(|fold| fold.end <= range.start);
    let mut start = range.start;
    let mut folds = folds[first..].iter();
    std::iter::from_fn(move || loop {
        if start >= range.end {
            return None;
        }
        match folds.next() {
            Some(fold) if fold.start < range.end => {
                let visible = start..fold.start.max(start);
                start = start.max(fold.end);
                if !visible.is_empty() {
                    return Some(visible);
                }
            }
            _ => {
                let visible = start..range.end;
                start = range.end;
                return Some(visible);
            }
        }
    })
}

#[cfg(test)]
mod test {
    use once_cell::sync::Lazy;

    use super::*;
    use crate::{Rope, Transaction};

    static LOADER: Lazy<Loader> = Lazy::new(crate::config::default_lang_loader);

    fn parse(language: &str, text: &Rope) -> Syntax {
        let language = LOADER.language_for_name(language).unwrap();
        Syntax::new(text.slice(..), language, &LOADER).unwrap()
    }

    /// Renders `source` with every foldable region of `language` closed.
    fn fold_all(language: &str, source: &str) -> String {
        let text = Rope::from(source);
        let syntax = parse(language, &text);
        let mut folds = Folds::default();
        folds.close_all(text.slice(..), &syntax, &LOADER, Selection::point(0));
        render(&text, &folds)
    }

    /// Renders the row of every foldable region of `language` in `source` with only that
    /// region closed.
    fn region_rows(language: &str, source: &str) -> Vec<String> {
        let text = Rope::from(source);
        let slice = text.slice(..);
        let syntax = parse(language, &text);
        regions(&syntax, slice, &LOADER, 0..text.len_chars())
            .into_iter()
            .map(|region| {
                let (first, last) = row_lines(&[region], slice, region.header_line(slice));
                let mut row: String = slice.slice(text.line_to_char(first)..region.start).into();
                row.push('…');
                row.extend(
                    slice
                        .slice(region.end..line_end_char_index(&slice, last))
                        .chars(),
                );
                row.trim().to_owned()
            })
            .collect()
    }

    #[test]
    fn rust_regions() {
        let source = "\
use std::io::{
    self,
    Read,
};

fn next(&mut self) -> Option<char> {
    if self.done() {
        None
    } else {
        self.src
            .chars()
            .next()
    }
}

fn f() {
    g(
        || {
            1
}) }
";
        assert_eq!(
            region_rows("rust", source),
            [
                "use std::io::{…};",
                "fn next(&mut self) -> Option<char> {…}",
                "if self.done() {…} else {",
                "} else {…}",
                // brackets pull up only when the tree pairs them: the closure's `}` does not
                // close the function's `{`
                "fn f() {…",
                "g(… }",
                "|| {…}) }",
            ]
        );
    }

    #[test]
    fn toggle() {
        let source = "impl A {\n    fn f() {\n        1\n    }\n}\n";
        let text = Rope::from(source);
        let slice = text.slice(..);
        let syntax = parse("rust", &text);
        let mut folds = Folds::default();
        let toggle = |folds: &mut Folds, pos, recursive| {
            let selection = folds.toggle(slice, &syntax, &LOADER, Selection::point(pos), recursive);
            (render(&text, folds), selection.primary().head)
        };
        // the cursor inside the body moves onto the fold cell
        assert_eq!(
            toggle(&mut folds, 30, false),
            ("impl A {\n    fn f() {…}\n}\n".into(), 21)
        );
        assert_eq!(toggle(&mut folds, 0, false), ("impl A {…}\n".into(), 0));
        // opening the impl leaves `fn f` closed
        assert_eq!(
            toggle(&mut folds, 0, false).0,
            "impl A {\n    fn f() {…}\n}\n"
        );
        // recursively closing the impl closes `fn f` too, and reopening it opens both
        assert_eq!(toggle(&mut folds, 0, true).0, "impl A {…}\n");
        assert_eq!(folds.closed().len(), 2);
        assert_eq!(toggle(&mut folds, 0, true).0, source);
    }

    #[test]
    fn trailing_comments() {
        // comments at the header's indentation or left of it stay visible
        assert_eq!(
            fold_all(
                "python",
                "def f():\n    x = 1\n    # todo\n\n# helpers\ndef g():\n    pass\n"
            ),
            "def f():…\n\n# helpers\ndef g():…\n"
        );
        assert_eq!(
            fold_all("yaml", "on:\n  push:\n    branches: [main]\n# about jobs\njobs:\n  test:\n    runs-on: ubuntu\n"),
            "on:…\n# about jobs\njobs:…\n"
        );
    }

    /// The fully folded rendering of a sample for every language whose `folds.scm` was reviewed.
    /// Haskell is left out: its grammar corrupts the heap for some inputs.
    #[test]
    fn fold_queries() {
        for (language, source, folded) in [
            (
                "rust",
                r##"use std::collections::HashMap;
use std::io::{
    self,
    Read,
};

impl Parser {
    fn next(&mut self) -> Option<char> {
        if self.done() {
            None
        } else {
            self.pos += 1;
            None
        }
    }
}

fn f() {
    g(
        || {
            1
}) }

fn main() {
    let total = items
        .iter()
        .sum::<u32>();
    let v = vec![
        1,
    ];
}
"##,
                r##"use std::collections::HashMap;…

impl Parser {…}

fn f() {…

fn main() {…}
"##,
            ),
            (
                "c",
                r##"#include <stdio.h>
#include <stdlib.h>

// Token kinds,
// in source order.
enum kind {
    KIND_A,
    KIND_B,
};

int main(void)
{
    switch (KIND_A) {
    case KIND_A:
        puts("a");
        break;
    }
    if (KIND_B > 1) {
        puts("b");
    } else {
        printf("%d\n",
               KIND_B);
    }
}
"##,
                r##"#include <stdio.h>…

// Token kinds,…
enum kind {…};

int main(void)…
"##,
            ),
            (
                "cpp",
                r##"#include <string>
#include <vector>

namespace text {
class Lexer;
}  // namespace text

class Lexer {
public:
    std::vector<std::string> run() {
        try {
            return split();
        } catch (const std::exception &e) {
            return {};
        }
    }
};

template <typename T>
T twice(T v) {
    return v + v;
}
"##,
                r##"#include <string>…

namespace text {…}  // namespace text

class Lexer {…};

template <typename T>
T twice(T v) {…}
"##,
            ),
            (
                "go",
                r##"package lexer
import (
	"fmt"
)

// Token is a lexed token.
// It carries its kind.
type Token struct {
	Kind string
}

func Count(n int) error {
	if n > 10 {
		return fmt.Errorf("too many: %d", n)
	} else {
		fmt.Println(n)
	}
	return nil
}

func (t Token) Describe(
	prefix string,
) string {
	return prefix + t.Kind
}
"##,
                r##"package lexer
import (…)

// Token is a lexed token.…
type Token struct {…}

func Count(n int) error {…}

func (t Token) Describe(…
"##,
            ),
            (
                "java",
                r##"import java.util.List;
import java.util.Map;

/**
 * Splits text into tokens.
 */
@SuppressWarnings({
    "unchecked",
})
public class Lexer {
    int count(List<String> tokens) {
        switch (tokens.size()) {
            case 0:
                return 0;
        }
        try {
            return tokens.size();
        } finally {
            tokens.forEach(t -> {
                System.out.println(t);
            });
        }
    }
}
"##,
                r##"import java.util.List;…

/**…
@SuppressWarnings({…})
public class Lexer {…}
"##,
            ),
            (
                "zig",
                r##"const std = @import("std");

/// A lexed token.
/// Carries its kind.
pub const Token = struct {
    kind: u8,
};

const help =
    \\usage: lex [file]
    \\  counts tokens
;

pub fn count(n: usize) !usize {
    if (n > 10) {
        return error.TooMany;
    } else {
        std.debug.print("{d}\n", .{
            n,
        });
    }
    return n;
}
"##,
                r##"const std = @import("std");

/// A lexed token.…
pub const Token = struct {…};

const help =
    \\usage: lex [file]…
;

pub fn count(n: usize) !usize {…}
"##,
            ),
            (
                "gdscript",
                r##"extends Node

## The player.
## Counts coins.

enum State {
	IDLE,
	RUNNING,
}

var coins := 0:
	set(value):
		coins = max(value, 0)

func collect(item: String) -> int:
	if item == "coin":
		coins += 1
	elif item == "gem":
		coins += 5
	else:
		pass
	match item:
		"coin":
			return 1
	return 0
"##,
                r##"extends Node

## The player.…

enum State {…}

var coins := 0:…

func collect(item: String) -> int:…
"##,
            ),
            (
                "python",
                r##"import os
import sys

# Two
# comments
def f(
    a,
):
    """Doc.
    """
    return a


if a:
    x = [
        1,
    ]
elif b:
    pass
else:
    call(
        1,
    )
"##,
                r##"import os…

# Two…
def f(…


if a:…
elif b:…
else:…
"##,
            ),
            (
                "bash",
                r##"# Two
# comments
f() {
    echo hi
}
if [ -f x ]; then
    a
elif [ -d x ]; then
    b
else
    c
fi
case $1 in
    a)
        x
        ;;
esac
for i in 1 2; do
    echo "$i"
done
arr=(
    a
)
"##,
                r##"# Two…
f() {…}
if [ -f x ]; then…
case $1 in…
for i in 1 2; do…
arr=(…)
"##,
            ),
            (
                "lua",
                r##"-- Two
-- comments
local t = {
  a = 1,
}
local function f(x)
  return x
end
if a then
  b()
elseif c then
  d()
else
  e()
end
for i = 1, 3 do
  print(i)
end
call(
  1,
  2
)
local s = [[
text
]]
"##,
                r##"-- Two…
local t = {…}
local function f(x)…
if a then…
for i = 1, 3 do…
call(…)
local s = [[…
"##,
            ),
            (
                "scala",
                r##"import a.B
import c.D

// Two
// comments
object Main {
  def f(x: Int): Int = {
    x
  }
}

val xs = List(
  1,
)

if (a) {
  b
} else {
  c
}

def g(n: Int): Int =
  if n > 0 then
    1
  else
    0
"##,
                r##"import a.B…

// Two…
object Main {…}

val xs = List(…)

if (a) {…} else {…}

def g(n: Int): Int =…
"##,
            ),
            (
                "scheme",
                r##";; Two
;; comments
(define (f x)
  (let ((a 1)
        (b 2))
    (+ a b)))

(define (g n)
  (cond ((< n 0)
         'neg)
        (else
         'pos)))

(for-each
 (lambda (x)
   (display x))
 '(1
   2))
"##,
                r##";; Two…
(define (f x)…

(define (g n)…

(for-each…
"##,
            ),
            (
                "markdown",
                r##"# A

Text.

## B

- one
  - sub
- two

```sh
echo hi
```

## C

> quote
> more

# D

End.
"##,
                r##"# A…

# D…
"##,
            ),
            (
                "typst",
                r##"// Two
// comments
#import "a.typ": x
#import "b.typ": y

#let f(x) = {
  x
}

= A

#if c {
  [a]
} else {
  [b]
}

== B

#figure(
  image("a.png"),
)

= C
"##,
                r##"// Two…
#import "a.typ": x…

#let f(x) = {…}

= A…

= C
"##,
            ),
            (
                "javascript",
                r##"import fs from "fs";
import {
  join,
} from "path";

@sealed
class Store {
  total() {
    return 0;
  }
}

if (ready) {
  start();
} else {
  wait();
}

app.get("/", function (req, res) {
  res.send("ok");
});

const sql = `
  SELECT 1
`;
"##,
                r##"import fs from "fs";…

@sealed
class Store {…}

if (ready) {…} else {…}

app.get("/", function (req, res) {…});

const sql = `…;
"##,
            ),
            (
                "typescript",
                r##"interface Props {
  id: string;
}

type Shape =
  | { kind: "circle" }
  | { kind: "square" };

enum Color {
  Red,
  Green,
}

declare function load(
  id: string,
): Promise<void>;

export class Service
  extends Base
  implements OnInit
{
  run() {
    go();
  }
}
"##,
                r##"interface Props {…}

type Shape =…

enum Color {…}

declare function load(…): Promise<void>;

export class Service…
"##,
            ),
            (
                "jsx",
                r##"<ul className="list">
  {items.map((item) => (
    <li key={item.id}>{item.name}</li>
  ))}
</ul>;

<Footer
  count={items.length}
/>;

const App = () => (
  <main>
    <List />
  </main>
);
"##,
                r##"<ul className="list">…;

<Footer…;

const App = () => (…);
"##,
            ),
            (
                "tsx",
                r##"interface Props {
  users: User[];
}

export function Table({ users }: Props) {
  return <tbody>{users.length}</tbody>;
}

<Table
  users={[]}
/>;

<section>
  <h1>Users</h1>
</section>;
"##,
                r##"interface Props {…}

export function Table({ users }: Props) {…}

<Table…;

<section>…;
"##,
            ),
            (
                "json",
                r##"[
  {
    "name": "demo",
    "files": [
      "dist"
    ],
    "empty": {}
  },
  {
    "name": "other"
  }
]
"##,
                r##"[…]
"##,
            ),
            (
                "css",
                r##"@import url("a.css");
@import url("b.css");

h1,
h2 {
  margin: 0;
}

.card {
  font-family:
    system-ui,
    sans-serif;
}

@media (min-width: 600px) {
  .card {
    padding: 16px;
  }
}

@keyframes spin {
  from {
    opacity: 0;
  }
}
"##,
                r##"@import url("a.css");…

h1,…

.card {…}

@media (min-width: 600px) {…}

@keyframes spin {…}
"##,
            ),
            (
                "html",
                r##"<!--
  Header
-->
<nav class="top">
  <a href="/">Home</a>
</nav>
<img
  src="logo.png"
/>
<style>
  body {
    margin: 0;
  }
</style>
<script>
  go(function () {
    run();
  });
</script>
"##,
                r##"<!--…
<nav class="top">…
<img…
<style>…
<script>…
"##,
            ),
            (
                "toml",
                r##"# Manifest
# for demo.
[package]
name = "demo"
authors = [
  "Ada",
]

# Dependencies
[dependencies]
serde = "1"
tokio = "1"

[[bin]]
name = "demo"
path = "main.rs"
"##,
                r##"# Manifest…
[package]…

# Dependencies
[dependencies]…

[[bin]]…
"##,
            ),
            (
                "yaml",
                r##"# CI settings
# for demo.
name: ci
jobs:
  test:
    steps:
      - uses: checkout
      - name: Test
        run: |
          npm test
env:
  CI: true
"##,
                r##"# CI settings…
name: ci
jobs:…
env:…
"##,
            ),
        ] {
            assert_eq!(fold_all(language, source), folded, "{language}");
        }
    }

    /// Folds the region between the first `«` and the first `»` in `text`, which are removed.
    fn region(text: &str) -> (Rope, ops::Range<usize>) {
        let start = text.chars().position(|c| c == '«').unwrap();
        let end = text.chars().position(|c| c == '»').unwrap() - 1;
        (Rope::from(text.replace(['«', '»'], "")), start..end)
    }

    /// Renders `text` with `folds` closed, drawing fold cells as `…`.
    fn render(text: &Rope, folds: &Folds) -> String {
        let text = text.slice(..);
        let mut rendered = String::new();
        let mut pos = 0;
        for fold in folds.outermost() {
            rendered.extend(text.slice(pos..fold.start).chars());
            rendered.push('…');
            pos = fold.end;
        }
        rendered.extend(text.slice(pos..).chars());
        rendered
    }

    fn fold(text: &Rope, region: ops::Range<usize>) -> Fold {
        let text = text.slice(..);
        let last = last_non_whitespace(text, region.clone()).unwrap();
        let line = text.char_to_line(last);
        let first = text.line_to_char(line) + text.line(line).first_non_whitespace_char().unwrap();
        let closer = (first == last && is_closing_bracket(text.char(last))).then_some(first);
        Fold::from_region(text, region, closer).unwrap()
    }

    fn folded(text: &str) -> String {
        let (text, region) = region(text);
        let mut folds = Folds::default();
        folds.close([fold(&text, region)]);
        render(&text, &folds)
    }

    #[test]
    fn region_rule() {
        // a closing bracket starting the last line is pulled up
        assert_eq!(
            folded("«fn new() -> Self {\n    Self { pos: 0 }\n}»\n"),
            "fn new() -> Self {…}\n"
        );
        // anything after the region on its last line stays visible
        assert_eq!(
            folded("class Lexer «{\n  int pos;\n}»;\n"),
            "class Lexer {…};\n"
        );
        // without a closer the rest of the region is hidden
        assert_eq!(
            folded("«main = do\n  putStrLn \"hi\"\n  pure ()»\nnext\n"),
            "main = do…\nnext\n"
        );
        // trailing whitespace and blank lines inside the region are not hidden
        assert_eq!(
            folded("«def f():\n    pass\n\n\n»x = 1\n"),
            "def f():…\n\n\nx = 1\n"
        );
        // CRLF: the fold cell is the whole line break
        assert_eq!(folded("«fn f() {\r\n    1\r\n}»\r\n"), "fn f() {…}\r\n");
    }

    #[test]
    fn region_without_hidden_content() {
        let (text, range) = region("«fn f() {}»\n");
        assert_eq!(Fold::from_region(text.slice(..), range, None), None);
        let (text, range) = region("«fn f() {\n}»\n");
        let closer = Some(range.end - 1);
        assert_eq!(Fold::from_region(text.slice(..), range, closer), None);
        let (text, range) = region("«fn f() {\n    \n}»\n");
        let closer = Some(range.end - 1);
        assert_eq!(Fold::from_region(text.slice(..), range, closer), None);
    }

    fn if_else() -> (Rope, Fold, Fold) {
        let text = Rope::from("if a {\n    b\n} else {\n    c\n}\nd\n");
        let then = fold(&text, 5..14);
        let otherwise = fold(&text, 20..29);
        (text, then, otherwise)
    }

    #[test]
    fn chained_folds() {
        let (text, then, otherwise) = if_else();
        let mut folds = Folds::default();
        folds.close([then, otherwise]);
        assert_eq!(render(&text, &folds), "if a {…} else {…}\nd\n");

        let slice = text.slice(..);
        let outermost = folds.outermost();
        for line in 0..=4 {
            assert_eq!(row_start_line(outermost, slice, line), 0, "line {line}");
        }
        assert_eq!(row_end_line(outermost, slice, 0), 4);
        assert_eq!(next_row(outermost, slice, 2, 1), 5);
        assert_eq!(prev_row(outermost, slice, 5, 1), 0);
        assert_eq!(folds.row(slice, 5), 1);
        assert_eq!(folds.row_start(slice, 1), 5);
        let target = |folds: &Folds, pos| {
            folds.toggle_target(slice, &Range::point(pos), || vec![then, otherwise])
        };
        assert_eq!(
            target(&folds, 0),
            Some(Toggle::Open(then)),
            "the first header"
        );
        assert_eq!(
            target(&folds, 14),
            Some(Toggle::Open(otherwise)),
            "`}} else {{` heads the second fold"
        );
        assert_eq!(
            target(&folds, 29),
            Some(Toggle::Open(otherwise)),
            "the final `}}` continues the second fold"
        );
        // with only the first fold closed, `} else {` folds the second block
        folds.open(&[otherwise]);
        assert_eq!(target(&folds, 14), Some(Toggle::Close(otherwise)));
        assert_eq!(target(&folds, 0), Some(Toggle::Open(then)));
    }

    #[test]
    fn nesting() {
        let text = Rope::from("impl A {\n    fn f() {\n        1\n    }\n}\n");
        let outer = fold(&text, 0..40);
        let inner = fold(&text, 13..38);
        let mut folds = Folds::default();
        folds.close([inner, outer]);
        assert_eq!(folds.closed(), [outer, inner]);
        assert_eq!(render(&text, &folds), "impl A {…}\n");
        folds.open(&[outer]);
        assert_eq!(render(&text, &folds), "impl A {\n    fn f() {…}\n}\n");
        folds.close([outer]);
        folds.open_recursive(&[outer]);
        assert!(folds.is_empty());
    }

    #[test]
    fn crossing_folds() {
        let old = Fold {
            start: 2,
            end: 10,
            pulled_up: false,
        };
        let new = Fold {
            start: 5,
            end: 20,
            pulled_up: false,
        };
        let mut folds = Folds::default();
        folds.close([old]);
        folds.close([new]);
        assert_eq!(folds.closed(), [new], "the new fold wins");
        let crossing = vec![(old, false), (new, false)];
        assert_eq!(nest(crossing), [old], "the earlier fold wins");
    }

    fn edited(text: &mut Rope, folds: &mut Folds, changes: Vec<(usize, usize, Option<&str>)>) {
        let transaction = Transaction::change(
            text,
            changes
                .into_iter()
                .map(|(from, to, insert)| (from, to, insert.map(Into::into))),
        );
        transaction.apply(text);
        folds.map(text.slice(..), transaction.changes());
    }

    fn function() -> (Rope, Folds) {
        let text = Rope::from("fn f() {\n    1\n}\nx\n");
        let mut folds = Folds::default();
        folds.close([fold(&text, 0..16)]);
        (text, folds)
    }

    #[test]
    fn edits() {
        // typing at the end of the header or before the closer stays outside the fold
        let (mut text, mut folds) = function();
        edited(
            &mut text,
            &mut folds,
            vec![(8, 8, Some(" // f")), (15, 15, Some("a "))],
        );
        assert_eq!(render(&text, &folds), "fn f() { // f…a }\nx\n");

        // re-indenting the closer keeps it pulled up
        let (mut text, mut folds) = function();
        edited(&mut text, &mut folds, vec![(15, 15, Some("  "))]);
        assert_eq!(text, "fn f() {\n    1\n  }\nx\n");
        assert_eq!(render(&text, &folds), "fn f() {…}\nx\n");

        // an edit inside the fold keeps it
        let (mut text, mut folds) = function();
        edited(&mut text, &mut folds, vec![(13, 14, Some("2\n    3"))]);
        assert_eq!(render(&text, &folds), "fn f() {…}\nx\n");

        // deleting the header's line break drops the fold
        let (mut text, mut folds) = function();
        edited(&mut text, &mut folds, vec![(8, 9, None)]);
        assert!(folds.is_empty());

        // deleting the hidden text drops the fold
        let (mut text, mut folds) = function();
        edited(&mut text, &mut folds, vec![(8, 15, None)]);
        assert_eq!(text, "fn f() {}\nx\n");
        assert!(folds.is_empty());

        // deleting everything drops the fold
        let (mut text, mut folds) = function();
        edited(&mut text, &mut folds, vec![(0, 19, None)]);
        assert!(folds.is_empty());

        // removing the `\r` of a CRLF header break keeps the fold on the `\n`
        let mut text = Rope::from("fn f() {\r\n    1\r\n}\r\n");
        let mut folds = Folds::default();
        folds.close([fold(&text, 0..18)]);
        edited(&mut text, &mut folds, vec![(8, 9, None)]);
        assert_eq!(render(&text, &folds), "fn f() {…}\r\n");

        // typing `\r` right before a header's `\n` makes it a CRLF, which the fold must start at
        let (mut text, mut folds) = function();
        edited(&mut text, &mut folds, vec![(8, 8, Some("\r"))]);
        assert!(folds.is_empty());
    }

    #[test]
    fn reveal() {
        let (text, mut folds) = function();
        let text = text.slice(..);
        let reveals = |selection: Selection| {
            let mut folds = folds.clone();
            folds.reveal(text, &selection)
        };
        // on the fold cell
        assert!(!reveals(Selection::single(8, 9)));
        // before and after the fold
        assert!(!reveals(Selection::single(0, 3)));
        assert!(!reveals(Selection::single(15, 16)));
        // the cursor inside the fold
        assert!(reveals(Selection::point(10)));
        // a range covering the whole fold
        assert!(!reveals(Selection::single(0, 17)));
        // a range covering part of it
        assert!(reveals(Selection::single(0, 12)));
        // a range covering all of it, with the cursor on the last hidden char
        assert!(reveals(Selection::single(9, 15)));
        assert!(folds.reveal(text, &Selection::point(10)));
        assert!(folds.is_empty());
    }

    #[test]
    fn reveal_nested() {
        let text = Rope::from("impl A {\n    fn f() {\n        1\n    }\n    fn g() {}\n}\n");
        let slice = text.slice(..);
        let outer = fold(&text, 0..54);
        let inner = fold(&text, 13..38);
        let mut folds = Folds::default();
        folds.close([outer, inner]);
        // revealing `fn g` only opens the outer fold
        assert!(folds.reveal(slice, &Selection::point(45)));
        assert_eq!(folds.closed(), [inner]);
        // revealing the body of `fn f` opens both
        folds.close([outer]);
        assert!(folds.reveal(slice, &Selection::point(30)));
        assert!(folds.is_empty());
    }

    #[test]
    fn snap() {
        let (text, folds) = function();
        let text = text.slice(..);
        let snapped = |selection| folds.snap(text, selection);
        assert_eq!(snapped(Selection::point(10)), Selection::point(8));
        assert_eq!(snapped(Selection::single(2, 12)), Selection::single(2, 9));
        assert_eq!(snapped(Selection::single(12, 2)), Selection::single(15, 2));
        // a range covering the fold is unchanged
        assert_eq!(snapped(Selection::single(0, 17)), Selection::single(0, 17));
    }

    #[test]
    fn widen_cells() {
        let (text, folds) = function();
        let text = text.slice(..);
        assert_eq!(
            folds.widen_cells(text, Selection::single(8, 9)),
            Selection::single(8, 15)
        );
        assert_eq!(
            folds.widen_cells(text, Selection::single(7, 9)),
            Selection::single(7, 9)
        );
    }

    #[test]
    fn toggle_targets() {
        let text = Rope::from("impl A {\n    fn f() {\n        1\n    }\n}\n");
        let slice = text.slice(..);
        let outer = fold(&text, 0..40);
        let inner = fold(&text, 13..38);
        let regions = [outer, inner];
        let close = |range| match Folds::default().toggle_target(slice, &range, || regions.to_vec())
        {
            Some(Toggle::Close(region)) => Some(region),
            _ => None,
        };
        assert_eq!(close(Range::point(0)), Some(outer), "header of the impl");
        assert_eq!(close(Range::point(13)), Some(inner), "header of fn f");
        assert_eq!(close(Range::point(9)), Some(inner), "indentation of fn f");
        assert_eq!(close(Range::point(30)), Some(inner), "body of fn f");
        assert_eq!(
            close(Range::point(39)),
            Some(outer),
            "closing brace of the impl"
        );
        assert_eq!(close(Range::new(9, 38)), Some(inner), "all of fn f");
        assert_eq!(
            close(Range::new(0, 30)),
            Some(outer),
            "impl header to fn body"
        );
        assert_eq!(
            Folds::default().toggle_target(slice, &Range::point(0), Vec::new),
            None
        );
    }

    #[test]
    fn visible() {
        let (_, then, otherwise) = if_else();
        let folds = [then, otherwise];
        let ranges = |range| {
            visible_ranges(&folds, range)
                .map(|range| (range.start, range.end))
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges(0..33), [(0, 6), (13, 21), (28, 33)]);
        assert_eq!(ranges(8..16), [(13, 16)]);
        assert_eq!(ranges(8..10), []);
        assert_eq!(ranges(30..33), [(30, 33)]);
    }

    /// Checks the row helpers against rows computed from the rendered text.
    fn check_rows(text: &Rope, folds: &Folds) {
        let slice = text.slice(..);
        let outermost = folds.outermost();
        let mut row = 0;
        let mut row_starts = vec![0];
        for line in 1..text.len_lines() {
            let line_start = text.line_to_char(line);
            let continues = outermost
                .iter()
                .any(|fold| fold.start < line_start && line_start <= fold.end);
            if !continues {
                row += 1;
                row_starts.push(line);
            }
            assert_eq!(folds.row(slice, line), row, "row of line {line}");
            assert_eq!(
                row_start_line(outermost, slice, line),
                row_starts[row],
                "row start of line {line}"
            );
        }
        for (row, &line) in row_starts.iter().enumerate() {
            assert_eq!(folds.row_start(slice, row), line, "start of row {row}");
            let next = row_starts.get(row + 1).copied().unwrap_or(line);
            assert_eq!(next_row(outermost, slice, line, 1), next);
            let prev = row_starts[row.saturating_sub(1)];
            assert_eq!(prev_row(outermost, slice, line, 1), prev);
        }
    }

    /// Nested and chained folds over `lines` lines, derived from `seed`.
    fn random_folds(text: &Rope, seed: &[(u8, u8)]) -> Vec<Fold> {
        let lines = text.len_lines() - 1;
        seed.iter()
            .filter_map(|&(a, b)| {
                let a = a as usize % lines;
                let b = b as usize % lines;
                let (first, last) = (a.min(b), a.max(b));
                let region = text.line_to_char(first)..text.line_to_char(last) + 1;
                Fold::from_region(text.slice(..), region, None)
            })
            .collect()
    }

    fn lines_text(lines: usize) -> Rope {
        Rope::from(
            (0..lines)
                .map(|i| format!("line {i}\n"))
                .collect::<String>(),
        )
    }

    quickcheck::quickcheck! {
        fn rows_match_rendering(seed: Vec<(u8, u8)>) -> bool {
            let text = lines_text(40);
            let mut folds = Folds::default();
            folds.close(random_folds(&text, &seed));
            check_rows(&text, &folds);
            true
        }

        fn folds_stay_nested_through_edits(seed: Vec<(u8, u8)>, edits: Vec<(u8, u8, bool)>) -> bool {
            let mut text = lines_text(30);
            let mut folds = Folds::default();
            folds.close(random_folds(&text, &seed));
            for (a, b, insert) in edits {
                let len = text.len_chars();
                let a = a as usize * len / 256;
                let b = (b as usize * len / 256).max(a);
                let insert = insert.then_some("x\ny ");
                edited(&mut text, &mut folds, vec![(a, b, insert)]);
                let closed = folds.closed();
                for (i, fold) in closed.iter().enumerate() {
                    assert_eq!(fold.revalidate(text.slice(..)), Some(*fold));
                    for other in &closed[i + 1..] {
                        let nested = other.end <= fold.end;
                        let disjoint = fold.end <= other.start;
                        assert!(fold.start <= other.start && (nested || disjoint));
                    }
                }
                check_rows(&text, &folds);
            }
            true
        }

        fn reveal_leaves_nothing_hidden(seed: Vec<(u8, u8)>, ranges: Vec<(u8, u8)>) -> bool {
            let text = lines_text(30);
            let slice = text.slice(..);
            let mut folds = Folds::default();
            folds.close(random_folds(&text, &seed));
            let len = text.len_chars();
            let ranges: crate::SmallVec<[Range; 1]> = ranges
                .iter()
                .map(|&(a, b)| Range::new(a as usize * len / 256, b as usize * len / 256))
                .collect();
            if ranges.is_empty() {
                return true;
            }
            let selection = Selection::new(ranges, 0).ensure_invariants(slice);
            folds.reveal(slice, &selection);
            selection.iter().all(|range| {
                folds.outermost().iter().all(|fold| {
                    !fold.cuts(slice, range) && !fold.interior(slice).contains(&range.cursor(slice))
                })
            })
        }
    }
}
