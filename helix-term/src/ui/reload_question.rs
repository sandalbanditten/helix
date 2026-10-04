//! The question whether to reload buffers with unsaved changes whose files changed on disk.

use std::time::SystemTime;

use helix_core::unicode::width::UnicodeWidthStr;
use helix_stdx::path::get_relative_path;
use helix_view::{
    document::DiskText, graphics::Rect, info::Info, input::KeyEvent, DocumentId, Editor,
};
use tui::buffer::Buffer as Surface;

use crate::{
    compositor::{Component, Context},
    key,
};

/// The buffers whose files changed on disk under unsaved changes, asked about one at a time.
#[derive(Default)]
pub struct ReloadQuestion {
    /// The buffers to ask about, in order.
    queue: Vec<Asked>,
}

/// A buffer asked about.
struct Asked {
    doc: DocumentId,
    /// What its file holds now.
    disk: DiskText,
    /// When the buffer last knew its file to change.
    known: SystemTime,
}

impl ReloadQuestion {
    /// Asks about `doc`, whose file now holds `disk`, after the buffers asked about already.
    pub fn ask(&mut self, doc: DocumentId, disk: DiskText, known: SystemTime) {
        let asked = Asked { doc, disk, known };
        match self.queue.iter_mut().find(|queued| queued.doc == doc) {
            Some(queued) => *queued = asked,
            None => self.queue.push(asked),
        }
    }

    /// Stops asking about `doc`, as its file holds what the buffer knows of it again.
    pub fn forget(&mut self, doc: DocumentId) {
        self.queue.retain(|asked| asked.doc != doc);
    }

    /// Whether there is a buffer to ask about.
    pub fn is_asking(&mut self, editor: &Editor) -> bool {
        self.queue.retain(|asked| {
            editor
                .document(asked.doc)
                .is_some_and(|doc| doc.last_saved_time() == asked.known)
        });
        !self.queue.is_empty()
    }

    /// Answers the question with `key`, if it is one of the answers.
    pub fn answer(&mut self, key: KeyEvent, editor: &mut Editor) {
        match key {
            key!('r') => self.reload(1, editor),
            key!('k') | key!(Esc) => self.keep(1, editor),
            key!('R') => self.reload(self.queue.len(), editor),
            key!('K') => self.keep(self.queue.len(), editor),
            _ => {}
        }
    }

    /// Reloads the first `count` buffers asked about.
    fn reload(&mut self, count: usize, editor: &mut Editor) {
        let answered: Vec<_> = self.queue.drain(..count).collect();
        let status = summary(&answered, "reloaded", editor);
        let mut failed = false;
        for asked in answered {
            if let Err(err) = editor.reload(asked.doc) {
                editor.set_error(err.to_string());
                failed = true;
            }
        }
        if !failed {
            editor.set_status(status);
        }
    }

    /// Keeps the text of the first `count` buffers asked about.
    fn keep(&mut self, count: usize, editor: &mut Editor) {
        let answered: Vec<_> = self.queue.drain(..count).collect();
        let status = summary(&answered, "kept", editor);
        for asked in answered {
            if let Some(doc) = editor.document_mut(asked.doc) {
                doc.ignore_disk_change(asked.disk);
            }
        }
        editor.set_status(status);
    }

    /// Draws the question about the first buffer.
    pub fn render(&self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let Some(name) = self
            .queue
            .first()
            .and_then(|asked| name(asked.doc, cx.editor))
        else {
            return;
        };
        let mut body = vec![
            ("r", "Reload".to_owned()),
            ("k", "Keep unsaved changes".to_owned()),
        ];
        let count = self.queue.len();
        if count > 1 {
            body.push(("R", format!("Reload all {count}")));
            body.push(("K", format!("Keep all {count}")));
        }
        let title = format!("{name} changed on disk");
        let mut info = Info::new(title.clone(), &body);
        info.width = info.width.max(title.width() as u16);
        info.render(area, surface, cx);
    }
}

/// The name a buffer goes by in messages: its path, relative to the working directory.
fn name(doc: DocumentId, editor: &Editor) -> Option<String> {
    let path = editor.document(doc)?.path()?;
    Some(get_relative_path(path).display().to_string())
}

/// Says what was `done` to the buffers `answered`.
fn summary(answered: &[Asked], done: &str, editor: &Editor) -> String {
    match answered {
        [asked] => format!("{} {done}", name(asked.doc, editor).unwrap_or_default()),
        answered => format!("{} buffers {done}", answered.len()),
    }
}
