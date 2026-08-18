//! Thin wrapper around the `herdr` CLI (via $HERDR_BIN_PATH, falling back to
//! `herdr` on PATH). Every plugin command runs on the herdr server with no TTY
//! (about the action/pane lifecycle) or inside the popup with a real TTY — the
//! wrapper keeps the calls honest: real argv, real exit status.

use std::ffi::OsStr;
use std::process::{Command, Output};

/// Resolve the herdr binary path. herdr injects $HERDR_BIN_PATH into every
/// plugin command; we fall back to `herdr` on PATH for manual/dry-run use.
pub fn bin() -> String {
    std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string())
}

/// Run `herdr <args>` and return the raw output.
pub fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(bin())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("telescope: failed to spawn herdr CLI {}: {e}", bin()))
}

/// Run `herdr <args>`, parse stdout as JSON, and return it as a serde_json::Value.
/// Returns Ok(None) when the command failed or stdout wasn't JSON.
pub fn json<I, S>(args: I) -> Option<serde_json::Value>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = run(args);
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}
