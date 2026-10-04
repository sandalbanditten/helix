//! Commands run as a group of processes, so that stopping one stops everything it started.

use std::{
    io,
    process::{Child, Command, ExitStatus},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// Spawns `command` leading a process group of its own: the leader to wait for, and the handle
/// that stops the group.
pub fn spawn_group(command: &mut Command) -> io::Result<(Leader, ProcessGroup)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe, and touches no memory of the parent.
        unsafe {
            command.pre_exec(|| match rustix::process::setsid() {
                Ok(_) => Ok(()),
                Err(err) => Err(err.into()),
            });
        }
    }
    let child = command.spawn()?;
    let status = Arc::new(GroupStatus::default());
    let group = ProcessGroup {
        pid: child.id(),
        status: status.clone(),
    };
    Ok((Leader { child, status }, group))
}

/// The process group a spawned command leads. Dropping it stops the group, unless its leader
/// was reaped.
#[derive(Debug)]
pub struct ProcessGroup {
    pid: u32,
    status: Arc<GroupStatus>,
}

/// What became of a process group, shared by its [`Leader`] and its [`ProcessGroup`].
#[derive(Debug, Default)]
struct GroupStatus {
    reaped: AtomicBool,
    killed: AtomicBool,
}

impl ProcessGroup {
    /// Stops every process of the group, unless its leader was reaped.
    pub fn kill(&self) -> io::Result<()> {
        if self.status.reaped.load(Ordering::Acquire) {
            return Ok(());
        }
        self.status.killed.store(true, Ordering::Release);
        kill_group(self.pid, false)
    }

    /// Tells the processes of the group that their terminal changed size.
    pub fn notify_resize(&self) -> io::Result<()> {
        if self.status.reaped.load(Ordering::Acquire) {
            return Ok(());
        }
        #[cfg(unix)]
        {
            signal_group(self.pid, rustix::process::Signal::WINCH)
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

/// The process leading a group, to wait for.
#[derive(Debug)]
pub struct Leader {
    child: Child,
    status: Arc<GroupStatus>,
}

impl Leader {
    /// Whether the leader exited, without reaping it.
    pub fn has_exited(&mut self) -> io::Result<bool> {
        #[cfg(all(unix, not(any(target_os = "openbsd", target_os = "redox"))))]
        {
            use rustix::process::{waitid, Pid, WaitId, WaitIdOptions};
            let pid = i32::try_from(self.child.id())
                .ok()
                .and_then(Pid::from_raw)
                .ok_or_else(|| io::Error::other("no such process"))?;
            let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
            Ok(waitid(WaitId::Pid(pid), options)?.is_some())
        }
        // Reaped here, the leader's pid might be reused while the group is signalled.
        #[cfg(not(all(unix, not(any(target_os = "openbsd", target_os = "redox")))))]
        {
            Ok(self.child.try_wait()?.is_some())
        }
    }

    /// Stops the processes of the group, by force if `force` is set.
    pub fn stop(&self, force: bool) -> io::Result<()> {
        kill_group(self.child.id(), force)
    }

    /// Whether the group was stopped through its [`ProcessGroup`].
    pub fn killed(&self) -> bool {
        self.status.killed.load(Ordering::Acquire)
    }

    /// Waits for the leader and reaps it, after which the group is no longer stopped.
    pub fn reap(mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait();
        self.status.reaped.store(true, Ordering::Release);
        status
    }
}

#[cfg(unix)]
fn kill_group(pid: u32, force: bool) -> io::Result<()> {
    use rustix::process::Signal;
    signal_group(pid, if force { Signal::KILL } else { Signal::TERM })
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: rustix::process::Signal) -> io::Result<()> {
    use rustix::process::{kill_process_group, Pid};
    // `kill(-1)` would signal every process we may signal.
    let pid = i32::try_from(pid)
        .ok()
        .filter(|&pid| pid > 1)
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::other("no such process group"))?;
    Ok(kill_process_group(pid, signal)?)
}

#[cfg(windows)]
fn kill_group(pid: u32, _force: bool) -> io::Result<()> {
    use std::process::Stdio;
    Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(drop)
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    use super::*;

    /// Runs `script` with `sh` in a group, and the pid it prints first.
    fn spawn(script: &str) -> (Leader, ProcessGroup, u32) {
        let (output, writer) = io::pipe().unwrap();
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdout(writer)
            .stdin(Stdio::null());
        let (leader, group) = spawn_group(&mut command).unwrap();
        drop(command);
        let mut line = String::new();
        BufReader::new(output).read_line(&mut line).unwrap();
        let pid: u32 = line.trim().parse().unwrap();
        // Stopped between its fork and its exec, the shell's child may hold signals back.
        until("the grandchild to start", || {
            std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap() == "sleep\n"
        });
        (leader, group, pid)
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Whether a process with `pid` still exists, waited for or not.
    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !stat.contains(") Z "))
    }

    #[test]
    fn dropping_the_group_stops_what_the_command_started() {
        let (leader, group, sleep) = spawn("sleep 30 & echo $!; wait");
        assert!(alive(sleep));
        drop(group);
        assert!(leader.killed());
        leader.reap().unwrap();
        until("the grandchild to stop", || !alive(sleep));
    }

    #[test]
    fn what_an_exited_leader_left_behind_is_stopped() {
        let (mut leader, group, sleep) = spawn("sleep 30 & echo $!");
        until("the leader to exit", || leader.has_exited().unwrap());
        // Not reaped yet, its group can still be stopped.
        assert!(alive(sleep));
        leader.stop(false).unwrap();
        until("the grandchild to stop", || !alive(sleep));
        assert!(!leader.killed());
        leader.reap().unwrap();
        drop(group);
    }

    #[test]
    fn processes_ignoring_terms_are_stopped_by_force() {
        let (mut leader, group, sleep) = spawn("trap '' TERM; sleep 30 & echo $!; wait");
        group.kill().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(alive(sleep) && !leader.has_exited().unwrap());
        leader.stop(true).unwrap();
        until("the grandchild to stop", || !alive(sleep));
        leader.reap().unwrap();
    }

    #[test]
    fn the_command_leads_a_session_of_its_own() {
        let (leader, group, sleep) = spawn("sleep 30 & echo $!; wait");
        // The session and the process group, fields 6 and 5 of the grandchild's stat.
        let stat = std::fs::read_to_string(format!("/proc/{sleep}/stat")).unwrap();
        let fields: Vec<&str> = stat.rsplit(')').next().unwrap().split(' ').collect();
        let leader_pid = group.pid.to_string();
        assert_eq!(fields[3..5], [leader_pid.as_str(), leader_pid.as_str()]);
        drop(group);
        leader.reap().unwrap();
    }

    #[test]
    fn a_reaped_group_is_not_stopped() {
        let mut command = Command::new("true");
        let (leader, group) = spawn_group(&mut command).unwrap();
        leader.reap().unwrap();
        // Signalling the group that is gone would fail.
        group.kill().unwrap();
    }
}
