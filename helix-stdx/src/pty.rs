//! Pseudo-terminals, to run commands as if a terminal showed their output.

use std::{
    fs::File,
    io::{self, Read},
};

/// The end of a pseudo-terminal that output is read from.
#[derive(Debug)]
pub struct Pty {
    master: File,
}

impl Pty {
    /// Opens a pseudo-terminal `cols` wide and `rows` high, and its terminal end for commands to
    /// write to. Fails where there are no pseudo-terminals, like on Windows.
    pub fn open(cols: u16, rows: u16) -> io::Result<(Self, File)> {
        imp::open(cols, rows)
    }

    /// Changes the size of the terminal, see
    /// [`ProcessGroup::notify_resize`](crate::process::ProcessGroup::notify_resize).
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        imp::resize(&self.master, cols, rows)
    }

    /// The columns and rows of the terminal.
    pub fn size(&self) -> io::Result<(u16, u16)> {
        imp::size(&self.master)
    }

    /// Another handle of the pseudo-terminal, to resize it or to learn its size elsewhere.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            master: self.master.try_clone()?,
        })
    }

    /// A reader of the output, which ends once no command has the terminal end open.
    pub fn reader(&self) -> io::Result<Reader> {
        Ok(Reader(self.master.try_clone()?))
    }
}

/// Reads the output of a [`Pty`].
#[derive(Debug)]
pub struct Reader(File);

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            // Linux fails reads once the terminal end is closed everywhere.
            #[cfg(unix)]
            Err(err) if err.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => Ok(0),
            result => result,
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "illumos"
))]
mod imp {
    use std::{fs::File, io};

    use rustix::{
        fs::{Mode, OFlags},
        pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags},
        termios::{
            tcgetattr, tcgetwinsize, tcsetattr, tcsetwinsize, OptionalActions, OutputModes, Winsize,
        },
    };

    use super::Pty;

    pub fn open(cols: u16, rows: u16) -> io::Result<(Pty, File)> {
        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY;
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
        let master = openpt(flags | OpenptFlags::CLOEXEC)?;
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
        let master = {
            use rustix::io::{fcntl_setfd, FdFlags};
            let master = openpt(flags)?;
            fcntl_setfd(&master, FdFlags::CLOEXEC)?;
            master
        };
        grantpt(&master)?;
        unlockpt(&master)?;
        let name = ptsname(&master, Vec::new())?;
        let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
        let terminal = rustix::fs::open(name.as_c_str(), flags, Mode::empty())?;
        // Left on, output processing turns each `\n` into `\r\n`, which makes reading the
        // output 30 times slower.
        let mut termios = tcgetattr(&terminal)?;
        termios.output_modes.remove(OutputModes::OPOST);
        tcsetattr(&terminal, OptionalActions::Now, &termios)?;
        let master = File::from(master);
        resize(&master, cols, rows)?;
        Ok((Pty { master }, File::from(terminal)))
    }

    pub fn resize(master: &File, cols: u16, rows: u16) -> io::Result<()> {
        let size = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        Ok(tcsetwinsize(master, size)?)
    }

    pub fn size(master: &File) -> io::Result<(u16, u16)> {
        let size = tcgetwinsize(master)?;
        Ok((size.ws_col, size.ws_row))
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "illumos"
)))]
mod imp {
    use std::{fs::File, io};

    use super::Pty;

    pub fn open(_cols: u16, _rows: u16) -> io::Result<(Pty, File)> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub fn resize(_master: &File, _cols: u16, _rows: u16) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub fn size(_master: &File) -> io::Result<(u16, u16)> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };

    use super::*;
    use crate::process::{spawn_group, Leader, ProcessGroup};

    /// Runs `script` with `sh` on the terminal end of a pty `cols` wide and `rows` high.
    fn run(cols: u16, rows: u16, script: &str) -> (Pty, Reader, Leader, ProcessGroup) {
        let (pty, terminal) = Pty::open(cols, rows).unwrap();
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(terminal.try_clone().unwrap())
            .stderr(terminal);
        let (leader, group) = spawn_group(&mut command).unwrap();
        // The command keeps the terminal end, which must close for the output to end.
        drop(command);
        let reader = pty.reader().unwrap();
        (pty, reader, leader, group)
    }

    fn output(script: &str) -> String {
        let (_pty, mut reader, leader, _group) = run(80, 24, script);
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        leader.reap().unwrap();
        output
    }

    #[test]
    fn commands_write_to_a_terminal() {
        assert_eq!(output("test -t 1 && test -t 2 && echo yes"), "yes\n");
        // There's no controlling terminal to prompt on.
        assert_eq!(output("cat /dev/tty 2>/dev/null || echo none"), "none\n");
    }

    #[test]
    fn output_arrives_unchanged_and_whole() {
        let output = output("seq 200000; printf 'a\\r\\nb'");
        let lines: Vec<&str> = output.split('\n').collect();
        assert_eq!(lines.len(), 200_002);
        assert_eq!(lines[199_999], "200000");
        assert_eq!(lines[200_000..], ["a\r", "b"]);
    }

    #[test]
    fn commands_learn_of_resizes() {
        let script =
            "trap 'stty size <&1; exit' WINCH; stty size <&1; while :; do sleep 0.01; done";
        let (pty, reader, leader, group) = run(100, 30, script);
        let mut lines = BufReader::new(reader).lines();
        assert_eq!(lines.next().unwrap().unwrap(), "30 100");
        pty.try_clone().unwrap().resize(120, 40).unwrap();
        assert_eq!(pty.size().unwrap(), (120, 40));
        group.notify_resize().unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "40 120");
        assert!(lines.next().is_none());
        leader.reap().unwrap();
    }
}
