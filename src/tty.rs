//! Small interactive-terminal helpers for the palette running in the popup.
//! The popup has a real TTY, so these read the user's keystrokes directly
//! (fzf is spawned for the pick lists; `ask` drops to a plain prompt).

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::process::Command;

use crate::herdr;

/// Print `$*` to stderr, hold briefly, then exit 1. Called at the END of a
/// dispatch error path so it passes through `close_self` before exiting, so a
/// failure surfaces visibly instead of vanishing with the popup.
pub fn die(msg: &str) {
    eprintln!("\n{msg}");
    eprintln!("Press any key to close…");
    let _ = read_key();
    std::process::exit(1);
}

/// Read a single key from the controlling terminal, if any.
fn read_key() -> Option<u8> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .open("/dev/tty")
        .ok()?;
    let mut buf = [0u8; 1];
    // Best-effort: the popup's stdin usually carries keystrokes too.
    tty.read_exact(&mut buf).ok()?;
    Some(buf[0])
}

/// Prompt the user for a single-line answer on the controlling TTY.
/// Empty or aborted input returns an empty string; callers treat that as cancel.
pub fn ask(prompt: &str) -> String {
    let mut tty = std::fs::OpenOptions::new()
        .write(true)
        .read(true)
        .open("/dev/tty")
        .ok()
        .unwrap_or_else(|| {
            // Fall back to stdout/stdin (works when not literally a tty, e.g. tests).
            std::fs::File::open("/dev/null")
                .unwrap_or_else(|_| std::fs::File::open("/dev/full").expect("no tty"))
        });
    let _ = tty.write_all(prompt.as_bytes());
    let _ = tty.flush();
    let mut buf = String::new();
    match (&mut tty as &mut dyn Read).read_to_string(&mut buf) {
        Ok(_) => buf.trim().to_string(),
        Err(_) => String::new(),
    }
}

/// Run a second fzf over `lines` (each `value\tlabel`), displaying the label and
/// returning the full chosen line. Returns None on cancel. `lines` may also be
/// single-column (label only); then the label is the value.
pub fn pick_lines(lines: &[String], prompt: &str) -> Option<String> {
    pick_lines_q(lines, prompt, "")
}

/// Like `pick_lines`, but seeds fzf's input box with `prefill` so the user
/// starts with the filename query already typed.
pub fn pick_lines_q(lines: &[String], prompt: &str, prefill: &str) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let input = lines.join("\n") + "\n";
    let is_tsv = lines[0].contains('\t');
    let with_nth = if is_tsv { "2" } else { "1" };
    let mut cmd = Command::new("fzf");
    cmd.args(["--delimiter=\t", &format!("--with-nth={with_nth}")])
        .args(["--prompt", prompt]);
    if !prefill.is_empty() {
        cmd.args(["--query", prefill]);
    }
    cmd.args([
        "--reverse",
        "--cycle",
        "--no-multi",
        "--tiebreak=begin,index",
    ]);
    let mut child = match cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return None,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run a `herdr` command, dying with its stderr on failure. stderr is captured
/// and surfaced so a failed pane/tab/workspace op doesn't vanish with the popup.
/// Accepts any iterator of `AsRef<OsStr>` — slice literals or `Vec<String>`.
pub fn run_cli<I, S>(args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arg_strings: Vec<String> = args
        .into_iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    let out = herdr::run(&arg_strings);
    if !out.status.success() {
        let cmdline = arg_strings.join(" ");
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        die(&format!("herdr {cmdline} failed: {err}"));
    }
}
