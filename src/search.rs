//! Content search (`/` mode): list files matching a ripgrep query and preview
//! the matching lines with the query highlighted.

use std::io::Write;
use std::process::Command;

use crate::files;

/// Print TSV file rows for the live fzf query (`FZF_QUERY`). Invoked as
/// `herdr-telescope rg-files` from fzf `reload-sync`.
pub fn run_rg_files() -> i32 {
    let cwd = search_cwd();
    let query = live_query();
    for line in matching_file_rows(&cwd, &query) {
        println!("{line}");
    }
    0
}

/// Print the fzf preview for the selected row. Invoked as
/// `herdr-telescope preview <tsv-line>` from `--preview`.
///
/// Search mode (`herdr search ▸ `) shows a bat-highlighted excerpt of field 2
/// (the file path). Other modes print field 5 (the hint).
pub fn run_rg_preview() -> i32 {
    // fzf passes the whole TSV as one argument; join in case the shell
    // split on tabs.
    let line = std::env::args().skip(2).collect::<Vec<_>>().join("\t");
    preview_row(
        &line,
        &std::env::var("FZF_PROMPT").unwrap_or_default(),
        &live_query(),
    );
    0
}

const SEARCH_PROMPT: &str = "herdr search ▸ ";

fn preview_row(line: &str, prompt: &str, query: &str) {
    let fields: Vec<&str> = line.split('\t').collect();
    if prompt == SEARCH_PROMPT {
        preview(fields.get(1).copied().unwrap_or(""), query);
        return;
    }
    if let Some(hint) = fields.get(4) {
        println!("{hint}");
    }
}

fn live_query() -> String {
    std::env::var("FZF_QUERY").unwrap_or_default()
}

fn search_cwd() -> String {
    std::env::var("TELESCOPE_CWD").unwrap_or_default()
}

/// Files under `cwd` whose contents match `query` (ripgrep regex).
pub fn matching_file_rows(cwd: &str, query: &str) -> Vec<String> {
    let query = query.trim();
    if query.is_empty() || cwd.is_empty() || !std::path::Path::new(cwd).is_dir() {
        return Vec::new();
    }
    let output = match Command::new("rg")
        .args([
            "--files-with-matches",
            "--color=never",
            "--hidden",
            "-g",
            "!.git",
        ])
        .arg("--")
        .arg(query)
        .current_dir(cwd)
        .output()
    {
        Ok(o) => o,
        Err(_) => {
            return vec![
                "files\t\trg not found\trg is required for / search\tinstall ripgrep".into(),
            ];
        }
    };
    if !output.status.success() && output.stdout.is_empty() {
        return Vec::new();
    }
    let mut paths = Vec::new();
    for line in output.stdout.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(s) = std::str::from_utf8(line) {
            paths.push(s.trim().to_string());
        }
        if paths.len() >= 5_000 {
            break;
        }
    }
    files::ensure_absolute(cwd, &mut paths);
    paths.iter().map(|p| files::file_row(cwd, p)).collect()
}

fn preview(file: &str, query: &str) {
    let query = query.trim();
    if file.is_empty() {
        return;
    }
    if query.is_empty() {
        println!("type a pattern after /");
        return;
    }
    let Some((lines, texts)) = match_hits(file, query) else {
        println!("rg not found");
        return;
    };
    if lines.is_empty() {
        println!("no matches");
        return;
    }
    if !bat_preview(file, &lines, &texts) {
        rg_fallback(file, query);
    }
    let _ = std::io::stdout().flush();
}

/// Matching line numbers and the exact texts rg reported (`--only-matching`).
fn match_hits(file: &str, query: &str) -> Option<(Vec<u32>, Vec<String>)> {
    let output = Command::new("rg")
        .args([
            "--color=never",
            "--line-number",
            "--no-heading",
            "--hidden",
            "--only-matching",
        ])
        .arg("--")
        .arg(query)
        .arg(file)
        .output()
        .ok()?;
    let mut lines = Vec::new();
    let mut texts = Vec::new();
    for raw in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((n, text)) = raw.split_once(':') else {
            continue;
        };
        let Ok(n) = n.parse::<u32>() else {
            continue;
        };
        if lines.last() != Some(&n) {
            lines.push(n);
        }
        if !text.is_empty() && !texts.iter().any(|t| t == text) {
            texts.push(text.to_string());
        }
        if lines.len() >= 50 {
            break;
        }
    }
    texts.sort_by_key(|t| std::cmp::Reverse(t.len()));
    Some((lines, texts))
}

/// Syntax-highlight matching excerpts with bat, then paint the search hits.
fn bat_preview(file: &str, hits: &[u32], texts: &[String]) -> bool {
    let width = std::env::var("FZF_PREVIEW_COLUMNS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|w| *w > 0)
        .unwrap_or(80);
    let mut cmd = Command::new("bat");
    cmd.args([
        "--paging=never",
        "--color=always",
        "--decorations=always",
        "--style=numbers,header",
        "--theme=ansi",
        "--wrap=never",
        "--terminal-width",
        &width.to_string(),
    ]);
    for n in hits {
        cmd.arg("--line-range").arg(format!("{n}::2"));
    }
    cmd.arg("--").arg(file);
    let output = match cmd.output() {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };
    let painted = highlight_visible(&String::from_utf8_lossy(&output.stdout), texts);
    print!("{painted}");
    true
}

/// Background only — no underline — so the hit word stands out on bat's colors.
const HIT_BG: &str = "\x1b[48;5;178m";
const HIT_END: &str = "\x1b[49m";

/// Paint `needles` in `colored` by walking visible characters (ANSI skipped).
fn highlight_visible(colored: &str, needles: &[String]) -> String {
    if needles.is_empty() {
        return colored.to_string();
    }
    colored
        .split_inclusive('\n')
        .map(|line| highlight_line(line, needles))
        .collect()
}

fn highlight_line(line: &str, needles: &[String]) -> String {
    let vis = visible_map(line);
    if vis.text.trim_start().starts_with("File:") {
        return line.to_string();
    }
    let gutter = gutter_bytes(&vis.text);
    let mut marked = vec![false; vis.text.len()];
    let search = &vis.text[gutter..];
    let mut i = 0;
    while i < search.len() {
        let rest = &search[i..];
        let mut hit = 0;
        for n in needles {
            if !n.is_empty() && rest.starts_with(n.as_str()) {
                hit = n.len();
                break;
            }
        }
        if hit > 0 {
            for b in marked[gutter + i..gutter + i + hit].iter_mut() {
                *b = true;
            }
            i += hit;
        } else {
            i += rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
    }
    apply_marks(line, &vis, &marked)
}

struct Vis {
    text: String,
    /// Byte offset in the original line of each visible UTF-8 char.
    starts: Vec<usize>,
}

fn visible_map(s: &str) -> Vis {
    let bytes = s.as_bytes();
    let mut text = String::new();
    let mut starts = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            }
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        text.push(ch);
        starts.push(i);
        i += ch.len_utf8();
    }
    Vis { text, starts }
}

fn gutter_bytes(visible: &str) -> usize {
    let b = visible.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    let digits = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i > digits && i < b.len() && b[i] == b' ' {
        i + 1
    } else {
        0
    }
}

fn apply_marks(line: &str, vis: &Vis, marked: &[bool]) -> String {
    if vis.starts.is_empty() {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut last = 0;
    let mut in_hit = false;
    let mut vis_byte = 0;
    for &start in &vis.starts {
        if start > last {
            emit_ansi(&mut out, &line[last..start], in_hit);
        }
        let ch_len = vis.text[vis_byte..].chars().next().unwrap().len_utf8();
        let on = marked.get(vis_byte).copied().unwrap_or(false);
        if on && !in_hit {
            out.push_str(HIT_BG);
            in_hit = true;
        } else if !on && in_hit {
            out.push_str(HIT_END);
            in_hit = false;
        }
        out.push_str(&line[start..start + ch_len]);
        last = start + ch_len;
        vis_byte += ch_len;
    }
    if last < line.len() {
        emit_ansi(&mut out, &line[last..], in_hit);
    }
    if in_hit {
        out.push_str(HIT_END);
    }
    out
}

fn emit_ansi(out: &mut String, chunk: &str, in_hit: bool) {
    out.push_str(chunk);
    if in_hit && chunk.contains('\u{1b}') {
        out.push_str(HIT_BG);
    }
}

fn rg_fallback(file: &str, query: &str) {
    let _ = Command::new("rg")
        .args([
            "--color=never",
            "--line-number",
            "--heading",
            "--context",
            "2",
            "--hidden",
        ])
        .arg("--")
        .arg(query)
        .arg(file)
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_lists_nothing() {
        assert!(matching_file_rows("/tmp", "").is_empty());
        assert!(matching_file_rows("", "foo").is_empty());
    }

    #[test]
    fn finds_file_containing_pattern() {
        if Command::new("rg").arg("--version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("telescope-rg-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("hit.rs"), "fn unique_token_xyz() {}\n").unwrap();
        std::fs::write(dir.join("miss.rs"), "fn other() {}\n").unwrap();
        std::fs::write(dir.join("sub/nested.rs"), "unique_token_xyz in nest\n").unwrap();

        let d = dir.to_str().unwrap();
        let rows = matching_file_rows(d, "unique_token_xyz");
        assert!(
            rows.iter().any(|r| r.contains("hit.rs")),
            "missing hit.rs, got {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("sub/nested.rs")),
            "missing nested.rs, got {rows:?}"
        );
        assert!(
            rows.iter().all(|r| !r.contains("miss.rs")),
            "miss.rs should not match, got {rows:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn match_lines_returns_hit_numbers() {
        if Command::new("rg").arg("--version").output().is_err() {
            return;
        }
        let path = std::env::temp_dir().join(format!("telescope-rg-prev-{}", std::process::id()));
        std::fs::write(
            &path,
            "fn unique_token_xyz() {}\nfn other() {}\n// unique_token_xyz\n",
        )
        .unwrap();
        let (hits, texts) = match_hits(path.to_str().unwrap(), "unique_token_xyz").unwrap();
        assert_eq!(hits, vec![1, 3]);
        assert_eq!(texts, vec!["unique_token_xyz".to_string()]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn highlight_paints_visible_hit_not_gutter() {
        let line = "  10 \x1b[32mfn unique_token_xyz() {}\x1b[0m\n";
        let out = highlight_line(line, &["unique_token_xyz".into()]);
        assert!(
            out.contains(HIT_BG) && out.contains("unique_token_xyz") && out.contains(HIT_END),
            "got {out:?}"
        );
        let vis = visible_map(&out);
        assert!(
            vis.text.contains("  10 fn unique_token_xyz() {}"),
            "gutter/text should stay intact, got {:?}",
            vis.text
        );
        let painted = highlight_visible("     File: src/foo.rs\n", &["File".into()]);
        assert!(
            !painted.contains(HIT_BG),
            "header must not be painted, got {painted:?}"
        );
    }
}
