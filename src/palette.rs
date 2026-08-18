//! The interactive part of the telescope: runs inside the popup pane (real TTY).
//! Builds the merged list — native actions, installed-plugin actions, and the
//! file-finder entry — pipes it to fzf, and dispatches the selection by calling
//! the `herdr` CLI directly. When the process exits, the popup pane is closed.

use crate::context::OriginContext;
use crate::herdr;
use crate::keys::Keys;
use crate::native;
use crate::tty::{ask, die, pick_lines, run_cli};

/// Version of the self plugin, used to hide our own actions from the list.
const SELF_PLUGIN: &str = "telescope";

// The popup's own pane id ($HERDR_PANE_ID) so we can close it on exit. Never
// confused with the origin pane in TELESCOPE_CTX (the pane the user meant).
thread_local! {
    static SELF_PANE: std::cell::Cell<String> = const { std::cell::Cell::new(String::new()) };
}

pub fn run() -> i32 {
    SELF_PANE.with(|c| c.set(std::env::var("HERDR_PANE_ID").unwrap_or_default()));

    // Debug: print the merged TSV rows and exit (no fzf, no TTY needed).
    if std::env::var("TELESCOPE_LIST_ONLY").is_ok() {
        let keys = crate::keys::load();
        for line in build_list(&keys) {
            println!("{line}");
        }
        return close_self(0);
    }

    let ctx = OriginContext::from_env();
    let keys = crate::keys::load();

    let lines = build_list(&keys);
    let Some(choice) = fzf_select(&lines) else {
        return close_self(0);
    };
    let fields: Vec<&str> = choice.split('\t').collect();
    let kind = fields.first().copied().unwrap_or("");
    let payload = fields.get(1).copied().unwrap_or("");
    dispatch(kind, payload, &ctx);

    close_self(0)
}

/// Close our own popup pane ($HERDR_PANE_ID) after dispatch and exit with `code`.
/// The popup placement does not reliably tear its own pane down when its command
/// exits (quick-actions observed the same), so we close explicitly.
fn close_self(code: i32) -> i32 {
    let self_pane = SELF_PANE.with(|c| c.replace(String::new()));
    if !self_pane.is_empty() {
        let _ = herdr::run(["plugin", "pane", "close", self_pane.as_str()]);
    }
    code
}

/// Build the TSV list: kind, payload, display, keywords, hint.
fn build_list(keys: &Keys) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    // Title column width sized from the NATIVE action titles only — a long
    // agent/tab label should not push every shortcut across the popup.
    let maxw = native::NATIVE_ACTIONS
        .iter()
        .map(|a| a.title.chars().count())
        .max()
        .unwrap_or(24);
    let col = col_width();
    let cap = if col > 46 { col.saturating_sub(24) } else { 22 };
    let title_w = maxw.min(cap);

    // 1. Native actions.
    for a in native::NATIVE_ACTIONS {
        let (title, shortcut, keywords) = native::row_display(a, keys);
        let display = native::build_display(&title, &shortcut, &keywords, title_w);
        out.push(tsv("native", a.id, &display, &keywords, a.hint));
    }

    // 2. Installed plugin actions (hide our own).
    if let Some(list) = herdr::json(["plugin", "action", "list"]) {
        if let Some(actions) = list.pointer("/result/actions").and_then(|v| v.as_array()) {
            for act in actions {
                let plugin_id = act.get("plugin_id").and_then(|v| v.as_str()).unwrap_or("");
                let action_id = act.get("action_id").and_then(|v| v.as_str()).unwrap_or("");
                let title = act.get("title").and_then(|v| v.as_str()).unwrap_or("");
                if plugin_id == SELF_PLUGIN {
                    continue;
                }
                let qid = format!("{}.{}", plugin_id, action_id);
                let display = format!("{}  {}", qid, title);
                let keywords = format!("plugin {} {} {}", plugin_id, title, qid);
                let hint = format!("herdr plugin action invoke {qid}");
                out.push(tsv("plugin", &qid, &display, &keywords, &hint));
            }
        }
    }

    // 3. File finder entry.
    let display = "\u{1b}[2mfiles\u{1b}[0m Search files…";
    let keywords = "find file grep search fd rg locate".to_string();
    let hint = "pick a file, then open it in a new pane or a new window".to_string();
    out.push(tsv("files", "", display, &keywords, &hint));

    out
}

fn tsv(kind: &str, payload: &str, display: &str, keywords: &str, hint: &str) -> String {
    format!("{}\t{}\t{}\t{}\t{}", kind, payload, display, keywords, hint)
}

fn col_width() -> usize {
    // Try the COLUMNS env (herdr sets it in popups); fall back to a sane default.
    if let Ok(binding) = std::env::var("COLUMNS") {
        if let Ok(w) = binding.trim().parse::<usize>() {
            if w > 0 {
                return w;
            }
        }
    }
    80
}

/// Run fzf over the rows; return the full chosen line (kind\tpayload\t…).
fn fzf_select(lines: &[String]) -> Option<String> {
    let input = lines.join("\n") + "\n";
    let preview_cmd = "printf '%s\n' {5}";
    let mut cmd = std::process::Command::new("fzf");
    cmd.args([
        "--delimiter=\t",
        "--with-nth=3",
        "--ansi",
        "--prompt=herdr telescope ▸ ",
        "--header=↑↓ select · enter run · esc cancel",
        "--reverse",
        "--cycle",
        "--no-multi",
        "--tiebreak=begin,index",
        "--info=inline",
    ])
    .args(["--preview", preview_cmd])
    .args(["--preview-window", "down,3,wrap,border-top"]);
    use std::io::Write;
    let mut child = match cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            die(&format!("telescope: could not start fzf: {e}"));
            return None;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None; // esc / ctrl-c
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Dispatch a selected row.
fn dispatch(kind: &str, payload: &str, ctx: &OriginContext) {
    match kind {
        "native" => dispatch_native(payload, ctx),
        "plugin" => dispatch_plugin(payload),
        "files" => crate::files::run(&ctx.cwd, &ctx.pane, &ctx.workspace),
        _ => (), // unknown -> nothing
    }
}

fn dispatch_native(payload: &str, ctx: &OriginContext) {
    // Route by id.
    match payload {
        // ---- tabs ----
        "new_tab" | "new_tab_named" => {
            let mut args: Vec<String> = vec!["tab", "create", "--focus"]
                .into_iter()
                .map(String::from)
                .collect();
            if !ctx.workspace.is_empty() {
                args.push("--workspace".into());
                args.push(ctx.workspace.clone());
            }
            if !ctx.cwd.is_empty() {
                args.push("--cwd".into());
                args.push(ctx.cwd.clone());
            }
            if payload == "new_tab_named" {
                if let Some(name) = ask_with_prompt("New tab name: ") {
                    args.push("--label".into());
                    args.push(name);
                } else {
                    return;
                }
            }
            run_cli(&args);
        }
        "close_tab" => {
            if ctx.tab.is_empty() {
                die("telescope: no origin tab to close.");
            }
            run_cli(["tab", "close", ctx.tab.as_str()]);
        }
        "rename_tab" => {
            if ctx.tab.is_empty() {
                die("telescope: no origin tab to rename.");
            }
            if let Some(name) = ask_with_prompt("New tab name: ") {
                run_cli(["tab", "rename", ctx.tab.as_str(), name.as_str()]);
            }
        }
        // ---- panes ----
        "split_vertical" | "split_horizontal" => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to split.");
            }
            let dir = if payload == "split_vertical" {
                "right"
            } else {
                "down"
            };
            let mut args: Vec<String> =
                vec!["pane", "split", &ctx.pane, "--direction", dir, "--focus"]
                    .into_iter()
                    .map(String::from)
                    .collect();
            if !ctx.cwd.is_empty() {
                args.push("--cwd".into());
                args.push(ctx.cwd.clone());
            }
            run_cli(&args);
        }
        "close_pane" => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to close.");
            }
            run_cli(["pane", "close", ctx.pane.as_str()]);
        }
        "zoom_pane" => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to zoom.");
            }
            run_cli(["pane", "zoom", ctx.pane.as_str(), "--toggle"]);
        }
        "rename_pane" => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to rename.");
            }
            if let Some(name) = ask_with_prompt("New pane name: ") {
                run_cli(["pane", "rename", ctx.pane.as_str(), name.as_str()]);
            }
        }
        d @ ("focus_left" | "focus_right" | "focus_up" | "focus_down") => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to focus from.");
            }
            let dir = d.trim_start_matches("focus_");
            run_cli([
                "pane",
                "focus",
                "--direction",
                dir,
                "--pane",
                ctx.pane.as_str(),
            ]);
        }
        d @ ("resize_left" | "resize_right" | "resize_up" | "resize_down") => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to resize.");
            }
            let dir = d.trim_start_matches("resize_");
            run_cli([
                "pane",
                "resize",
                "--direction",
                dir,
                "--pane",
                ctx.pane.as_str(),
            ]);
        }
        d @ ("swap_left" | "swap_right" | "swap_up" | "swap_down") => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to swap.");
            }
            let dir = d.trim_start_matches("swap_");
            run_cli([
                "pane",
                "swap",
                "--direction",
                dir,
                "--pane",
                ctx.pane.as_str(),
            ]);
        }
        // ---- panes: move ----
        "move_pane_tab" => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to move.");
            }
            let target = pick_target_tab(ctx);
            if let Some(t) = target {
                run_cli([
                    "pane",
                    "move",
                    ctx.pane.as_str(),
                    "--tab",
                    t.as_str(),
                    "--focus",
                ]);
            }
        }
        "move_pane_new_tab" => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to move.");
            }
            run_cli(["pane", "move", ctx.pane.as_str(), "--new-tab", "--focus"]);
        }
        "move_pane_new_workspace" => {
            if ctx.pane.is_empty() {
                die("telescope: no origin pane to move.");
            }
            run_cli([
                "pane",
                "move",
                ctx.pane.as_str(),
                "--new-workspace",
                "--focus",
            ]);
        }
        // ---- agents ----
        "start_agent" => dispatch_start_agent(ctx),
        "prompt_agent" => dispatch_prompt_agent(),
        "interrupt_agent" => {
            let target = pick_agent();
            if let Some(t) = target {
                run_cli(["agent", "send-keys", t.as_str(), "esc"]);
            }
        }
        "rename_agent" => {
            let target = pick_agent();
            if let Some(t) = target {
                if let Some(name) = ask_with_prompt("New agent name (a-z0-9_-): ") {
                    run_cli(["agent", "rename", t.as_str(), name.as_str()]);
                }
            }
        }
        // ---- workspaces ----
        "new_workspace" | "new_workspace_here" => {
            let mut args: Vec<String> = vec!["workspace", "create", "--focus"]
                .into_iter()
                .map(String::from)
                .collect();
            if payload == "new_workspace_here" {
                if ctx.cwd.is_empty() {
                    die("telescope: no origin cwd for the new workspace.");
                }
                if let Some(name) = ask_with_prompt("New workspace name: ") {
                    args.push("--cwd".into());
                    args.push(ctx.cwd.clone());
                    args.push("--label".into());
                    args.push(name);
                } else {
                    return;
                }
            }
            run_cli(&args);
        }
        "close_workspace" => {
            if ctx.workspace.is_empty() {
                die("telescope: no origin workspace to close.");
            }
            run_cli(["workspace", "close", ctx.workspace.as_str()]);
        }
        "rename_workspace" => {
            if ctx.workspace.is_empty() {
                die("telescope: no origin workspace to rename.");
            }
            if let Some(name) = ask_with_prompt("New workspace name: ") {
                run_cli(["workspace", "rename", ctx.workspace.as_str(), name.as_str()]);
            }
        }
        // ---- worktrees ----
        "new_worktree" => {
            if ctx.workspace.is_empty() {
                die("telescope: no origin workspace to create a worktree in.");
            }
            run_cli([
                "worktree",
                "create",
                "--workspace",
                ctx.workspace.as_str(),
                "--focus",
            ]);
        }
        "new_worktree_branch" => {
            if ctx.workspace.is_empty() {
                die("telescope: no origin workspace to create a worktree in.");
            }
            if let Some(branch) = ask_with_prompt("Branch name: ") {
                let mut args: Vec<String> = vec![
                    "worktree",
                    "create",
                    "--workspace",
                    &ctx.workspace,
                    "--branch",
                    &branch,
                    "--focus",
                ]
                .into_iter()
                .map(String::from)
                .collect();
                if let Some(base) = ask_with_prompt_default("Base ref (empty = default): ", "") {
                    if !base.is_empty() {
                        args.push("--base".into());
                        args.push(base);
                    }
                }
                run_cli(&args);
            }
        }
        "remove_worktree" => {
            if ctx.workspace.is_empty() {
                die("telescope: no origin workspace to remove.");
            }
            if confirm(format!(
                "Remove the worktree checkout for {}? [y/N] ",
                ctx.workspace
            )) {
                run_cli(["worktree", "remove", "--workspace", ctx.workspace.as_str()]);
            }
        }
        // ---- session ----
        "reload_config" => {
            run_cli(["server", "reload-config"]);
        }
        _ => die(&format!("telescope: unknown native action '{payload}'.")),
    }
}

fn dispatch_plugin(qid: &str) {
    // `herdr plugin action invoke` is fire-and-forget: it returns as soon as the
    // action is DISPATCHED. Poll the plugin log until a terminal state so a
    // failed action surfaces its error instead of the popup vanishing on a
    // silent no-op.
    match herdr::run(["plugin", "action", "invoke", qid]) {
        resp if !resp.status.success() => {
            let err = String::from_utf8_lossy(&resp.stderr).trim().to_string();
            die(&format!("telescope: failed to invoke {qid}\n{err}"));
        }
        resp => {
            let body: serde_json::Value =
                serde_json::from_slice(&resp.stdout).unwrap_or(serde_json::Value::Null);
            let log_id = body
                .pointer("/result/log/log_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let plugin_id = body
                .pointer("/result/log/plugin_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if log_id.is_empty() || plugin_id.is_empty() {
                return; // old herdr; skip polling.
            }
            // Poll up to ~5s.
            for _ in 0..25 {
                std::thread::sleep(std::time::Duration::from_millis(200));
                let Some(page) = herdr::json([
                    "plugin", "log", "list", "--plugin", plugin_id, "--limit", "20",
                ]) else {
                    continue;
                };
                let Some(entry) = page
                    .pointer("/result/logs")
                    .and_then(|v| v.as_array())
                    .and_then(|logs| {
                        logs.iter()
                            .find(|e| e.get("log_id").and_then(|x| x.as_str()) == Some(log_id))
                    })
                else {
                    continue;
                };
                match entry.get("status").and_then(|v| v.as_str()).unwrap_or("") {
                    "succeeded" => return,
                    "failed" => {
                        let code = entry
                            .get("exit_code")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(-1);
                        let err = entry.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
                        die(&format!("telescope: {qid} failed (exit {code})\n{err}"));
                    }
                    _ => {}
                }
            }
        }
    }
}

fn dispatch_start_agent(ctx: &OriginContext) {
    if ctx.pane.is_empty() {
        die("telescope: no origin pane to split for the agent.");
    }
    // Agent kinds scraped from the installed binary's own completion (falls back
    // to the common set). Rough sed-style extraction.
    let kinds = agent_kinds();
    let kind = pick_lines(&kinds, "agent kind ▸ ");
    let Some(kind) = kind else { return };
    let Some(name) = ask_with_prompt("Agent name (a-z0-9_-): ") else {
        return;
    };
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        die(&format!(
            "telescope: agent names must match [a-z][a-z0-9_-]{{0,31}} (got '{name}')."
        ));
    }
    // Split origin, then start the agent in the new pane.
    let mut split_args: Vec<String> = vec![
        "pane",
        "split",
        &ctx.pane,
        "--direction",
        "right",
        "--focus",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if !ctx.cwd.is_empty() {
        split_args.push("--cwd".into());
        split_args.push(ctx.cwd.clone());
    }
    let resp = herdr::run(split_args.iter().map(String::as_str).collect::<Vec<_>>());
    if !resp.status.success() {
        let err = String::from_utf8_lossy(&resp.stderr).to_string();
        die(&format!(
            "telescope: pane split failed before starting the agent.\n{err}"
        ));
    }
    let body: serde_json::Value =
        serde_json::from_slice(&resp.stdout).unwrap_or(serde_json::Value::Null);
    let new_pane = body
        .pointer("/result/pane/pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if new_pane.is_empty() {
        die("telescope: could not read the new pane id from pane split.");
    }
    run_cli([
        "agent",
        "start",
        name.as_str(),
        "--kind",
        kind.as_str(),
        "--pane",
        new_pane.as_str(),
    ]);
}

fn dispatch_prompt_agent() {
    let target = pick_agent();
    let Some(target) = target else { return };
    if let Some(text) = ask_with_prompt("Prompt: ") {
        run_cli(["agent", "prompt", target.as_str(), text.as_str()]);
    }
}

fn agent_kinds() -> Vec<String> {
    // Try the installed binary's completion for the supported kinds.
    if let Ok(out) = std::process::Command::new(herdr::bin())
        .arg("completion")
        .arg("zsh")
        .output()
    {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            // Look for `--kind[Supported agent kind (...)]`.
            if let Some(idx) = text.find("--kind[Supported agent kind") {
                let rest = &text[idx + "--kind[Supported agent kind".len()..];
                let open = rest.find('(');
                let close = rest.find(')');
                if let (Some(o), Some(c)) = (open, close) {
                    let kinds: Vec<String> = rest[o + 1..c]
                        .split(' ')
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !kinds.is_empty() {
                        return kinds;
                    }
                }
            }
        }
    }
    vec![
        "claude".into(),
        "codex".into(),
        "gemini".into(),
        "cursor".into(),
        "opencode".into(),
        "copilot".into(),
        "amp".into(),
        "droid".into(),
    ]
}

fn pick_agent() -> Option<String> {
    let Some(list) = herdr::json(["agent", "list"]) else {
        return None;
    };
    let Some(agents) = list.pointer("/result/agents").and_then(|v| v.as_array()) else {
        die("telescope: no live agents.");
        return None;
    };
    let mut rows = Vec::with_capacity(agents.len());
    for a in agents {
        let pane_id = a.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
        let title = a
            .get("terminal_title_stripped")
            .and_then(|v| v.as_str())
            .or_else(|| a.get("agent").and_then(|v| v.as_str()))
            .unwrap_or(pane_id)
            .to_string();
        let status = a
            .get("agent_status")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        rows.push(format!("{pane_id}\t{title} · {status} · {pane_id}"));
    }
    if rows.is_empty() {
        die("telescope: no live agents.");
        return None;
    }
    pick_lines(&rows, "agent ▸ ").map(|line| line.split('\t').next().unwrap_or("").to_string())
}

fn pick_target_tab(ctx: &OriginContext) -> Option<String> {
    let Some(list) = herdr::json(["tab", "list", "--workspace", ctx.workspace.as_str()]) else {
        return None;
    };
    let Some(tabs) = list.pointer("/result/tabs").and_then(|v| v.as_array()) else {
        return None;
    };
    let mut rows = Vec::new();
    for t in tabs {
        let id = t.get("tab_id").and_then(|v| v.as_str()).unwrap_or("");
        if id == ctx.tab {
            continue;
        }
        let label = t
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or(id)
            .to_string();
        let n = t.get("pane_count").and_then(|v| v.as_i64()).unwrap_or(0);
        rows.push(format!("{id}\t{label}  ({n} panes)"));
    }
    if rows.is_empty() {
        die("telescope: no other tab to move the pane to.");
        return None;
    }
    pick_lines(&rows, "move to tab ▸ ")
        .map(|line| line.split('\t').next().unwrap_or("").to_string())
}

fn confirm(prompt: String) -> bool {
    matches!(ask(&prompt).as_str(), "y" | "Y" | "yes" | "YES")
}

/// Read one line from the user via /dev/tty; empty/abort means cancel.
fn ask_with_prompt(prompt: &str) -> Option<String> {
    let ans = ask(prompt);
    if ans.is_empty() {
        None
    } else {
        Some(ans)
    }
}

fn ask_with_prompt_default(prompt: &str, default: &str) -> Option<String> {
    let ans = ask(prompt);
    if ans.is_empty() {
        Some(default.to_string())
    } else {
        Some(ans)
    }
}
