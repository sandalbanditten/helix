//! The text of a compilation buffer: the output of its command as a terminal shows it, between a
//! header and a footer.

use std::{path::Path, time::Duration};

use jiff::Zoned;

/// The lines before the output: the command, then where and when it started.
pub fn header(command: &str, dir: &Path, started: &Zoned) -> String {
    let dir = helix_stdx::path::fold_home_dir(dir);
    format!(
        "{command}\nin {}, started {}\n\n",
        dir.display(),
        started.strftime("%H:%M:%S")
    )
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The command exited with a code, 0 when it succeeded.
    Exited(i32),
    /// A signal from elsewhere ended the command.
    Signal(i32),
    /// It was stopped from the editor.
    Killed,
}

impl End {
    pub fn failed(self) -> bool {
        self != Self::Exited(0)
    }

    /// Tells how the compilation ended, for the status line.
    pub fn status(self) -> String {
        match self {
            Self::Exited(0) => "Compilation finished".to_owned(),
            Self::Exited(code) => format!("Compilation exited with code {code}"),
            Self::Signal(signal) => format!("Compilation killed by signal {signal}"),
            Self::Killed => "Compilation killed".to_owned(),
        }
    }
}

/// The lines after the output: how and when the run ended, and after how long.
pub fn footer(end: End, ended: &Zoned, elapsed: Duration) -> String {
    let how = match end {
        End::Exited(0) => "Finished".to_owned(),
        End::Exited(code) => format!("Exited with code {code}"),
        End::Signal(signal) => format!("Killed by signal {signal}"),
        End::Killed => "Killed".to_owned(),
    };
    format!(
        "\n{how} at {} after {}\n",
        ended.strftime("%H:%M:%S"),
        duration(elapsed)
    )
}

/// A duration as precise as it is worth: `2.81 s`, `12.3 s` or `1:02:03`.
fn duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.2} s")
    } else if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let secs = elapsed.as_secs();
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_footer() {
        let at: Zoned = "2026-10-01T14:03:12+02:00[Europe/Copenhagen]"
            .parse()
            .unwrap();
        let dir = std::env::temp_dir().join("demo");
        assert_eq!(
            header("cargo build", &dir, &at),
            format!("cargo build\nin {}, started 14:03:12\n\n", dir.display())
        );
        for (end, elapsed, footer_text) in [
            (
                End::Exited(101),
                2.81,
                "\nExited with code 101 at 14:03:12 after 2.81 s\n",
            ),
            (
                End::Exited(0),
                12.34,
                "\nFinished at 14:03:12 after 12.3 s\n",
            ),
            (End::Killed, 3723.0, "\nKilled at 14:03:12 after 1:02:03\n"),
            (
                End::Signal(9),
                0.5,
                "\nKilled by signal 9 at 14:03:12 after 0.50 s\n",
            ),
        ] {
            assert_eq!(
                footer(end, &at, Duration::from_secs_f64(elapsed)),
                footer_text
            );
        }
        assert_eq!(
            End::Exited(101).status(),
            "Compilation exited with code 101"
        );
        assert!(!End::Exited(0).failed());
        assert!(End::Killed.failed());
    }
}
