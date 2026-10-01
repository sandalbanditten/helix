//! Running the command of a compilation buffer, reading its output off the main thread.

use std::{
    io::{self, Read},
    mem,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use helix_stdx::process::{self, GroupStatus, ProcessGroup};
use helix_view::{DocumentId, Editor};
use tokio::runtime::Handle;

use super::{
    locus::{Finder, Locus},
    output::{End, Lines},
};
use crate::{compositor::Compositor, job};

/// Output read but not yet in the buffer.
#[derive(Debug, Default)]
pub struct Output {
    pub text: String,
    /// The chars of `text`.
    pub chars: usize,
    /// The loci in `text`, counting their chars from its start.
    pub loci: Vec<Locus>,
    /// How the run ended, once it did; nothing is read after.
    pub end: Option<End>,
}

impl Output {
    /// Takes the lines of about the first `max` bytes, and how the run ended once nothing is
    /// left.
    fn take_front(&mut self, max: usize) -> Output {
        let bytes = self.text.as_bytes();
        let split = bytes[..max.min(bytes.len())]
            .iter()
            .rposition(|&byte| byte == b'\n')
            .or_else(|| Some(max + bytes.get(max..)?.iter().position(|&byte| byte == b'\n')?))
            .map_or(bytes.len(), |line_break| line_break + 1);
        if split == bytes.len() {
            return mem::take(self);
        }
        let rest = self.text.split_off(split);
        let text = mem::replace(&mut self.text, rest);
        let chars = text.chars().count();
        let rest = self
            .loci
            .split_off(self.loci.partition_point(|locus| locus.start < chars));
        let loci = mem::replace(&mut self.loci, rest);
        for locus in &mut self.loci {
            locus.start -= chars;
        }
        self.chars -= chars;
        Output {
            text,
            chars,
            loci,
            end: None,
        }
    }

    /// Adds the lines `text` with the `loci` in them.
    fn push(&mut self, text: &str, loci: Vec<Locus>) {
        let start = self.chars;
        self.loci.extend(loci.into_iter().map(|mut locus| {
            locus.start += start;
            locus
        }));
        self.text.push_str(text);
        self.chars += text.chars().count();
    }
}

impl Output {
    fn is_empty(&self) -> bool {
        self.text.is_empty() && self.end.is_none()
    }
}

/// The output a reader thread hands over to the main thread.
#[derive(Debug, Default)]
struct Pending {
    output: Output,
    /// Whether a callback that takes output is queued. Only one is, so that output arriving
    /// while the editor is busy makes the next batch larger rather than queueing more renders.
    queued: bool,
}

/// The pending output, and what tells the reader that some was taken.
#[derive(Debug, Default)]
struct Shared {
    pending: Mutex<Pending>,
    taken: Condvar,
}

/// A callback appends at most this much output, so that keys wait for no more than that.
const BATCH: usize = 256 * 1024;
/// The reader waits while this much output is pending, rather than filling memory with output
/// that arrives faster than the editor takes it.
const PENDING: usize = 4 * 1024 * 1024;

/// Runs `command` with `shell` in `dir`, stdout and stderr both into one pipe, and appends what
/// it writes to the compilation buffer `doc` as run `run`, with the loci `finder` finds in it.
/// The returned group stops it.
pub fn spawn(
    shell: &[String],
    command: &str,
    dir: &Path,
    finder: Finder,
    doc: DocumentId,
    run: u64,
) -> io::Result<ProcessGroup> {
    let Some((program, args)) = shell.split_first() else {
        return Err(io::Error::other("No shell set"));
    };
    let (reader, writer) = io::pipe()?;
    let mut process = Command::new(program);
    process
        .args(args)
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    let (child, group) = process::spawn_group(&mut process)?;
    // The command keeps the write end, which must close for the output to end.
    drop(process);

    let status = group.status();
    let handle = Handle::current();
    std::thread::Builder::new()
        .name("compilation".to_owned())
        .spawn(move || read(reader, child, finder, &status, &handle, doc, run))?;
    Ok(group)
}

/// Reads the output of `child` until it ends, then waits for it.
fn read(
    mut pipe: io::PipeReader,
    mut child: Child,
    mut finder: Finder,
    status: &GroupStatus,
    handle: &Handle,
    doc: DocumentId,
    run: u64,
) {
    let shared = Arc::new(Shared::default());
    let mut lines = Lines::default();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let read = match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                log::warn!("cannot read compilation output: {err}");
                break;
            }
        };
        let text = lines.push(&buf[..read]);
        if !text.is_empty() {
            let loci = finder.find(&text);
            hand_over(&shared, handle, doc, run, |output| output.push(&text, loci));
        }
    }
    let rest = lines.finish();
    let rest_loci = finder.find(&rest);
    let exit = child.wait();
    // Once waited for, its pid may be reused: the group must not be stopped any more.
    status.exited();
    let end = if status.killed() {
        End::Killed
    } else {
        match exit {
            Ok(exit) => exit_end(exit),
            Err(err) => {
                log::warn!("cannot wait for the compilation: {err}");
                End::Exited(-1)
            }
        }
    };
    hand_over(&shared, handle, doc, run, |output| {
        output.push(&rest, rest_loci);
        output.end = Some(end);
    });
}

#[cfg(unix)]
fn exit_end(exit: std::process::ExitStatus) -> End {
    use std::os::unix::process::ExitStatusExt;
    match (exit.code(), exit.signal()) {
        (Some(code), _) => End::Exited(code),
        (None, Some(signal)) => End::Signal(signal),
        (None, None) => End::Exited(-1),
    }
}

#[cfg(not(unix))]
fn exit_end(exit: std::process::ExitStatus) -> End {
    End::Exited(exit.code().unwrap_or(-1))
}

/// Adds to the pending output with `add`, and queues a callback that appends it to the buffer
/// unless one is queued already. Waits while much is pending.
fn hand_over(
    shared: &Arc<Shared>,
    handle: &Handle,
    doc: DocumentId,
    run: u64,
    add: impl FnOnce(&mut Output),
) {
    let mut pending = shared.pending.lock().unwrap();
    add(&mut pending.output);
    loop {
        if !pending.queued && !pending.output.is_empty() {
            pending.queued = true;
            drop(pending);
            // Waits while the job queue is full rather than dropping the callback.
            handle.block_on(job::dispatch(take(shared.clone(), doc, run)));
            pending = shared.pending.lock().unwrap();
        }
        if pending.output.text.len() <= PENDING {
            return;
        }
        (pending, _) = shared
            .taken
            .wait_timeout(pending, Duration::from_millis(100))
            .unwrap();
    }
}

/// The callback that appends the next batch of pending output to the buffer, and queues itself
/// again for the rest, after the keys that came meanwhile.
fn take(
    shared: Arc<Shared>,
    doc: DocumentId,
    run: u64,
) -> impl FnOnce(&mut Editor, &mut Compositor) + Send + 'static {
    move |editor, _compositor| {
        let (output, rest) = {
            let mut pending = shared.pending.lock().unwrap();
            let output = pending.output.take_front(BATCH);
            pending.queued = !pending.output.is_empty();
            (output, pending.queued)
        };
        shared.taken.notify_all();
        if rest {
            tokio::spawn(job::dispatch(take(shared, doc, run)));
        }
        super::append(editor, doc, run, output);
    }
}

#[cfg(test)]
mod tests {
    use helix_core::{diagnostic::Severity, Position};

    use super::*;

    fn locus(start: usize) -> Locus {
        Locus {
            start,
            len: 1,
            severity: Severity::Error,
            message: String::new(),
            path: "a.rs".into(),
            position: Position::default(),
        }
    }

    #[test]
    fn output_is_taken_in_whole_lines() {
        let mut output = Output::default();
        output.push("äb\ncd\n", vec![locus(0), locus(3)]);
        output.push("ef\n", vec![locus(1)]);
        output.end = Some(End::Exited(0));

        // A batch ends at the last line break within it, or the first after it.
        let first = output.take_front(5);
        assert_eq!(
            (first.text.as_str(), first.chars, first.end),
            ("äb\n", 3, None)
        );
        assert_eq!(first.loci, [locus(0)]);
        assert_eq!((output.text.as_str(), output.chars), ("cd\nef\n", 6));
        assert_eq!(output.loci, [locus(0), locus(4)]);
        let second = output.take_front(1);
        assert_eq!(second.text, "cd\n");
        // The end comes with the last lines.
        let last = output.take_front(64);
        assert_eq!(
            (last.text.as_str(), last.end),
            ("ef\n", Some(End::Exited(0)))
        );
        assert_eq!(last.loci, [locus(1)]);
        assert!(output.is_empty());
    }
}
