//! Running the command of a compilation buffer, reading its output off the main thread.

use std::{
    io::{self, Read},
    mem,
    ops::Range,
    path::Path,
    process::{Command, Stdio},
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

use helix_stdx::{
    process::{self, Leader, ProcessGroup},
    pty::Pty,
};
use helix_view::{graphics::Style, DocumentId, Editor};
use tokio::runtime::Handle;

use super::{
    locus::{Finder, Locus},
    output::{End, Shown},
    screen::Screen,
};
use crate::{compositor::Compositor, job};

/// Output read but not yet in the buffer.
#[derive(Debug, Default)]
pub struct Output {
    /// The chars at the end of the buffer that `text` replaces.
    pub replace: usize,
    pub text: String,
    /// The chars of `text`.
    pub chars: usize,
    /// The loci in `text`, counting their chars from its start.
    pub loci: Vec<Locus>,
    /// The styles of char ranges of `text`, in order and apart.
    pub styles: Vec<(Range<usize>, Style)>,
    /// How the run ended, once it did; nothing is read after.
    pub end: Option<End>,
}

impl Output {
    /// Takes the lines of about the first `max` bytes, and how the run ended once nothing is
    /// left.
    fn take_front(&mut self, max: usize) -> Output {
        if self.text.len() <= max {
            return mem::take(self);
        }
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
        let rest = self.styles.split_off(
            self.styles
                .partition_point(|(range, _)| range.start < chars),
        );
        let styles = mem::replace(&mut self.styles, rest);
        for (range, _) in &mut self.styles {
            *range = range.start - chars..range.end - chars;
        }
        self.chars -= chars;
        Output {
            replace: mem::take(&mut self.replace),
            text,
            chars,
            loci,
            styles,
            end: None,
        }
    }

    /// Adds `shown` with the `loci` in it, in place of the last `replace` chars of the output.
    fn push(&mut self, replace: usize, shown: &Shown, loci: Vec<Locus>) {
        let pending = replace.min(self.chars);
        if pending > 0 {
            let at = self
                .text
                .char_indices()
                .rev()
                .nth(pending - 1)
                .map_or(0, |(at, _)| at);
            self.text.truncate(at);
            self.chars -= pending;
            let kept = self
                .styles
                .partition_point(|(range, _)| range.start < self.chars);
            self.styles.truncate(kept);
            let kept = self.loci.partition_point(|locus| locus.start < self.chars);
            self.loci.truncate(kept);
        }
        self.replace += replace - pending;
        let start = self.chars;
        self.loci.extend(loci.into_iter().map(|mut locus| {
            locus.start += start;
            locus
        }));
        let styles = shown.styles.iter();
        self.styles
            .extend(styles.map(|(range, style)| (range.start + start..range.end + start, *style)));
        self.text.push_str(&shown.text);
        self.chars += shown.chars;
    }

    fn is_empty(&self) -> bool {
        self.replace == 0 && self.text.is_empty() && self.end.is_none()
    }
}

/// The output the reader hands over to the main thread.
#[derive(Debug, Default)]
struct Pending {
    output: Output,
    /// Whether a callback that takes output is queued.
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
/// The reader waits while this much output is pending.
const PENDING: usize = 4 * 1024 * 1024;
/// How often the reader looks at the leader when no output arrives.
const TICK: Duration = Duration::from_millis(50);
/// The screen shows anew at most this often, as a progress bar is redrawn say.
const REFRESH: Duration = Duration::from_millis(100);
/// How long what keeps running has before it is stopped by force, after a kill or after the
/// leader exited.
const GRACE: Duration = Duration::from_secs(1);

/// The environment of commands writing to a terminal.
const TERMINAL: [(&str, &str); 4] = [
    ("TERM", "xterm-256color"),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("MANPAGER", "cat"),
];

/// What makes tools color their output into a pipe, unless the environment says otherwise.
const FORCE_COLORS: [(&str, &str); 3] = [
    ("CARGO_TERM_COLOR", "always"),
    ("CLICOLOR_FORCE", "1"),
    ("FORCE_COLOR", "1"),
];

/// Runs `command` with `shell` in `dir`, and appends what it writes to the compilation buffer
/// `doc` as run `run`, with the loci `finder` finds in it. Returns the group that stops it, and
/// its terminal of `size`, if there is one.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    shell: &[String],
    command: &str,
    dir: &Path,
    (cols, rows): (u16, u16),
    colors: bool,
    finder: Finder,
    doc: DocumentId,
    run: u64,
) -> io::Result<(ProcessGroup, Option<Pty>)> {
    let Some((program, args)) = shell.split_first() else {
        return Err(io::Error::other("No shell set"));
    };
    let mut process = Command::new(program);
    process
        .args(args)
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null());
    let (output, terminal, screen): (Box<dyn Read + Send>, _, _) = match Pty::open(cols, rows) {
        Ok((terminal, end)) => {
            process.stdout(end.try_clone()?).stderr(end).envs(TERMINAL);
            let output = terminal.reader()?;
            (
                Box::new(output),
                Some(terminal),
                Screen::new(cols, rows, colors),
            )
        }
        Err(err) => {
            if err.kind() != io::ErrorKind::Unsupported {
                log::warn!("cannot open a pseudo-terminal, running on a pipe: {err}");
            }
            let (output, end) = io::pipe()?;
            process.stdout(end.try_clone()?).stderr(end);
            if colors {
                for (name, value) in FORCE_COLORS {
                    if std::env::var_os(name).is_none() {
                        process.env(name, value);
                    }
                }
            }
            // Nothing redraws output on a pipe, so the screen is one row.
            (Box::new(output), None, Screen::new(u16::MAX, 1, colors))
        }
    };
    let (leader, group) = process::spawn_group(&mut process)?;
    // The command keeps the end it writes to, which must close for the output to end.
    drop(process);

    // A few chunks in flight at most, so that a busy editor makes the command wait.
    let (chunks, received) = mpsc::sync_channel(16);
    std::thread::Builder::new()
        .name("compilation output".to_owned())
        .spawn(move || read(output, &chunks))?;
    let size = terminal.as_ref().map(Pty::try_clone).transpose()?;
    let handle = Handle::current();
    std::thread::Builder::new()
        .name("compilation".to_owned())
        .spawn(move || follow(&received, leader, screen, size, finder, &handle, doc, run))?;
    Ok((group, terminal))
}

/// Reads `output` until it ends, sending what it reads to `chunks`.
fn read(mut output: impl Read, chunks: &SyncSender<Vec<u8>>) {
    let mut buf = vec![0; 64 * 1024];
    loop {
        match output.read(&mut buf) {
            Ok(0) => return,
            // The run stopped following its output.
            Ok(read) if chunks.send(buf[..read].to_vec()).is_err() => return,
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                log::warn!("cannot read compilation output: {err}");
                return;
            }
        }
    }
}

/// Hands over the output in `chunks` as it arrives, until it ends and the leader exited.
#[allow(clippy::too_many_arguments)]
fn follow(
    chunks: &Receiver<Vec<u8>>,
    mut leader: Leader,
    mut screen: Screen,
    size: Option<Pty>,
    mut finder: Finder,
    handle: &Handle,
    doc: DocumentId,
    run: u64,
) {
    let shared = Arc::new(Shared::default());
    let hand_over = |add: &mut dyn FnMut(&mut Output)| hand_over(&shared, handle, doc, run, add);
    // The screen as the buffer shows it at its end, and when it showed.
    let mut shown = Shown::default();
    let mut shown_at: Option<Instant> = None;
    let mut changed = false;
    let mut open = true;
    let mut exited: Option<Instant> = None;
    let mut killed: Option<Instant> = None;
    let mut forced = false;
    loop {
        if open {
            // Until the screen is due to show again, once it changed.
            let wait = match shown_at {
                Some(at) if changed => REFRESH.saturating_sub(at.elapsed()).min(TICK),
                _ => TICK,
            };
            match chunks.recv_timeout(wait) {
                Ok(chunk) => {
                    if let Some((cols, rows)) = size.as_ref().and_then(|pty| pty.size().ok()) {
                        screen.resize(cols, rows);
                    }
                    screen.push(&chunk);
                    changed = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => open = false,
            }
        } else {
            std::thread::sleep(TICK / 5);
        }

        let due = shown_at.is_none_or(|at| at.elapsed() >= REFRESH);
        if changed && (due || screen.scrolled_len() >= BATCH) {
            let (replace, text, mut loci) = change(
                &mut shown,
                screen.take_scrolled(),
                screen.shown(),
                &mut finder,
            );
            if replace > 0 || !text.text.is_empty() {
                hand_over(&mut |output| output.push(replace, &text, mem::take(&mut loci)));
            }
            shown_at = Some(Instant::now());
            changed = false;
        }

        if exited.is_none() && leader.has_exited().unwrap_or(true) {
            exited = Some(Instant::now());
            // What it left running would keep the output, and the run, going.
            let _ = leader.stop(false);
        }
        if killed.is_none() && leader.killed() {
            killed = Some(Instant::now());
        }
        let overdue = |since: Option<Instant>| since.is_some_and(|since| since.elapsed() >= GRACE);
        if !forced && (overdue(killed) || open && overdue(exited)) {
            let _ = leader.stop(true);
            forced = true;
        }
        // Processes of other groups may keep the output open longer.
        let given_up = exited.is_some_and(|exited| exited.elapsed() >= 2 * GRACE);
        if exited.is_some() && (!open || given_up) {
            break;
        }
    }

    let (replace, text, mut loci) =
        change(&mut shown, screen.finish(), Shown::default(), &mut finder);
    let killed = leader.killed();
    let end = match leader.reap() {
        _ if killed => End::Killed,
        Ok(exit) => exit_end(exit),
        Err(err) => {
            log::warn!("cannot wait for the compilation: {err}");
            End::Exited(-1)
        }
    };
    hand_over(&mut |output| {
        output.push(replace, &text, mem::take(&mut loci));
        output.end = Some(end);
    });
}

/// How the end of the buffer changes from the screen it shows, `shown`, once the lines
/// `scrolled` scrolled away and the screen shows `screen`. `shown` becomes `screen`.
fn change(
    shown: &mut Shown,
    scrolled: Shown,
    screen: Shown,
    finder: &mut Finder,
) -> (usize, Shown, Vec<Locus>) {
    let mut loci = finder.find(&scrolled.text);
    let start = scrolled.chars;
    loci.extend(finder.peek(&screen.text).into_iter().map(|mut locus| {
        locus.start += start;
        locus
    }));
    let mut text = scrolled;
    text.append(&screen);
    let (chars, bytes) = shown.common_lines(&text);
    let text = text.split_off(chars, bytes);
    loci.retain(|locus| locus.start >= chars);
    for locus in &mut loci {
        locus.start -= chars;
    }
    let replace = shown.chars - chars;
    *shown = screen;
    (replace, text, loci)
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

/// Adds to the pending output with `add`, and queues a callback that appends it to the buffer.
fn hand_over(
    shared: &Arc<Shared>,
    handle: &Handle,
    doc: DocumentId,
    run: u64,
    add: &mut dyn FnMut(&mut Output),
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

/// The callback that appends the next batch of pending output to the buffer.
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

    fn plain(text: &str) -> Shown {
        Shown {
            text: text.to_owned(),
            chars: text.chars().count(),
            styles: Vec::new(),
        }
    }

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
        output.push(0, &plain("äb\ncd\n"), vec![locus(0), locus(3)]);
        output.push(0, &plain("ef\n"), vec![locus(1)]);
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

    #[test]
    fn styles_go_with_their_text() {
        let red = Style::default().fg(helix_view::graphics::Color::Indexed(1));
        let styled = |text: &str, range: Range<usize>| Shown {
            styles: vec![(range, red)],
            ..plain(text)
        };
        let mut output = Output::default();
        output.push(0, &styled("ab\ncd\n", 3..5), Vec::new());
        output.push(0, &styled("e", 0..1), Vec::new());
        assert_eq!(output.styles, [(3..5, red), (6..7, red)]);
        // The unfinished line goes with its styles, and batches take theirs.
        output.push(1, &styled("ef\n", 1..2), Vec::new());
        assert_eq!(output.styles, [(3..5, red), (7..8, red)]);
        let first = output.take_front(1);
        assert_eq!((first.text.as_str(), first.styles), ("ab\n", Vec::new()));
        assert_eq!(output.styles, [(0..2, red), (4..5, red)]);
    }

    #[test]
    fn unfinished_lines_are_replaced() {
        // Still pending, the unfinished line goes; in the buffer, the batch replaces it there.
        let mut output = Output::default();
        output.push(0, &plain("one\ntw"), Vec::new());
        output.push(2, &plain("two\nthr"), Vec::new());
        assert_eq!(
            (output.replace, output.text.as_str(), output.chars),
            (0, "one\ntwo\nthr", 11)
        );
        let taken = output.take_front(64);
        assert_eq!(taken.text, "one\ntwo\nthr");
        output.push(3, &plain("thrëe\n"), vec![locus(0)]);
        assert_eq!(
            (output.replace, output.text.as_str(), output.chars),
            (3, "thrëe\n", 6)
        );
        assert_eq!(output.loci, [locus(0)]);
        // A batch carries what it replaces, the rest none.
        output.push(0, &plain("four\n"), Vec::new());
        let first = output.take_front(1);
        assert_eq!((first.replace, first.text.as_str()), (3, "thrëe\n"));
        assert_eq!((output.replace, output.text.as_str()), (0, "four\n"));
    }
}
