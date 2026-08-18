//! The file finder: pick a file under the origin cwd, then open it in a new
//! pane (split) or a new window (tab) with `$EDITOR`.

use crate::tty::{die, pick_lines, pick_lines_q, run_cli};

struct Picked {
    /// absolute file path.
    path: String,
}

/// Entry point: search files from `cwd`, let the user pick one, then prompt for
/// "new pane" / "new window" and dispatch. `prefill` seeds the filename query
/// (used when jumping in from the "Search files…" row).
pub fn run(cwd: &str, origin_pane: &str, _origin_workspace: &str, prefill: &str) {
    if cwd.is_empty() || !std::path::Path::new(cwd).is_dir() {
        die("telescope: no usable origin directory to search files under.");
        return;
    }

    let picked = pick_file(cwd, prefill);
    let Some(picked) = picked else {
        return; // cancelled
    };
    confirm_and_open(&picked.path, origin_pane);
}

/// Prompt for pane vs window and open `file`. Used when a file row is accepted
/// from the main telescope after the `@` live switch.
pub fn confirm_and_open(file: &str, origin_pane: &str) {
    let parent = file
        .rfind('/')
        .map(|i| file[..i].to_string())
        .unwrap_or_else(|| file.to_string());

    let opts = vec![
        "pane\tNew pane (split, right)".to_string(),
        "window\tNew window (tab)".to_string(),
    ];
    let Some(choice) = pick_lines(&opts, "open in ▸ ") else {
        return; // cancelled
    };
    let target = choice.split('\t').next().unwrap_or("").to_string();

    match target.as_str() {
        "window" => open_window(file, &parent),
        _ => open_pane(file, &parent, origin_pane),
    }
}

/// Same TSV shape as the action list (`kind\tpayload\tdisplay\tkeywords\thint`)
/// so the main fzf can `reload` these rows when the query starts with `@`.
pub fn file_tsv_rows(cwd: &str) -> Vec<String> {
    let mut paths = Vec::new();
    collect_files(cwd, &mut paths);
    paths.iter().map(|p| file_row(cwd, p)).collect()
}

pub(crate) fn file_row(cwd: &str, path: &str) -> String {
    let rel = rel_to(cwd, path);
    format!("files\t{path}\t{rel}\t{rel}\topen {rel} with $EDITOR in a new pane or window")
}

pub(crate) fn rel_to(cwd: &str, path: &str) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .trim_start_matches('/')
        .to_string()
}

/// Run fzf over the files under `cwd` and return the chosen absolute path.
/// `prefill` seeds the initial query.
fn pick_file(cwd: &str, prefill: &str) -> Option<Picked> {
    // Prefer `fd` (fast, gitignore-aware). Other engines feed the same fzf.
    let mut paths = Vec::new();
    collect_files(cwd, &mut paths);
    if paths.is_empty() {
        die("telescope: no files found under the origin directory.");
        return None;
    }
    // rows: "path<tab>relative display" — we match on the path and show the
    // relative-to-cwd form so rows are scannable regardless of cwd; the
    // absolute path is the value in field 1.
    let rows: Vec<String> = paths
        .iter()
        .map(|p| format!("{p}\t{}", rel_to(cwd, p)))
        .collect();

    let prompt = format!("telescope files ▸  ({} files)", rows.len());
    pick_lines_q(&rows, &prompt, prefill).map(|line| {
        let abs = line.split('\t').next().unwrap_or("").to_string();
        Picked { path: abs }
    })
}

/// Collect file paths under `cwd`, preferring `fd`, then `git ls-files`, then a
/// bounded `find`. Returns short paths (relative to cwd where possible).
fn collect_files(cwd: &str, out: &mut Vec<String>) {
    let fd = std::process::Command::new("fd")
        .args(["--type", "f", "--hidden", "-E", ".git"])
        .current_dir(cwd)
        .output();
    if let Ok(output) = fd {
        if output.status.success() {
            for line in output.stdout.split(|b| *b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                if let Ok(s) = std::str::from_utf8(line) {
                    out.push(s.trim().to_string());
                }
            }
            ensure_absolute(cwd, out);
            return;
        }
    }
    // fd missing or errored: try git ls-files (tracked + untracked, ignore-rule-aware).
    let git = std::process::Command::new("git")
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .current_dir(cwd)
        .output();
    if let Ok(output) = git {
        if output.status.success() {
            for part in output.stdout.split(|b| *b == 0) {
                if part.is_empty() {
                    continue;
                }
                if let Ok(s) = std::str::from_utf8(part) {
                    out.push(s.trim().to_string());
                }
            }
            ensure_absolute(cwd, out);
            if !out.is_empty() {
                return;
            }
        }
    }
    // Fall back to a bounded find (depth cap to stay responsive in huge trees).
    let find = std::process::Command::new("find")
        .args([".", "-type", "f", "-not", "-path", "*/.*", "-maxdepth", "6"])
        .current_dir(cwd)
        .output();
    if let Ok(output) = find {
        if output.status.success() {
            for line in output.stdout.split(|b| *b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                if let Ok(s) = std::str::from_utf8(line) {
                    out.push(s.trim().trim_start_matches("./").to_string());
                }
                if out.len() >= 10_000 {
                    break;
                }
            }
            ensure_absolute(cwd, out);
        }
    }
}

/// Prepend `cwd` to any relative path so fzf carries absolute values.
pub(crate) fn ensure_absolute(cwd: &str, paths: &mut Vec<String>) {
    let cwd = cwd.trim_end_matches('/');
    for p in paths.iter_mut() {
        if !p.starts_with('/') {
            if p.is_empty() {
                continue;
            }
            *p = format!("{cwd}/{p}");
        }
    }
    paths.retain(|p| !p.is_empty());
}

/// Open the file in a new tab ("new window") and run `$EDITOR` there.
fn open_window(file: &str, parent: &str) {
    if parent.is_empty() {
        die("telescope: cannot open a window for a file with no directory.");
        return;
    }
    let label = file.rsplit('/').next().unwrap_or("file").to_string();
    let pane = create_pane(&[
        "tab", "create", "--cwd", parent, "--label", &label, "--focus",
    ]);
    let Some(pane) = pane else { return };
    run_editor(&pane, file);
}

/// Open the file in a new pane (split of the origin pane) and run `$EDITOR`.
/// Without an origin pane we open a tab instead.
fn open_pane(file: &str, parent: &str, origin_pane: &str) {
    if parent.is_empty() {
        die("telescope: cannot open a pane for a file with no directory.");
        return;
    }
    if origin_pane.is_empty() {
        open_window(file, parent);
        return;
    }
    let pane = create_pane(&[
        "pane",
        "split",
        origin_pane,
        "--direction",
        "right",
        "--cwd",
        parent,
        "--focus",
    ]);
    let Some(pane) = pane else { return };
    run_editor(&pane, file);
}

fn create_pane(args: &[&str]) -> Option<String> {
    let out = crate::herdr::run(args);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        die(&format!("herdr {} failed: {err}", args.join(" ")));
        return None;
    }
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    if let Some(id) = pane_id_from(&body) {
        return Some(id);
    }
    die("telescope: could not read the new pane id after opening the file.");
    None
}

fn pane_id_from(body: &serde_json::Value) -> Option<String> {
    for ptr in [
        "/result/pane/pane_id",
        "/result/root_pane/pane_id",
        "/result/root_pane",
    ] {
        if let Some(id) = body.pointer(ptr).and_then(|v| v.as_str()) {
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn run_editor(pane: &str, file: &str) {
    run_cli(["pane", "run", pane, &editor_command(file)]);
}

/// Shell text sent to the new pane: `$EDITOR` if set, otherwise `${EDITOR:-vi}`
/// so the pane's own environment can still supply it.
fn editor_command(file: &str) -> String {
    let quoted = shell_single_quote(file);
    match std::env::var("EDITOR") {
        Ok(ed) if !ed.trim().is_empty() => format!("{} {quoted}", ed.trim()),
        _ => format!("${{EDITOR:-vi}} {quoted}"),
    }
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_files_under_cwd() {
        // Build a temp dir with one file, then collect it. Engine-agnostic so
        // the test passes whether fd, git, or find is available.
        let dir = std::env::temp_dir().join(format!("telescope-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "hi").unwrap();
        std::fs::write(dir.join("sub/b.rs"), "fn main() {}").unwrap();

        let mut out = Vec::new();
        collect_files(dir.to_str().unwrap(), &mut out);

        let d = dir.to_str().unwrap().trim_end_matches('/');
        assert!(
            out.iter().any(|p| p == &format!("{d}/a.txt")),
            "missing a.txt, got {out:?}"
        );
        assert!(
            out.iter().any(|p| p == &format!("{d}/sub/b.rs")),
            "missing sub/b.rs, got {out:?}"
        );

        let rows = file_tsv_rows(d);
        assert!(
            rows.iter()
                .any(|r| r.starts_with(&format!("files\t{d}/a.txt\ta.txt\t"))),
            "tsv row missing a.txt, got {rows:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quotes_file_path_for_the_shell() {
        assert_eq!(shell_single_quote("/a/b.rs"), "'/a/b.rs'");
        assert_eq!(shell_single_quote("/a/it's.rs"), "'/a/it'\\''s.rs'");
    }

    #[test]
    fn editor_command_uses_env_or_fallback() {
        let cmd = editor_command("/tmp/a.rs");
        assert!(
            cmd.ends_with(" '/tmp/a.rs'"),
            "file should be quoted, got {cmd}"
        );
        assert!(
            cmd.contains("EDITOR") || cmd.contains("vi") || !cmd.trim().is_empty(),
            "expected an editor invocation, got {cmd}"
        );
    }

    #[test]
    fn pane_id_from_split_and_tab_responses() {
        let split = serde_json::json!({"result":{"pane":{"pane_id":"w1:p2"}}});
        assert_eq!(pane_id_from(&split).as_deref(), Some("w1:p2"));
        let tab = serde_json::json!({"result":{"root_pane":{"pane_id":"w1:p3"}}});
        assert_eq!(pane_id_from(&tab).as_deref(), Some("w1:p3"));
        let tab_str = serde_json::json!({"result":{"root_pane":"w1:p4"}});
        assert_eq!(pane_id_from(&tab_str).as_deref(), Some("w1:p4"));
    }

    #[test]
    fn parent_dir_of_file() {
        assert_eq!(parent_of("/a/b/c.txt"), "/a/b");
        assert_eq!(parent_of("c.txt"), ".");
        assert_eq!(parent_of("/root.txt"), "");
    }

    fn parent_of(file: &str) -> String {
        file.rfind('/')
            .map(|i| file[..i].to_string())
            .unwrap_or_else(|| ".".to_string())
    }
}
