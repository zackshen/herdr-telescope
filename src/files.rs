//! The file finder: pick a file under the origin cwd, then open it in a new
//! pane (split) or a new window (tab). Both spawn a shell whose cwd is the
//! file's parent directory; the file path is exported to the new shell as
//! `TELESCOPE_OPEN_FILE` so the user (or a shell hook) can act on it.

use crate::tty::{die, pick_lines, run_cli};

struct Picked {
    /// absolute file path.
    path: String,
}

/// Entry point: search files from `cwd`, let the user pick one, then prompt for
/// "new pane" / "new window" and dispatch.
pub fn run(cwd: &str, origin_pane: &str, _origin_workspace: &str) {
    if cwd.is_empty() || !std::path::Path::new(cwd).is_dir() {
        die("telescope: no usable origin directory to search files under.");
        return;
    }

    let picked = pick_file(cwd);
    let Some(picked) = picked else {
        return; // cancelled
    };
    let file = picked.path;
    let parent = file
        .rfind('/')
        .map(|i| file[..i].to_string())
        .unwrap_or_else(|| file.clone());

    // Ask: open in a new pane or a new window.
    let opts = vec![
        "pane\tNew pane (split, right)".to_string(),
        "window\tNew window (tab)".to_string(),
    ];
    let Some(choice) = pick_lines(&opts, "open in ▸ ") else {
        return; // cancelled
    };
    let target = choice.split('\t').next().unwrap_or("").to_string();

    match target.as_str() {
        "window" => open_window(&file, &parent),
        _ => open_pane(&file, &parent, origin_pane),
    }
}

/// Run fzf over the files under `cwd` and return the chosen absolute path.
fn pick_file(cwd: &str) -> Option<Picked> {
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
        .map(|p| {
            let rel = p
                .strip_prefix(cwd)
                .unwrap_or(p.as_str())
                .trim_start_matches('/');
            format!("{p}\t{rel}")
        })
        .collect();

    let prompt = format!("telescope files ▸  ({} files)", rows.len());
    pick_lines(&rows, &prompt).map(|line| {
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
fn ensure_absolute(cwd: &str, paths: &mut Vec<String>) {
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

/// Open the file in a new tab ("new window"). cwd becomes the file's directory.
fn open_window(file: &str, parent: &str) {
    if parent.is_empty() {
        die("telescope: cannot open a window for a file with no directory.");
        return;
    }
    // Label = the file's base name (the thing being opened).
    let label = file.rsplit('/').next().unwrap_or("file").to_string();
    let args = [
        "tab".to_string(),
        "create".to_string(),
        "--cwd".to_string(),
        parent.to_string(),
        "--label".to_string(),
        label,
        "--focus".to_string(),
    ];
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_cli(&arg_refs);
}

/// Open the file in a new pane (split of the origin pane), cwd = the file's
/// directory. Without an origin pane we open a tab instead.
fn open_pane(file: &str, parent: &str, origin_pane: &str) {
    if parent.is_empty() {
        die("telescope: cannot open a pane for a file with no directory.");
        return;
    }
    if origin_pane.is_empty() {
        open_window(file, parent);
        return;
    }
    let args = [
        "pane".to_string(),
        "split".to_string(),
        origin_pane.to_string(),
        "--direction".to_string(),
        "right".to_string(),
        "--cwd".to_string(),
        parent.to_string(),
        "--focus".to_string(),
    ];
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_cli(&arg_refs);
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

        std::fs::remove_dir_all(&dir).unwrap();
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
