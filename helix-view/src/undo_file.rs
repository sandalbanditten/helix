//! The undo files of documents: where they are, reading one when its file is opened, and writing
//! it after the file is written, in the format of [`helix_core::history::undo_file`].

use std::{
    ffi::OsString,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use helix_core::{
    history::{
        undo_file::{self, Head, TextHash},
        History,
    },
    Rope,
};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

/// The longest file name most file systems take, in bytes.
const MAX_NAME: usize = 255;

/// The files that get no undo file: messages git asks for, which are new each time.
const GIT_MESSAGES: &[&str] = &[
    "COMMIT_EDITMSG",
    "MERGE_MSG",
    "TAG_EDITMSG",
    "SQUASH_MSG",
    "git-rebase-todo",
];

/// What an undo file is like on disk, which tells whether it changed since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
}

impl Stamp {
    fn of(file: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(file)?;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}

/// What a document knows of its undo file.
#[derive(Debug, Clone)]
pub struct UndoFileState {
    /// The undo file.
    file: PathBuf,
    /// The revisions in it, or on their way there: the first one the next write appends.
    revisions: usize,
    /// The undo file as the last write of it left it, or as it was read.
    stamp: Arc<Mutex<Option<Stamp>>>,
}

/// Whether `path` gets an undo file.
pub fn is_kept(path: &Path) -> bool {
    let git_message = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| GIT_MESSAGES.contains(&name));
    let temporary = temporary_dirs().iter().any(|dir| path.starts_with(dir));
    path.to_str().is_some() && !git_message && !temporary
}

fn temporary_dirs() -> Vec<PathBuf> {
    let temp = std::env::temp_dir();
    let mut dirs = vec![helix_stdx::path::canonicalize(&temp), temp];
    if cfg!(unix) {
        dirs.push(PathBuf::from("/dev/shm"));
    }
    dirs
}

/// The name of the undo file of `path`: the path with its separators turned into `%`, or a hash
/// of a path too long for that.
pub fn name(path: &Path) -> Option<OsString> {
    let escaped = escape(path)?;
    let name = if escaped.len() <= MAX_NAME {
        escaped
    } else {
        format!("#{}", hex(&Sha256::digest(path.to_str()?.as_bytes())))
    };
    Some(name.into())
}

/// `path` with its separators turned into `%`.
fn escape(path: &Path) -> Option<String> {
    let escaped = path
        .to_str()?
        .chars()
        .map(|c| {
            if std::path::is_separator(c) || (cfg!(windows) && c == ':') {
                '%'
            } else {
                c
            }
        })
        .collect();
    Some(escaped)
}

/// The undo file of `path` in `dir`.
pub fn file_of(dir: &Path, path: &Path) -> Option<PathBuf> {
    name(path).map(|name| dir.join(name))
}

/// The hash of `text` that undo files record.
pub fn hash(text: &Rope) -> TextHash {
    let mut hasher = Sha256::new();
    for chunk in text.chunks() {
        hasher.update(chunk.as_bytes());
    }
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Reads the undo file in `dir` of the file at `path`, whose text is `text`. Returns its history
/// if it is the history of this file and text.
pub fn read(
    dir: &Path,
    path: &Path,
    text: &Rope,
    max_revisions: usize,
) -> Option<(History, UndoFileState)> {
    let file = file_of(dir, path)?;
    let bytes = match fs::read(&file) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("cannot read undo file {}: {err}", file.display());
            return None;
        }
    };
    let stamp = Stamp::of(&file).ok();
    let undo = match undo_file::read(&bytes) {
        Ok(undo) => undo,
        Err(err) => {
            log::info!("ignoring undo file {}: {err}", file.display());
            return None;
        }
    };
    if undo.path.as_deref() != Some(path) {
        log::info!(
            "ignoring undo file {}: it is another file's",
            file.display()
        );
        return None;
    }
    if undo.head.len != text.len_bytes() || undo.head.hash != hash(text) {
        log::info!(
            "ignoring undo file {}: the file changed since it was written",
            file.display()
        );
        return None;
    }
    let mut history = undo.history;
    // A pruned history is numbered anew, so it no longer continues the file.
    let pruned = max_revisions > 0 && history.prune(max_revisions);
    let state = UndoFileState {
        file,
        revisions: history.len(),
        stamp: Arc::new(Mutex::new(stamp.filter(|_| !pruned))),
    };
    Some((history, state))
}

/// Moves the undo files in `dir` of the file or directory moved from `from` to `to`.
pub fn moved(dir: &Path, from: &Path, to: &Path) -> io::Result<()> {
    carry(dir, from, to, |from, to| fs::rename(from, to))
}

/// Copies the undo files in `dir` of the file or directory copied from `from` to `to`.
pub fn copied(dir: &Path, from: &Path, to: &Path) -> io::Result<()> {
    carry(dir, from, to, |from, to| fs::copy(from, to).map(drop))
}

/// Carries the undo files of `from` over to `to` with `carry`, each recording its new path.
fn carry(
    dir: &Path,
    from: &Path,
    to: &Path,
    carry: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    for (file, path) in undo_files(dir, from)? {
        let relative = path
            .strip_prefix(from)
            .expect("undo files are below `from`");
        // `join("")` would append a separator.
        let path = if relative.as_os_str().is_empty() {
            to.to_path_buf()
        } else {
            to.join(relative)
        };
        let mut bytes = Vec::new();
        let (Some(target), Some(())) =
            (file_of(dir, &path), undo_file::put_path(&mut bytes, &path))
        else {
            continue;
        };
        carry(&file, &target)?;
        fs::OpenOptions::new()
            .append(true)
            .open(&target)?
            .write_all(&bytes)?;
    }
    Ok(())
}

/// The undo files in `dir` of the file at `path` or of the files below it, with their files.
fn undo_files(dir: &Path, path: &Path) -> io::Result<Vec<(PathBuf, PathBuf)>> {
    let Some(escaped) = escape(path) else {
        return Ok(Vec::new());
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // A name only begins like it; the path recorded in the file tells.
        let candidate = name.starts_with('#')
            || name
                .strip_prefix(escaped.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('%'));
        if !candidate {
            continue;
        }
        let file = entry.path();
        let recorded = fs::read(&file)
            .ok()
            .and_then(|bytes| undo_file::read_path(&bytes));
        if let Some(recorded) = recorded.filter(|recorded| recorded.starts_with(path)) {
            files.push((file, recorded));
        }
    }
    Ok(files)
}

/// A write of an undo file, prepared along with the write of its file and made after it.
#[derive(Debug)]
pub struct PendingWrite {
    file: PathBuf,
    /// The records to write, but for the write itself.
    bytes: Vec<u8>,
    /// Whether `bytes` continue the undo file rather than replace it.
    append: bool,
    /// The revision the file is written at.
    revision: usize,
    stamp: Arc<Mutex<Option<Stamp>>>,
}

/// Prepares the write of the undo file in `dir` of `path`, about to be written at the current
/// revision of `history`.
pub fn prepare(
    state: &mut Option<UndoFileState>,
    history: &History,
    dir: &Path,
    path: &Path,
) -> Option<PendingWrite> {
    let file = file_of(dir, path)?;
    let continued = state.as_ref().filter(|state| {
        let stamp = *state.stamp.lock();
        state.file == file && stamp.is_some() && Stamp::of(&file).ok() == stamp
    });
    let (bytes, append) = match continued {
        Some(state) => {
            let mut bytes = Vec::new();
            undo_file::put_revisions(&mut bytes, history, state.revisions);
            (bytes, true)
        }
        None => {
            let mut bytes = undo_file::header(history);
            undo_file::put_whole(&mut bytes, history, path)?;
            (bytes, false)
        }
    };
    let stamp = match state.take() {
        Some(state) if state.file == file => state.stamp,
        _ => Arc::default(),
    };
    *state = Some(UndoFileState {
        file: file.clone(),
        revisions: history.len(),
        stamp: stamp.clone(),
    });
    Some(PendingWrite {
        file,
        bytes,
        append,
        revision: history.current_revision(),
        stamp,
    })
}

impl PendingWrite {
    /// Writes the undo file now that its file holds `text`.
    pub fn write(mut self, text: &Rope) -> io::Result<()> {
        let head = Head {
            revision: self.revision,
            len: text.len_bytes(),
            hash: hash(text),
        };
        undo_file::put_written(&mut self.bytes, &head);
        let expected = self.stamp.lock().take();
        if self.append {
            if expected.is_none() || Stamp::of(&self.file).ok() != expected {
                // Written by another Helix in between: the next write writes it anew.
                log::info!(
                    "undo file {} changed since it was last written",
                    self.file.display()
                );
                return Ok(());
            }
            let mut file = fs::OpenOptions::new().append(true).open(&self.file)?;
            file.write_all(&self.bytes)?;
        } else {
            self.replace()?;
        }
        *self.stamp.lock() = Stamp::of(&self.file).ok();
        Ok(())
    }

    /// Writes the undo file anew, all at once: in a temporary file that takes its place.
    fn replace(&self) -> io::Result<()> {
        let dir = self.file.parent().expect("undo files are in a directory");
        if !dir.is_dir() {
            fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Undo files hold the text of their files.
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
        }
        // Temporary files are readable by their owner only.
        let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
        temporary.write_all(&self.bytes)?;
        temporary.persist(&self.file).map_err(|err| err.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use helix_core::{history::State, Selection, Transaction};

    use super::*;

    /// A history of `text` with `edits` revisions, each typing `x` at the start.
    fn edited(text: &mut Rope, history: &mut History, edits: usize) {
        for _ in 0..edits {
            let state = State {
                doc: text.clone(),
                selection: Selection::point(0),
            };
            let transaction = Transaction::change(text, [(0, 0, Some("x".into()))].into_iter());
            history.commit_revision(&transaction, &state);
            transaction.apply(text);
        }
    }

    fn written(state: &mut Option<UndoFileState>, history: &History, dir: &Path, text: &Rope) {
        let path = Path::new("/project/main.rs");
        prepare(state, history, dir, path)
            .unwrap()
            .write(text)
            .unwrap();
    }

    #[test]
    fn names_escape_the_path() {
        assert_eq!(
            name(Path::new("/home/me/x%y/main.rs")).unwrap(),
            "%home%me%x%y%main.rs"
        );
        let long = format!("/{}", "a".repeat(300));
        let name = name(Path::new(&long)).unwrap();
        assert!(name.to_str().unwrap().starts_with('#'));
        assert_eq!(name.len(), 65);
    }

    #[test]
    fn some_files_get_none() {
        assert!(is_kept(Path::new("/home/me/main.rs")));
        assert!(!is_kept(Path::new("/home/me/repo/.git/COMMIT_EDITMSG")));
        assert!(!is_kept(&std::env::temp_dir().join("scratch.txt")));
    }

    #[test]
    fn writes_append_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = Path::new("/project/main.rs");
        let (mut text, mut history) = (Rope::from("fn main() {}\n"), History::default());
        let mut state = None;

        edited(&mut text, &mut history, 2);
        written(&mut state, &history, dir.path(), &text);
        let first = fs::metadata(file_of(dir.path(), path).unwrap())
            .unwrap()
            .len();
        edited(&mut text, &mut history, 1);
        let pending = prepare(&mut state, &history, dir.path(), path).unwrap();
        assert!(pending.append);
        pending.write(&text).unwrap();
        assert!(
            fs::metadata(file_of(dir.path(), path).unwrap())
                .unwrap()
                .len()
                > first
        );

        let (read, state) = read(dir.path(), path, &text, 0).unwrap();
        assert_eq!(read.len(), 4);
        assert_eq!(read.current_revision(), 3);
        assert_eq!(read.saves(), [2, 3]);
        assert_eq!(state.revisions, 4);
        // Another text, another file or no file: no history.
        assert!(super::read(dir.path(), path, &Rope::from("x"), 0).is_none());
        assert!(super::read(dir.path(), Path::new("/project/lib.rs"), &text, 0).is_none());
    }

    #[test]
    fn another_write_in_between_makes_the_next_write_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = Path::new("/project/main.rs");
        let (mut text, mut history) = (Rope::from("a\n"), History::default());
        let mut state = None;
        edited(&mut text, &mut history, 1);
        written(&mut state, &history, dir.path(), &text);

        // Another Helix writes the same file and its undo file.
        let (mut theirs, mut their_history) = (Rope::from("a\n"), History::default());
        edited(&mut theirs, &mut their_history, 3);
        written(&mut None, &their_history, dir.path(), &theirs);

        edited(&mut text, &mut history, 1);
        let pending = prepare(&mut state, &history, dir.path(), path).unwrap();
        assert!(!pending.append);
        pending.write(&text).unwrap();
        let (read, _) = read(dir.path(), path, &text, 0).unwrap();
        assert_eq!(read.len(), 3);
    }

    /// Writes undo files for the files at `paths`, each with one revision.
    fn undo_files_for(dir: &Path, paths: &[&Path]) -> Rope {
        let (mut text, mut history) = (Rope::from("a\n"), History::default());
        edited(&mut text, &mut history, 1);
        for path in paths {
            prepare(&mut None, &history, dir, path)
                .unwrap()
                .write(&text)
                .unwrap();
        }
        text
    }

    #[test]
    fn undo_files_follow_moves() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let long = format!("/p/src/{}.rs", "n".repeat(300));
        let text = undo_files_for(
            dir,
            &[
                Path::new("/p/src/main.rs"),
                Path::new("/p/src/a/lib.rs"),
                Path::new(&long),
                // Begins like a file in `/p/src`, but is none.
                Path::new("/p/src%x.rs"),
                Path::new("/p/srcs/main.rs"),
            ],
        );
        let has = |path: &str| read(dir, Path::new(path), &text, 0).is_some();

        moved(dir, Path::new("/p/src/main.rs"), Path::new("/p/main.rs")).unwrap();
        assert!(has("/p/main.rs") && !has("/p/src/main.rs"));

        moved(dir, Path::new("/p/src"), Path::new("/q/code")).unwrap();
        assert!(has("/q/code/a/lib.rs") && !has("/p/src/a/lib.rs"));
        assert!(has(&long.replace("/p/src", "/q/code")) && !has(&long));
        assert!(has("/p/src%x.rs") && has("/p/srcs/main.rs"));
        assert!(!has("/q/code%x.rs"));

        copied(dir, Path::new("/q/code"), Path::new("/r")).unwrap();
        assert!(has("/r/a/lib.rs") && has("/q/code/a/lib.rs"));
        // A move onto a file replaces its undo file.
        moved(dir, Path::new("/r/a/lib.rs"), Path::new("/p/main.rs")).unwrap();
        assert!(has("/p/main.rs") && !has("/r/a/lib.rs"));
        // Nothing to carry, or no undo directory at all.
        moved(dir, Path::new("/nowhere"), Path::new("/else")).unwrap();
        moved(&dir.join("missing"), Path::new("/p"), Path::new("/q")).unwrap();
    }

    /// A text of about `bytes` bytes, like source code.
    fn source(bytes: usize) -> Rope {
        let line = "    let value = compute(first, second) + offset; // a comment\n";
        Rope::from(line.repeat(bytes / line.len()))
    }

    /// Commits `revisions` revisions to `history` of `text`, each typing a word somewhere, every
    /// seventh after undoing the one before: a history with branches.
    fn typed(text: &mut Rope, history: &mut History, revisions: usize) {
        let mut seed = 12345_u64;
        for revision in 0..revisions {
            if revision % 7 == 6 {
                if let Some(undo) = history.undo().cloned() {
                    undo.apply(text);
                }
            }
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let at = (seed >> 33) as usize % text.len_chars();
            let state = State {
                doc: text.clone(),
                selection: Selection::point(at),
            };
            let transaction =
                Transaction::change(text, [(at, at, Some("word ".into()))].into_iter());
            history.commit_revision(&transaction, &state);
            transaction.apply(text);
        }
    }

    fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
        let start = std::time::Instant::now();
        let result = f();
        eprintln!("{what}: {:?}", start.elapsed());
        result
    }

    /// Times reading and writing undo files of large histories, and moving a directory among many
    /// undo files. Run it with `cargo test --release -p helix-view --lib measure_undo_files --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_undo_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = Path::new("/project/src/main.rs");
        for size in [250_000, 10_000_000] {
            let text = source(size);
            timed(&format!("hash of {} KB", size / 1000), || hash(&text));
        }
        for revisions in [1000, 10_000] {
            let (mut text, mut history) = (source(250_000), History::default());
            typed(&mut text, &mut history, revisions);
            let mut state = None;
            let pending = timed(&format!("{revisions} revisions: prepare whole"), || {
                prepare(&mut state, &history, dir.path(), path).unwrap()
            });
            timed(&format!("{revisions} revisions: write whole"), || {
                pending.write(&text).unwrap()
            });
            let file = file_of(dir.path(), path).unwrap();
            eprintln!(
                "{revisions} revisions: {} KB",
                fs::metadata(&file).unwrap().len() / 1000
            );
            timed(&format!("{revisions} revisions: read"), || {
                read(dir.path(), path, &text, 0).unwrap()
            });
            timed(
                &format!("{revisions} revisions: read, pruned to 1000"),
                || read(dir.path(), path, &text, 1000).unwrap(),
            );
            typed(&mut text, &mut history, 10);
            let pending = timed(&format!("{revisions} revisions: prepare 10 more"), || {
                prepare(&mut state, &history, dir.path(), path).unwrap()
            });
            assert!(pending.append);
            timed(&format!("{revisions} revisions: append 10"), || {
                pending.write(&text).unwrap()
            });
        }

        let many = tempfile::tempdir().unwrap();
        let mut history = History::default();
        let mut small = Rope::from("a\n");
        edited(&mut small, &mut history, 1);
        let paths: Vec<PathBuf> = (0..10_000)
            .map(|i| PathBuf::from(format!("/project/dir{}/file{i}.rs", i / 100)))
            .collect();
        timed("10000 undo files: write", || {
            for path in &paths {
                prepare(&mut None, &history, many.path(), path)
                    .unwrap()
                    .write(&small)
                    .unwrap();
            }
        });
        timed("10000 undo files: move a directory of 100", || {
            moved(
                many.path(),
                Path::new("/project/dir42"),
                Path::new("/project/moved"),
            )
            .unwrap()
        });
        assert!(read(
            many.path(),
            Path::new("/project/moved/file4200.rs"),
            &small,
            0
        )
        .is_some());
    }

    #[test]
    fn pruned_histories_are_written_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = Path::new("/project/main.rs");
        let (mut text, mut history) = (Rope::from("a\n"), History::default());
        edited(&mut text, &mut history, 5);
        written(&mut None, &history, dir.path(), &text);

        let (history, mut state) = read(dir.path(), path, &text, 3)
            .map(|(h, s)| (h, Some(s)))
            .unwrap();
        assert_eq!(history.len(), 3);
        let pending = prepare(&mut state, &history, dir.path(), path).unwrap();
        assert!(!pending.append);
        pending.write(&text).unwrap();
        assert_eq!(read(dir.path(), path, &text, 0).unwrap().0.len(), 3);
    }
}
