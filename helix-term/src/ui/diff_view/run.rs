//! Working out how two texts line up: by `difft`, or by Helix's own line diff.

use std::{
    process::Stdio,
    sync::{Arc, OnceLock},
};

use helix_core::Rope;
use helix_view::{
    diff_view::{builtin, difftastic, Alignment},
    editor::DiffTool,
};

/// How a diff came out: the alignment, and why it isn't difftastic's if `tool` asked for it.
#[derive(Clone)]
pub struct Outcome {
    pub alignment: Arc<Alignment>,
    pub fallback: Option<String>,
}

/// Whether `difft` is installed, looked for once.
pub fn has_difft() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| helix_stdx::env::which("difft").is_ok())
}

/// Lines up `old` and `new`, the texts of the file shown as `path`, with `tool`.
pub async fn align(tool: DiffTool, path: String, old: Rope, new: Rope) -> Outcome {
    let fallback = match tool {
        DiffTool::Builtin => None,
        DiffTool::Difftastic if !has_difft() => Some("difft is not installed".to_owned()),
        DiffTool::Difftastic => match difftastic(&path, &old, &new).await {
            Ok(json) => {
                let (old, new) = (old.clone(), new.clone());
                let parsed = tokio::task::spawn_blocking(move || {
                    difftastic::parse(&json, old.slice(..), new.slice(..))
                })
                .await;
                match parsed {
                    Ok(Ok(alignment)) => {
                        return Outcome {
                            alignment: Arc::new(alignment),
                            fallback: None,
                        }
                    }
                    Ok(Err(err)) => Some(format!("difft gave {err}")),
                    Err(err) => Some(format!("reading difft's output failed: {err}")),
                }
            }
            Err(err) => Some(err),
        },
    };
    let alignment =
        tokio::task::spawn_blocking(move || builtin::align(old.slice(..), new.slice(..)))
            .await
            .unwrap_or_default();
    Outcome {
        alignment: Arc::new(alignment),
        fallback,
    }
}

/// Runs `difft` on `old` and `new`, the texts of the file shown as `path`, returning the JSON it
/// prints.
async fn difftastic(path: &str, old: &Rope, new: &Rope) -> Result<Vec<u8>, String> {
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    let (old_file, new_file) = (dir.path().join("old"), dir.path().join("new"));
    for (file, text) in [(&old_file, old), (&new_file, new)] {
        let text = text.to_string();
        tokio::fs::write(file, text)
            .await
            .map_err(|err| err.to_string())?;
    }
    let output = tokio::process::Command::new("difft")
        .env("DFT_DISPLAY", "json")
        .env("DFT_UNSTABLE", "yes")
        .arg(path)
        .arg(&old_file)
        .args(["0", "100644"])
        .arg(&new_file)
        .args(["0", "100644"])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|err| format!("difft failed to run: {err}"))?;
    if !output.status.success() {
        return Err(failure(&String::from_utf8_lossy(&output.stderr)));
    }
    Ok(output.stdout)
}

/// What went wrong, from what `difft` printed to `stderr`.
fn failure(stderr: &str) -> String {
    let mut lines = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    match lines.next() {
        Some(line) if line.contains(" panicked at ") => {
            format!(
                "difft panicked: {}",
                lines.next().unwrap_or("no reason given")
            )
        }
        Some(line) => format!("difft failed: {line}"),
        None => "difft failed: no reason given".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_tell_what_went_wrong() {
        assert_eq!(
            failure("\nthread 'main' (1) panicked at src/display/hunks.rs:667:31:\nHunk lines should be present\nnote: run with `RUST_BACKTRACE=1`\n"),
            "difft panicked: Hunk lines should be present"
        );
        assert_eq!(
            failure("error: unexpected argument\n"),
            "difft failed: error: unexpected argument"
        );
        assert_eq!(failure(""), "difft failed: no reason given");
    }

    /// Times `difft` and reading its output on changes to this repository's biggest files. Run it with
    /// `cargo test --release -p helix-term --lib measure_difftastic -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_difftastic() {
        use std::time::Instant;

        let show = |revision: &str| {
            let output = std::process::Command::new("git")
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .args(["show", revision])
                .output()
                .unwrap();
            assert!(output.status.success(), "git show {revision}");
            Rope::from(String::from_utf8(output.stdout).unwrap())
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for (path, old, new) in [
            ("helix-term/src/ui/editor.rs", "54ad394~10", "54ad394"),
            ("helix-term/src/commands.rs", "54ad394~60", "54ad394"),
        ] {
            let old = show(&format!("{old}:{path}"));
            let new = show(&format!("{new}:{path}"));
            let start = Instant::now();
            let json = runtime.block_on(difftastic(path, &old, &new));
            let ran = start.elapsed();
            let json = match json {
                Ok(json) => json,
                Err(err) => {
                    let start = Instant::now();
                    let outcome = runtime.block_on(align(DiffTool::Builtin, path.into(), old, new));
                    eprintln!(
                        "{path}: {err} after {ran:?}; builtin diff {:?}, {} hunks",
                        start.elapsed(),
                        outcome.alignment.hunks().len()
                    );
                    continue;
                }
            };
            let start = Instant::now();
            let alignment = difftastic::parse(&json, old.slice(..), new.slice(..)).unwrap();
            eprintln!(
                "{path}: {} KB, difft {ran:?}, {} KB of JSON read in {:?}, {} hunks",
                new.len_bytes() / 1000,
                json.len() / 1000,
                start.elapsed(),
                alignment.hunks().len()
            );
        }
    }
}
