use std::cell::Cell;

use crate::doc_formatter::{DocumentFormatter, FormattedGrapheme, GraphemeSource, TextFormat};
use crate::fold::Fold;
use crate::text_annotations::{
    Conceal, ConcealSource, InlineAnnotation, LineAnnotation, Overlay, TextAnnotations,
};
use crate::Position;

impl TextFormat {
    fn new_test(softwrap: bool) -> Self {
        TextFormat {
            soft_wrap: softwrap,
            tab_width: 2,
            max_wrap: 3,
            max_indent_retain: 4,
            wrap_indicator: ".".into(),
            wrap_indicator_highlight: None,
            // use a prime number to allow lining up too often with repeat
            viewport_width: 17,
            soft_wrap_at_text_width: false,
        }
    }
}

impl<'t> DocumentFormatter<'t> {
    fn collect_to_str(&mut self) -> String {
        use std::fmt::Write;
        let mut res = String::new();
        let viewport_width = self.text_fmt.viewport_width;
        let soft_wrap_at_text_width = self.text_fmt.soft_wrap_at_text_width;
        let mut line = 0;

        for grapheme in self {
            if grapheme.visual_pos.row != line {
                line += 1;
                assert_eq!(grapheme.visual_pos.row, line);
                write!(res, "\n{}", ".".repeat(grapheme.visual_pos.col)).unwrap();
            }
            if !soft_wrap_at_text_width {
                assert!(
                    grapheme.visual_pos.col <= viewport_width as usize,
                    "softwrapped failed {}<={viewport_width}",
                    grapheme.visual_pos.col
                );
            }
            write!(res, "{}", grapheme.raw).unwrap();
        }

        res
    }
}

fn softwrap_text(text: &str) -> String {
    DocumentFormatter::new_at_prev_checkpoint(
        text.into(),
        &TextFormat::new_test(true),
        &TextAnnotations::default(),
        0,
    )
    .collect_to_str()
}

#[test]
fn basic_softwrap() {
    assert_eq!(
        softwrap_text(&"foo ".repeat(10)),
        "foo foo foo foo \n.foo foo foo foo \n.foo foo  "
    );
    assert_eq!(
        softwrap_text(&"fooo ".repeat(10)),
        "fooo fooo fooo \n.fooo fooo fooo \n.fooo fooo fooo \n.fooo  "
    );

    // check that we don't wrap unnecessarily
    assert_eq!(softwrap_text("\t\txxxx1xxxx2xx\n"), "    xxxx1xxxx2xx \n ");
}

#[test]
fn softwrap_indentation() {
    assert_eq!(
        softwrap_text("\t\tfoo1 foo2 foo3 foo4 foo5 foo6\n"),
        "    foo1 foo2 \n.....foo3 foo4 \n.....foo5 foo6 \n "
    );
    assert_eq!(
        softwrap_text("\t\t\tfoo1 foo2 foo3 foo4 foo5 foo6\n"),
        "      foo1 foo2 \n.foo3 foo4 foo5 \n.foo6 \n "
    );
}

#[test]
fn long_word_softwrap() {
    assert_eq!(
        softwrap_text("\t\txxxx1xxxx2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n"),
        "    xxxx1xxxx2xxx\n.....x3xxxx4xxxx5\n.....xxxx6xxxx7xx\n.....xx8xxxx9xxx \n "
    );
    assert_eq!(
        softwrap_text("xxxxxxxx1xxxx2xxx\n"),
        "xxxxxxxx1xxxx2xxx\n. \n "
    );
    assert_eq!(
        softwrap_text("\t\txxxx1xxxx 2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n"),
        "    xxxx1xxxx \n.....2xxxx3xxxx4x\n.....xxx5xxxx6xxx\n.....x7xxxx8xxxx9\n.....xxx \n "
    );
    assert_eq!(
        softwrap_text("\t\txxxx1xxx 2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n"),
        "    xxxx1xxx 2xxx\n.....x3xxxx4xxxx5\n.....xxxx6xxxx7xx\n.....xx8xxxx9xxx \n "
    );
}

#[test]
fn softwrap_multichar_grapheme() {
    assert_eq!(
        softwrap_text("xxxx xxxx xxx a\u{0301}bc\n"),
        "xxxx xxxx xxx \n.ábc \n "
    )
}

fn softwrap_text_at_text_width(text: &str) -> String {
    let mut text_fmt = TextFormat::new_test(true);
    text_fmt.soft_wrap_at_text_width = true;
    let annotations = TextAnnotations::default();
    let mut formatter =
        DocumentFormatter::new_at_prev_checkpoint(text.into(), &text_fmt, &annotations, 0);
    formatter.collect_to_str()
}
#[test]
fn long_word_softwrap_text_width() {
    assert_eq!(
        softwrap_text_at_text_width("xxxxxxxx1xxxx2xxx\nxxxxxxxx1xxxx2xxx"),
        "xxxxxxxx1xxxx2xxx \nxxxxxxxx1xxxx2xxx "
    );
}

fn overlay_text(text: &str, char_pos: usize, softwrap: bool, overlays: &[Overlay]) -> String {
    DocumentFormatter::new_at_prev_checkpoint(
        text.into(),
        &TextFormat::new_test(softwrap),
        TextAnnotations::default().add_overlay(overlays, None),
        char_pos,
    )
    .collect_to_str()
}

#[test]
fn overlay() {
    assert_eq!(
        overlay_text(
            "foobar",
            0,
            false,
            &[Overlay::new(0, "X"), Overlay::new(2, "\t")],
        ),
        "Xo  bar "
    );
    assert_eq!(
        overlay_text(
            &"foo ".repeat(10),
            0,
            true,
            &[
                Overlay::new(2, "\t"),
                Overlay::new(5, "\t"),
                Overlay::new(16, "X"),
            ]
        ),
        "fo   f  o foo \n.foo Xoo foo foo \n.foo foo foo  "
    );
}

fn annotate_text(text: &str, softwrap: bool, annotations: &[InlineAnnotation]) -> String {
    DocumentFormatter::new_at_prev_checkpoint(
        text.into(),
        &TextFormat::new_test(softwrap),
        TextAnnotations::default().add_inline_annotations(annotations, None),
        0,
    )
    .collect_to_str()
}

#[test]
fn annotation() {
    assert_eq!(
        annotate_text("bar", false, &[InlineAnnotation::new(0, "foo")]),
        "foobar "
    );
    assert_eq!(
        annotate_text(
            &"foo ".repeat(10),
            true,
            &[InlineAnnotation::new(0, "foo ")]
        ),
        "foo foo foo foo \n.foo foo foo foo \n.foo foo foo  "
    );
}

#[test]
fn annotation_and_overlay() {
    let annotations = [InlineAnnotation {
        char_idx: 0,
        text: "fooo".into(),
    }];
    let overlay = [Overlay {
        char_idx: 0,
        grapheme: "\t".into(),
    }];
    assert_eq!(
        DocumentFormatter::new_at_prev_checkpoint(
            "bbar".into(),
            &TextFormat::new_test(false),
            TextAnnotations::default()
                .add_inline_annotations(annotations.as_slice(), None)
                .add_overlay(overlay.as_slice(), None),
            0,
        )
        .collect_to_str(),
        "fooo  bar "
    );
}

fn fold(start: usize, end: usize) -> Fold {
    Fold {
        start,
        end,
        pulled_up: false,
    }
}

fn folded_text(text: &str, softwrap: bool, folds: &[Fold], char_pos: usize) -> String {
    DocumentFormatter::new_at_prev_checkpoint(
        text.into(),
        &TextFormat::new_test(softwrap),
        TextAnnotations::default().add_folds(folds, " … ".into()),
        char_pos,
    )
    .collect_to_str()
}

#[test]
fn folds() {
    let text = "fn f() {\n    1\n}\nx\n";
    let folds = [fold(8, 15)];
    for softwrap in [false, true] {
        assert_eq!(
            folded_text(text, softwrap, &folds, 0),
            "fn f() { … } \nx \n "
        );
    }
    // a formatter starting inside a fold starts at the fold's row
    assert_eq!(folded_text(text, false, &folds, 11), "fn f() { … } \nx \n ");
    assert_eq!(folded_text(text, false, &folds, 15), "fn f() { … } \nx \n ");
    // a fold hiding the end of the text
    assert_eq!(
        folded_text("fn f() {\n    1\n}", false, &[fold(8, 16)], 0),
        "fn f() { …  "
    );
    // a fold ending beyond a truncated text is not folded
    assert_eq!(
        folded_text("fn f() {\n    1", false, &folds, 0),
        "fn f() { \n    1 "
    );
}

#[test]
fn chained_folds() {
    let text = "a {\n b\n} b {\n c\n}\nd\n";
    let folds = [fold(3, 7), fold(12, 16)];
    assert_eq!(
        folded_text(text, false, &folds, 0),
        "a { … } b { … } \nd \n "
    );
    assert_eq!(
        folded_text(text, false, &folds, 14),
        folded_text(text, false, &folds, 0)
    );

    let mut annotations = TextAnnotations::default();
    annotations.add_folds(&folds, " … ".into());
    let text_fmt = TextFormat::new_test(false);
    let formatter =
        DocumentFormatter::new_at_prev_checkpoint(text.into(), &text_fmt, &annotations, 0);
    let lines: Vec<_> = formatter
        .map(|grapheme| {
            (
                grapheme.char_idx,
                grapheme.line_idx,
                grapheme.source.is_fold(),
            )
        })
        .filter(|&(char_idx, _, _)| [2, 3, 7, 12, 16, 18].contains(&char_idx))
        .collect();
    assert_eq!(
        lines,
        [
            (2, 0, false),
            (3, 0, true),
            (7, 2, false),
            (12, 2, true),
            (16, 4, false),
            (18, 5, false)
        ]
    );
}

#[test]
fn soft_wrapped_fold() {
    // the placeholder is a word of its own, so the row wraps around it
    assert_eq!(
        folded_text("aaaa bbb ccc {\n  x\n} y\n", true, &[fold(14, 19)], 0),
        "aaaa bbb ccc { … \n.} y \n "
    );
    // a placeholder that does not fit the row wraps as a whole
    assert_eq!(
        folded_text("aaaa bbbb cccc {\n  x\n} y\n", true, &[fold(16, 21)], 0),
        "aaaa bbbb cccc {\n. … } y \n "
    );
}

#[test]
fn fold_placeholder_width() {
    let text = "a {\n b\n}\n";
    let folds = [fold(3, 7)];
    for (placeholder, width) in [("…", 1), (" … ", 3), ("⋯⋯", 2), ("折", 2), ("folded", 6)]
    {
        let mut annotations = TextAnnotations::default();
        annotations.add_folds(&folds, placeholder.into());
        for softwrap in [false, true] {
            let text_fmt = TextFormat::new_test(softwrap);
            let graphemes: Vec<_> =
                DocumentFormatter::new_at_prev_checkpoint(text.into(), &text_fmt, &annotations, 0)
                    .map(|g| (g.char_idx, g.visual_pos, g.width(), g.is_whitespace()))
                    .collect();
            // the placeholder is measured as a whole and never whitespace, whatever it starts with
            assert_eq!(graphemes[2], (2, Position::new(0, 2), 1, false));
            assert_eq!(graphemes[3], (3, Position::new(0, 3), width, false));
            assert_eq!(
                graphemes[4],
                (7, Position::new(0, 3 + width), 1, false),
                "{placeholder:?}"
            );
        }
    }
    // a placeholder starting with a word char does not join the following word
    let folds = [fold(16, 20)];
    let mut annotations = TextAnnotations::default();
    annotations.add_folds(&folds, "a".into());
    assert_eq!(
        DocumentFormatter::new_at_prev_checkpoint(
            "aaaa bbbb cccc {\n x\n}\n".into(),
            &TextFormat::new_test(true),
            &annotations,
            0
        )
        .collect_to_str(),
        "aaaa bbbb cccc {a\n.} \n "
    );
}

#[test]
fn fold_and_annotations() {
    let text = "fn f() {\n    1\n}\nx\n";
    let folds = [fold(8, 15)];
    let inline = [
        InlineAnnotation::new(8, "A"),
        InlineAnnotation::new(10, "B"),
        InlineAnnotation::new(15, "C"),
    ];
    let overlays = [Overlay::new(12, "X"), Overlay::new(17, "Y")];
    assert_eq!(
        DocumentFormatter::new_at_prev_checkpoint(
            text.into(),
            &TextFormat::new_test(false),
            TextAnnotations::default()
                .add_inline_annotations(&inline, None)
                .add_overlay(&overlays, None)
                .add_folds(&folds, " … ".into()),
            0,
        )
        .collect_to_str(),
        // annotations at the fold's start are shown before it, hidden ones are skipped
        "fn f() {A … C} \nY \n "
    );
}

/// Inserts a virtual line after every line containing one of the anchors.
struct VirtualLineAtAnchors {
    anchors: Vec<usize>,
    next: usize,
    pending: bool,
}

impl VirtualLineAtAnchors {
    fn next_anchor(&mut self, char_idx: usize) -> usize {
        self.next = self.anchors.partition_point(|&anchor| anchor < char_idx);
        self.anchors.get(self.next).copied().unwrap_or(usize::MAX)
    }
}

impl LineAnnotation for VirtualLineAtAnchors {
    fn reset_pos(&mut self, char_idx: usize) -> usize {
        self.pending = false;
        self.next_anchor(char_idx)
    }

    fn skip_concealed_anchors(&mut self, conceal_end_char_idx: usize) -> usize {
        self.next_anchor(conceal_end_char_idx)
    }

    fn process_anchor(&mut self, grapheme: &FormattedGrapheme) -> usize {
        self.pending = true;
        self.next_anchor(grapheme.char_idx + 1)
    }

    fn insert_virtual_lines(&mut self, _: usize, _: Position, _: usize) -> Position {
        Position::new(std::mem::take(&mut self.pending) as usize, 0)
    }
}

#[test]
fn fold_and_virtual_lines() {
    let text = "fn f() {\n    1\n}\nx\n";
    let folds = [fold(8, 15)];
    let text_fmt = TextFormat::new_test(false);
    let mut annotations = TextAnnotations::default();
    annotations.add_folds(&folds, " … ".into());
    // anchors on the header, inside the fold and after it
    annotations.add_line_annotation(Box::new(VirtualLineAtAnchors {
        anchors: vec![2, 12, 17],
        next: 0,
        pending: false,
    }));
    let rows: Vec<_> =
        DocumentFormatter::new_at_prev_checkpoint(text.into(), &text_fmt, &annotations, 0)
            .filter(|grapheme| [0, 8, 15, 17, 19].contains(&grapheme.char_idx))
            .map(|grapheme| (grapheme.char_idx, grapheme.visual_pos.row))
            .collect();
    // the header's anchor adds a line below the fold's row, the hidden one adds none
    assert_eq!(rows, [(0, 0), (8, 0), (15, 0), (17, 2), (19, 4)]);
}

fn concealed_text(text: &str, softwrap: bool, conceals: &[Conceal], char_pos: usize) -> String {
    DocumentFormatter::new_at_prev_checkpoint(
        text.into(),
        &TextFormat::new_test(softwrap),
        TextAnnotations::default().add_conceals(conceals),
        char_pos,
    )
    .collect_to_str()
}

#[test]
fn conceals() {
    let text = "$2 alpha^2$\n";
    let conceals = [Conceal::new(3, 8, "α")];
    for softwrap in [false, true] {
        assert_eq!(concealed_text(text, softwrap, &conceals, 0), "$2 α^2$ \n ");
    }
    // a formatter starting inside a conceal starts at its line
    assert_eq!(concealed_text(text, false, &conceals, 5), "$2 α^2$ \n ");
    // a conceal ending beyond a truncated text is not concealed
    assert_eq!(concealed_text("$2 alp", false, &conceals, 0), "$2 alp ");

    let mut annotations = TextAnnotations::default();
    annotations.add_conceals(&conceals[..]);
    let text_fmt = TextFormat::new_test(false);
    let graphemes: Vec<_> =
        DocumentFormatter::new_at_prev_checkpoint(text.into(), &text_fmt, &annotations, 0)
            .map(|g| {
                let concealed = matches!(g.source, GraphemeSource::Conceal { .. });
                (g.char_idx, g.visual_pos.col, g.doc_chars(), concealed)
            })
            .collect();
    assert_eq!(graphemes[3], (3, 3, 5, true), "the conceal covers `alpha`");
    assert_eq!(
        graphemes[4],
        (8, 4, 1, false),
        "`^` follows in the next column"
    );
}

#[test]
fn conceal_widths() {
    // a wide replacement takes two columns
    let conceals = [Conceal::new(0, 11, "😀")];
    let mut annotations = TextAnnotations::default();
    annotations.add_conceals(&conceals[..]);
    let text_fmt = TextFormat::new_test(false);
    let graphemes: Vec<_> = DocumentFormatter::new_at_prev_checkpoint(
        "#emoji.face x".into(),
        &text_fmt,
        &annotations,
        0,
    )
    .map(|g| (g.char_idx, g.visual_pos.col, g.width()))
    .collect();
    assert_eq!(graphemes[..3], [(0, 0, 2), (11, 2, 1), (12, 3, 1)]);

    // text that only fits a row once it is concealed is not wrapped
    let text = "alpha beta gamma delta\n";
    let conceals = [
        Conceal::new(0, 5, "α"),
        Conceal::new(6, 10, "β"),
        Conceal::new(11, 16, "γ"),
        Conceal::new(17, 22, "δ"),
    ];
    assert_eq!(
        concealed_text(text, true, &[], 0),
        "alpha beta gamma \n.delta \n "
    );
    assert_eq!(concealed_text(text, true, &conceals, 0), "α β γ δ \n ");
}

#[test]
fn conceal_and_annotations() {
    let inline = [
        InlineAnnotation::new(3, "A"),
        InlineAnnotation::new(5, "B"),
        InlineAnnotation::new(8, "C"),
    ];
    let overlays = [Overlay::new(4, "X"), Overlay::new(9, "Y")];
    let conceals = [Conceal::new(3, 8, "α")];
    assert_eq!(
        DocumentFormatter::new_at_prev_checkpoint(
            "$2 alpha^2$\n".into(),
            &TextFormat::new_test(false),
            TextAnnotations::default()
                .add_inline_annotations(&inline, None)
                .add_overlay(&overlays, None)
                .add_conceals(&conceals[..]),
            0,
        )
        .collect_to_str(),
        // annotations at the conceal's start are shown before it, concealed ones are skipped
        "$2 AαC^Y$ \n "
    );
}

#[test]
fn conceals_in_folds() {
    let text = "fn f() {\n    alpha\n}\nx\n";
    let folds = [fold(8, 19)];
    // one conceal is hidden by the fold, the one after it is shown
    let conceals = [Conceal::new(13, 18, "α"), Conceal::new(21, 22, "χ")];
    assert_eq!(
        DocumentFormatter::new_at_prev_checkpoint(
            text.into(),
            &TextFormat::new_test(false),
            TextAnnotations::default()
                .add_folds(&folds, " … ".into())
                .add_conceals(&conceals[..]),
            0,
        )
        .collect_to_str(),
        "fn f() { … } \nχ \n "
    );
}

/// Hands out conceals in windows of `window` chars, like a source that computes them on the fly.
struct WindowedConceals<'a> {
    conceals: &'a [Conceal],
    window: usize,
    fetches: &'a Cell<usize>,
}

impl ConcealSource for WindowedConceals<'_> {
    fn conceals_from(&self, char_idx: usize, conceals: &mut Vec<Conceal>) -> usize {
        self.fetches.set(self.fetches.get() + 1);
        let end = char_idx + self.window;
        let in_window = self
            .conceals
            .iter()
            .filter(|conceal| (char_idx..end).contains(&conceal.start));
        conceals.extend(in_window);
        end
    }
}

#[test]
fn conceals_are_fetched_on_the_fly() {
    let text = "alpha beta\ngamma delta\n";
    let conceals = [
        Conceal::new(0, 5, "α"),
        Conceal::new(6, 10, "β"),
        Conceal::new(11, 16, "γ"),
        Conceal::new(17, 22, "δ"),
    ];
    let fetches = Cell::new(0);
    let mut annotations = TextAnnotations::default();
    annotations.add_conceals(WindowedConceals {
        conceals: &conceals,
        window: 3,
        fetches: &fetches,
    });
    let text_fmt = TextFormat::new_test(false);
    let format = |char_pos| {
        DocumentFormatter::new_at_prev_checkpoint(text.into(), &text_fmt, &annotations, char_pos)
            .collect_to_str()
    };
    assert_eq!(format(0), "α β \nγ δ \n ");
    let fetched_once = fetches.get();
    // traversals that start earlier or later fetch again as needed
    assert_eq!(format(13), "γ δ \n ");
    assert_eq!(format(0), "α β \nγ δ \n ");
    assert!(fetches.get() > fetched_once);

    // a conceal that starts inside a grapheme is skipped, later ones are not
    let conceals = [Conceal::new(1, 2, "X"), Conceal::new(3, 4, "Y")];
    assert_eq!(
        concealed_text("a\u{301}bc", false, &conceals, 0),
        "a\u{301}bY "
    );
}
