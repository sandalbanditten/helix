//! The editor's state of spell checking, which `helix-term`'s spelling handler drives.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    future::Future,
};

use helix_core::{
    diagnostic::DiagnosticProvider, ChangeSet, Rope, SpellingLanguage, Tendril, Transaction,
};
use helix_event::{send_blocking, TaskController, TaskHandle};
use tokio::sync::mpsc::Sender;

use crate::{action::Action, events::DiagnosticsDidChange, DocumentId, Editor};

#[derive(Debug)]
pub struct SpellingHandler {
    pub event_tx: Sender<SpellingEvent>,
    /// Full-document checks, keyed by document.
    pub requests: HashMap<DocumentId, TaskController>,
    /// Dictionaries which are loading or failed to load.
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

    /// Starts a new full check for `document`, cancelling any previous one.
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
    /// A document changed; re-check the regions around the change.
    DocumentChanged {
        doc: DocumentId,
        old_text: Rope,
        text: Rope,
        changes: ChangeSet,
        version: i32,
    },
}

impl Editor {
    /// Re-resolves the spelling settings of a document and checks it in full.
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

/// Spelling actions sort after LSP code actions (which use a higher priority).
const SPELLING_ACTION_PRIORITY: u8 = 0;

/// Appends a word to the `language`'s personal dictionary file.
fn persist_to_personal_dictionary(language: &SpellingLanguage, word: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    let path = helix_loader::personal_dictionary_file(language.as_str());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{word}")
}

impl Editor {
    /// Code actions for the misspellings overlapping the primary selection: the suggestions, and
    /// adding the word to a dictionary.
    pub fn spelling_actions(
        &self,
    ) -> impl Future<Output = anyhow::Result<Vec<Action>>> + Send + 'static {
        let (view, doc) = current_ref!(self);
        // The dictionaries this document is checked against, in configuration order.
        let dictionaries: Vec<_> = doc
            .spelling_languages()
            .iter()
            .filter_map(|language| {
                Some((language.clone(), self.dictionaries.get(language)?.clone()))
            })
            .collect();
        let doc_id = doc.id();
        let view_id = view.id;
        let version = doc.version();
        let selection = doc.selection(view_id).primary();
        let text = doc.text();
        let misspellings: Vec<_> = doc
            .diagnostics()
            .iter()
            .filter(|diagnostic| {
                diagnostic.provider == DiagnosticProvider::Spelling
                    && selection.overlaps(&helix_core::Range::new(
                        diagnostic.range.start,
                        diagnostic.range.end,
                    ))
            })
            .map(|diagnostic| {
                let range = diagnostic.range;
                let word = Cow::from(text.slice(range.start..range.end)).into_owned();
                (range, word)
            })
            .collect();

        async move {
            if dictionaries.is_empty() || misspellings.is_empty() {
                return Ok(Vec::new());
            }
            let actions = tokio::task::spawn_blocking(move || {
                let mut actions = Vec::new();
                for (range, word) in misspellings {
                    // Offer the suggestions from every dictionary, in order, without duplicates.
                    let mut suggestions = Vec::new();
                    let mut candidates = Vec::new();
                    for (_, dictionary) in &dictionaries {
                        dictionary.read().suggest(&word, &mut candidates);
                        suggestions.append(&mut candidates);
                    }
                    let mut seen = HashSet::new();
                    suggestions.retain(|suggestion| seen.insert(suggestion.clone()));

                    for suggestion in suggestions {
                        let title = format!("Replace '{word}' with '{suggestion}'");
                        actions.push(Action::new(
                            title,
                            SPELLING_ACTION_PRIORITY,
                            move |editor| {
                                // An edit since the menu opened may have moved the misspelling.
                                let Some(doc) = editor.documents.get_mut(&doc_id) else {
                                    return;
                                };
                                if doc.version() != version
                                    || editor
                                        .tree
                                        .try_get(view_id)
                                        .is_none_or(|view| view.doc != doc_id)
                                {
                                    return;
                                }
                                let view = editor.tree.get_mut(view_id);
                                let transaction = Transaction::change(
                                    doc.text(),
                                    std::iter::once((
                                        range.start,
                                        range.end,
                                        Some(Tendril::from(suggestion.as_str())),
                                    )),
                                );
                                doc.apply(&transaction, view_id);
                                doc.append_changes_to_history(view);
                            },
                        ));
                    }

                    // "Add to dictionary" targets one dictionary, so offer one action per language.
                    for (language, _) in &dictionaries {
                        let language = language.clone();
                        let word = word.clone();
                        let title = format!("Add '{word}' to dictionary '{language}'");
                        actions.push(Action::new(
                            title,
                            SPELLING_ACTION_PRIORITY,
                            move |editor| editor.add_to_dictionary(&language, &word),
                        ));
                    }
                }
                actions
            })
            .await?;
            Ok(actions)
        }
    }

    /// Adds `word` to the dictionary of `language` and its personal dictionary, and re-checks the
    /// documents using it.
    fn add_to_dictionary(&mut self, language: &SpellingLanguage, word: &str) {
        let Some(dictionary) = self.dictionaries.get(language) else {
            return;
        };
        let added = dictionary.write().add(word);
        if let Err(err) = added {
            self.set_error(format!(
                "Could not add '{word}' to dictionary '{language}': {err:?}"
            ));
            return;
        }
        if let Err(err) = persist_to_personal_dictionary(language, word) {
            self.set_error(format!(
                "Could not save '{word}' to the personal dictionary '{language}': {err}"
            ));
        }
        send_blocking(
            &self.handlers.spelling.event_tx,
            SpellingEvent::DictionaryLoaded {
                language: language.clone(),
            },
        );
    }
}
