# herdr-telescope

An [fzf](https://github.com/junegunn/fzf) command telescope for [herdr](https://herdr.dev),
implemented in Rust. Merges three things into one small, **centered popup**:

1. **herdr's native actions** — the tab/pane/workspace/worktree/agent/session actions
   herdr itself binds (the list [herdr-quick-actions](https://github.com/enekos/herdr-quick-actions)
   surfaces), each row showing the keybinding herdr actually has for it.
2. **every action of every installed plugin** — via `herdr plugin action list`
   (what [herdr-command-palette](https://github.com/JanTvrdik/herdr-command-palette)
   surfaces).
3. **a file finder** — pick a file under the origin cwd, then open it in a **new pane**
   or a **new window**. Type `@` in the telescope to switch the list to files immediately
   (e.g. type `@` then `palette.rs`, or paste `@palette.rs`) — no need to select
   "Search files…" first. Type `/` to search file *contents* with `rg` (left: matching
   files, right: highlighted lines). Backspace on an empty query to return to actions.

The UI follows herdr-quick-actions: a modal `popup` (70%×70%, centered over the active
pane) that runs fzf in a real TTY. Every row ends with a preview strip showing the exact
`herdr` command it will run.

```
herdr telescope ▸ clo
┌─────────────────────────────────────────────────────────────────────────────┐
│ ↑↓ select · enter run · esc cancel                                          │
│ > Close pane                          ctrl+b x   kill remove quit delete    │
│   Close tab                           ctrl+b shift+x  kill remove quit …     │
│   Close workspace                     ctrl+b shift+d  kill remove project    │
│   es.quick-actions.open  Quick actions (native tab/pane/workspace/… )       │
│   🔍 Search files…                                                            │
├─────────────────────────────────────────────────────────────────────────────┤
│ herdr pane close <pane>                                                      │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Requirements

- [herdr](https://herdr.dev) ≥ 0.8.0
- [fzf](https://github.com/junegunn/fzf) ≥ 0.48 (`transform` / `reload-sync`; 0.74 used here)
- `fd` (optional, preferred for the file finder) or a `git` repo; falls back to `find`.
- [`rg`](https://github.com/BurntSushi/ripgrep) for `/` content search.
- [`bat`](https://github.com/sharkdp/bat) (optional) to syntax-highlight the right-hand search preview.
- Rust toolchain only if you build from source. `herdr plugin install` downloads a prebuilt binary.

## Install

Prebuilt binaries, no Rust toolchain needed:

```bash
herdr plugin install zackshen/herdr-telescope
```

`[[build]]` downloads `herdr-telescope` for this platform from the matching
[GitHub Release](https://github.com/zackshen/herdr-telescope/releases) into `bin/`.

To update, reinstall:

```bash
herdr plugin uninstall telescope && herdr plugin install zackshen/herdr-telescope
```

Or link a local checkout (link does **not** run `[[build]]`):

```bash
cargo build --release
mkdir -p bin && cp target/release/herdr-telescope bin/
herdr plugin link ./herdr-telescope   # from the parent directory
```

herdr does NOT bind keys declared in a plugin manifest. Add a binding to
`~/.config/herdr/config.toml` and reload:

```toml
[[keys.command]]
key = "prefix+p"
type = "plugin_action"
command = "telescope.open"
description = "Telescope (actions & files)"
```

```bash
herdr server reload-config
```

Now `prefix` then `p` (Ctrl+B, P with the default prefix) opens the telescope.

## What's in the list

- **Native actions** (ranked list): New/close/rename tab, split panes, toggle zoom,
  close/rename pane, focus/resize/swap/move pane, start an agent in a new split,
  prompt/interrupt/rename agent, new/close/rename workspace, worktrees, and reload
  config. The shortcut column is resolved live from `herdr --default-config` plus your
  `config.toml`; a `[[keys.command]]` binding that shadows a built-in shows no shortcut.
  Open workspaces appear as `workspace: <name>` in the same list — type part of
  the name to jump (`capehor` → `workspace: capehorn-next`).
- **Plugin actions**: every `plugin.action  <title>` from `herdr plugin action list`
  (your own `telescope` actions are hidden). Selecting one runs
  `herdr plugin action invoke <id>` and polls the plugin log so failures surface.
- **Search files…**: filters the origin cwd with `fd` (gitignore-aware), falling back
  to `git ls-files -co --exclude-standard`, then `find` (depth 6, capped). Pick a file,
  then choose:
  **Shortcut:** type `@` in the main telescope to switch the list to files in place
  (the `@` is consumed; then type `C` or `@C` to filter names matching `C`).
  Type `/` to search contents with ripgrep: the list becomes matching files and
  the preview moves to the right, highlighting hits. Backspace on an empty
  query to return to actions.

  | choice   | what happens                                                              |
  |----------|---------------------------------------------------------------------------|
  | New pane | split the origin pane, then `herdr pane run <new> "$EDITOR <file>"`       |
  | New window | new tab, then `herdr pane run <root> "$EDITOR <file>"`                  |

  Both drop you in the file's directory and open the file with `$EDITOR`
  (falls back to `vi` if unset). No origin pane → falls back to a new window.
  The action you invoked it from acts on the *origin* pane, not the popup itself.

## How it works

herdr actions run on the server with **no TTY**, so an action can't run fzf directly:

1. `telescope.open` captures the origin pane/tab/workspace/cwd from
   `HERDR_PLUGIN_CONTEXT_JSON` and opens a **small, centered popup**
   (`placement = "popup"` — a real modal window, like quick-actions), forwarding the
   origin ids as a single `TELESCOPE_CTX` env blob. The popup *does* get a TTY.
2. Inside the popup, `herdr-telescope palette` builds the merged TSV list, pipes it to
   fzf, and dispatches the selection by calling the `herdr` CLI directly.
   Keybindings and plugin actions are resolved live; nothing is hardcoded against a
   herdr version.
3. On exit the popup is closed explicitly (popup placement doesn't reliably tear
   itself down).

## Debugging

The palette mode can be driven without a TTY to inspect the generated rows:

```bash
export TELESCOPE_CTX='{"pane":"w1:p1","tab":"w1:t1","workspace":"w1","cwd":"/repo"}'
./target/release/herdr-telescope palette   # interactive fzf (run in a terminal)
```

## License

MIT
