use crate::doc_formatter::{DocumentFormatter, FormattedGrapheme, TextFormat};
use crate::fold::Fold;
use crate::text_annotations::{InlineAnnotation, LineAnnotation, Overlay, TextAnnotations};
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
        TextAnnotations::default().add_folds(folds, '…'),
        char_pos,
    )
    .collect_to_str()
}

#[test]
fn folds() {
    let text = "fn f() {\n    1\n}\nx\n";
    let folds = [fold(8, 15)];
    for softwrap in [false, true] {
        assert_eq!(folded_text(text, softwrap, &folds, 0), "fn f() {…} \nx \n ");
    }
    // a formatter starting inside a fold starts at the fold's row
    assert_eq!(folded_text(text, false, &folds, 11), "fn f() {…} \nx \n ");
    assert_eq!(folded_text(text, false, &folds, 15), "fn f() {…} \nx \n ");
    // a fold hiding the end of the text
    assert_eq!(
        folded_text("fn f() {\n    1\n}", false, &[fold(8, 16)], 0),
        "fn f() {… "
    );
    // a fold ending beyond a truncated text is not folded
    assert_eq!(
        folded_text("fn f() {\n    1", false, &folds, 0),
        "fn f() { \n    1 "
    );
}

#[test]
fn chained_folds() {
    let text = "if a {\n    b\n} else {\n    c\n}\nd\n";
    let folds = [fold(6, 13), fold(21, 28)];
    assert_eq!(
        folded_text(text, false, &folds, 0),
        "if a {…} else {…} \nd \n "
    );
    assert_eq!(
        folded_text(text, false, &folds, 23),
        folded_text(text, false, &folds, 0)
    );

    let mut annotations = TextAnnotations::default();
    annotations.add_folds(&folds, '…');
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
        .filter(|&(char_idx, _, _)| [5, 6, 13, 21, 28, 30].contains(&char_idx))
        .collect();
    assert_eq!(
        lines,
        [
            (5, 0, false),
            (6, 0, true),
            (13, 2, false),
            (21, 2, true),
            (28, 4, false),
            (30, 5, false)
        ]
    );
}

#[test]
fn soft_wrapped_fold() {
    // the placeholder is a word of its own, so the row wraps around it
    assert_eq!(
        folded_text("aaaa bbbb cccc {\n  x\n} y\n", true, &[fold(16, 21)], 0),
        "aaaa bbbb cccc {…\n.} y \n "
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
                .add_folds(&folds, '…'),
            0,
        )
        .collect_to_str(),
        // annotations at the fold's start are shown before it, hidden ones are skipped
        "fn f() {A…C} \nY \n "
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
    annotations.add_folds(&folds, '…');
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
