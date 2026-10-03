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
pub struct Outcome {
    pub alignment: Arc<Alignment>,
    pub fallback: Option<String>,
}

/// Whether `difft` is installed, looked for once.
pub fn has_difft() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| helix_stdx::env::which("difft").is_ok())
}

/// Lines up `old` and `new`, the texts of the file shown as `path`, with `tool`. difftastic falls
/// back to the builtin diff where it isn't installed or fails.
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
/// prints. The path picks the language, as when git runs it.
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no reason given");
        return Err(format!("difft failed: {reason}"));
    }
    Ok(output.stdout)
}
