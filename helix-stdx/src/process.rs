//! Commands run as a group of processes, so that stopping one stops everything it started.

use std::{
    io,
    process::{Child, Command},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// Spawns `command` leading a process group of its own, and the handle that stops the group.
///
/// Whoever waits for the child marks the group [`GroupStatus::exited`] once it returns, so that
/// dropping the handle afterwards stops nothing.
pub fn spawn_group(command: &mut Command) -> io::Result<(Child, ProcessGroup)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn()?;
    let group = ProcessGroup {
        pid: child.id(),
        status: Arc::default(),
    };
    Ok((child, group))
}

/// The process group a spawned command leads. Dropping it stops the group, unless its leader
/// has exited.
#[derive(Debug)]
pub struct ProcessGroup {
    pid: u32,
    status: Arc<GroupStatus>,
}

/// What became of a process group, shared with whoever waits for its leader.
#[derive(Debug, Default)]
pub struct GroupStatus {
    exited: AtomicBool,
    killed: AtomicBool,
}

impl GroupStatus {
    /// Notes that the leader exited and was waited for, so the group is no longer stopped.
    pub fn exited(&self) {
        self.exited.store(true, Ordering::Release);
    }

    /// Whether the group was stopped through its [`ProcessGroup`].
    pub fn killed(&self) -> bool {
        self.killed.load(Ordering::Acquire)
    }
}

impl ProcessGroup {
    pub fn status(&self) -> Arc<GroupStatus> {
        self.status.clone()
    }

    /// Stops every process of the group, unless its leader has exited: with `SIGTERM` on Unix,
    /// with `taskkill /T /F` on Windows.
    pub fn kill(&self) -> io::Result<()> {
        if self.status.exited.load(Ordering::Acquire) {
            return Ok(());
        }
        self.status.killed.store(true, Ordering::Release);
        kill_group(self.pid)
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[cfg(unix)]
fn kill_group(pid: u32) -> io::Result<()> {
    use rustix::process::{kill_process_group, Pid, Signal};
    // `kill(-1)` would signal every process we may signal.
    let pid = i32::try_from(pid)
        .ok()
        .filter(|&pid| pid > 1)
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::other("no such process group"))?;
    Ok(kill_process_group(pid, Signal::TERM)?)
}

#[cfg(windows)]
fn kill_group(pid: u32) -> io::Result<()> {
    use std::process::Stdio;
    Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    use super::*;

    /// Whether a process with `pid` still exists, waited for or not.
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
            && !std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|stat| stat.contains(") Z "))
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_group_stops_what_the_command_started() {
        // The shell starts a `sleep` of its own and prints its pid.
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(Stdio::piped())
            .stdin(Stdio::null());
        let (mut child, group) = spawn_group(&mut command).unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        let sleep: u32 = line.trim().parse().unwrap();
        // Stopped between its fork and its exec, the shell's child may hold the signal back.
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(format!("/proc/{sleep}/comm")).unwrap() != "sleep\n" {
            assert!(Instant::now() < deadline, "the grandchild never starts");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(alive(sleep));

        let status = group.status();
        drop(group);
        assert!(status.killed());
        child.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(sleep) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!alive(sleep), "the grandchild still runs");
    }

    #[test]
    fn an_exited_group_is_not_stopped() {
        let mut command = Command::new(if cfg!(windows) { "cmd" } else { "true" });
        if cfg!(windows) {
            command.args(["/C", "exit"]);
        }
        let (mut child, group) = spawn_group(&mut command).unwrap();
        child.wait().unwrap();
        let status = group.status();
        status.exited();
        drop(group);
        assert!(!status.killed());
    }
}
