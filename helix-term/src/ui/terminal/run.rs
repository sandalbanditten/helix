//! Running the program of a terminal buffer: its output read, and its input written, off the main
//! thread.

use std::{
    fs::File,
    io::{self, Write},
    mem,
    path::Path,
    process::Command,
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

use helix_stdx::{
    process::{self, ProcessGroup},
    pty::Pty,
};
use helix_view::{DocumentId, Editor};
use tokio::runtime::Handle;

use crate::{
    compositor::Compositor,
    job,
    ui::{
        compilation::{
            output::End,
            run::{read, Lifetime, PENDING, TICK},
        },
        screen::{Modes, Screen, Shown},
    },
};

/// Output read but not yet in the buffer.
#[derive(Debug, Default)]
pub struct Output {
    /// The chars at the end of the buffer that `text` replaces.
    pub replace: usize,
    pub text: Shown,
    /// The chars of the screen at the end of the buffer, once `text` is in it.
    pub screen: usize,
    /// The chars of the screen before its top row, and before its cursor.
    pub top: usize,
    pub cursor: usize,
    pub title: Option<String>,
    pub modes: Modes,
    /// Whether the program cleared the lines that scrolled away.
    pub cleared: bool,
    pub end: Option<End>,
}

impl Output {
    /// Adds `text` in place of the last `replace` chars of the output.
    fn push(&mut self, replace: usize, text: &Shown) {
        let pending = replace.min(self.text.chars);
        self.text.truncate(self.text.chars - pending);
        self.replace += replace - pending;
        self.text.append(text);
    }
}

#[derive(Debug, Default)]
struct Pending {
    output: Option<Output>,
    /// Whether a callback that takes the output is queued.
    queued: bool,
}

#[derive(Debug, Default)]
struct Shared {
    pending: Mutex<Pending>,
    taken: Condvar,
}

/// A frame the program draws shows once it is drawn, or after this long.
const FRAME: Duration = Duration::from_millis(100);
/// Output read at once at most, before it shows.
const CHUNKS: usize = 16;

/// Runs `command` in `dir` on a terminal of `size`, and shows it in the terminal buffer `doc` as
/// run `run`. Programs asking for the colors of the terminal are told `palette`. Returns the group
/// that stops it, its terminal, and where its input goes.
pub fn spawn(
    command: &[String],
    dir: &Path,
    (cols, rows): (u16, u16),
    palette: [Option<(u8, u8, u8)>; 2],
    doc: DocumentId,
    run: u64,
) -> io::Result<(ProcessGroup, Pty, Sender<Vec<u8>>)> {
    let Some((program, args)) = command.split_first() else {
        return Err(io::Error::other("No shell set"));
    };
    let (pty, terminal) = Pty::open(cols, rows)?;
    let mut process = Command::new(program);
    process
        .args(args)
        .current_dir(dir)
        .stdin(terminal.try_clone()?)
        .stdout(terminal.try_clone()?)
        .stderr(terminal)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env_remove("COLUMNS")
        .env_remove("LINES");
    let (leader, group) = process::spawn_terminal(&mut process)?;
    // The program keeps the terminal end, which must close for the output to end.
    drop(process);

    let (chunks, received) = mpsc::sync_channel(CHUNKS);
    let output = pty.reader()?;
    std::thread::Builder::new()
        .name("terminal output".to_owned())
        .spawn(move || read(output, &chunks))?;
    let (input, keys) = mpsc::channel();
    let writer = pty.writer()?;
    std::thread::Builder::new()
        .name("terminal input".to_owned())
        .spawn(move || write(writer, &keys))?;
    let screen = Screen::terminal(cols, rows, palette);
    let size = pty.try_clone()?;
    let replies = input.clone();
    let handle = Handle::current();
    std::thread::Builder::new()
        .name("terminal".to_owned())
        .spawn(move || {
            let lifetime = Lifetime::new(leader);
            follow(
                &received, lifetime, screen, &size, &replies, &handle, doc, run,
            );
        })?;
    Ok((group, pty, input))
}

/// Writes the input in `keys` to the program, until there is no more.
fn write(mut writer: File, keys: &Receiver<Vec<u8>>) {
    for input in keys {
        if writer.write_all(&input).is_err() {
            return;
        }
    }
}

/// Shows the output in `chunks` as it arrives, until it ends and the leader exited.
#[allow(clippy::too_many_arguments)]
fn follow(
    chunks: &Receiver<Vec<u8>>,
    mut lifetime: Lifetime,
    mut screen: Screen,
    size: &Pty,
    replies: &Sender<Vec<u8>>,
    handle: &Handle,
    doc: DocumentId,
    run: u64,
) {
    let shared = Arc::new(Shared::default());
    // The screen as the buffer shows it at its end.
    let mut shown = Shown::default();
    let mut changed = false;
    let mut open = true;
    let mut drawing: Option<Instant> = None;
    loop {
        if open {
            match chunks.recv_timeout(TICK) {
                Ok(chunk) => {
                    if let Ok((cols, rows)) = size.size() {
                        screen.resize(cols, rows);
                    }
                    screen.push(&chunk);
                    for chunk in chunks.try_iter().take(CHUNKS) {
                        screen.push(&chunk);
                    }
                    let answers = screen.take_replies();
                    if !answers.is_empty() {
                        let _ = replies.send(answers);
                    }
                    changed = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => open = false,
            }
        } else {
            std::thread::sleep(TICK / 5);
        }

        drawing = screen
            .modes()
            .synchronized
            .then(|| drawing.unwrap_or_else(Instant::now));
        if changed && drawing.is_none_or(|since| since.elapsed() >= FRAME) {
            hand_over(&shared, handle, doc, run, |output| {
                update(output, &mut shown, &mut screen)
            });
            changed = false;
        }
        if lifetime.is_over(open) {
            break;
        }
    }

    let text = screen.finish();
    let replace = mem::take(&mut shown).chars;
    let end = lifetime.end();
    hand_over(&shared, handle, doc, run, |output| {
        output.push(replace, &text);
        output.screen = 0;
        (output.top, output.cursor) = (0, 0);
        output.end = Some(end);
    });
}

/// Adds the change of the screen to `output`, `shown` being the screen as the buffer shows it.
fn update(output: &mut Output, shown: &mut Shown, screen: &mut Screen) {
    let mut text = screen.take_scrolled();
    let now = screen.shown();
    text.append(&now);
    let (chars, bytes) = shown.common_lines(&text);
    let text = text.split_off(chars, bytes);
    output.push(shown.chars - chars, &text);
    output.screen = now.chars;
    (output.top, output.cursor) = (screen.top(), screen.cursor());
    output.title = screen.title().map(ToOwned::to_owned);
    output.modes = screen.modes();
    output.cleared |= screen.take_cleared();
    *shown = now;
}

/// Updates the pending output with `update`, and queues a callback that shows it.
fn hand_over(
    shared: &Arc<Shared>,
    handle: &Handle,
    doc: DocumentId,
    run: u64,
    update: impl FnOnce(&mut Output),
) {
    let mut pending = shared.pending.lock().unwrap();
    update(pending.output.get_or_insert_with(Output::default));
    loop {
        if !pending.queued {
            pending.queued = true;
            drop(pending);
            // Waits while the job queue is full rather than dropping the callback.
            handle.block_on(job::dispatch(take(shared.clone(), doc, run)));
            pending = shared.pending.lock().unwrap();
        }
        if pending
            .output
            .as_ref()
            .is_none_or(|output| output.text.text.len() <= PENDING)
        {
            return;
        }
        (pending, _) = shared
            .taken
            .wait_timeout(pending, Duration::from_millis(100))
            .unwrap();
    }
}

/// The callback that shows the pending output in the buffer.
fn take(
    shared: Arc<Shared>,
    doc: DocumentId,
    run: u64,
) -> impl FnOnce(&mut Editor, &mut Compositor) + Send + 'static {
    move |editor, _compositor| {
        let output = {
            let mut pending = shared.pending.lock().unwrap();
            pending.queued = false;
            pending.output.take()
        };
        shared.taken.notify_all();
        if let Some(output) = output {
            super::show(editor, doc, run, output);
        }
    }
}
