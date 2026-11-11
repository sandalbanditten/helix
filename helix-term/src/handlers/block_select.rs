//! Block select: one range per line between the columns of two corners.

// Columns count the text, not the rows soft wrap makes of it, like `copy_selection_on_line`.
#![allow(deprecated)]

use std::borrow::Cow;

use helix_core::{
    graphemes::{grapheme_width, next_grapheme_boundary, prev_grapheme_boundary},
    line_ending::line_end_char_index,
    visual_coords_at_pos, Position, Range, RopeSlice, Selection, SmallVec,
};
use helix_event::register_hook;
use helix_stdx::rope::RopeSliceExt;
use helix_view::{
    current, current_ref, document::Mode, editor::BlockSelect, events::DocumentFocusLost,
    handlers::Handlers, Editor,
};

use crate::events::{OnModeSwitch, PostCommand};

pub(super) fn register_hooks(_handlers: &Handlers) {
    register_hook!(move |event: &mut PostCommand<'_, '_>| {
        let count = event.cx.count();
        follow(event.cx.editor, Some(event.command.name()), count);
        Ok(())
    });
    register_hook!(move |event: &mut OnModeSwitch<'_, '_>| {
        if event.new_mode != Mode::Select {
            event.cx.editor.block_select = None;
        }
        Ok(())
    });
    register_hook!(move |event: &mut DocumentFocusLost<'_>| {
        event.editor.block_select = None;
        Ok(())
    });
}

/// Selects the block from the anchor to the cursor of the primary range.
pub fn start(editor: &mut Editor) {
    let (view, doc) = current!(editor);
    let text = doc.text().slice(..);
    let range = doc.selection(view.id).primary();
    let anchor = if range.anchor <= range.head {
        range.anchor
    } else {
        prev_grapheme_boundary(text, range.anchor)
    };
    let at = |pos| visual_coords_at_pos(text, pos, doc.tab_width());
    let (anchor, head) = (at(anchor), at(range.cursor(text)));
    select(editor, anchor, head, false);
}

/// Selects the block from `anchor` to `head`.
pub fn select(editor: &mut Editor, anchor: Position, head: Position, ragged: bool) {
    let (view, doc) = current!(editor);
    let selection = block(doc.text().slice(..), anchor, head, doc.tab_width(), ragged);
    doc.set_selection(view.id, selection.clone());
    editor.block_select = Some(BlockSelect {
        anchor,
        head,
        ragged,
        selection,
        version: doc.version(),
    });
}

/// Moves the head after `command`, or after a change of the selection outside commands, like a
/// search. A change that doesn't keep the anchors ends block select.
pub fn follow(editor: &mut Editor, command: Option<&str>, count: usize) {
    let Some(block) = &editor.block_select else {
        return;
    };
    let (view, doc) = current_ref!(editor);
    let selection = doc.selection(view.id);
    if *selection == block.selection {
        return;
    }
    let Some(BlockSelect {
        mut anchor,
        mut head,
        mut ragged,
        selection: block,
        version,
    }) = editor.block_select.take()
    else {
        return;
    };
    let text = doc.text().slice(..);
    if editor.mode != Mode::Select || doc.version() != version {
        return;
    }
    let last_line = text.len_lines().saturating_sub(1);
    let last_line = match text.line(last_line).len_chars() {
        0 => last_line.saturating_sub(1),
        _ => last_line,
    };
    let cursor = visual_coords_at_pos(text, selection.primary().cursor(text), doc.tab_width());
    match command.unwrap_or_default() {
        "flip_selections" => {
            std::mem::swap(&mut anchor, &mut head);
            ragged = false;
        }
        _ if !keeps_anchors(text, selection, &block) => return,
        "move_line_down"
        | "extend_line_down"
        | "move_visual_line_down"
        | "extend_visual_line_down" => head.row = (head.row + count).min(last_line),
        "move_line_up" | "extend_line_up" | "move_visual_line_up" | "extend_visual_line_up" => {
            head.row = head.row.saturating_sub(count)
        }
        "goto_line_end"
        | "extend_to_line_end"
        | "goto_line_end_newline"
        | "extend_to_line_end_newline" => {
            head = cursor;
            ragged = true;
        }
        // Other vertical motions keep the column.
        _ if selection.primary().old_visual_position.is_some() => head.row = cursor.row,
        _ => {
            head = cursor;
            ragged = false;
        }
    }
    select(editor, anchor, head, ragged);
}

/// Whether the ranges of `selection` start from chars the ranges of `block` started from, as
/// after motions, which may merge ranges.
fn keeps_anchors(text: RopeSlice, selection: &Selection, block: &Selection) -> bool {
    let anchor = |range: &Range| {
        if range.anchor <= range.head {
            range.anchor
        } else {
            prev_grapheme_boundary(text, range.anchor)
        }
    };
    let anchors: Vec<_> = block.iter().map(anchor).collect();
    selection
        .iter()
        .all(|range| anchors.binary_search(&anchor(range)).is_ok())
}

/// The ranges of the lines from `anchor` to `head`, by line and visual column, but for lines too
/// short to reach the left column other than the head's. With `ragged`, they run to their ends.
pub fn block(
    text: RopeSlice,
    anchor: Position,
    head: Position,
    tab_width: usize,
    ragged: bool,
) -> Selection {
    let line_end = line_end_char_index(&text, head.row);
    let head_col = if line_end == text.line_to_char(head.row) {
        0
    } else {
        let last = visual_coords_at_pos(text, prev_grapheme_boundary(text, line_end), tab_width);
        if ragged {
            last.col
        } else {
            head.col.min(last.col)
        }
    };
    let forward = ragged || head_col >= anchor.col;
    let (left, right) = (anchor.col.min(head_col), anchor.col.max(head_col));
    let right = if ragged { usize::MAX } else { right };
    let lines = anchor.row.min(head.row)..=anchor.row.max(head.row);
    let mut ranges = SmallVec::with_capacity(lines.clone().count());
    let mut primary = 0;
    for line in lines {
        let is_head = line == head.row;
        let range = match span(text, line, left, right, tab_width) {
            Some((from, to)) if forward => Range::new(from, to),
            Some((from, to)) => Range::new(to, from),
            None if is_head => {
                let start = text.line_to_char(line);
                Range::new(start, next_grapheme_boundary(text, start))
            }
            None => continue,
        };
        if is_head {
            primary = ranges.len();
        }
        ranges.push(range);
    }
    Selection::new(ranges, primary)
}

/// The chars of `line` from the visual column `left` to `right`, or to its end, if it reaches
/// `left`.
fn span(
    text: RopeSlice,
    line: usize,
    left: usize,
    right: usize,
    tab_width: usize,
) -> Option<(usize, usize)> {
    let start = text.line_to_char(line);
    let end = line_end_char_index(&text, line);
    let (mut col, mut pos, mut from) = (0, start, None);
    for grapheme in text.slice(start..end).graphemes() {
        let width = if grapheme == "\t" {
            tab_width - col % tab_width
        } else {
            grapheme_width(&Cow::from(grapheme))
        };
        col += width;
        if from.is_none() && col > left {
            from = Some(pos);
        }
        pos += grapheme.len_chars();
        if col > right {
            break;
        }
    }
    Some((from?, pos))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use helix_core::Rope;

    use super::*;

    /// The text of each range of the block from `anchor` to `head`, and the primary's.
    fn texts(
        text: &str,
        anchor: (usize, usize),
        head: (usize, usize),
        ragged: bool,
    ) -> (Vec<String>, String) {
        let text = Rope::from(text);
        let text = text.slice(..);
        let selection = block(
            text,
            Position::new(anchor.0, anchor.1),
            Position::new(head.0, head.1),
            4,
            ragged,
        );
        let texts = selection.fragments(text).map(String::from).collect();
        (texts, selection.primary().fragment(text).into())
    }

    #[test]
    fn blocks_take_the_columns_between_the_corners() {
        let text = "let alpha = 1;\nlet beta = 22;\nx\n\nfn f() {}\n";
        assert_eq!(
            texts(text, (0, 4), (1, 7), false),
            (vec!["alph".into(), "beta".into()], "beta".into())
        );
        // Lines too short to reach the left column are left out, and empty ones.
        assert_eq!(texts(text, (0, 4), (4, 6), false).0, ["alp", "bet", "() "]);
        // Reversed corners select the same columns, the cursor on the left.
        let text = Rope::from(text);
        let selection = block(
            text.slice(..),
            Position::new(1, 7),
            Position::new(0, 4),
            4,
            false,
        );
        assert_eq!(selection.primary(), Range::new(8, 4));
        assert_eq!(selection.ranges()[1], Range::new(23, 19));
    }

    #[test]
    fn ragged_blocks_run_to_the_line_ends() {
        let text = "let alpha = 1;\nab\nlet beta = 22;\n";
        assert_eq!(
            texts(text, (0, 4), (2, 0), true),
            (
                vec!["alpha = 1;".into(), "beta = 22;".into()],
                "beta = 22;".into()
            )
        );
    }

    #[test]
    fn the_head_line_stays_when_short() {
        // The cursor goes to the last char of a short line, the block with it.
        let text = "abcdef\nab\n\n";
        assert_eq!(
            texts(text, (0, 4), (1, 5), false),
            (vec!["bcde".into(), "b".into()], "b".into())
        );
        // On an empty line, it stays on the line break.
        assert_eq!(
            texts(text, (0, 4), (2, 4), false),
            (vec!["abcde".into(), "ab".into(), "\n".into()], "\n".into())
        );
    }

    #[test]
    fn columns_count_tabs_and_wide_chars() {
        // With tabs 4 wide, the tab takes columns 0 to 3.
        let text = "\tab\n日本語x\nabcdefg\n";
        assert_eq!(texts(text, (0, 4), (2, 5), false).0, ["ab", "語", "ef"]);
    }

    /// `cargo test --release -p helix-term --lib block_select::tests::measure -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure() {
        let text = Rope::from("let value = 42; // a line of code\n".repeat(100_000));
        let text = text.slice(..);
        let (anchor, head) = (Position::new(0, 4), Position::new(99_999, 9));
        let start = Instant::now();
        let selection = block(text, anchor, head, 4, false);
        println!("block of 100k lines: {:?}", start.elapsed());
        let start = Instant::now();
        assert!(keeps_anchors(text, &selection, &selection));
        println!("anchors of 100k lines: {:?}", start.elapsed());
    }
}
