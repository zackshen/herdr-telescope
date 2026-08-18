//! Resolve herdr's effective keybindings for the shortcut column.
//!
//! herdr has no "list my effective keybindings" API, so we reconstruct it from
//! the two files that define it:
//!   - `herdr --default-config` — the installed binary's own defaults, which
//!     ship as *commented* assignments under `[keys]`.
//!   - the user's `config.toml` (or $HERDR_CONFIG_PATH) — uncommented `[keys]`
//!     entries win; an explicit "" means unbound.
//!
//! A key claimed by a `[[keys.command]]` block shadows the built-in that shipped
//! on it, so we report those as unbound (showing a shortcut that no longer fires
//! is worse than showing none).

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct Keys {
    /// name -> binding, e.g. "split_vertical" -> "prefix+v".
    map: HashMap<String, String>,
    /// the prefix key, e.g. "ctrl+b".
    pub prefix: String,
    /// bindings claimed by [[keys.command]] blocks (shadowed).
    shadow: Vec<String>,
}

fn home_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("HERDR_CONFIG_PATH") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/herdr/config.toml")
}

/// Parse the `[keys]` table out of a TOML document where each assignment may be
/// prefixed with a leading comment marker (the default-config ships commented).
/// Keys whose value is a non-empty string are collected into `map`.
fn collect_keys(text: &str, uncomment: bool, map: &mut HashMap<String, String>) {
    let mut cleaned = String::with_capacity(text.len());
    for raw in text.lines() {
        let mut line = raw;
        if uncomment {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('#') {
                line = rest;
            }
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    if let Ok(table) = cleaned.parse::<toml::Table>() {
        if let Some(toml::Value::Table(keys)) = table.get("keys") {
            for (name, val) in keys {
                if let toml::Value::String(s) = val {
                    if !s.is_empty() {
                        map.insert(name.clone(), s.clone());
                    }
                }
            }
        }
    }
}

/// Extract the `key` values from every `[[keys.command]]` block.
fn collect_shadow(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(table) = text.parse::<toml::Table>() {
        if let Some(toml::Value::Array(commands)) = table.get("keys").and_then(|t| {
            if let toml::Value::Table(kt) = t {
                kt.get("command")
            } else {
                None
            }
        }) {
            for cmd in commands {
                if let toml::Value::Table(ct) = cmd {
                    if let Some(toml::Value::String(k)) = ct.get("key") {
                        out.push(k.clone());
                    }
                }
            }
        }
    }
    out
}

/// Load the effective keybinding map.
pub fn load() -> Keys {
    let mut keys = Keys::default();

    // Defaults from the installed binary (commented, so uncomment while parsing).
    if let Ok(out) = std::process::Command::new(crate::herdr::bin())
        .arg("--default-config")
        .output()
    {
        if out.status.success() {
            if let Ok(text) = String::from_utf8(out.stdout) {
                collect_keys(&text, true, &mut keys.map);
            }
        }
    }

    // User config overrides the defaults.
    let config_path = home_config_path();
    if let Ok(text) = std::fs::read_to_string(&config_path) {
        collect_keys(&text, false, &mut keys.map);
        keys.shadow = collect_shadow(&text);
    }

    keys.prefix = keys
        .map
        .get("prefix")
        .cloned()
        .unwrap_or_else(|| "ctrl+b".to_string());
    keys
}

impl Keys {
    pub fn is_shadowed(&self, binding: &str) -> bool {
        self.shadow.iter().any(|b| b == binding)
    }

    /// Render a binding to the keystrokes the user actually types.
    /// `index` fills in "…1..9" indexed bindings (switch_tab etc.).
    pub fn shortcut(&self, name: &str, index: Option<u8>) -> String {
        if name.is_empty() {
            return String::new();
        }
        let Some(binding) = self.map.get(name) else {
            return String::new();
        };
        if binding.is_empty() {
            return String::new();
        }
        let binding = if let Some(i) = index {
            binding.replace("1..9", &i.to_string())
        } else {
            binding.clone()
        };
        if binding.ends_with("1..9") {
            return String::new(); // indexed binding with no index to fill
        }
        if self.is_shadowed(&binding) {
            return String::new();
        }
        if let Some(suffix) = binding.strip_prefix("prefix+") {
            format!("{} {}", self.prefix, suffix)
        } else {
            binding
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_from(map: &[(&str, &str)], shadow: &[&str]) -> Keys {
        let mut k = Keys {
            map: map
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            prefix: "ctrl+b".to_string(),
            shadow: shadow.iter().map(|s| s.to_string()).collect(),
        };
        if let Some(p) = k.map.get("prefix") {
            k.prefix = p.clone();
        }
        k
    }

    #[test]
    fn renders_prefix_binding() {
        let k = keys_from(&[("split_vertical", "prefix+v")], &[]);
        assert_eq!(k.shortcut("split_vertical", None), "ctrl+b v");
    }

    #[test]
    fn renders_direct_binding() {
        let k = keys_from(&[("zoom", "prefix+z")], &[]);
        assert_eq!(k.shortcut("zoom", None), "ctrl+b z");
    }

    #[test]
    fn unbound_and_missing_render_empty() {
        let k = keys_from(&[("close_pane", "")], &[]);
        assert_eq!(k.shortcut("close_pane", None), "");
        assert_eq!(k.shortcut("nope", None), "");
        assert_eq!(k.shortcut(":", None), ""); // ":" = no shortcut column
    }

    #[test]
    fn fills_indexed_and_drops_unfillable() {
        let k = keys_from(&[("switch_tab", "prefix+1..9")], &[]);
        assert_eq!(k.shortcut("switch_tab", Some(3)), "ctrl+b 3");
        // no index -> can't fill -> empty
        assert_eq!(k.shortcut("switch_tab", None), "");
    }

    #[test]
    fn shadowed_binding_is_empty() {
        // A [[keys.command]] on prefix+z shadows the built-in zoom.
        let k = keys_from(&[("zoom", "prefix+z")], &["prefix+z"]);
        assert_eq!(k.shortcut("zoom", None), "");
    }
}
