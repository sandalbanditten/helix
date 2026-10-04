//! Alignments from the JSON `difft` prints for a pair of files with `DFT_DISPLAY=json`, an
//! unstable format read here as difftastic 0.71 prints it.

use std::collections::BTreeMap;

use helix_core::{diff::text_lines, RopeSlice};
use serde::Deserialize;

use super::{
    alignment::{Alignment, LineChange, Row},
    builtin,
};

#[derive(Debug, Deserialize)]
struct File {
    status: Status,
    #[serde(default)]
    aligned_lines: Vec<(Option<u32>, Option<u32>)>,
    #[serde(default)]
    chunks: Vec<Vec<ChunkLine>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Unchanged,
    Created,
    Deleted,
    Changed,
}

#[derive(Debug, Deserialize)]
struct ChunkLine {
    lhs: Option<SideLine>,
    rhs: Option<SideLine>,
}

#[derive(Debug, Deserialize)]
struct SideLine {
    line_number: u32,
    changes: Vec<Change>,
}

#[derive(Debug, Deserialize)]
struct Change {
    start: u32,
    end: u32,
}

/// Why difftastic's output could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unexpected output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("its lines do not match the texts")]
    Lines,
}

/// Reads the JSON `difft` printed for the texts `old` and `new`.
pub fn parse(json: &[u8], old: RopeSlice, new: RopeSlice) -> Result<Alignment, Error> {
    let file: File = serde_json::from_slice(json)?;
    let lines = [text_lines(old) as u32, text_lines(new) as u32];
    let (rows, mut changes): (Vec<Row>, [BTreeMap<u32, LineChange>; 2]) = match file.status {
        // Unchanged as far as difftastic cares, which leaves out e.g. blank lines: the lines
        // still have to line up.
        Status::Unchanged => (builtin::align(old, new).rows().to_vec(), Default::default()),
        Status::Created | Status::Deleted => {
            let rows = (0..lines[0])
                .map(|line| Row {
                    old: Some(line),
                    new: None,
                })
                .chain((0..lines[1]).map(|line| Row {
                    old: None,
                    new: Some(line),
                }))
                .collect();
            let whole = |lines: u32| (0..lines).map(|line| (line, LineChange::Whole)).collect();
            (rows, [whole(lines[0]), whole(lines[1])])
        }
        Status::Changed => {
            // difftastic also aligns the empty line after a final line break, or one past the
            // end without it, which are no lines here.
            let line = |line: Option<u32>, side: usize| line.filter(|&line| line < lines[side]);
            let rows = file
                .aligned_lines
                .iter()
                .map(|&(old, new)| Row {
                    old: line(old, 0),
                    new: line(new, 1),
                })
                .filter(|row| row.old.is_some() || row.new.is_some())
                .collect();
            let mut parts = [BTreeMap::new(), BTreeMap::new()];
            for chunk_line in file.chunks.iter().flatten() {
                for (side, side_line) in [(0, &chunk_line.lhs), (1, &chunk_line.rhs)] {
                    let Some(side_line) = side_line.as_ref() else {
                        continue;
                    };
                    parts[side]
                        .entry(side_line.line_number)
                        .or_insert_with(Vec::new)
                        .extend(
                            side_line
                                .changes
                                .iter()
                                .map(|change| change.start..change.end),
                        );
                }
            }
            let [old_parts, new_parts] = parts;
            let changes = |text: RopeSlice, side: usize, parts: BTreeMap<u32, Vec<_>>| {
                parts
                    .into_iter()
                    .filter(|(line, _)| *line < lines[side])
                    .filter_map(|(line, parts)| {
                        let change = LineChange::of_parts(text.line(line as usize), parts)?;
                        Some((line, change))
                    })
                    .collect()
            };
            (
                rows,
                [changes(old, 0, old_parts), changes(new, 1, new_parts)],
            )
        }
    };
    // Blank lines hold no tokens for difftastic to list as changed, yet facing fillers they were
    // added or removed. Other lines facing fillers may have only moved.
    for row in &rows {
        let (side, line, text) = match (row.old, row.new) {
            (Some(line), None) => (0, line, old),
            (None, Some(line)) => (1, line, new),
            _ => continue,
        };
        if text.line(line as usize).chars().all(char::is_whitespace) {
            changes[side].entry(line).or_insert(LineChange::Whole);
        }
    }
    let changes = changes.map(|changes| changes.into_iter().collect());
    Alignment::new(rows, lines, changes).ok_or(Error::Lines)
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use helix_core::Rope;

    use super::*;
    use crate::diff_view::alignment::Side;

    fn parse_texts(json: &str, old: &str, new: &str) -> Result<Alignment, Error> {
        parse(
            json.as_bytes(),
            Rope::from(old).slice(..),
            Rope::from(new).slice(..),
        )
    }

    fn rows(alignment: &Alignment) -> Vec<(Option<u32>, Option<u32>)> {
        alignment
            .rows()
            .iter()
            .map(|row| (row.old, row.new))
            .collect()
    }

    /// What difftastic 0.71 printed for `old.rs` and `new.rs` below.
    const EXPANDED: &str = r#"{"aligned_lines":[[0,0],[1,1],[null,2],[null,3],[null,4],[2,5],[3,null],[4,6],[5,7]],"chunks":[[{"lhs":{"line_number":1,"changes":[{"start":25,"end":26,"content":"c","highlight":"normal"}]},"rhs":{"line_number":1,"changes":[]}},{"lhs":{"line_number":2,"changes":[{"start":13,"end":17,"content":"\"hi\"","highlight":"string"}]},"rhs":{"line_number":5,"changes":[{"start":13,"end":20,"content":"\"hello\"","highlight":"string"}]}},{"lhs":{"line_number":3,"changes":[{"start":4,"end":7,"content":"foo","highlight":"normal"},{"start":7,"end":8,"content":"(","highlight":"delimiter"},{"start":8,"end":9,"content":"1","highlight":"keyword"},{"start":9,"end":10,"content":")","highlight":"delimiter"},{"start":10,"end":11,"content":";","highlight":"normal"}]}}]],"language":"Rust","path":"new.rs","status":"changed"}"#;
    const EXPANDED_OLD: &str =
        "fn main() {\n    let x = Self { a, b, c };\n    println!(\"hi\");\n    foo(1);\n}\n";
    const EXPANDED_NEW: &str = "fn main() {\n    let x = Self {\n        a,\n        b,\n    };\n    println!(\"hello\");\n}\n";

    #[test]
    fn changed_files_line_up_as_difftastic_aligns_them() {
        let alignment = parse_texts(EXPANDED, EXPANDED_OLD, EXPANDED_NEW).unwrap();
        assert_eq!(
            rows(&alignment),
            [
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (None, Some(2)),
                (None, Some(3)),
                (None, Some(4)),
                (Some(2), Some(5)),
                (Some(3), None),
                (Some(4), Some(6)),
            ],
            "the row past the final line break is left out"
        );
        assert_eq!(
            alignment.change(Side::Old, 1),
            Some(&LineChange::Parts(vec![25..26]))
        );
        assert_eq!(alignment.change(Side::New, 1), None, "no change listed");
        assert_eq!(
            alignment.change(Side::New, 5),
            Some(&LineChange::Parts(vec![13..20]))
        );
        assert_eq!(
            alignment.change(Side::Old, 3),
            Some(&LineChange::Whole),
            "every token of `foo(1);` changed"
        );
        // Lines moved to rows of their own change nothing, yet face fillers.
        assert_eq!(alignment.change(Side::New, 2), None);
        assert_eq!(alignment.hunks(), [1..7]);
    }

    #[test]
    fn blank_lines_facing_fillers_are_added_or_removed() {
        // What difftastic 0.71 printed for a blank line added between functions and one in `b`.
        let json = r#"{"aligned_lines":[[0,0],[null,1],[1,2],[2,3],[null,4],[null,5],[3,6],[4,7]],"chunks":[[{"rhs":{"line_number":5,"changes":[{"start":4,"end":5,"content":"y","highlight":"normal"},{"start":5,"end":6,"content":"(","highlight":"delimiter"},{"start":6,"end":7,"content":")","highlight":"delimiter"},{"start":7,"end":8,"content":";","highlight":"normal"}]}}]],"language":"Rust","path":"new.rs","status":"changed"}"#;
        let alignment = parse_texts(
            json,
            "fn a() {}\nfn b() {\n    x();\n}\n",
            "fn a() {}\n\nfn b() {\n    x();\n    \n    y();\n}\n",
        )
        .unwrap();
        assert_eq!(alignment.change(Side::New, 1), Some(&LineChange::Whole));
        assert_eq!(
            alignment.change(Side::New, 4),
            Some(&LineChange::Whole),
            "only whitespace"
        );
        assert_eq!(alignment.change(Side::New, 5), Some(&LineChange::Whole));
        assert_eq!(alignment.change(Side::New, 3), None);
        assert_eq!(alignment.changes(Side::New, 0..8).len(), 3);
    }

    #[test]
    fn crlf_and_missing_final_line_breaks_line_up() {
        let crlf = r#"{"aligned_lines":[[0,0],[1,1],[2,2]],"chunks":[[{"lhs":{"line_number":1,"changes":[{"start":3,"end":4,"content":"b","highlight":"normal"}]},"rhs":{"line_number":1,"changes":[{"start":3,"end":4,"content":"c","highlight":"normal"}]}}]],"language":"Rust","path":"crlf_new.rs","status":"changed"}"#;
        let alignment = parse_texts(
            crlf,
            "fn a() {}\r\nfn b() {}\r\n",
            "fn a() {}\r\nfn c() {}\r\n",
        )
        .unwrap();
        assert_eq!(rows(&alignment), [(Some(0), Some(0)), (Some(1), Some(1))]);
        assert_eq!(
            alignment.change(Side::New, 1),
            Some(&LineChange::Parts(vec![3..4]))
        );
        let no_final_break = r#"{"aligned_lines":[[0,0],[1,1],[2,2]],"chunks":[[{"lhs":{"line_number":1,"changes":[{"start":0,"end":1,"content":"y","highlight":"normal"}]},"rhs":{"line_number":1,"changes":[{"start":0,"end":1,"content":"z","highlight":"normal"}]}}]],"language":"Text","path":"nonl_new.txt","status":"changed"}"#;
        let alignment = parse_texts(no_final_break, "x\ny", "x\nz").unwrap();
        assert_eq!(rows(&alignment), [(Some(0), Some(0)), (Some(1), Some(1))]);
        assert_eq!(alignment.change(Side::Old, 1), Some(&LineChange::Whole));
    }

    #[test]
    fn created_deleted_and_unchanged_files_come_without_rows() {
        let created = r#"{"language":"Text","path":"ab.rs","status":"created"}"#;
        let alignment = parse_texts(created, "", "a\nb\n").unwrap();
        assert_eq!(rows(&alignment), [(None, Some(0)), (None, Some(1))]);
        assert_eq!(alignment.change(Side::New, 1), Some(&LineChange::Whole));
        let deleted = r#"{"language":"Text","path":"empty.rs","status":"deleted"}"#;
        let alignment = parse_texts(deleted, "a\nb\n", "").unwrap();
        assert_eq!(rows(&alignment), [(Some(0), None), (Some(1), None)]);
        let unchanged = r#"{"language":"Text","path":"nl.txt","status":"unchanged"}"#;
        let alignment = parse_texts(unchanged, "x\n\ny\n", "x\ny\n").unwrap();
        assert_eq!(
            rows(&alignment),
            [(Some(0), Some(0)), (Some(1), None), (Some(2), Some(1))]
        );
        assert_eq!(
            alignment.change(Side::Old, 1),
            Some(&LineChange::Whole),
            "a blank line removed"
        );
    }

    #[test]
    fn output_not_matching_the_texts_is_an_error() {
        let longer = "a\n".repeat(10);
        assert!(matches!(
            parse_texts(EXPANDED, &longer, EXPANDED_NEW),
            Err(Error::Lines)
        ));
        assert!(matches!(
            parse_texts("not json", "", ""),
            Err(Error::Json(_))
        ));
    }
}
