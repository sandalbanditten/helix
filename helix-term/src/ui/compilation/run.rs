//! Running the command of a compilation buffer, reading its output off the main thread.

use std::{
    io::{self, Read},
    mem,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use helix_stdx::process::{self, GroupStatus, ProcessGroup};
use helix_view::DocumentId;
use tokio::runtime::Handle;

use super::output::{End, Lines};
use crate::job;

/// Output read but not yet in the buffer.
#[derive(Debug, Default)]
pub struct Output {
    pub text: String,
    /// How the run ended, once it did; nothing is read after.
    pub end: Option<End>,
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
    /// Whether a callback that takes the output is queued. Only one is, so that output arriving
    /// while the editor is busy makes the next batch larger rather than queueing more renders.
    queued: bool,
}

/// Runs `command` with `shell` in `dir`, stdout and stderr both into one pipe, and appends what
/// it writes to the compilation buffer `doc` as run `run`. The returned group stops it.
pub fn spawn(
    shell: &[String],
    command: &str,
    dir: &Path,
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
        .spawn(move || read(reader, child, &status, &handle, doc, run))?;
    Ok(group)
}

/// Reads the output of `child` until it ends, then waits for it.
fn read(
    mut pipe: io::PipeReader,
    mut child: Child,
    status: &GroupStatus,
    handle: &Handle,
    doc: DocumentId,
    run: u64,
) {
    let pending = Arc::new(Mutex::new(Pending::default()));
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
            hand_over(&pending, handle, doc, run, |output| {
                output.text.push_str(&text)
            });
        }
    }
    let rest = lines.finish();
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
    hand_over(&pending, handle, doc, run, |output| {
        output.text.push_str(&rest);
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

/// Adds to the pending output with `add`, and queues a callback that appends all of it to the
/// buffer unless one is queued already.
fn hand_over(
    pending: &Arc<Mutex<Pending>>,
    handle: &Handle,
    doc: DocumentId,
    run: u64,
    add: impl FnOnce(&mut Output),
) {
    {
        let mut pending = pending.lock().unwrap();
        add(&mut pending.output);
        if pending.queued || pending.output.is_empty() {
            return;
        }
        pending.queued = true;
    }
    let pending = pending.clone();
    // Waits while the job queue is full rather than dropping the callback.
    handle.block_on(job::dispatch(move |editor, _compositor| {
        let output = {
            let mut pending = pending.lock().unwrap();
            pending.queued = false;
            mem::take(&mut pending.output)
        };
        super::append(editor, doc, run, output);
    }));
}
