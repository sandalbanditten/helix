//! Tells language servers which document the focused view shows and the first line it shows
//! (`helix/viewport`), so that previews can follow the editor. Servers opt in with the
//! experimental capability `helixViewport`.

use std::time::Duration;

use helix_event::{register_hook, send_blocking, AsyncHook};
use helix_lsp::lsp;
use helix_view::events::LanguageServerInitialized;
use helix_view::handlers::{lsp::ViewportEvent, Handlers};
use helix_view::{current_ref, DocumentId, Editor};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::job;

/// The `helix/viewport` notification.
enum HelixViewport {}

impl lsp::notification::Notification for HelixViewport {
    type Params = HelixViewportParams;
    const METHOD: &'static str = "helix/viewport";
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HelixViewportParams {
    uri: lsp::Url,
    top_line: u32,
}

#[derive(Default)]
pub(super) struct Handler {
    last: Option<ViewportEvent>,
}

impl AsyncHook for Handler {
    type Event = ViewportEvent;

    fn handle_event(&mut self, event: ViewportEvent, timeout: Option<Instant>) -> Option<Instant> {
        self.last = Some(event);
        // Throttle rather than debounce: a long scroll keeps the server up to date.
        timeout.or_else(|| Some(Instant::now() + Duration::from_millis(30)))
    }

    fn finish_debounce(&mut self) {
        if let Some(event) = self.last.take() {
            job::dispatch_blocking(move |editor, _| notify(editor, &event));
        }
    }
}

fn notify(editor: &Editor, event: &ViewportEvent) {
    let Some(doc) = editor.document(event.doc) else {
        return;
    };
    let Some(uri) = doc.url() else {
        return;
    };
    for server in doc.language_servers() {
        let wants = server.is_initialized()
            && server
                .capabilities()
                .experimental
                .as_ref()
                .and_then(|experimental| experimental.get("helixViewport"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        if wants {
            server.notify::<HelixViewport>(HelixViewportParams {
                uri: uri.clone(),
                top_line: event.top_line as u32,
            });
        }
    }
}

/// The focused view's document and the first line it shows.
fn focused(editor: &Editor) -> ViewportEvent {
    let (view, doc) = current_ref!(editor);
    let text = doc.text();
    ViewportEvent {
        doc: doc.id(),
        top_line: text.char_to_line(doc.view_offset(view.id).anchor.min(text.len_chars())),
    }
}

/// Sends the focused view's document and top line when they differ from `last`. Called on every
/// render, so it sees the view's scroll target, not the frames animating towards it.
pub fn follow(editor: &Editor, last: &mut Option<(DocumentId, usize)>) {
    let event = focused(editor);
    let current = Some((event.doc, event.top_line));
    if *last != current {
        *last = current;
        send_blocking(&editor.handlers.viewport, event);
    }
}

pub(super) fn register_hooks(handlers: &Handlers) {
    let tx = handlers.viewport.clone();
    // A server that just started hasn't heard about the view yet.
    register_hook!(move |event: &mut LanguageServerInitialized<'_>| {
        send_blocking(&tx, focused(event.editor));
        Ok(())
    });
}
