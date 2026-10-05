//! Spell checking as a non-LSP diagnostic source. Small edits are re-checked around the change on
//! the main loop; full checks run in the background.

use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
    io,
    iter::Peekable,
    ops::{ControlFlow, Range},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, Context as _};
use helix_core::{
    chars::char_is_word,
    diagnostic::{Diagnostic, DiagnosticProvider, Range as DiagnosticRange, Severity},
    diff::compare_ropes,
    encoding::Encoding,
    syntax::{config::SpellingFilter, Loader},
    ChangeSet, Rope, RopeSlice, SpellingLanguage, Syntax,
};
use helix_event::{register_hook, send_blocking, AsyncHook, TaskHandle};
use helix_stdx::rope::{Regex, RopeSliceExt as _};
use helix_view::{
    events::{
        ConfigDidChange, DiagnosticsDidChange, DocumentDidChange, DocumentDidClose,
        DocumentDidOpen, DocumentDidSave,
    },
    handlers::{
        spelling::{DictionaryLoad, SpellingEvent},
        Handlers,
    },
    Dictionary, Document, DocumentId, Editor,
};
use once_cell::sync::Lazy;
use parking_lot::{RwLock, RwLockReadGuard};
use spellbook::{ParseDictionaryError, ParseDictionaryErrorSource};
use tokio::time::Instant;

use crate::job;

const PROVIDER: DiagnosticProvider = DiagnosticProvider::Spelling;

/// How long to wait after the last change before re-checking.
const DEBOUNCE: Duration = Duration::from_secs(1);
/// Char padding around each edit when re-checking incrementally.
const WINDOW_PADDING: usize = 50;
/// The most chars re-checked incrementally on the main loop.
const MAX_INCREMENTAL_CHARS: usize = 4096;
/// Regions are checked in chunks of about this many chars.
const CHUNK_CHARS: usize = 16 * 1024;
/// A full check stops at this many misspellings.
const MAX_MISSPELLINGS: usize = 10_000;
/// The most lines of a `.dic` file that are dropped because spellbook rejects them.
const MAX_REJECTED_LINES: usize = 64;

#[derive(Debug)]
struct Change {
    old_text: Rope,
    text: Rope,
    changes: ChangeSet,
    version: i32,
    /// Whether `changes` coalesces several changes and must be recomputed from `old_text` and `text`.
    dirty: bool,
}

#[derive(Debug, Default)]
pub(super) struct SpellingHook {
    changes: HashMap<DocumentId, Change>,
}

impl AsyncHook for SpellingHook {
    type Event = SpellingEvent;

    fn handle_event(&mut self, event: Self::Event, timeout: Option<Instant>) -> Option<Instant> {
        match event {
            SpellingEvent::DictionaryLoaded { language } => {
                job::dispatch_blocking(move |editor, _| {
                    let docs: Vec<_> = editor
                        .documents()
                        .filter(|doc| doc.spelling_languages().contains(&language))
                        .map(Document::id)
                        .collect();
                    for doc in docs {
                        check_document(editor, doc);
                    }
                });
                timeout
            }
            SpellingEvent::CheckDocument { doc } => {
                // The full check supersedes a pending incremental one.
                self.changes.remove(&doc);
                job::dispatch_blocking(move |editor, _| check_document(editor, doc));
                timeout
            }
            SpellingEvent::DocumentChanged {
                doc,
                old_text,
                text,
                changes,
                version,
            } => {
                if let Some(pending) = self.changes.get_mut(&doc) {
                    // Coalesce with the pending change: keep the original `old_text`, advance to the
                    // latest `text`/`version`, and recompute the changeset lazily (see `Change`).
                    pending.text = text;
                    pending.version = version;
                    pending.dirty = true;
                } else {
                    self.changes.insert(
                        doc,
                        Change {
                            old_text,
                            text,
                            changes,
                            version,
                            dirty: false,
                        },
                    );
                }
                Some(Instant::now() + DEBOUNCE)
            }
        }
    }

    fn finish_debounce(&mut self) {
        for (doc, mut change) in self.changes.drain() {
            if change.dirty {
                change.changes = compare_ropes(&change.old_text, &change.text)
                    .changes()
                    .clone();
            }
            let changes = change.changes;
            let version = change.version;
            job::dispatch_blocking(move |editor, _| {
                recheck_document(editor, doc, changes, version);
            });
        }
    }
}

/// Re-checks a document around `changes`, or in full when that is not possible.
fn recheck_document(editor: &mut Editor, doc_id: DocumentId, changes: ChangeSet, version: i32) {
    let Some(doc) = editor.documents.get(&doc_id) else {
        return;
    };
    if doc.spelling_languages().is_empty() {
        return;
    }
    let windows = (doc.version() == version && !editor.handlers.spelling.is_checking(doc_id))
        .then(|| incremental_windows(doc.text().slice(..), &changes))
        .flatten();
    let Some(windows) = windows else {
        check_document(editor, doc_id);
        return;
    };
    let languages = doc.spelling_languages().to_vec();
    let Some(dictionaries) = lookup_dictionaries(editor, &languages) else {
        return;
    };

    let doc = doc!(editor, &doc_id);
    let filter = SpellingFilter::new(&doc.spelling_config());
    let loader = editor.syn_loader.load();
    let text = doc.text().slice(..);
    let mut scan = Scan::new(&dictionaries, &filter, None);
    for window in &windows {
        for region in spell_check_regions(doc.syntax(), &loader, text, window.clone()) {
            // An incremental check is small enough to never be capped.
            let _ = scan.check_region(text, region);
        }
    }
    let misspellings = scan.misspellings;

    // The splice clears whole windows, so a word that left a checked region (e.g. a comment edited
    // into code) loses its misspelling.
    doc_mut!(editor, &doc_id).splice_diagnostics(misspellings, &windows, &PROVIDER);
    helix_event::dispatch(DiagnosticsDidChange {
        editor,
        doc: doc_id,
    });
}

/// The char ranges of `text` to re-check around `changes`, or `None` when they would cover more
/// than [`MAX_INCREMENTAL_CHARS`].
fn incremental_windows(text: RopeSlice, changes: &ChangeSet) -> Option<Vec<Range<usize>>> {
    let mut windows: Vec<Range<usize>> = Vec::new();
    let mut len = 0;
    for (_, window) in changes.changed_ranges(WINDOW_PADDING) {
        let window = widen_to_tokens(text, window)?;
        len += window.len();
        if len > MAX_INCREMENTAL_CHARS {
            return None;
        }
        match windows.last_mut() {
            Some(last) if window.start <= last.end => last.end = last.end.max(window.end),
            _ => windows.push(window),
        }
    }
    Some(windows)
}

/// Widens a window of `text` to whitespace, or `None` when it would span more than
/// [`MAX_INCREMENTAL_CHARS`].
fn widen_to_tokens(text: RopeSlice, window: Range<usize>) -> Option<Range<usize>> {
    let token_len = |chars: &mut dyn Iterator<Item = char>| {
        chars
            .take(MAX_INCREMENTAL_CHARS + 1)
            .take_while(|ch| !ch.is_whitespace())
            .count()
    };
    let start = window.start - token_len(&mut text.chars_at(window.start).reversed());
    let end = window.end + token_len(&mut text.chars_at(window.end));
    (end - start <= MAX_INCREMENTAL_CHARS).then_some(start..end)
}

/// Checks an entire document off the main loop and replaces its misspellings wholesale.
fn check_document(editor: &mut Editor, doc_id: DocumentId) {
    let Some(doc) = editor.documents.get(&doc_id) else {
        return;
    };
    if doc.spelling_languages().is_empty() {
        return;
    }
    let languages = doc.spelling_languages().to_vec();
    let Some(dictionaries) = lookup_dictionaries(editor, &languages) else {
        return;
    };

    let doc = doc!(editor, &doc_id);
    let version = doc.version();
    let text = doc.text().clone();
    // Cloning the syntax bumps a few refcounts on its (persistent) trees; cheap enough to snapshot
    // for the off-thread check.
    let syntax = doc.syntax().cloned();
    let filter = SpellingFilter::new(&doc.spelling_config());
    let name = doc.display_name().into_owned();
    let loader = editor.syn_loader.load_full();
    let cancel = editor.handlers.spelling.open_request(doc_id);

    tokio::task::spawn_blocking(move || {
        let text = text.slice(..);
        let misspellings = check_text(
            &dictionaries,
            &filter,
            text,
            syntax.as_ref(),
            &loader,
            &cancel,
        );
        if cancel.is_canceled() {
            return;
        }
        if misspellings.len() == MAX_MISSPELLINGS {
            log::warn!("stopped spell checking {name} at {MAX_MISSPELLINGS} misspellings");
        }
        job::dispatch_blocking(move |editor, _| {
            // A newer check, a settings change or closing the document canceled this one.
            if cancel.is_canceled() {
                return;
            }
            let Some(doc) = editor.documents.get_mut(&doc_id) else {
                return;
            };
            if doc.version() != version {
                check_document(editor, doc_id);
                return;
            }
            doc.replace_diagnostics(misspellings, &[], &PROVIDER);
            helix_event::dispatch(DiagnosticsDidChange {
                editor,
                doc: doc_id,
            });
        });
    });
}

/// Checks every spell-checked region of `text`, until canceled or capped.
fn check_text(
    dictionaries: &[Arc<RwLock<Dictionary>>],
    filter: &SpellingFilter,
    text: RopeSlice,
    syntax: Option<&Syntax>,
    loader: &Loader,
    cancel: &TaskHandle,
) -> Vec<Diagnostic> {
    let mut scan = Scan::new(dictionaries, filter, Some(cancel));
    for region in spell_check_regions(syntax, loader, text, 0..text.len_chars()) {
        if scan.check_region(text, region).is_break() {
            break;
        }
    }
    scan.misspellings
}

/// Returns the dictionaries for `languages`, or `None` while any are still loading.
fn lookup_dictionaries(
    editor: &mut Editor,
    languages: &[SpellingLanguage],
) -> Option<Vec<Arc<RwLock<Dictionary>>>> {
    let mut dictionaries = Vec::with_capacity(languages.len());
    let mut missing = false;
    for language in languages {
        // Call through for every language so all missing loads are kicked off, not just the first.
        match lookup_dictionary(editor, language) {
            Some(dictionary) => dictionaries.push(dictionary),
            None => missing = true,
        }
    }
    (!missing).then_some(dictionaries)
}

/// Returns the dictionary for `language`, kicking off an async load (once) if it isn't loaded yet.
fn lookup_dictionary(
    editor: &mut Editor,
    language: &SpellingLanguage,
) -> Option<Arc<RwLock<Dictionary>>> {
    if let Some(dictionary) = editor.dictionaries.get(language) {
        return Some(dictionary.clone());
    }
    if let Entry::Vacant(entry) = editor
        .handlers
        .spelling
        .dictionary_loads
        .entry(language.clone())
    {
        entry.insert(DictionaryLoad::Loading);
        load_dictionary(language.clone());
    }
    None
}

fn load_dictionary(language: SpellingLanguage) {
    tokio::task::spawn_blocking(move || {
        let dictionary = read_dictionary(&language);
        job::dispatch_blocking(move |editor, _| match dictionary {
            Ok(dictionary) => {
                editor.handlers.spelling.dictionary_loads.remove(&language);
                editor
                    .dictionaries
                    .insert(language.clone(), Arc::new(RwLock::new(dictionary)));
                send_blocking(
                    &editor.handlers.spelling.event_tx,
                    SpellingEvent::DictionaryLoaded { language },
                );
            }
            Err(err) => {
                log::error!("could not load spelling dictionary '{language}': {err:#}");
                editor.set_error(format!(
                    "Could not load spelling dictionary '{language}': {err:#}"
                ));
                editor
                    .handlers
                    .spelling
                    .dictionary_loads
                    .insert(language, DictionaryLoad::Failed);
            }
        });
    });
}

/// Reads the dictionary of `language` from the runtime directories, with the words of its personal
/// dictionary added.
fn read_dictionary(language: &SpellingLanguage) -> anyhow::Result<Dictionary> {
    let read = |extension: &str| {
        let path =
            helix_loader::runtime_file(format!("dictionaries/{language}/{language}.{extension}"));
        std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))
    };
    let aff = read("aff")?;
    let dic = read("dic")?;
    let encoding = dictionary_encoding(&aff)?;
    let mut dictionary = parse_dictionary(&decode(encoding, &aff), decode(encoding, &dic).into())?;

    let path = helix_loader::personal_dictionary_file(language.as_str());
    match std::fs::read_to_string(&path) {
        Ok(words) => {
            for word in words.lines().map(str::trim).filter(|word| !word.is_empty()) {
                if let Err(err) = dictionary.add(word) {
                    log::warn!("ignoring personal dictionary entry {word:?}: {err:?}");
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => (),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
    Ok(dictionary)
}

/// The encoding of a dictionary's files, which its `.aff` file names with the `SET` directive.
fn dictionary_encoding(aff: &[u8]) -> anyhow::Result<&'static Encoding> {
    let label = aff.split(|&byte| byte == b'\n').find_map(|line| {
        let mut words = line
            .split(u8::is_ascii_whitespace)
            .filter(|word| !word.is_empty());
        (words.next() == Some(b"SET"))
            .then(|| words.next())
            .flatten()
    });
    let Some(label) = label else {
        return Ok(helix_core::encoding::UTF_8);
    };
    // Hunspell spells Windows-1251 as `microsoft-cp1251`; the other names are also encoding labels.
    let label = label.to_ascii_lowercase();
    let label = label.strip_prefix(b"microsoft-").unwrap_or(&label);
    Encoding::for_label(label).ok_or_else(|| {
        anyhow!(
            "unsupported encoding '{}'",
            String::from_utf8_lossy(label).trim()
        )
    })
}

fn decode<'a>(encoding: &'static Encoding, bytes: &'a [u8]) -> Cow<'a, str> {
    let (text, _, malformed) = encoding.decode(bytes);
    if malformed {
        log::warn!("replaced malformed {} in a dictionary", encoding.name());
    }
    text
}

/// Parses a dictionary, dropping the lines of the `.dic` file it cannot read.
fn parse_dictionary(aff: &str, mut dic: String) -> anyhow::Result<Dictionary> {
    for _ in 0..MAX_REJECTED_LINES {
        match Dictionary::new(aff, &dic) {
            Ok(dictionary) => return Ok(dictionary),
            Err(ParseDictionaryError {
                kind,
                source: ParseDictionaryErrorSource::Dic,
                line_number: Some(line),
            }) if line > 1 => {
                let entry = remove_line(&mut dic, line);
                log::warn!("skipping dictionary entry {:?}: {kind}", entry.trim_end());
            }
            Err(err) => return Err(anyhow!("{err}")),
        }
    }
    Dictionary::new(aff, &dic).map_err(|err| anyhow!("{err}"))
}

/// Removes the 1-indexed line `number` from `text` and returns it.
fn remove_line(text: &mut String, number: usize) -> String {
    let start = text
        .match_indices('\n')
        .nth(number - 2)
        .map_or(text.len(), |(newline, _)| newline + 1);
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |newline| start + newline + 1);
    text.drain(start..end).collect()
}

/// The char ranges within `region` that the `spellcheck.scm` queries select, or all of it
/// without a syntax tree.
//
// `Syntax::spell_regions` works in byte offsets (tree-sitter's native unit) while the spelling
// diagnostics, like all diagnostics, are in char offsets, so we convert at this boundary. The
// conversions go away once diagnostics move to byte offsets.
fn spell_check_regions(
    syntax: Option<&Syntax>,
    loader: &Loader,
    text: RopeSlice,
    region: Range<usize>,
) -> Vec<Range<usize>> {
    let Some(syntax) = syntax else {
        return vec![region];
    };
    let bytes = text.char_to_byte(region.start)..text.char_to_byte(region.end);
    syntax
        .spell_regions(text, loader, bytes)
        .into_iter()
        .map(|region| text.byte_to_char(region.start)..text.byte_to_char(region.end))
        .collect()
}

/// A word: a run of letters, marks and digits, up to a camelCase hump.
const WORD: &str = r"[\p{Lu}\p{Lt}\p{Nd}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?:['’-][\p{Ll}\p{Lm}\p{Lo}\p{M}]+)*|[\p{Lu}\p{Lt}\p{Nd}]+";
static WORDS: Lazy<Regex> = Lazy::new(|| Regex::new(WORD).unwrap());
/// Words joined by dots and ending in one, like `f.eks.`, `Dvs.` or the end of a sentence, which a
/// dictionary may know as a whole, as an abbreviation.
static DOTTED_WORDS: Lazy<Regex> =
    Lazy::new(|| Regex::new(&format!(r"(?:{WORD})+(?:\.(?:{WORD})+)*\.")).unwrap());
/// URLs and email addresses, whose words are skipped.
static IGNORED_SPANS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[a-zA-Z][a-zA-Z0-9+.-]*://\S+|[\w.+-]+@[A-Za-z0-9-]+\.[\w.-]+").unwrap()
});

/// Checks the words in regions of a text against a document's dictionaries.
struct Scan<'a> {
    dictionaries: &'a [Arc<RwLock<Dictionary>>],
    guards: Vec<RwLockReadGuard<'a, Dictionary>>,
    filter: &'a SpellingFilter,
    cancel: Option<&'a TaskHandle>,
    misspellings: Vec<Diagnostic>,
}

impl<'a> Scan<'a> {
    fn new(
        dictionaries: &'a [Arc<RwLock<Dictionary>>],
        filter: &'a SpellingFilter,
        cancel: Option<&'a TaskHandle>,
    ) -> Self {
        Self {
            dictionaries,
            guards: Vec::with_capacity(dictionaries.len()),
            filter,
            cancel,
            misspellings: Vec::new(),
        }
    }

    /// Checks the words in the char `region` of `text`, until canceled or [`MAX_MISSPELLINGS`].
    fn check_region(&mut self, text: RopeSlice, region: Range<usize>) -> ControlFlow<()> {
        let mut start = region.start;
        while start < region.end {
            // Chunks end at whitespace, which neither words nor URLs span.
            let end = match start + CHUNK_CHARS {
                end if end >= region.end => region.end,
                end => {
                    end + text
                        .chars_at(end)
                        .take(region.end - end)
                        .take_while(|ch| !ch.is_whitespace())
                        .count()
                }
            };
            self.guards.clear();
            if self.cancel.is_some_and(TaskHandle::is_canceled) {
                return ControlFlow::Break(());
            }
            self.guards
                .extend(self.dictionaries.iter().map(|dictionary| dictionary.read()));
            self.check_chunk(text, start..end)?;
            start = end;
        }
        ControlFlow::Continue(())
    }

    fn check_chunk(&mut self, text: RopeSlice, chunk: Range<usize>) -> ControlFlow<()> {
        let input = || text.regex_input_at(chunk.clone());
        let mut ignored_spans = IGNORED_SPANS
            .find_iter(input())
            .map(|span| span.range())
            .peekable();
        let guards = &self.guards;
        let mut known_dotted_words = DOTTED_WORDS
            .find_iter(input())
            .map(|span| span.range())
            .filter(|span| {
                let words = Cow::from(text.byte_slice(span.clone()));
                guards.iter().any(|dictionary| dictionary.check(&words))
            })
            .peekable();
        for word in WORDS.find_iter(input()) {
            let range = word.range();
            if overlaps(&mut ignored_spans, &range) || overlaps(&mut known_dotted_words, &range) {
                continue;
            }
            let word = Cow::from(text.byte_slice(range.clone()));
            if self.filter.ignores(&word)
                || self.guards.iter().any(|dictionary| dictionary.check(&word))
            {
                continue;
            }
            if self.misspellings.len() == MAX_MISSPELLINGS {
                return ControlFlow::Break(());
            }
            let range = text.byte_to_char(range.start)..text.byte_to_char(range.end);
            self.misspellings.push(misspelling(text, range, &word));
        }
        ControlFlow::Continue(())
    }
}

/// Whether `word` overlaps one of the ordered `spans`, dropping the spans before it.
fn overlaps(spans: &mut Peekable<impl Iterator<Item = Range<usize>>>, word: &Range<usize>) -> bool {
    // The words are ordered too, so a span that ends before this word ends before all the
    // following ones.
    while spans.next_if(|span| span.end <= word.start).is_some() {}
    spans.peek().is_some_and(|span| span.start < word.end)
}

fn misspelling(text: RopeSlice, range: Range<usize>, word: &str) -> Diagnostic {
    let Range { start, end } = range;
    // Mirror `lsp_diagnostic_to_diagnostic` so edit-mapping associations behave the same.
    let ends_at_word = start != end && end != 0 && text.get_char(end - 1).is_some_and(char_is_word);
    let starts_at_word = start != end && text.get_char(start).is_some_and(char_is_word);
    Diagnostic {
        range: DiagnosticRange { start, end },
        ends_at_word,
        starts_at_word,
        zero_width: start == end,
        line: text.char_to_line(start),
        message: format!("Possible spelling mistake: '{word}'"),
        severity: Some(Severity::Hint),
        code: None,
        provider: PROVIDER,
        tags: Vec::new(),
        source: Some(Cow::Borrowed("spelling")),
        data: None,
    }
}

pub(super) fn register_hooks(handlers: &Handlers) {
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        // The language and `.editorconfig` of the document are known now.
        event.editor.refresh_spelling(event.doc);
        Ok(())
    });

    let tx = handlers.spelling.event_tx.clone();
    register_hook!(move |event: &mut DocumentDidChange<'_>| {
        // Mirror the word index: ignore synthetic edits so they don't churn the diagnostics.
        if !event.ghost_transaction && !event.doc.spelling_languages().is_empty() {
            send_blocking(
                &tx,
                SpellingEvent::DocumentChanged {
                    doc: event.doc.id(),
                    old_text: event.old_text.clone(),
                    text: event.doc.text().clone(),
                    changes: event.changes.clone(),
                    version: event.doc.version(),
                },
            );
        }
        Ok(())
    });

    register_hook!(move |event: &mut DocumentDidSave<'_>| {
        // An edit can move text into or out of the checked regions beyond the window the
        // incremental check covers, like an opening code fence, and change the detected language,
        // so saving checks in full.
        event.editor.refresh_spelling(event.doc);
        Ok(())
    });

    register_hook!(move |event: &mut DocumentDidClose<'_>| {
        // Cancel any in-flight full check for the closed document.
        event
            .editor
            .handlers
            .spelling
            .requests
            .remove(&event.doc.id());
        Ok(())
    });

    register_hook!(move |event: &mut ConfigDidChange<'_>| {
        // A config change can enable, disable or change the spelling of any document, and a
        // reload may fix a dictionary that failed to load.
        event.editor.handlers.spelling.retry_failed_dictionaries();
        let docs: Vec<_> = event.editor.documents().map(Document::id).collect();
        for doc in docs {
            event.editor.refresh_spelling(doc);
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use helix_core::syntax::config::SpellingConfig;
    use helix_event::TaskController;

    fn dictionary(language: &str) -> Arc<RwLock<Dictionary>> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../runtime/dictionaries");
        let read = |extension| std::fs::read(format!("{dir}/{language}/{language}.{extension}"));
        let aff = read("aff").unwrap();
        let encoding = dictionary_encoding(&aff).unwrap();
        let dic = decode(encoding, &read("dic").unwrap()).into_owned();
        let dictionary = parse_dictionary(&decode(encoding, &aff), dic).unwrap();
        Arc::new(RwLock::new(dictionary))
    }

    /// The `en_US` dictionary vendored under `runtime/dictionaries/`.
    fn en_us() -> Arc<RwLock<Dictionary>> {
        static EN_US: Lazy<Arc<RwLock<Dictionary>>> = Lazy::new(|| dictionary("en_US"));
        EN_US.clone()
    }

    /// The `da_DK` dictionary vendored under `runtime/dictionaries/`.
    fn da_dk() -> Arc<RwLock<Dictionary>> {
        static DA_DK: Lazy<Arc<RwLock<Dictionary>>> = Lazy::new(|| dictionary("da_DK"));
        DA_DK.clone()
    }

    /// A throwaway dictionary containing exactly `words`.
    fn mini_dictionary(words: &[&str]) -> Arc<RwLock<Dictionary>> {
        let dic = format!("{}\n{}\n", words.len(), words.join("\n"));
        Arc::new(RwLock::new(Dictionary::new("SET UTF-8\n", &dic).unwrap()))
    }

    /// A filter that skips nothing (the default config).
    fn no_filter() -> SpellingFilter {
        SpellingFilter::new(&SpellingConfig::default())
    }

    fn check_with(
        dictionaries: &[Arc<RwLock<Dictionary>>],
        filter: &SpellingFilter,
        text: &str,
    ) -> Vec<String> {
        let rope = Rope::from_str(text);
        let mut scan = Scan::new(dictionaries, filter, None);
        let _ = scan.check_region(rope.slice(..), 0..rope.len_chars());
        scan.misspellings
            .iter()
            .map(|diagnostic| {
                rope.slice(diagnostic.range.start..diagnostic.range.end)
                    .to_string()
            })
            .collect()
    }

    fn check(text: &str) -> Vec<String> {
        check_with(&[en_us()], &no_filter(), text)
    }

    #[test]
    fn a_word_known_to_any_dictionary_is_accepted() {
        // "wrld" is not in en_US, so en_US alone flags it.
        assert_eq!(check("wrld"), ["wrld"]);
        // A second dictionary that knows "wrld" makes the OR accept it.
        let dictionaries = [en_us(), mini_dictionary(&["wrld"])];
        assert!(check_with(&dictionaries, &no_filter(), "wrld").is_empty());
    }

    #[test]
    fn flags_only_the_misspelled_word() {
        let rope = Rope::from_str("the quik brown fox");
        let dictionaries = [en_us()];
        let filter = no_filter();
        let mut scan = Scan::new(&dictionaries, &filter, None);
        let _ = scan.check_region(rope.slice(..), 0..18);
        assert_eq!(scan.misspellings.len(), 1, "{:?}", scan.misspellings);
        let d = &scan.misspellings[0];
        assert_eq!((d.range.start, d.range.end), (4, 8));
        assert_eq!(d.provider, DiagnosticProvider::Spelling);
        assert_eq!(d.severity, Some(Severity::Hint));
        assert_eq!(d.message, "Possible spelling mistake: 'quik'");
        assert!(d.starts_at_word && d.ends_at_word && !d.zero_width);
    }

    #[test]
    fn region_scopes_the_scan() {
        // The same misspelling appears twice; only the one inside the region is reported.
        let rope = Rope::from_str("quik brown quik");
        let dictionaries = [en_us()];
        let filter = no_filter();
        let mut scan = Scan::new(&dictionaries, &filter, None);
        let _ = scan.check_region(rope.slice(..), 11..15);
        let ranges: Vec<_> = scan
            .misspellings
            .iter()
            .map(|d| (d.range.start, d.range.end))
            .collect();
        assert_eq!(ranges, [(11, 15)]);
    }

    #[test]
    fn offsets_are_char_indices_across_multibyte_text() {
        // A 4-byte emoji precedes the misspelling: the diagnostic range must be in chars (2..6),
        // not bytes (5..9), exercising the byte→char conversion.
        let rope = Rope::from_str("🚀 quik");
        let dictionaries = [en_us()];
        let filter = no_filter();
        let mut scan = Scan::new(&dictionaries, &filter, None);
        let _ = scan.check_region(rope.slice(..), 0..6);
        let ranges: Vec<_> = scan
            .misspellings
            .iter()
            .map(|d| (d.range.start, d.range.end))
            .collect();
        assert_eq!(ranges, [(2, 6)]);
    }

    #[test]
    fn tokenizes_unicode_words() {
        // A dictionary without words flags every token, which shows how the text is tokenized.
        let tokens = |text| check_with(&[mini_dictionary(&[])], &no_filter(), text);
        assert_eq!(
            tokens("smørrebrød Ærø naïve"),
            ["smørrebrød", "Ærø", "naïve"]
        );
        assert_eq!(
            tokens("parseHTTP snake_case"),
            ["parse", "HTTP", "snake", "case"]
        );
        assert_eq!(tokens("don't it’s e-mail"), ["don't", "it’s", "e-mail"]);
        assert_eq!(
            tokens("'quoted' -dash- O'Brien"),
            ["quoted", "dash", "O", "Brien"]
        );
    }

    #[test]
    fn checks_danish() {
        let text = "smørrebrød sommerhusudlejning kærlihed Ærø hvorden";
        assert_eq!(
            check_with(&[da_dk()], &no_filter(), text),
            ["kærlihed", "hvorden"]
        );
    }

    #[test]
    fn knows_abbreviations_by_their_dots() {
        let danish = |text| check_with(&[da_dk()], &no_filter(), text);
        assert!(danish("Dvs. det er f.eks. godt, bl.a. osv. Kl. 5 er ca. 3 timer.").is_empty());
        // A misspelling leaves out the dot after it.
        assert_eq!(
            danish("Dvs det er gdot. Dsv. f.esk."),
            ["Dvs", "gdot", "Dsv", "esk"]
        );
        // Dotted words no dictionary knows are checked one by one.
        assert!(check("Call self.offset. The end.").is_empty());
    }

    #[test]
    fn skips_words_inside_urls_and_emails() {
        // Only the prose misspelling "teh" is flagged; the misspelled-looking host/path fragments
        // ("barbaz", "exampel") inside the URL and email are skipped.
        assert_eq!(
            check("teh https://github.com/foo/barbaz me@exampel.org wrld"),
            ["teh", "wrld"]
        );
    }

    #[test]
    fn filter_skips_allowlisted_short_and_ignored_words() {
        let filter = SpellingFilter::new(&SpellingConfig {
            words: vec!["Helix".into()],
            ignore_regexes: vec!["^[A-Z0-9_]+$".into()],
            min_word_length: Some(3),
            ..Default::default()
        });
        // "Helix" is allowlisted (case-insensitively), "HE" is too short, and "ABC123" matches the
        // ignore regex; only the genuine misspelling "teh" survives.
        assert_eq!(
            check_with(&[en_us()], &filter, "Helix helix HE teh ABC123"),
            ["teh"]
        );
    }

    #[test]
    fn scan_stops_when_canceled() {
        let mut controller = TaskController::new();
        let cancel = controller.restart();
        let text = Rope::from_str(&"qwx ".repeat(CHUNK_CHARS));
        let dictionaries = [en_us()];
        let filter = no_filter();
        let mut scan = Scan::new(&dictionaries, &filter, Some(&cancel));
        let flow = scan.check_region(text.slice(..), 0..CHUNK_CHARS);
        assert!(flow.is_continue());
        let checked = scan.misspellings.len();

        controller.cancel();
        let flow = scan.check_region(text.slice(..), CHUNK_CHARS..text.len_chars());
        assert!(flow.is_break());
        assert_eq!(scan.misspellings.len(), checked);
    }

    #[test]
    fn chunks_end_at_whitespace() {
        // Every word of a region longer than a chunk is checked whole.
        let text = format!("{} qwx wrld", "a".repeat(CHUNK_CHARS - 2));
        assert_eq!(check(&text)[1..], ["qwx", "wrld"]);
    }

    #[test]
    fn scan_stops_at_max_misspellings() {
        let text = Rope::from_str(&"qwx ".repeat(MAX_MISSPELLINGS + 1));
        let dictionaries = [en_us()];
        let filter = no_filter();
        let mut scan = Scan::new(&dictionaries, &filter, None);
        let flow = scan.check_region(text.slice(..), 0..text.len_chars());
        assert!(flow.is_break());
        assert_eq!(scan.misspellings.len(), MAX_MISSPELLINGS);
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn incremental_windows_cover_whole_tokens() {
        let windows = |text: &str, at: usize| {
            let text = Rope::from_str(text);
            let transaction = helix_core::Transaction::insert(
                &text,
                &helix_core::Selection::point(at),
                "x".into(),
            );
            let mut new_text = text.clone();
            transaction.apply(&mut new_text);
            incremental_windows(new_text.slice(..), transaction.changes())
        };

        // Padding the edit at the end of the URL starts the window within it, so it is widened to
        // all of the URL.
        let prose = "prose ".repeat(20);
        let url = format!("https://example.com/{}", "segment/".repeat(10));
        let text = format!("{prose}{url} end");
        let end = text.chars().count() + 1;
        assert_eq!(
            windows(&text, prose.len() + url.len()),
            Some(vec![prose.len()..end])
        );

        // A window which can't be widened to whitespace within the limit is not checked here.
        let text = "x".repeat(MAX_INCREMENTAL_CHARS * 2);
        assert_eq!(windows(&text, 10), None);
    }

    #[test]
    fn lenient_dictionary_parsing() {
        let aff = "SET UTF-8\nFLAG num\n";
        let dic = "3\nhus/1\n\"A/S\"\nbil\n".to_string();
        let dictionary = parse_dictionary(aff, dic).unwrap();
        assert!(dictionary.check("hus") && dictionary.check("bil"));
    }

    #[test]
    fn decodes_dictionaries_by_their_encoding() {
        let aff = b"# Latin-1\nSET ISO8859-1\nTRY abc\n";
        let encoding = dictionary_encoding(aff).unwrap();
        assert_eq!(decode(encoding, b"1\nh\xe6k\n"), "1\nhæk\n");
        assert_eq!(
            dictionary_encoding(b"SET microsoft-cp1251\n").unwrap(),
            helix_core::encoding::WINDOWS_1251
        );
        assert_eq!(
            dictionary_encoding(b"TRY abc\n").unwrap(),
            helix_core::encoding::UTF_8
        );
        assert!(dictionary_encoding(b"SET ISCII-DEVANAGARI\n").is_err());
    }

    #[test]
    fn syntax_scoping_checks_comments_not_code() {
        // `teh` in the comment is a misspelling; the identically misspelled identifier `teh_value`
        // is code and must not be flagged.
        let loader = helix_core::config::default_lang_loader();
        let rope = Rope::from_str("// teh bug\nlet teh_value = 1;\n");
        let language = loader.language_for_name("rust").unwrap();
        let syntax = Syntax::new(rope.slice(..), language, &loader).unwrap();
        let dictionaries = [en_us()];
        let filter = no_filter();
        let mut scan = Scan::new(&dictionaries, &filter, None);
        for region in
            spell_check_regions(Some(&syntax), &loader, rope.slice(..), 0..rope.len_chars())
        {
            let _ = scan.check_region(rope.slice(..), region);
        }
        let ranges: Vec<_> = scan
            .misspellings
            .iter()
            .map(|d| (d.range.start, d.range.end))
            .collect();
        assert_eq!(ranges, [(3, 6)], "only the comment occurrence");
    }
}
