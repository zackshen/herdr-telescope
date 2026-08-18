//! herdr-telescope — an fzf command telescope for herdr, implemented in Rust.
//!
//! Two modes, driven by the manifest:
//!   `open`    — the `telescope.open` ACTION. Runs on the herdr server with no
//!               TTY. Captures the origin pane/tab/workspace/cwd from herdr's
//!               injected context, forwards it to the popup as TELESCOPE_CTX,
//!               and opens the centered `palette` popup pane.
//!   `palette` — the `telescope.palette` PANE. Runs inside the popup with a real
//!               TTY. Builds the merged fzf list (native actions, installed
//!               plugin actions, file finder) and dispatches the selection.

mod context;
mod files;
mod herdr;
mod keys;
mod native;
mod palette;
mod tty;

use std::process::ExitCode;

use context::OriginContext;

fn main() -> ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "open" => open_action(),
        "palette" => ExitCode::from(palette::run() as u8),
        _ => {
            eprintln!("telescope: usage: herdr-telescope (open|palette)");
            ExitCode::from(2)
        }
    }
}

/// The `telescope.open` action: forward origin context and open the popup pane.
fn open_action() -> ExitCode {
    let ctx = OriginContext::capture_from_herdr_context();

    let mut args: Vec<String> = vec![
        "plugin".into(),
        "pane".into(),
        "open".into(),
        "--plugin".into(),
        "telescope".into(),
        "--entrypoint".into(),
        "palette".into(),
        "--focus".into(),
        "--env".into(),
        format!("TELESCOPE_CTX={}", ctx.to_env()),
    ];

    // Forward --cwd only when it's a real directory; otherwise the popup
    // resolves relative to the plugin root (harmless for palette mode, which
    // reads only TELESCOPE_CTX and $HERDR_PLUGIN_ROOT).
    if !ctx.cwd.is_empty() && std::path::Path::new(&ctx.cwd).is_dir() {
        args.push("--cwd".into());
        args.push(ctx.cwd.clone());
    }

    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = herdr::run(&arg_refs);
    if !out.status.success() {
        eprintln!(
            "telescope: failed to open popup: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
