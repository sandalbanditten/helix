//! Spell checking as a non-LSP diagnostic source.
//!
//! This is the editor-side state for the spell checker. The detection logic (the debounced hook,
//! dictionary loading and the word checking itself) lives in `helix-term`'s spelling handler, which
//! drives this state through [`SpellingEvent`]s and the editor's dictionaries.

use std::collections::HashMap;

use helix_core::{diagnostic::DiagnosticProvider, ChangeSet, Rope, SpellingLanguage};
use helix_event::{send_blocking, TaskController, TaskHandle};
use tokio::sync::mpsc::Sender;

use crate::{events::DiagnosticsDidChange, DocumentId, Editor};

#[derive(Debug)]
pub struct SpellingHandler {
    pub event_tx: Sender<SpellingEvent>,
    /// Full-document checks, keyed by document. Starting a new full check for a document cancels
    /// the previous one (incremental checks run synchronously and need no cancellation).
    pub requests: HashMap<DocumentId, TaskController>,
    /// Dictionaries which are loading or failed to load, so the same one isn't loaded twice
    /// concurrently and a missing one isn't retried on every check.
    pub dictionary_loads: HashMap<SpellingLanguage, DictionaryLoad>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryLoad {
    Loading,
    Failed,
}

impl SpellingHandler {
    pub fn new(event_tx: Sender<SpellingEvent>) -> Self {
        Self {
            event_tx,
            requests: HashMap::new(),
            dictionary_loads: HashMap::new(),
        }
    }

    /// Starts a new full check for `document`, cancelling any previous one, and returns a handle
    /// the background task uses to observe cancellation.
    pub fn open_request(&mut self, document: DocumentId) -> TaskHandle {
        self.requests.entry(document).or_default().restart()
    }

    /// Whether a full check of `document` is in flight.
    pub fn is_checking(&self, document: DocumentId) -> bool {
        self.requests
            .get(&document)
            .is_some_and(TaskController::is_running)
    }

    /// Forgets the dictionaries which failed to load, so the next check tries to load them again.
    pub fn retry_failed_dictionaries(&mut self) {
        self.dictionary_loads
            .retain(|_, load| *load != DictionaryLoad::Failed);
    }
}

#[derive(Debug)]
pub enum SpellingEvent {
    /// A dictionary finished loading or changed; re-check the open documents that use it.
    DictionaryLoaded { language: SpellingLanguage },
    /// A document was opened, saved or its spelling settings changed; check it in full.
    CheckDocument { doc: DocumentId },
    /// A document changed; re-check the regions around the change (or rescan, see the term-side
    /// handler). Carries the snapshot needed to recompute the affected regions.
    DocumentChanged {
        doc: DocumentId,
        old_text: Rope,
        text: Rope,
        changes: ChangeSet,
        version: i32,
    },
}

impl Editor {
    /// Re-resolves the spelling settings of a document after they may have changed, and checks it
    /// in full, or clears its misspellings when spell checking is now off. Cancels any full check
    /// in flight.
    pub fn refresh_spelling(&mut self, doc_id: DocumentId) {
        self.handlers.spelling.requests.remove(&doc_id);
        let Some(doc) = self.documents.get_mut(&doc_id) else {
            return;
        };
        doc.detect_spelling();
        if !doc.spelling_languages().is_empty() {
            send_blocking(
                &self.handlers.spelling.event_tx,
                SpellingEvent::CheckDocument { doc: doc_id },
            );
        } else if doc
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.provider == DiagnosticProvider::Spelling)
        {
            doc.replace_diagnostics([], &[], &DiagnosticProvider::Spelling);
            helix_event::dispatch(DiagnosticsDidChange {
                editor: self,
                doc: doc_id,
            });
        }
    }
}
