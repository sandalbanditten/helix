//! The pages of the pager that `man` and git format again for the width of the views showing them.

use std::{
    collections::HashMap,
    io::{self, PipeReader, Read},
    process::{Child, Command},
};

use helix_core::{Selection, Transaction};
use helix_view::{
    doc_mut,
    editor::GutterType,
    pager::{self, Formatter, Page, Source},
    view::ViewPosition,
    Document, DocumentId, Editor, View,
};
use tokio::runtime::Handle;

use crate::job;

/// The digits of the line numbers that pages leave room for, so that their width doesn't change
/// with their length: line numbers widen as the output of a command arrives.
const LINE_NUMBER_DIGITS: usize = 5;

/// Has the commands that showed pages in the pager format them again where the width of their
/// text changed: the narrowest of the views showing a page, like Neovim's `:Man`, and at most the
/// width the command allows. Each page waits for the formatting under way.
pub fn fit_pages(editor: &mut Editor) {
    let mut widths: HashMap<DocumentId, u16> = HashMap::new();
    for (view, _) in editor.tree.views() {
        let doc = &editor.documents[&view.doc];
        if source(editor, doc.id()).is_some() {
            let width = text_width(view, doc);
            widths
                .entry(doc.id())
                .and_modify(|narrowest| *narrowest = (*narrowest).min(width))
                .or_insert(width);
        }
    }
    for (doc, width) in widths {
        let empty = editor.documents[&doc].text().len_chars() == 0;
        let Some(source) = source_mut(editor, doc) else {
            continue;
        };
        // A line as wide as the view would wrap before its line break.
        let width = width.saturating_sub(1);
        let width = source
            .producer
            .max_width()
            .map_or(width, |max| width.min(max));
        if source.width == Some(width) || source.formatting.is_some() {
            continue;
        }
        source.formatting = Some(width);
        let mut command = source.producer.command(width);
        // With no place in the page to keep, the output shows as it arrives.
        if empty {
            fill(command, doc, width);
            continue;
        }
        job::in_background(
            move || {
                let output = command
                    .output()
                    .ok()
                    .filter(|output| output.status.success() && !output.stdout.is_empty())?;
                Some(pager::page(&String::from_utf8_lossy(&output.stdout)))
            },
            move |editor, _, page| {
                if let Some((text, page)) = page {
                    editor.repage(doc, text, page);
                }
                formatted(editor, doc, width);
            },
        );
    }
}

/// The width of the text of `view`, with room for line numbers of [`LINE_NUMBER_DIGITS`].
fn text_width(view: &View, doc: &Document) -> u16 {
    let line_numbers = GutterType::LineNumbers;
    let room = if view.gutters().contains(&line_numbers) {
        LINE_NUMBER_DIGITS.saturating_sub(line_numbers.width(view, doc))
    } else {
        0
    };
    view.inner_width(doc).saturating_sub(room as u16)
}

/// Marks the page `doc` formatted for `width`, not to format it again for it even if the command
/// failed.
fn formatted(editor: &mut Editor, doc: DocumentId, width: u16) {
    if let Some(source) = source_mut(editor, doc) {
        source.formatting = None;
        source.width = Some(width);
    }
}

/// Appends the output of `command` to the empty page `doc` as it arrives, with what it writes to
/// stderr, as git sends both to its pager. If the command doesn't run, the page shows what was
/// piped into the pager instead.
fn fill(command: Command, doc: DocumentId, width: u16) {
    let handle = Handle::current();
    std::thread::spawn(move || {
        let (output, child): (Box<dyn Read>, _) = match spawn(command) {
            Ok((output, child)) => (Box::new(output), Some(child)),
            Err(err) => {
                log::warn!("cannot format the page again: {err}");
                (Box::new(io::stdin()), None)
            }
        };
        stream(output, |text, page| {
            handle.block_on(job::dispatch(move |editor, _| {
                append(editor, doc, text, page);
            }));
        });
        if let Some(mut child) = child {
            let _ = child.wait();
        }
        handle.block_on(job::dispatch(move |editor, _| {
            formatted(editor, doc, width)
        }));
    });
}

fn spawn(mut command: Command) -> io::Result<(PipeReader, Child)> {
    let (output, end) = io::pipe()?;
    command.stdout(end.try_clone()?).stderr(end);
    // The command keeps the ends it writes to, which must close for the output to end.
    command.spawn().map(|child| (output, child))
}

/// Reads `output` until it ends, handing the text of the whole lines of each read, with their
/// styles, to `append`.
fn stream(mut output: impl Read, mut append: impl FnMut(String, Page)) {
    let mut formatter = Formatter::default();
    let mut buf = vec![0; 64 * 1024];
    let mut unformatted = Vec::new();
    loop {
        let read = match output.read(&mut buf) {
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                log::warn!("cannot read the output for the pager: {err}");
                0
            }
        };
        unformatted.extend_from_slice(&buf[..read]);
        // The rest once the output ended.
        let lines = if read == 0 {
            unformatted.len()
        } else {
            unformatted
                .iter()
                .rposition(|&byte| byte == b'\n')
                .map_or(0, |line_break| line_break + 1)
        };
        if lines > 0 {
            let lines: Vec<u8> = unformatted.drain(..lines).collect();
            let (text, page) = formatter.format(&String::from_utf8_lossy(&lines));
            append(text, page);
        }
        if read == 0 {
            return;
        }
    }
}

/// Appends `text`, with the styles of `page`, to the page `doc_id`. Views of the page stay where
/// they are, at its start while it was empty.
fn append(editor: &mut Editor, doc_id: DocumentId, text: String, page: Page) {
    let Some(doc) = editor.document(doc_id).filter(|doc| doc.page.is_some()) else {
        return;
    };
    let end = doc.text().len_chars();
    let transaction = Transaction::change(doc.text(), [(end, end, Some(text.into()))].into_iter());
    let view = editor.get_synced_view_id(doc_id);
    let doc = doc_mut!(editor, &doc_id);
    doc.apply_output(&transaction, view);
    if let Some(shown) = doc.page.as_mut() {
        let styles = page.styles.into_iter();
        shown
            .styles
            .extend(styles.map(|(range, style)| (range.start + end..range.end + end, style)));
    }
    if end == 0 {
        let views: Vec<_> = doc.selections().keys().copied().collect();
        for view in views {
            doc.set_selection(view, Selection::point(0));
            doc.set_view_offset(view, ViewPosition::default());
        }
    }
}

fn source(editor: &Editor, doc: DocumentId) -> Option<&Source> {
    editor.document(doc)?.page.as_ref()?.source.as_ref()
}

fn source_mut(editor: &mut Editor, doc: DocumentId) -> Option<&mut Source> {
    editor.document_mut(doc)?.page.as_mut()?.source.as_mut()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use helix_view::graphics::{Color, Style};

    use super::*;

    /// Output read in the parts `reads`.
    struct Reads(VecDeque<&'static [u8]>);

    impl Read for Reads {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(read) = self.0.pop_front() else {
                return Ok(0);
            };
            buf[..read.len()].copy_from_slice(read);
            Ok(read.len())
        }
    }

    #[test]
    fn output_streams_in_whole_lines() {
        // A line, a char, and an SGR sequence split across reads.
        let reads: [&[u8]; 4] = [
            b"one\ntw",
            "o \u{e9}".as_bytes(),
            b"\x1b[3",
            b"1mred\n\x08end",
        ];
        let mut appended = Vec::new();
        stream(Reads(reads.into()), |text, page| {
            appended.push((text, page.styles));
        });
        let red = Style::default().fg(Color::Indexed(1));
        assert_eq!(
            appended,
            [
                ("one\n".to_owned(), vec![]),
                ("two \u{e9}red\n".to_owned(), vec![(5..9, red)]),
                ("end".to_owned(), vec![(0..3, red)]),
            ]
        );
    }
}
