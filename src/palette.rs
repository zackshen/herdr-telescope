//! The interactive part of the telescope: runs inside the popup pane (real TTY).
//! Builds the merged list — native actions, installed-plugin actions, and the
//! file-finder entry — pipes it to fzf, and dispatches the selection by calling
//! the `herdr` CLI directly. When the process exits, the popup pane is closed.

use crate::context::OriginContext;
use crate::herdr;
use crate::keys::Keys;
use crate::native;
use crate::tty::{ask, die, paint_popup, pick_lines, run_cli, FZF_OPAQUE};

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
    let files = if !ctx.cwd.is_empty() && std::path::Path::new(&ctx.cwd).is_dir() {
        crate::files::file_tsv_rows(&ctx.cwd)
    } else {
        Vec::new()
    };
    let sel = fzf_select(&lines, &files, &ctx.cwd).unwrap_or_default();

    if sel.line.is_empty() {
        return close_self(0);
    }
    let fields: Vec<&str> = sel.line.split('\t').collect();
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

    // 3. Live workspaces — searchable by name (`capehor` → workspace: capehorn-next).
    if let Some(list) = herdr::json(["workspace", "list"]) {
        if let Some(workspaces) = list
            .pointer("/result/workspaces")
            .and_then(|v| v.as_array())
        {
            for w in workspaces {
                let id = w.get("workspace_id").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() {
                    continue;
                }
                let label = w.get("label").and_then(|v| v.as_str()).unwrap_or(id);
                out.push(workspace_row(id, label));
            }
        }
    }

    // 4. File finder + content search entries.
    out.push(tsv(
        "files",
        "",
        "\u{1b}[2mfiles\u{1b}[0m Search files…",
        "find file grep search fd rg locate",
        "type @ to jump here, then pick a file",
    ));
    out.push(tsv(
        "search",
        "",
        "\u{1b}[2msearch\u{1b}[0m Search contents…",
        "rg grep content text /",
        "type / to jump here and search with ripgrep",
    ));

    out
}

fn workspace_row(id: &str, label: &str) -> String {
    let name = if label.is_empty() { id } else { label };
    tsv(
        "workspace",
        id,
        &format!("\u{1b}[2mworkspace:\u{1b}[0m {name}"),
        &format!("workspace goto jump focus project {name} {id}"),
        &format!("herdr workspace focus {id}"),
    )
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

/// The selected TSV line from the main fzf.
#[derive(Debug, Default)]
struct Selection {
    line: String,
}

/// Prompt shown in action mode. Mode switches key off these exact strings.
const ACTIONS_PROMPT: &str = "herdr telescope ▸ ";
const FILES_PROMPT: &str = "herdr files ▸ ";
const SEARCH_PROMPT: &str = "herdr search ▸ ";
const ACTIONS_HEADER: &str = "↑↓ select · enter run · @ files · / search · esc cancel";
const FILES_HEADER: &str = "↑↓ select · enter open · backspace to return";
const SEARCH_HEADER: &str = "↑↓ select · enter open · rg live · backspace to return";

const ACTION_PREVIEW_WIN: &str = "down,3,wrap,border-top";
const SEARCH_PREVIEW_WIN: &str = "right,60%,wrap,border-left";

/// Consume a leading `@` or `/` so the remainder is the real query.
const STRIP_AT: &str = r#"transform-query[printf %s "${FZF_QUERY#@}"]"#;
const STRIP_SLASH: &str = r#"transform-query[printf %s "${FZF_QUERY#/}"]"#;

fn mode_of(prompt: &str) -> Mode {
    if prompt == FILES_PROMPT {
        Mode::Files
    } else if prompt == SEARCH_PROMPT {
        Mode::Search
    } else {
        Mode::Actions
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Actions,
    Files,
    Search,
}

/// fzf `change:transform` helper. `@` → filename list, `/` → rg content
/// search (fzf's own matcher is disabled so the query is the rg pattern).
pub fn at_switch(actions: &str, files: &str, query: &str, prompt: &str, exe: &str) -> String {
    let mode = mode_of(prompt);
    if query.starts_with('@') {
        return match mode {
            Mode::Files => format!("{STRIP_AT}\n"),
            Mode::Search => format!(
                "enable-search+reload-sync[cat -- {files}]+change-prompt[{FILES_PROMPT}]+change-header[{FILES_HEADER}]+change-preview-window[{ACTION_PREVIEW_WIN}]+{STRIP_AT}\n"
            ),
            Mode::Actions => format!(
                "reload-sync[cat -- {files}]+change-prompt[{FILES_PROMPT}]+change-header[{FILES_HEADER}]+{STRIP_AT}\n"
            ),
        };
    }
    if query.starts_with('/') {
        let rg = format!("{exe} rg-files");
        return match mode {
            Mode::Search => format!("{STRIP_SLASH}\n"),
            _ => format!(
                "disable-search+reload-sync[{rg}]+change-prompt[{SEARCH_PROMPT}]+change-header[{SEARCH_HEADER}]+change-preview-window[{SEARCH_PREVIEW_WIN}]+{STRIP_SLASH}\n"
            ),
        };
    }
    if mode == Mode::Search {
        format!("reload-sync[{exe} rg-files]\n")
    } else {
        let _ = actions;
        String::new()
    }
}

/// `backward-eof` helper: leave file/search mode when the query is empty.
pub fn at_back(actions: &str, prompt: &str) -> String {
    match mode_of(prompt) {
        Mode::Files | Mode::Search => format!(
            "enable-search+reload-sync[cat -- {actions}]+change-prompt[{ACTIONS_PROMPT}]+change-header[{ACTIONS_HEADER}]+change-preview-window[{ACTION_PREVIEW_WIN}]\n"
        ),
        Mode::Actions => String::new(),
    }
}

/// Entry point for the fzf transform helpers (`at-switch` / `at-back`).
pub fn run_at_helper() -> i32 {
    let mut args = std::env::args().skip(1);
    let kind = args.next().unwrap_or_default();
    let actions = args.next().unwrap_or_default();
    let files = args.next().unwrap_or_default();
    let query = std::env::var("FZF_QUERY").unwrap_or_default();
    let prompt = std::env::var("FZF_PROMPT").unwrap_or_default();
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "herdr-telescope".into());
    let out = match kind.as_str() {
        "at-back" => at_back(&actions, &prompt),
        _ => at_switch(&actions, &files, &query, &prompt, &exe),
    };
    print!("{out}");
    0
}

fn fzf_select(lines: &[String], files: &[String], cwd: &str) -> Option<Selection> {
    let tmp = std::env::temp_dir().join(format!("telescope-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let actions_path = tmp.join("actions");
    let files_path = tmp.join("files");
    let _ = std::fs::write(&actions_path, lines.join("\n") + "\n");
    let _ = std::fs::write(&files_path, files.join("\n") + "\n");

    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "herdr-telescope".into());
    let change_bind = format!(
        "change:transform[{exe} at-switch {actions} {files}]",
        actions = actions_path.display(),
        files = files_path.display(),
    );
    let back_bind = format!(
        "backward-eof:transform[{exe} at-back {actions}]",
        actions = actions_path.display(),
    );
    paint_popup();
    let mut cmd = std::process::Command::new("fzf");
    cmd.args(["--delimiter=\t", "--with-nth=3", "--ansi"])
        .args(["--bind", &change_bind])
        .args(["--bind", &back_bind])
        .args([format!("--prompt={ACTIONS_PROMPT}")])
        .args([format!("--header={ACTIONS_HEADER}")])
        .args(["--color", FZF_OPAQUE])
        .args([
            "--reverse",
            "--cycle",
            "--no-multi",
            "--tiebreak=begin,index",
            "--info=inline",
        ])
        .args(["--preview", &format!("{exe} preview {{}}")])
        .args(["--preview-window", ACTION_PREVIEW_WIN]);
    if !cwd.is_empty() {
        cmd.env("TELESCOPE_CWD", cwd);
    }
    use std::io::Write;
    let mut child = match cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            die(&format!("telescope: could not start fzf: {e}"));
            return None;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all((lines.join("\n") + "\n").as_bytes());
    }
    let out = child.wait_with_output().ok();
    let _ = std::fs::remove_dir_all(&tmp);
    let out = out?;
    if !out.status.success() {
        return Some(Selection::default());
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let line = text
        .lines()
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    Some(Selection { line })
}

/// Dispatch a selected row.
fn dispatch(kind: &str, payload: &str, ctx: &OriginContext) {
    match kind {
        "native" => dispatch_native(payload, ctx),
        "plugin" => dispatch_plugin(payload),
        "files" if payload.is_empty() => crate::files::run(&ctx.cwd, &ctx.pane, &ctx.workspace, ""),
        "files" => crate::files::confirm_and_open(payload, &ctx.pane),
        "workspace" => {
            if payload.is_empty() {
                die("telescope: no workspace id to focus.");
            }
            run_cli(["workspace", "focus", payload]);
        }
        "search" => {}
        _ => (), // unknown -> nothing
    }
}

/// Extra answers a native action may need after the main fzf (typed line,
/// second-stage pick, y/N). Tests inject these so argv planning does not
/// open a TTY.
#[derive(Debug, Default, Clone)]
struct NativeInput {
    line: Option<String>,
    extra: Option<String>,
    pick: Option<String>,
    confirm: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum NativeErr {
    Cancel,
    MissingOrigin(&'static str),
    Unknown,
    MultiStep,
}

/// Line prompt shown after fzf for this action. Enter must submit (see `ask`).
fn line_prompt(id: &str) -> Option<&'static str> {
    match id {
        "new_tab_named" | "rename_tab" => Some("New tab name: "),
        "rename_pane" => Some("New pane name: "),
        "new_workspace_here" | "rename_workspace" => Some("New workspace name: "),
        "new_worktree_branch" => Some("Branch name: "),
        "start_agent" => Some("Agent name (a-z0-9_-): "),
        "rename_agent" => Some("New agent name (a-z0-9_-): "),
        "prompt_agent" => Some("Prompt: "),
        _ => None,
    }
}

fn gather_native_input(id: &str, ctx: &OriginContext) -> Option<NativeInput> {
    let mut input = NativeInput::default();
    match id {
        "move_pane_tab" => input.pick = Some(pick_target_tab(ctx)?),
        "interrupt_agent" | "rename_agent" | "prompt_agent" => input.pick = Some(pick_agent()?),
        "remove_worktree" => {
            input.confirm = confirm(format!(
                "Remove the worktree checkout for {}? [y/N] ",
                ctx.workspace
            ));
        }
        _ => {}
    }
    if let Some(prompt) = line_prompt(id) {
        if id != "start_agent" {
            input.line = Some(ask_with_prompt(prompt)?);
        }
    }
    if id == "new_worktree_branch" {
        input.extra = ask_with_prompt_default("Base ref (empty = default): ", "");
    }
    Some(input)
}

/// herdr argv for a native action. Interactive steps are already in `input`.
fn native_args(
    id: &str,
    ctx: &OriginContext,
    input: &NativeInput,
) -> Result<Vec<String>, NativeErr> {
    let line = || {
        input
            .line
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or(NativeErr::Cancel)
    };
    let pick = || {
        input
            .pick
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or(NativeErr::Cancel)
    };
    match id {
        "new_tab" | "new_tab_named" => {
            let mut args = vec!["tab".into(), "create".into(), "--focus".into()];
            if !ctx.workspace.is_empty() {
                args.push("--workspace".into());
                args.push(ctx.workspace.clone());
            }
            if !ctx.cwd.is_empty() {
                args.push("--cwd".into());
                args.push(ctx.cwd.clone());
            }
            if id == "new_tab_named" {
                args.push("--label".into());
                args.push(line()?);
            }
            Ok(args)
        }
        "close_tab" => {
            if ctx.tab.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin tab to close.",
                ));
            }
            Ok(vec!["tab".into(), "close".into(), ctx.tab.clone()])
        }
        "rename_tab" => {
            if ctx.tab.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin tab to rename.",
                ));
            }
            Ok(vec![
                "tab".into(),
                "rename".into(),
                ctx.tab.clone(),
                line()?,
            ])
        }
        "split_vertical" | "split_horizontal" => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to split.",
                ));
            }
            let dir = if id == "split_vertical" {
                "right"
            } else {
                "down"
            };
            let mut args = vec![
                "pane".into(),
                "split".into(),
                ctx.pane.clone(),
                "--direction".into(),
                dir.into(),
                "--focus".into(),
            ];
            if !ctx.cwd.is_empty() {
                args.push("--cwd".into());
                args.push(ctx.cwd.clone());
            }
            Ok(args)
        }
        "close_pane" => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to close.",
                ));
            }
            Ok(vec!["pane".into(), "close".into(), ctx.pane.clone()])
        }
        "zoom_pane" => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to zoom.",
                ));
            }
            Ok(vec![
                "pane".into(),
                "zoom".into(),
                ctx.pane.clone(),
                "--toggle".into(),
            ])
        }
        "rename_pane" => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to rename.",
                ));
            }
            Ok(vec![
                "pane".into(),
                "rename".into(),
                ctx.pane.clone(),
                line()?,
            ])
        }
        d if d.starts_with("focus_") => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to focus from.",
                ));
            }
            Ok(vec![
                "pane".into(),
                "focus".into(),
                "--direction".into(),
                d.trim_start_matches("focus_").into(),
                "--pane".into(),
                ctx.pane.clone(),
            ])
        }
        d if d.starts_with("resize_") => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to resize.",
                ));
            }
            Ok(vec![
                "pane".into(),
                "resize".into(),
                "--direction".into(),
                d.trim_start_matches("resize_").into(),
                "--pane".into(),
                ctx.pane.clone(),
            ])
        }
        d if d.starts_with("swap_") => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to swap.",
                ));
            }
            Ok(vec![
                "pane".into(),
                "swap".into(),
                "--direction".into(),
                d.trim_start_matches("swap_").into(),
                "--pane".into(),
                ctx.pane.clone(),
            ])
        }
        "move_pane_tab" => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to move.",
                ));
            }
            Ok(vec![
                "pane".into(),
                "move".into(),
                ctx.pane.clone(),
                "--tab".into(),
                pick()?,
                "--focus".into(),
            ])
        }
        "move_pane_new_tab" => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to move.",
                ));
            }
            Ok(vec![
                "pane".into(),
                "move".into(),
                ctx.pane.clone(),
                "--new-tab".into(),
                "--focus".into(),
            ])
        }
        "move_pane_new_workspace" => {
            if ctx.pane.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin pane to move.",
                ));
            }
            Ok(vec![
                "pane".into(),
                "move".into(),
                ctx.pane.clone(),
                "--new-workspace".into(),
                "--focus".into(),
            ])
        }
        "start_agent" => Err(NativeErr::MultiStep),
        "prompt_agent" => Ok(vec!["agent".into(), "prompt".into(), pick()?, line()?]),
        "interrupt_agent" => Ok(vec![
            "agent".into(),
            "send-keys".into(),
            pick()?,
            "esc".into(),
        ]),
        "rename_agent" => Ok(vec!["agent".into(), "rename".into(), pick()?, line()?]),
        "new_workspace" | "new_workspace_here" => {
            let mut args = vec!["workspace".into(), "create".into(), "--focus".into()];
            if id == "new_workspace_here" {
                if ctx.cwd.is_empty() {
                    return Err(NativeErr::MissingOrigin(
                        "telescope: no origin cwd for the new workspace.",
                    ));
                }
                args.push("--cwd".into());
                args.push(ctx.cwd.clone());
                args.push("--label".into());
                args.push(line()?);
            }
            Ok(args)
        }
        "close_workspace" => {
            if ctx.workspace.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin workspace to close.",
                ));
            }
            Ok(vec![
                "workspace".into(),
                "close".into(),
                ctx.workspace.clone(),
            ])
        }
        "rename_workspace" => {
            if ctx.workspace.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin workspace to rename.",
                ));
            }
            Ok(vec![
                "workspace".into(),
                "rename".into(),
                ctx.workspace.clone(),
                line()?,
            ])
        }
        "new_worktree" => {
            if ctx.workspace.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin workspace to create a worktree in.",
                ));
            }
            Ok(vec![
                "worktree".into(),
                "create".into(),
                "--workspace".into(),
                ctx.workspace.clone(),
                "--focus".into(),
            ])
        }
        "new_worktree_branch" => {
            if ctx.workspace.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin workspace to create a worktree in.",
                ));
            }
            let mut args = vec![
                "worktree".into(),
                "create".into(),
                "--workspace".into(),
                ctx.workspace.clone(),
                "--branch".into(),
                line()?,
                "--focus".into(),
            ];
            if let Some(base) = input
                .extra
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                args.push("--base".into());
                args.push(base.to_string());
            }
            Ok(args)
        }
        "remove_worktree" => {
            if ctx.workspace.is_empty() {
                return Err(NativeErr::MissingOrigin(
                    "telescope: no origin workspace to remove.",
                ));
            }
            if !input.confirm {
                return Err(NativeErr::Cancel);
            }
            Ok(vec![
                "worktree".into(),
                "remove".into(),
                "--workspace".into(),
                ctx.workspace.clone(),
            ])
        }
        "reload_config" => Ok(vec!["server".into(), "reload-config".into()]),
        _ => Err(NativeErr::Unknown),
    }
}

fn dispatch_native(payload: &str, ctx: &OriginContext) {
    if payload == "start_agent" {
        dispatch_start_agent(ctx);
        return;
    }
    let Some(input) = gather_native_input(payload, ctx) else {
        return;
    };
    match native_args(payload, ctx, &input) {
        Ok(args) => run_cli(&args),
        Err(NativeErr::Cancel) => {}
        Err(NativeErr::MissingOrigin(msg)) => die(msg),
        Err(NativeErr::Unknown) => die(&format!("telescope: unknown native action '{payload}'.")),
        Err(NativeErr::MultiStep) => die(&format!("telescope: unknown native action '{payload}'.")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_switch_enters_files_on_at() {
        let out = at_switch("/tmp/a", "/tmp/f", "@", ACTIONS_PROMPT, "tel");
        assert!(
            out.contains("reload-sync[cat -- /tmp/f]"),
            "should reload files, got {out:?}"
        );
        assert!(out.contains(&format!("change-prompt[{FILES_PROMPT}]")));
        assert!(out.contains(STRIP_AT));
    }

    #[test]
    fn at_switch_paste_at_c_strips_prefix() {
        let out = at_switch("/tmp/a", "/tmp/f", "@C", ACTIONS_PROMPT, "tel");
        assert!(out.contains("reload-sync[cat -- /tmp/f]"));
        assert!(out.contains(STRIP_AT));
    }

    #[test]
    fn at_switch_does_not_bounce_while_filtering_files() {
        assert_eq!(at_switch("/tmp/a", "/tmp/f", "C", FILES_PROMPT, "tel"), "");
        assert_eq!(
            at_switch("/tmp/a", "/tmp/f", "@C", FILES_PROMPT, "tel"),
            format!("{STRIP_AT}\n")
        );
    }

    #[test]
    fn at_switch_enters_search_on_slash() {
        let out = at_switch("/tmp/a", "/tmp/f", "/", ACTIONS_PROMPT, "tel");
        assert!(out.contains("disable-search"), "got {out:?}");
        assert!(out.contains("reload-sync[tel rg-files]"), "got {out:?}");
        assert!(out.contains(&format!("change-prompt[{SEARCH_PROMPT}]")));
        assert!(
            !out.contains("change-preview["),
            "preview stays on --preview {{}}, got {out:?}"
        );
        assert!(out.contains(&format!("change-preview-window[{SEARCH_PREVIEW_WIN}]")));
        assert!(out.contains(STRIP_SLASH));
    }

    #[test]
    fn at_switch_reloads_rg_while_typing_in_search() {
        let out = at_switch("/tmp/a", "/tmp/f", "foo", SEARCH_PROMPT, "tel");
        assert_eq!(out, "reload-sync[tel rg-files]\n");
    }

    #[test]
    fn at_back_returns_to_actions_from_files_or_search() {
        for prompt in [FILES_PROMPT, SEARCH_PROMPT] {
            let out = at_back("/tmp/a", prompt);
            assert!(
                out.contains("reload-sync[cat -- /tmp/a]"),
                "should reload actions, got {out:?}"
            );
            assert!(out.contains("enable-search"));
            assert!(out.contains(&format!("change-prompt[{ACTIONS_PROMPT}]")));
        }
    }

    #[test]
    fn at_switch_noop_while_still_in_actions() {
        assert_eq!(
            at_switch("/tmp/a", "/tmp/f", "clo", ACTIONS_PROMPT, "tel"),
            ""
        );
        assert_eq!(at_switch("/tmp/a", "/tmp/f", "", ACTIONS_PROMPT, "tel"), "");
        assert_eq!(at_back("/tmp/a", ACTIONS_PROMPT), "");
    }

    #[test]
    fn workspace_row_is_searchable_by_name() {
        let row = workspace_row("w6", "capehorn-next");
        let fields: Vec<&str> = row.split('\t').collect();
        assert_eq!(fields[0], "workspace");
        assert_eq!(fields[1], "w6");
        assert!(fields[2].contains("workspace:"));
        assert!(fields[2].contains("capehorn-next"));
        assert!(fields[3].contains("capehorn-next"));
        assert!(fields[3].contains("workspace"));
        assert_eq!(fields[4], "herdr workspace focus w6");
    }

    fn origin() -> OriginContext {
        OriginContext {
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            workspace: "w1".into(),
            cwd: "/repo".into(),
        }
    }

    fn typed(line: &str) -> NativeInput {
        NativeInput {
            line: Some(line.into()),
            ..NativeInput::default()
        }
    }

    fn argv(id: &str, input: NativeInput) -> Vec<String> {
        native_args(id, &origin(), &input).expect(id)
    }

    #[test]
    fn every_native_action_is_planned() {
        let ids: Vec<&str> = native::NATIVE_ACTIONS.iter().map(|a| a.id).collect();
        for id in &ids {
            let mut input = NativeInput {
                line: Some("notes".into()),
                extra: Some("main".into()),
                pick: Some("w1:t2".into()),
                confirm: true,
            };
            if *id == "interrupt_agent" || *id == "rename_agent" || *id == "prompt_agent" {
                input.pick = Some("reviewer".into());
            }
            let got = native_args(id, &origin(), &input);
            if *id == "start_agent" {
                assert_eq!(got, Err(NativeErr::MultiStep), "{id}");
            } else {
                assert!(got.is_ok(), "{id} should plan argv, got {got:?}");
            }
        }
        assert_eq!(
            native_args("not_an_action", &origin(), &typed("x")),
            Err(NativeErr::Unknown)
        );
    }

    #[test]
    fn prompt_actions_submit_the_typed_line_on_enter() {
        // These used to freeze: ask() waited for EOF instead of newline.
        let cases = [
            (
                "new_tab_named",
                &[
                    "tab",
                    "create",
                    "--focus",
                    "--workspace",
                    "w1",
                    "--cwd",
                    "/repo",
                    "--label",
                    "notes",
                ][..],
            ),
            ("rename_tab", &["tab", "rename", "w1:t1", "notes"][..]),
            ("rename_pane", &["pane", "rename", "w1:p1", "notes"][..]),
            (
                "new_workspace_here",
                &[
                    "workspace",
                    "create",
                    "--focus",
                    "--cwd",
                    "/repo",
                    "--label",
                    "notes",
                ][..],
            ),
            (
                "rename_workspace",
                &["workspace", "rename", "w1", "notes"][..],
            ),
            (
                "new_worktree_branch",
                &[
                    "worktree",
                    "create",
                    "--workspace",
                    "w1",
                    "--branch",
                    "notes",
                    "--focus",
                ][..],
            ),
        ];
        for (id, want) in cases {
            assert_eq!(argv(id, typed("notes")), want, "{id}");
            assert_eq!(
                native_args(id, &origin(), &typed("")),
                Err(NativeErr::Cancel),
                "{id} empty line cancels"
            );
            assert_eq!(line_prompt(id).is_some(), true, "{id} must prompt");
        }
        let agent = NativeInput {
            line: Some("hello".into()),
            pick: Some("reviewer".into()),
            ..NativeInput::default()
        };
        assert_eq!(
            argv("prompt_agent", agent.clone()),
            ["agent", "prompt", "reviewer", "hello"]
        );
        assert_eq!(
            argv(
                "rename_agent",
                NativeInput {
                    line: Some("bot".into()),
                    pick: Some("reviewer".into()),
                    ..NativeInput::default()
                }
            ),
            ["agent", "rename", "reviewer", "bot"]
        );
    }

    #[test]
    fn fire_and_forget_native_args() {
        assert_eq!(
            argv("new_tab", NativeInput::default()),
            [
                "tab",
                "create",
                "--focus",
                "--workspace",
                "w1",
                "--cwd",
                "/repo"
            ]
        );
        assert_eq!(
            argv("close_tab", NativeInput::default()),
            ["tab", "close", "w1:t1"]
        );
        assert_eq!(
            argv("split_vertical", NativeInput::default()),
            [
                "pane",
                "split",
                "w1:p1",
                "--direction",
                "right",
                "--focus",
                "--cwd",
                "/repo"
            ]
        );
        assert_eq!(
            argv("split_horizontal", NativeInput::default()),
            [
                "pane",
                "split",
                "w1:p1",
                "--direction",
                "down",
                "--focus",
                "--cwd",
                "/repo"
            ]
        );
        assert_eq!(
            argv("close_pane", NativeInput::default()),
            ["pane", "close", "w1:p1"]
        );
        assert_eq!(
            argv("zoom_pane", NativeInput::default()),
            ["pane", "zoom", "w1:p1", "--toggle"]
        );
        for (id, dir) in [
            ("focus_left", "left"),
            ("focus_right", "right"),
            ("focus_up", "up"),
            ("focus_down", "down"),
        ] {
            assert_eq!(
                argv(id, NativeInput::default()),
                ["pane", "focus", "--direction", dir, "--pane", "w1:p1"]
            );
        }
        for (id, dir) in [
            ("resize_left", "left"),
            ("resize_right", "right"),
            ("resize_up", "up"),
            ("resize_down", "down"),
        ] {
            assert_eq!(
                argv(id, NativeInput::default()),
                ["pane", "resize", "--direction", dir, "--pane", "w1:p1"]
            );
        }
        for (id, dir) in [
            ("swap_left", "left"),
            ("swap_right", "right"),
            ("swap_up", "up"),
            ("swap_down", "down"),
        ] {
            assert_eq!(
                argv(id, NativeInput::default()),
                ["pane", "swap", "--direction", dir, "--pane", "w1:p1"]
            );
        }
        assert_eq!(
            argv(
                "move_pane_tab",
                NativeInput {
                    pick: Some("w1:t2".into()),
                    ..NativeInput::default()
                }
            ),
            ["pane", "move", "w1:p1", "--tab", "w1:t2", "--focus"]
        );
        assert_eq!(
            argv("move_pane_new_tab", NativeInput::default()),
            ["pane", "move", "w1:p1", "--new-tab", "--focus"]
        );
        assert_eq!(
            argv("move_pane_new_workspace", NativeInput::default()),
            ["pane", "move", "w1:p1", "--new-workspace", "--focus"]
        );
        assert_eq!(
            argv(
                "interrupt_agent",
                NativeInput {
                    pick: Some("reviewer".into()),
                    ..NativeInput::default()
                }
            ),
            ["agent", "send-keys", "reviewer", "esc"]
        );
        assert_eq!(
            argv("new_workspace", NativeInput::default()),
            ["workspace", "create", "--focus"]
        );
        assert_eq!(
            argv("close_workspace", NativeInput::default()),
            ["workspace", "close", "w1"]
        );
        assert_eq!(
            argv("new_worktree", NativeInput::default()),
            ["worktree", "create", "--workspace", "w1", "--focus"]
        );
        assert_eq!(
            native_args(
                "remove_worktree",
                &origin(),
                &NativeInput {
                    confirm: false,
                    ..NativeInput::default()
                }
            ),
            Err(NativeErr::Cancel)
        );
        assert_eq!(
            argv(
                "remove_worktree",
                NativeInput {
                    confirm: true,
                    ..NativeInput::default()
                }
            ),
            ["worktree", "remove", "--workspace", "w1"]
        );
        assert_eq!(
            argv("reload_config", NativeInput::default()),
            ["server", "reload-config"]
        );
        let with_base = NativeInput {
            line: Some("feat".into()),
            extra: Some("main".into()),
            ..NativeInput::default()
        };
        assert_eq!(
            argv("new_worktree_branch", with_base),
            [
                "worktree",
                "create",
                "--workspace",
                "w1",
                "--branch",
                "feat",
                "--focus",
                "--base",
                "main"
            ]
        );
    }

    #[test]
    fn missing_origin_refuses_scoped_actions() {
        let empty = OriginContext::default();
        let input = NativeInput::default();
        for id in [
            "close_tab",
            "rename_tab",
            "split_vertical",
            "close_pane",
            "zoom_pane",
            "rename_pane",
            "focus_left",
            "resize_left",
            "swap_left",
            "move_pane_tab",
            "move_pane_new_tab",
            "move_pane_new_workspace",
            "new_workspace_here",
            "close_workspace",
            "rename_workspace",
            "new_worktree",
            "new_worktree_branch",
            "remove_worktree",
        ] {
            let mut i = input.clone();
            i.line = Some("x".into());
            i.pick = Some("w1:t2".into());
            i.confirm = true;
            assert!(
                matches!(
                    native_args(id, &empty, &i),
                    Err(NativeErr::MissingOrigin(_))
                ),
                "{id}"
            );
        }
    }
}
