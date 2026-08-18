//! Origin-context handling: which pane/tab/workspace/cwd the user invoked the
//! telescope from.
//!
//! The `open` ACTION runs on the herdr server before the popup exists. It reads
//! HERDR_PLUGIN_CONTEXT_JSON (the origin pane/tab/workspace/cwd), stamps them
//! into a small JSON blob, and forwards it to the popup pane as TELESCOPE_CTX via
//! `plugin pane open --env`. The `palette` pane is a brand-new pane — without
//! this hand-off, every pane/tab/workspace-scoped dispatch would target the
//! popup itself instead of the pane the user actually meant.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OriginContext {
    pub pane: String,
    pub tab: String,
    pub workspace: String,
    pub cwd: String,
}

impl OriginContext {
    /// Read the origin context from the forwarded TELESCOPE_CTX blob.
    pub fn from_env() -> Self {
        let raw = std::env::var("TELESCOPE_CTX").unwrap_or_default();
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Capture the origin context from herdr's injected plugin-context JSON.
    #[allow(clippy::field_reassign_with_default)]
    pub fn capture_from_herdr_context() -> Self {
        let mut ctx = OriginContext::default();
        let raw = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").unwrap_or_default();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            ctx.pane = str_of(&v, &["focused_pane_id"]).unwrap_or_default();
            ctx.tab = str_of(&v, &["tab_id"]).unwrap_or_default();
            ctx.workspace = str_of(&v, &["workspace_id"]).unwrap_or_default();
            ctx.cwd = str_of(&v, &["focused_pane_cwd"])
                .or_else(|| str_of(&v, &["workspace_cwd"]))
                .unwrap_or_default();
        }
        if ctx.cwd.is_empty() {
            ctx.cwd = std::env::var("HERDR_WORKSPACE_CWD").unwrap_or_default();
        }
        ctx
    }

    /// Serialize this context for forwarding into the popup.
    pub fn to_env(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

fn str_of(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let mut cur = v;
    for k in keys {
        cur = cur.get(*k)?;
    }
    match cur {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}
