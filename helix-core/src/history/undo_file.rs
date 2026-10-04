//! Undo files: a [`History`] on disk, as a log of records that each write of the file appends to.
//!
//! An undo file starts with a header (`HXUNDO`, a version byte and the time of the root
//! revision) followed by records, each a little-endian `u32` length, a kind byte and a payload:
//! the file's path, a revision, a write of a revision, or the head revision the file holds.

use std::{
    collections::BTreeSet,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use smallvec::SmallVec;

use super::{History, Revision};
use crate::{ChangeSet, Operation, Range, Selection, Transaction};

const MAGIC: &[u8] = b"HXUNDO";
const VERSION: u8 = 1;

const PATH: u8 = 0;
const REVISION: u8 = 1;
const WRITE: u8 = 2;
const HEAD: u8 = 3;

/// The hash of a text: SHA-256 as the caller computes it.
pub type TextHash = [u8; 32];

/// The revision the file holds, and what its text is like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head {
    pub revision: usize,
    /// The length of the text, in bytes.
    pub len: usize,
    pub hash: TextHash,
}

/// What an undo file holds.
#[derive(Debug)]
pub struct UndoFile {
    /// The file the history belongs to.
    pub path: Option<PathBuf>,
    /// The history, at the head.
    pub history: History,
    pub head: Head,
}

/// Why an undo file cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// It is no undo file, or one of another version.
    Header,
    /// It holds records that make no sense.
    Malformed(&'static str),
    /// It records no head, so nothing tells which revision the file holds.
    NoHead,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Header => f.write_str("not an undo file of this version"),
            Self::Malformed(what) => write!(f, "malformed undo file: {what}"),
            Self::NoHead => f.write_str("undo file without a head"),
        }
    }
}

impl std::error::Error for Error {}

/// The header of a new undo file for `history`.
pub fn header(history: &History) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.push(VERSION);
    put_time(&mut out, history.revisions[0].timestamp);
    out
}

/// Appends a record of `path`, the file the history belongs to.
pub fn put_path(out: &mut Vec<u8>, path: &Path) -> Option<()> {
    let path = path.to_str()?;
    record(out, PATH, |out| put_str(out, path));
    Some(())
}

/// Appends records of the revisions of `history` from `from` on, which follow the ones already
/// in the file.
pub fn put_revisions(out: &mut Vec<u8>, history: &History, from: usize) {
    for revision in &history.revisions[from.max(1)..] {
        record(out, REVISION, |out| {
            put_usize(out, revision.parent);
            put_time(out, revision.timestamp);
            put_transaction(out, &revision.transaction);
            put_transaction(out, &revision.inversion);
        });
    }
}

/// Appends the records of a write of the file: of the revision written, which is the new head.
pub fn put_written(out: &mut Vec<u8>, head: &Head) {
    record(out, WRITE, |out| put_usize(out, head.revision));
    record(out, HEAD, |out| {
        put_usize(out, head.revision);
        put_usize(out, head.len);
        out.extend_from_slice(&head.hash);
    });
}

/// The records of `history` for an undo file of `path`, up to the writes made before.
pub fn put_whole(out: &mut Vec<u8>, history: &History, path: &Path) -> Option<()> {
    put_path(out, path)?;
    put_revisions(out, history, 1);
    for &revision in &history.saves {
        record(out, WRITE, |out| put_usize(out, revision));
    }
    Some(())
}

/// Reads the undo file `bytes`.
pub fn read(bytes: &[u8]) -> Result<UndoFile, Error> {
    let mut reader = Reader::new(bytes)?;
    let mut history = History::default();
    history.revisions[0].timestamp = reader.root_time;
    let mut path = None;
    let mut head = None;
    while let Some((kind, mut payload)) = reader.next_record() {
        match kind {
            PATH => path = Some(PathBuf::from(payload.str()?)),
            REVISION => {
                let index = history.revisions.len();
                let parent = payload.usize()?;
                if parent >= index {
                    return Err(Error::Malformed("a revision before its parent"));
                }
                let timestamp = payload.time()?;
                let transaction = payload.transaction()?;
                let inversion = payload.transaction()?;
                if inversion.selection().is_none() {
                    return Err(Error::Malformed("an inversion without a selection"));
                }
                history.revisions[parent].last_child = NonZeroUsize::new(index);
                history.revisions.push(Revision {
                    parent,
                    last_child: None,
                    transaction,
                    inversion,
                    timestamp,
                });
            }
            WRITE => {
                let revision = payload.usize()?;
                if revision >= history.revisions.len() {
                    return Err(Error::Malformed("a write of an unknown revision"));
                }
                history.record_save(revision);
            }
            HEAD => {
                let revision = payload.usize()?;
                if revision >= history.revisions.len() {
                    return Err(Error::Malformed("a head of an unknown revision"));
                }
                let len = payload.usize()?;
                let hash = payload.hash()?;
                head = Some(Head {
                    revision,
                    len,
                    hash,
                });
            }
            _ => return Err(Error::Malformed("an unknown record")),
        }
        if !payload.is_empty() {
            return Err(Error::Malformed("a record longer than its contents"));
        }
    }
    let head = head.ok_or(Error::NoHead)?;
    history.current = head.revision;
    Ok(UndoFile {
        path,
        history,
        head,
    })
}

/// The file the undo file `bytes` belongs to, if it tells.
pub fn read_path(bytes: &[u8]) -> Option<PathBuf> {
    let mut reader = Reader::new(bytes).ok()?;
    let mut path = None;
    while let Some((kind, mut payload)) = reader.next_record() {
        if kind == PATH {
            path = Some(PathBuf::from(payload.str().ok()?));
        }
    }
    path
}

impl History {
    /// Drops the oldest revisions until at most `max` are left, keeping the current one and its
    /// ancestors. Returns whether any were dropped.
    pub fn prune(&mut self, max: usize) -> bool {
        let len = self.revisions.len();
        if len <= max {
            return false;
        }
        let mut on_path = vec![false; len];
        let mut node = self.current;
        loop {
            on_path[node] = true;
            if node == 0 {
                break;
            }
            node = self.revisions[node].parent;
        }
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); len];
        for (index, revision) in self.revisions.iter().enumerate().skip(1) {
            children[revision.parent].push(index);
        }
        let mut kept = vec![true; len];
        let mut kept_children: Vec<usize> = children.iter().map(Vec::len).collect();
        let mut leaves: BTreeSet<usize> = (0..len)
            .filter(|&index| kept_children[index] == 0 && !on_path[index])
            .collect();
        let (mut root, mut count) = (0, len);
        while count > max.max(1) {
            if root != self.current && kept_children[root] == 1 {
                kept[root] = false;
                root = children[root]
                    .iter()
                    .copied()
                    .find(|&child| kept[child])
                    .expect("the root has a kept child");
            } else if let Some(leaf) = leaves.pop_first() {
                kept[leaf] = false;
                let parent = self.revisions[leaf].parent;
                kept_children[parent] -= 1;
                if kept_children[parent] == 0 && !on_path[parent] {
                    leaves.insert(parent);
                }
            } else {
                break;
            }
            count -= 1;
        }

        let mut renumbered = vec![0; len];
        let mut revisions = Vec::with_capacity(count);
        for (index, revision) in std::mem::take(&mut self.revisions).into_iter().enumerate() {
            if !kept[index] {
                continue;
            }
            renumbered[index] = revisions.len();
            let revision = if index == root {
                let empty = History::default().revisions.remove(0);
                Revision {
                    timestamp: revision.timestamp,
                    ..empty
                }
            } else {
                Revision {
                    parent: renumbered[revision.parent],
                    last_child: None,
                    ..revision
                }
            };
            revisions.push(revision);
        }
        for index in 1..revisions.len() {
            let parent = revisions[index].parent;
            revisions[parent].last_child = NonZeroUsize::new(index);
        }
        self.revisions = revisions;
        self.current = renumbered[self.current];
        let saves = std::mem::take(&mut self.saves);
        for revision in saves.into_iter().filter(|&revision| kept[revision]) {
            self.record_save(renumbered[revision]);
        }
        true
    }
}

/// Appends a record of `kind` with the payload `write` puts.
fn record(out: &mut Vec<u8>, kind: u8, write: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    out.push(kind);
    write(out);
    let len = u32::try_from(out.len() - start - 4).expect("records are shorter than 4 GiB");
    out[start..start + 4].copy_from_slice(&len.to_le_bytes());
}

fn put_usize(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_usize(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

/// Milliseconds since the epoch; earlier times count as the epoch.
fn put_time(out: &mut Vec<u8>, time: SystemTime) {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    put_usize(out, usize::try_from(millis).unwrap_or(usize::MAX));
}

fn put_transaction(out: &mut Vec<u8>, transaction: &Transaction) {
    let changes = transaction.changes().changes();
    put_usize(out, changes.len());
    for operation in changes {
        match operation {
            Operation::Retain(n) => {
                out.push(0);
                put_usize(out, *n);
            }
            Operation::Delete(n) => {
                out.push(1);
                put_usize(out, *n);
            }
            Operation::Insert(text) => {
                out.push(2);
                put_str(out, text);
            }
        }
    }
    match transaction.selection() {
        Some(selection) => {
            out.push(1);
            put_usize(out, selection.primary_index());
            put_usize(out, selection.len());
            for range in selection.ranges() {
                put_usize(out, range.anchor);
                put_usize(out, range.head);
            }
        }
        None => out.push(0),
    }
}

/// Reads the records of an undo file one after another.
struct Reader<'a> {
    rest: &'a [u8],
    root_time: SystemTime,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, Error> {
        let rest = bytes
            .strip_prefix(MAGIC)
            .and_then(|rest| rest.strip_prefix(&[VERSION]))
            .ok_or(Error::Header)?;
        let mut payload = Payload(rest);
        let root_time = payload.time().map_err(|_| Error::Header)?;
        Ok(Self {
            rest: payload.0,
            root_time,
        })
    }

    /// The next record's kind and payload; none at the end, or where the last one is cut short.
    fn next_record(&mut self) -> Option<(u8, Payload<'a>)> {
        let (len, rest) = self.rest.split_first_chunk::<4>()?;
        let len = u32::from_le_bytes(*len) as usize;
        if len == 0 || rest.len() < len {
            self.rest = &[];
            return None;
        }
        let (record, rest) = rest.split_at(len);
        self.rest = rest;
        Some((record[0], Payload(&record[1..])))
    }
}

/// The bytes of a record still to read.
struct Payload<'a>(&'a [u8]);

impl Payload<'_> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let (&byte, rest) = self
            .0
            .split_first()
            .ok_or(Error::Malformed("a record cut short"))?;
        self.0 = rest;
        Ok(byte)
    }

    fn usize(&mut self) -> Result<usize, Error> {
        let mut n: usize = 0;
        for shift in (0..usize::BITS).step_by(7) {
            let byte = self.byte()?;
            n |= usize::from(byte & 0x7f)
                .checked_shl(shift)
                .ok_or(Error::Malformed("a number too large"))?;
            if byte & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(Error::Malformed("a number too large"))
    }

    fn str(&mut self) -> Result<&str, Error> {
        let len = self.usize()?;
        if self.0.len() < len {
            return Err(Error::Malformed("a record cut short"));
        }
        let (bytes, rest) = self.0.split_at(len);
        self.0 = rest;
        std::str::from_utf8(bytes).map_err(|_| Error::Malformed("text that is no UTF-8"))
    }

    fn time(&mut self) -> Result<SystemTime, Error> {
        let millis = self.usize()?;
        Ok(UNIX_EPOCH + Duration::from_millis(millis as u64))
    }

    fn hash(&mut self) -> Result<TextHash, Error> {
        let (hash, rest) = self
            .0
            .split_first_chunk::<32>()
            .ok_or(Error::Malformed("a record cut short"))?;
        self.0 = rest;
        Ok(*hash)
    }

    fn transaction(&mut self) -> Result<Transaction, Error> {
        let operations = self.usize()?;
        let mut changes = ChangeSet::with_capacity(operations.min(self.0.len()));
        for _ in 0..operations {
            match self.byte()? {
                0 => changes.retain(self.usize()?),
                1 => changes.delete(self.usize()?),
                2 => changes.insert(self.str()?.into()),
                _ => return Err(Error::Malformed("an unknown change")),
            }
        }
        let transaction = Transaction::from(changes);
        if self.byte()? == 0 {
            return Ok(transaction);
        }
        let primary = self.usize()?;
        let len = self.usize()?;
        if len == 0 || primary >= len {
            return Err(Error::Malformed("a selection without its primary range"));
        }
        let mut ranges = SmallVec::with_capacity(len.min(self.0.len()));
        for _ in 0..len {
            ranges.push(Range::new(self.usize()?, self.usize()?));
        }
        Ok(transaction.with_selection(Selection::new(ranges, primary)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{history::State, Rope};

    /// A history that types and deletes in two branches, with the texts of its revisions.
    fn branching() -> (History, Vec<Rope>) {
        let mut history = History::default();
        let mut state = State {
            doc: Rope::from("hello\n"),
            selection: Selection::point(0),
        };
        let mut texts = vec![state.doc.clone()];
        let mut change = |history: &mut History, state: &mut State, change| {
            let transaction = Transaction::change(&state.doc, [change].into_iter())
                .with_selection(Selection::single(1, 2));
            history.commit_revision(&transaction, state);
            transaction.apply(&mut state.doc);
            texts.push(state.doc.clone());
        };
        change(&mut history, &mut state, (5, 5, Some(" world".into()))); // 1
        change(&mut history, &mut state, (0, 1, Some("J".into()))); // 2
        let undo = history.undo().unwrap().clone();
        undo.apply(&mut state.doc);
        change(&mut history, &mut state, (0, 5, None)); // 3, from 1
        change(&mut history, &mut state, (0, 0, Some("ünï ".into()))); // 4
        history.record_save(2);
        history.record_save(4);
        (history, texts)
    }

    fn head_of(history: &History, text: &Rope) -> Head {
        Head {
            revision: history.current_revision(),
            len: text.len_bytes(),
            hash: [7; 32],
        }
    }

    /// A whole undo file of `history` for `path`, written at `head`.
    fn whole(history: &History, path: &str, head: &Head) -> Vec<u8> {
        let mut out = header(history);
        put_whole(&mut out, history, Path::new(path)).unwrap();
        put_written(&mut out, head);
        out
    }

    /// The text of each revision of `history`, found from the current one's `text`.
    fn texts_of(history: &mut History, text: &Rope) -> Vec<Rope> {
        let current = history.current_revision();
        (0..history.revisions.len())
            .map(|revision| {
                let mut doc = text.clone();
                for transaction in history.jump_to(revision) {
                    transaction.apply(&mut doc);
                }
                history.jump_to(current);
                doc
            })
            .collect()
    }

    #[test]
    fn histories_read_back_as_written() {
        let (history, texts) = branching();
        let head = head_of(&history, &texts[4]);
        let bytes = whole(&history, "/tmp/a.txt", &head);
        let mut file = read(&bytes).unwrap();
        assert_eq!(file.path.as_deref(), Some(Path::new("/tmp/a.txt")));
        assert_eq!(file.head, head);
        assert_eq!(file.history.current_revision(), 4);
        assert_eq!(file.history.saves(), [2, 4]);
        for (revision, read) in history.revisions.iter().zip(&file.history.revisions) {
            assert_eq!(revision.parent, read.parent);
            assert_eq!(revision.last_child, read.last_child);
            assert_eq!(revision.transaction, read.transaction);
            assert_eq!(revision.inversion, read.inversion);
            let millis = |time: SystemTime| time.duration_since(UNIX_EPOCH).unwrap().as_millis();
            assert_eq!(millis(revision.timestamp), millis(read.timestamp));
        }
        assert_eq!(texts_of(&mut file.history, &texts[4]), texts);
    }

    #[test]
    fn appended_records_continue_the_file() {
        let (mut history, texts) = branching();
        let mut state = State {
            doc: texts[4].clone(),
            selection: Selection::point(0),
        };
        let first = head_of(&history, &state.doc);
        let mut bytes = whole(&history, "/a", &first);
        let written = history.revisions.len();
        let transaction = Transaction::change(&state.doc, [(0, 0, Some("x".into()))].into_iter());
        history.commit_revision(&transaction, &state);
        transaction.apply(&mut state.doc);
        put_revisions(&mut bytes, &history, written);
        put_path(&mut bytes, Path::new("/b")).unwrap();
        let second = head_of(&history, &state.doc);
        put_written(&mut bytes, &second);

        let file = read(&bytes).unwrap();
        assert_eq!(file.path.as_deref(), Some(Path::new("/b")));
        assert_eq!(read_path(&bytes).as_deref(), Some(Path::new("/b")));
        assert_eq!(file.head, second);
        assert_eq!(file.history.revisions.len(), 6);
        assert_eq!(file.history.revisions[5].parent, 4);
        assert_eq!(file.history.saves(), [2, 4, 5]);
    }

    #[test]
    fn a_record_cut_short_is_left_out() {
        let (history, texts) = branching();
        let head = head_of(&history, &texts[4]);
        let mut bytes = whole(&history, "/a", &head);
        let complete = bytes.len();
        put_written(
            &mut bytes,
            &Head {
                revision: 1,
                ..head
            },
        );
        // Cut in the head record, the write before it counts while the head stays.
        for end in complete..bytes.len() {
            assert_eq!(read(&bytes[..end]).unwrap().head, head, "cut at {end}");
        }
        assert_eq!(read(&bytes).unwrap().head.revision, 1);
    }

    #[test]
    fn broken_files_are_rejected() {
        let (history, texts) = branching();
        let head = head_of(&history, &texts[4]);
        assert_eq!(read(b"HXUNDO\x09").unwrap_err(), Error::Header);
        assert_eq!(read(b"something else").unwrap_err(), Error::Header);
        assert_eq!(read(&header(&history)).unwrap_err(), Error::NoHead);

        let malformed = |put: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = header(&history);
            put(&mut bytes);
            matches!(read(&bytes), Err(Error::Malformed(_)))
        };
        // A head of a revision that comes later.
        assert!(malformed(&|out| put_written(out, &head)));
        // A revision before its parent.
        assert!(malformed(&|out| record(out, REVISION, |out| put_usize(
            out, 3
        ))));
        // A revision cut short inside its record.
        assert!(malformed(&|out| record(out, REVISION, |out| put_usize(
            out, 0
        ))));
        // A record of an unknown kind.
        assert!(malformed(&|out| record(out, 9, |_| {})));
        // A record with bytes left over.
        assert!(malformed(
            &|out| record(out, WRITE, |out| out.extend([0, 0]))
        ));
    }

    #[test]
    fn pruning_keeps_the_current_branch_whole() {
        let (mut history, texts) = branching();
        // 0 ─ 1 ┬ 2
        //       └ 3 ─ 4 (current)
        assert!(!history.prune(5));
        // The root goes first, then leaf 2 as revision 1 has two children.
        assert!(history.prune(3));
        assert_eq!(history.current_revision(), 2);
        assert_eq!(history.saves(), [2]);
        let kept = [1, 3, 4].map(|revision| texts[revision].clone());
        assert_eq!(texts_of(&mut history, &texts[4]), kept);
        // Then revision 1, the root with a single child.
        assert!(history.prune(2));
        let kept = [3, 4].map(|revision| texts[revision].clone());
        assert_eq!(texts_of(&mut history, &texts[4]), kept);
    }

    #[test]
    fn pruning_can_leave_the_current_revision_alone() {
        let (mut history, texts) = branching();
        history.jump_to(2);
        assert!(history.prune(1));
        assert_eq!(history.current_revision(), 0);
        assert_eq!(history.saves(), [0]);
        assert_eq!(texts_of(&mut history, &texts[2]), [texts[2].clone()]);
    }

    quickcheck::quickcheck! {
        /// Random edits, undos and jumps; then the history goes through an undo file and is
        /// pruned. Every revision left has the text it had.
        fn pruned_histories_keep_their_texts(steps: Vec<(u8, u8, u8)>, max: u8) -> bool {
            let mut history = History::default();
            let mut state = State {
                doc: Rope::from("abc"),
                selection: Selection::point(0),
            };
            let mut texts = vec![state.doc.clone()];
            for (kind, at, len) in steps.into_iter().take(60) {
                let doc_len = state.doc.len_chars();
                match kind % 4 {
                    0 if history.revisions.len() > 1 => {
                        let target = usize::from(at) % history.revisions.len();
                        for transaction in history.jump_to(target) {
                            transaction.apply(&mut state.doc);
                        }
                    }
                    1 => {
                        if let Some(transaction) = history.undo().cloned() {
                            transaction.apply(&mut state.doc);
                        }
                    }
                    _ => {
                        let from = usize::from(at) % (doc_len + 1);
                        let to = (from + usize::from(len % 3)).min(doc_len);
                        let text = (kind % 2 == 0).then(|| "xé".into());
                        let transaction =
                            Transaction::change(&state.doc, [(from, to, text)].into_iter());
                        if transaction.changes().is_empty() {
                            continue;
                        }
                        history.commit_revision(&transaction, &state);
                        transaction.apply(&mut state.doc);
                        texts.push(state.doc.clone());
                        if at % 3 == 0 {
                            history.record_save(history.current_revision());
                        }
                    }
                }
            }
            let head = head_of(&history, &state.doc);
            let mut file = read(&whole(&history, "/a", &head)).unwrap();
            if texts_of(&mut file.history, &state.doc) != texts {
                return false;
            }
            let max = usize::from(max % 8) + 1;
            file.history.prune(max);
            // Each revision left has the text of one before, the current one the current text.
            let pruned = texts_of(&mut file.history, &state.doc);
            file.history.revisions.len() <= max
                && pruned.iter().all(|text| texts.contains(text))
                && pruned[file.history.current_revision()] == state.doc
        }
    }
}
