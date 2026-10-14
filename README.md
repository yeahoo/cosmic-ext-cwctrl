# cosmic-ext-cwctrl

[![CI](https://github.com/yeahoo/cosmic-ext-cwctrl/actions/workflows/ci.yml/badge.svg)](https://github.com/yeahoo/cosmic-ext-cwctrl/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**English** | [简体中文](README.zh.md)

Query and control windows on the COSMIC™ desktop from the command line.

A Rust rewrite of [`cosmic-ext-window-helper`](https://github.com/lapause/cosmic-ext-window-helper)
that starts ~300× faster and additionally exposes window geometry.

## Why

The upstream Python implementation takes ~300 ms per invocation, because it pays
for the interpreter, for parsing a 264 KB protocol database, and — most of all —
for unconditionally waiting on a ~150 ms data batch from the compositor. That is
painful when the tool is bound to a keyboard shortcut. This rewrite is a plain
synchronous Wayland client with no async runtime and no interpreter.

| | cwctrl | upstream (Python) |
|---|---|---|
| `activate` / `close` / `maximize` / … | **1.0 ms** | 307 ms |
| `list -n` | **1.0 ms** | — |
| `list` (with geometry) | 150 ms | 317 ms (and no geometry) |
| binary | 2.0 MB | 5.8 MB venv |
| RSS | ~4 MB | ~26 MB |

## Install

```bash
cargo install --git https://github.com/yeahoo/cosmic-ext-cwctrl
```

Or from a clone:

```bash
git clone https://github.com/yeahoo/cosmic-ext-cwctrl
cd cosmic-ext-cwctrl
cargo build --release
install -Dm755 target/release/cwctrl ~/.local/bin/cwctrl
```

Requires Rust 1.86+ and a running COSMIC session (`cosmic-comp`).

## Usage

```
cwctrl <COMMAND> [ARGS...] [QUERY]

  state  [QUERY]                        Print window state as JSON (with geometry)
  list   [-n] [QUERY]                   Same as state (JSON array)
  activate [QUERY]                      Focus the most recently used matching window
  close    [QUERY]                      Close matching windows
  minimize <true|false> [QUERY]         Minimize / unminimize
  maximize <true|false> [QUERY]         Maximize / unmaximize
  fullscreen <true|false> [QUERY]       Fullscreen / unfullscreen
  sticky   <true|false> [QUERY]         Stick / unstick
  move_to  <WORKSPACE> <OUTPUT> [QUERY] Move windows to a workspace
  cycle    [-b] [QUERY]                 Cycle focus through the matching windows
  interfaces                            List Wayland interfaces the compositor exposes

  -n, --no-wait   do not wait for geometry (~1 ms, geometry will be null)
```

### Query syntax

```
string fields: id, app_id, title, workspace.name, output.name
operators:     =   equal
               !=  not equal
               ~=  regular expression
               Values must be quoted. Append `i` for case-insensitive matching.
bool fields:   is_active, is_active_app_id, is_maximized, is_minimized,
               is_fullscreen, is_sticky, workspace.is_visible,
               workspace.has_focus, output.has_focus
combinators:   and / or / not, plus parentheses
```

Examples:

```bash
cwctrl list "app_id ~= 'chrome'i"
cwctrl list "workspace.is_visible and not is_active"
cwctrl activate "title ~= 'wezterm'"
cwctrl maximize true "app_id = 'com.microsoft.VSCode'"
cwctrl move_to 2 HDMI-A-1 "app_id = 'google-chrome'"
cwctrl close "is_minimized and app_id = 'org.gnome.Calculator'"
```

Typical run-or-raise shortcut (`exit code 10` means "no match"):

```bash
cwctrl activate "app_id = 'com.system76.CosmicTerm'" || cosmic-term
```

### Picking which window to raise

`activate` raises **one** window, not every match:

1. if focus is already on a matching window, nothing happens;
2. otherwise the **most recently used** match is raised.

That order is tracked in `$XDG_RUNTIME_DIR/cwctrl-focus-history`, because neither
Wayland nor COSMIC exposes a focus stack. `cycle` updates the same list, so the
two commands stay consistent.

This is what makes `Alt+S` return to *the* terminal you were actually using,
rather than whichever one happens to be enumerated first:

```text
wezterm1  wezterm2  wezterm3     # you were on wezterm3
Alt+E  ->  chrome
Alt+S  ->  wezterm3              # not wezterm1
```

The other commands (`close`, `maximize`, …) still act on every match.

### Output

```json
[
  {
    "id": "jTQOIgg8kjnhQtLhJ7fOawc79y1MXJOb",
    "app_id": "google-chrome",
    "title": "Node.js — Google Chrome",
    "is_active": false,
    "is_active_app_id": false,
    "is_maximized": false,
    "is_minimized": false,
    "is_fullscreen": false,
    "is_sticky": false,
    "workspace": { "name": "1", "is_visible": true, "has_focus": true },
    "output":    { "name": "HDMI-A-1", "has_focus": true },
    "geometry":  { "output": "HDMI-A-1", "x": 2649, "y": 32,
                   "width": 2645, "height": 1486 }
  }
]
```

`geometry` is in **logical pixels**, `x`/`y` relative to the top-left corner of
that output (logical = physical / scale factor). It can be fed straight to
`cosmic-screenshot`, which uses the same coordinate space.

`id` is the `ext_foreign_toplevel_handle_v1` identifier and is stable across
invocations.

Fields that could not be read under `--no-wait` are reported as `null` rather
than `false`, so "unknown" is never silently reported as "no".

### Language

Messages follow the locale, checked in this order: `CWCTRL_LANG`, `LC_ALL`,
`LC_MESSAGES`, `LANG`. A value starting with `zh` selects Chinese; anything else
selects English. To force a language:

```bash
CWCTRL_LANG=zh cwctrl --help
```

## Implementation notes

Protocols used:

| Protocol | Purpose |
|---|---|
| `ext_foreign_toplevel_list_v1` | enumerate all toplevels (incl. XWayland), gives `identifier`/`title`/`app_id` |
| `zcosmic_toplevel_info_v1` | `get_cosmic_toplevel()` to obtain the enhanced handle |
| `zcosmic_toplevel_handle_v1` | `state` / `geometry` / `output_enter` / `ext_workspace_enter` |
| `zcosmic_toplevel_manager_v1` | `activate` / `close` / `set_maximized` / `set_fullscreen` / `move_to_ext_workspace` / … |
| `ext_workspace_manager_v1` | workspace names and active state |
| `wl_output` / `wl_seat` | output names; a seat is required by `activate` |

Two things that are easy to get wrong:

1. **`zcosmic_toplevel_info_v1` no longer pushes `title`/`app_id`/`toplevel`**
   as of version 2 (they are `deprecated-since="2"` in the XML). You must first
   obtain an `ext_foreign_toplevel_handle_v1` and then call
   `get_cosmic_toplevel()` to get a `zcosmic_toplevel_handle_v1`.

2. **`state`, `geometry` and `workspace` are pushed as a fixed ~150 ms batch**,
   not answered on request. Measured 30 times here: 150.1–152.3 ms, no jitter.
   So the tool waits *only when it needs those fields*: control commands and
   `list -n` return in ~1 ms, `list` waits, and a control command whose query
   uses state fields retries once if the first pass matched nothing
   (`Query::uses_deferred_state()`).

### Why `cosmic-protocols` is not used

The [`cosmic-protocols`](https://crates.io/crates/cosmic-protocols) crate is
licensed **GPL-3.0-only**, which would make this binary GPL as well. However, the
protocol XML files it ships are licensed MIT/HPND ("Permission to use, copy,
modify, distribute, and sell … without fee"). This project therefore generates
its bindings directly from the vendored XML in `protocols/` using the MIT-licensed
`wayland-scanner`, keeping the whole project MIT.

All dependencies are permissive (`cargo metadata` → no GPL/AGPL). The upstream
copyright notices are preserved inside each XML file.

## Credits

- Behaviour and command set modelled on
  [`cosmic-ext-window-helper`](https://github.com/lapause/cosmic-ext-window-helper)
  by Pierre Guillaume (MIT).
- Protocol definitions from
  [`pop-os/cosmic-protocols`](https://github.com/pop-os/cosmic-protocols).
- Built on [`wayland-rs`](https://github.com/Smithay/wayland-rs).

## License

MIT — see [LICENSE](LICENSE).
