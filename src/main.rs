//! cwctrl — fast COSMIC window query & control

mod i18n;
mod input;
mod protocols;
mod query;
mod state;

use std::error::Error;
use std::time::Duration;

use input::{Button, Keyboard, Mouse, ScrollAxis};
use query::Query;
use state::{
    ST_ACTIVATED, ST_FULLSCREEN, ST_MAXIMIZED, ST_MINIMIZED, ST_STICKY, State, WS_ACTIVE, Window,
};
use wayland_client::Proxy;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default animation length for a smooth `mousemove` / `click --at`, in ms.
const DEFAULT_DURATION_MS: u64 = 400;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<i32, Box<dyn Error>> {
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        print!("{}", i18n::m().help);
        return Ok(0);
    };
    match cmd {
        "-h" | "--help" | "help" => {
            print!("{}", i18n::m().help);
            Ok(0)
        }
        "-V" | "--version" | "version" => {
            println!("cwctrl {VERSION}");
            Ok(0)
        }
        "interfaces" => cmd_interfaces(),
        "mousemove" => cmd_mousemove(&args[1..]),
        "mousedown" => cmd_mouse_button(true, &args[1..]),
        "mouseup" => cmd_mouse_button(false, &args[1..]),
        "click" => cmd_click(&args[1..]),
        "scroll" => cmd_scroll(&args[1..]),
        "keydown" => cmd_key(KeyMode::Down, &args[1..]),
        "keyup" => cmd_key(KeyMode::Up, &args[1..]),
        "key" => cmd_key(KeyMode::Tap, &args[1..]),
        "state" | "list" => cmd_list(&args[1..]),
        "activate" => cmd_activate(&args[1..]),
        "close" => cmd_control(cmd, None, &args[1..]),
        "cycle" => cmd_cycle(&args[1..]),
        "minimize" | "maximize" | "fullscreen" | "sticky" => {
            let toggle = args
                .get(1)
                .map(|s| s.as_str())
                .ok_or_else(|| i18n::fill1(i18n::m().toggle_required, cmd))?;
            let on = parse_toggle(toggle)?;
            cmd_control(cmd, Some(on), &args[2..])
        }
        "move_to" => {
            if args.len() < 3 {
                return Err(i18n::m().move_to_arity.into());
            }
            cmd_move_to(&args[1], &args[2], &args[3..])
        }
        other => Err(i18n::fill1(i18n::m().unknown_command, other).into()),
    }
}

fn parse_toggle(s: &str) -> Result<bool, Box<dyn Error>> {
    match s {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(i18n::fill1(i18n::m().toggle_invalid, s).into()),
    }
}

/// List window state.
/// COSMIC pushes geometry/workspace in a fixed ~150ms deferred batch; that
/// throttling is server-side and cannot be bypassed by the client. We wait by
/// default to return complete data, while `-n/--no-wait` takes the fast path.
fn cmd_list(rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let mut no_wait = false;
    let mut parts: Vec<String> = Vec::new();
    for a in rest {
        match a.as_str() {
            "-n" | "--no-wait" | "--fast" => no_wait = true,
            _ => parts.push(a.clone()),
        }
    }
    let q = Query::parse(&parts.join(" "))?;
    let wait = if no_wait {
        Duration::ZERO
    } else {
        Duration::from_millis(250)
    };
    let (_conn, _queue, st) = state::connect(wait)?;
    let active_app = st.active_window().and_then(|w| w.app_id.clone());

    let mut out = String::from("[\n");
    let mut first = true;
    let mut n = 0usize;
    for w in st.windows.iter().filter(|w| !w.closed) {
        if !q.matches(&st, w) {
            continue;
        }
        if !first {
            out.push_str(",\n");
        }
        first = false;
        n += 1;
        out.push_str(&window_json(&st, w, active_app.as_deref(), 2));
    }
    out.push_str("\n]");
    println!("{out}");
    let _ = n;
    Ok(0)
}

// ---------------------------------------------------------------- focus history
//
// Wayland exposes no focus stack, and `zcosmic_toplevel_handle_v1` has no
// "last activated" field, so run-or-raise can only know which window of an app
// was used most recently if the client records it itself. This keeps an MRU list
// of toplevel identifiers in a small file under $XDG_RUNTIME_DIR.

fn history_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(std::path::PathBuf::from(dir).join("cwctrl-focus-history"))
}

fn load_history() -> Vec<String> {
    history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn save_history(h: &[String]) {
    if let Some(p) = history_path() {
        let _ = std::fs::write(p, h.join("\n"));
    }
}

/// Move an identifier to the front (most recently used).
fn touch_history(h: &mut Vec<String>, id: &str) {
    h.retain(|x| x != id);
    h.insert(0, id.to_string());
    h.truncate(128);
}

/// Unique identifier of a window, if the compositor gave us one.
fn ident(st: &State, idx: usize) -> Option<String> {
    st.windows[idx].identifier.clone()
}

/// `activate` picks a *single* window instead of acting on every match.
///
/// Rules (ordinary run-or-raise semantics):
///   1. focus is already on one of the matching windows -> do nothing
///   2. otherwise -> raise the most recently used one of the group, falling
///      back to the first match when there is no history yet
///
/// Upstream activates every matching window in turn, so whichever one happens to
/// be enumerated last wins — with wezterm1/2/3 open, pressing Alt+S from another
/// app always lands on wezterm1. That is what this fixes.
fn cmd_activate(rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let q = Query::parse(&rest.join(" "))?;
    let (conn, mut queue, mut st) = state::connect(Duration::ZERO)?;

    let mut matched = select(&st, &q);
    if matched.is_empty() && q.uses_deferred_state() {
        state::sync_more(&mut queue, &mut st, Duration::from_millis(250))?;
        matched = select(&st, &q);
    }
    if matched.is_empty() {
        eprintln!("{}", i18n::m().no_match);
        return Ok(10);
    }

    let mgr = st
        .toplevel_manager
        .clone()
        .ok_or(i18n::m().no_toplevel_manager)?;
    let seat = st.seat.clone().ok_or(i18n::m().no_seat)?;
    let mut hist = load_history();

    // 1. Already on this app: leave focus alone, just refresh the MRU entry.
    if let Some(&cur) = matched.iter().find(|&&i| st.windows[i].has(ST_ACTIVATED)) {
        if let Some(id) = ident(&st, cur) {
            touch_history(&mut hist, &id);
        }
        save_history(&hist);
        return Ok(0);
    }

    // 2. Raise the most recently used match.
    let target = matched
        .iter()
        .filter_map(|&i| st.windows[i].identifier.as_deref().map(|id| (i, id)))
        .min_by_key(|(_, id)| hist.iter().position(|h| h == *id).unwrap_or(usize::MAX))
        .map(|(i, _)| i)
        .unwrap_or(matched[0]);

    // Remember where the user came from, so the next press can return there.
    if let Some(cur) = st.active() {
        if let Some(id) = ident(&st, cur) {
            touch_history(&mut hist, &id);
        }
    }
    if let Some(ch) = st.windows[target].cosmic.clone() {
        mgr.activate(&ch, &seat);
    }
    if let Some(id) = ident(&st, target) {
        touch_history(&mut hist, &id);
    }
    save_history(&hist);

    conn.flush()?;
    Ok(0)
}

/// Control actions that apply to *every* matching window.
///
/// `activate` is deliberately not here: it must pick a single window, see
/// [`cmd_activate`].
///
/// If the query uses state fields (is_active, ...) and that batch has not arrived
/// yet, retry after one more wait, so we never silently match nothing.
fn cmd_control(action: &str, toggle: Option<bool>, rest: &[String]) -> Result<i32, Box<dyn Error>> {
    // Validate up front rather than relying on `unreachable!()` inside the loop.
    if !matches!(
        action,
        "close" | "minimize" | "maximize" | "fullscreen" | "sticky"
    ) {
        return Err(format!("unsupported control action '{action}'").into());
    }

    let q = Query::parse(&rest.join(" "))?;
    let (conn, mut queue, mut st) = state::connect(Duration::ZERO)?;

    let mut matched = select(&st, &q);
    // Wait one more round only when the query depends on deferred fields; string
    // queries (app_id/title/id) arrive immediately, so no match really means no
    // match and waiting 150ms would be wasted.
    if matched.is_empty() && q.uses_deferred_state() {
        state::sync_more(&mut queue, &mut st, Duration::from_millis(250))?;
        matched = select(&st, &q);
    }

    let mgr = st
        .toplevel_manager
        .clone()
        .ok_or(i18n::m().no_toplevel_manager)?;
    if matched.is_empty() {
        eprintln!("{}", i18n::m().no_match);
        return Ok(10);
    }

    // Reverse the order so the first-enumerated window is acted on last (as upstream does).
    for &i in matched.iter().rev() {
        let w = &st.windows[i];
        let Some(ch) = w.cosmic.as_ref() else {
            continue;
        };
        match action {
            "close" => mgr.close(ch),
            "minimize" => {
                if toggle.unwrap_or(true) {
                    mgr.set_minimized(ch)
                } else {
                    mgr.unset_minimized(ch)
                }
            }
            "maximize" => {
                if toggle.unwrap_or(true) {
                    mgr.set_maximized(ch)
                } else {
                    mgr.unset_maximized(ch)
                }
            }
            "fullscreen" => {
                if toggle.unwrap_or(true) {
                    let out = st
                        .window_output_id(w)
                        .and_then(|id| st.outputs.iter().find(|(o, _)| o.id() == id))
                        .map(|(o, _)| o);
                    mgr.set_fullscreen(ch, out)
                } else {
                    mgr.unset_fullscreen(ch)
                }
            }
            "sticky" => {
                if toggle.unwrap_or(true) {
                    mgr.set_sticky(ch)
                } else {
                    mgr.unset_sticky(ch)
                }
            }
            _ => unreachable!(),
        }
    }
    conn.flush()?;
    Ok(0)
}

fn cmd_move_to(ws: &str, out: &str, rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let q = Query::parse(&rest.join(" "))?;
    let (conn, mut queue, mut st) = state::connect(Duration::ZERO)?;

    let mut matched = select(&st, &q);
    // Wait one more round only when the query depends on deferred fields; string
    // queries (app_id/title/id) arrive immediately, so no match really means no
    // match and waiting 150ms would be wasted.
    if matched.is_empty() && q.uses_deferred_state() {
        state::sync_more(&mut queue, &mut st, Duration::from_millis(250))?;
        matched = select(&st, &q);
    }

    let mgr = st
        .toplevel_manager
        .clone()
        .ok_or(i18n::m().no_toplevel_manager)?;

    let output = st
        .outputs
        .iter()
        .find(|(o, _)| st.output_name(&o.id()) == Some(out))
        .map(|(o, _)| o)
        .ok_or_else(|| i18n::fill1(i18n::m().no_output, out))?;

    let workspace = st
        .workspaces
        .values()
        .find(|e| e.name.as_deref() == Some(ws))
        .map(|e| &e.handle)
        .ok_or_else(|| i18n::fill1(i18n::m().no_workspace, ws))?;

    if matched.is_empty() {
        eprintln!("{}", i18n::m().no_match);
        return Ok(10);
    }
    for &i in matched.iter().rev() {
        let w = &st.windows[i];
        if let Some(ch) = w.cosmic.as_ref() {
            mgr.move_to_ext_workspace(ch, workspace, output);
        }
    }
    conn.flush()?;
    Ok(0)
}

/// Window indices matching the query.
fn select(st: &State, q: &Query) -> Vec<usize> {
    st.windows
        .iter()
        .enumerate()
        .filter(|(_, w)| !w.closed && q.matches(st, w))
        .map(|(i, _)| i)
        .collect()
}

/// Cycle focus through the matching windows of one group.
///
/// No daemon and no state file are needed: the currently focused window is
/// itself in the matching set (via `is_active`), so we simply skip past it.
/// Upstream reaches the same effect with a pidfile plus `SIGUSR1`/`SIGUSR2`
/// and a timeout window; the stateless version is simpler and more reliable.
///
/// `is_active` comes from COSMIC's deferred batch, so this command must wait.
fn cmd_cycle(rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let mut backward = false;
    let mut parts: Vec<String> = Vec::new();
    for a in rest {
        match a.as_str() {
            "-b" | "--backward" => backward = true,
            _ => parts.push(a.clone()),
        }
    }
    let q = Query::parse(&parts.join(" "))?;
    let (conn, _queue, st) = state::connect(Duration::from_millis(250))?;

    let matched = select(&st, &q);
    if matched.is_empty() {
        eprintln!("{}", i18n::m().no_match);
        return Ok(10);
    }

    let n = matched.len();
    // Where the focused window sits in the list; if focus is outside this group,
    // start from the head (or the tail when going backward).
    let cur = matched
        .iter()
        .position(|&i| st.windows[i].has(ST_ACTIVATED));
    let next = match (cur, backward) {
        (Some(p), false) => (p + 1) % n,
        (Some(p), true) => (p + n - 1) % n,
        (None, false) => 0,
        (None, true) => n - 1,
    };

    let mgr = st
        .toplevel_manager
        .clone()
        .ok_or(i18n::m().no_toplevel_manager)?;
    let seat = st.seat.clone().ok_or(i18n::m().no_seat)?;
    let landed = matched[next];
    if let Some(ch) = st.windows[landed].cosmic.clone() {
        mgr.activate(&ch, &seat);
    }
    // Keep the MRU list in sync, so a later `activate` returns to whichever
    // window cycling left us on.
    if let Some(id) = ident(&st, landed) {
        let mut hist = load_history();
        touch_history(&mut hist, &id);
        save_history(&hist);
    }
    conn.flush()?;
    Ok(0)
}

fn cmd_interfaces() -> Result<i32, Box<dyn Error>> {
    let (_conn, _queue, st) = state::connect(Duration::ZERO)?;
    let mut list = st.globals.clone();
    list.sort();
    for (name, ver) in list {
        println!("{name}@{ver}");
    }
    Ok(0)
}

// ------------------------------------------------------------------ JSON

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn jstr(s: &str) -> String {
    format!("\"{}\"", esc(s))
}

fn jobj_or_null(s: &str) -> String {
    if s.is_empty() {
        "null".to_string()
    } else {
        jstr(s)
    }
}

/// State fields: emit null until the state event arrives, so that "unknown" is
/// never reported as false.
fn jb(known: bool, v: bool) -> String {
    if known {
        v.to_string()
    } else {
        "null".to_string()
    }
}

fn window_json(st: &State, w: &Window, active_app: Option<&str>, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let id = w.identifier.as_deref().unwrap_or("");
    let app = w.app_id.as_deref().unwrap_or("");
    let title = w.title.as_deref().unwrap_or("");
    let active = w.has(ST_ACTIVATED);

    let ws = w.workspace.as_ref();
    let ws_name = ws.and_then(|i| st.workspace_name(i)).unwrap_or("");
    let ws_visible = ws.map(|i| st.workspace_is_visible(i)).unwrap_or(false);
    let ws_bits = ws
        .and_then(|i| st.workspace_state.get(i))
        .copied()
        .unwrap_or(0);
    let ws_focus = ws.is_some() && ws == st.active_workspace().as_ref();

    let out_id = st.window_output_id(w);
    let out_name = out_id
        .as_ref()
        .and_then(|i| st.output_name(i))
        .unwrap_or("");
    let out_focus = out_id.is_some() && out_id == st.active_output();

    let geom = match &w.geometry {
        Some(g) => {
            let gname = g
                .output
                .as_ref()
                .and_then(|i| st.output_name(i))
                .unwrap_or("");
            format!(
                "{{\n{pad}    \"output\": {},\n{pad}    \"x\": {},\n{pad}    \"y\": {},\n{pad}    \"width\": {},\n{pad}    \"height\": {}\n{pad}  }}",
                jobj_or_null(gname),
                g.x,
                g.y,
                g.width,
                g.height
            )
        }
        None => "null".to_string(),
    };

    let ws_json = if ws.is_none() {
        "null".to_string()
    } else {
        format!(
            "{{\n{pad}    \"name\": {},\n{pad}    \"is_visible\": {},\n{pad}    \"has_focus\": {}\n{pad}  }}",
            jobj_or_null(ws_name),
            ws_visible,
            ws_focus
        )
    };
    let out_json = if out_name.is_empty() {
        "null".to_string()
    } else {
        format!(
            "{{\n{pad}    \"name\": {},\n{pad}    \"has_focus\": {}\n{pad}  }}",
            jstr(out_name),
            out_focus
        )
    };

    let known = w.state_known;
    let _ = ws_bits;
    let _ = (
        ST_MAXIMIZED,
        ST_MINIMIZED,
        ST_FULLSCREEN,
        ST_STICKY,
        WS_ACTIVE,
    );

    format!(
        "{pad}{{\n\
         {pad}  \"id\": {id},\n\
         {pad}  \"app_id\": {app},\n\
         {pad}  \"title\": {title},\n\
         {pad}  \"is_active\": {active},\n\
         {pad}  \"is_active_app_id\": {active_app_id},\n\
         {pad}  \"is_maximized\": {max},\n\
         {pad}  \"is_minimized\": {min},\n\
         {pad}  \"is_fullscreen\": {full},\n\
         {pad}  \"is_sticky\": {sticky},\n\
         {pad}  \"workspace\": {ws_json},\n\
         {pad}  \"output\": {out_json},\n\
         {pad}  \"geometry\": {geom}\n\
         {pad}}}",
        pad = pad,
        id = jstr(id),
        app = jstr(app),
        title = jstr(title),
        active = jb(known, active),
        active_app_id = jb(
            known,
            active_app
                .map(|a| !app.is_empty() && a == app)
                .unwrap_or(false)
        ),
        max = jb(known, w.has(ST_MAXIMIZED)),
        min = jb(known, w.has(ST_MINIMIZED)),
        full = jb(known, w.has(ST_FULLSCREEN)),
        sticky = jb(known, w.has(ST_STICKY)),
        ws_json = ws_json,
        out_json = out_json,
        geom = geom,
    )
}

// ------------------------------------------------------------------ input injection
//
// Implemented via the kernel uinput interface (see src/input.rs). Mouse and
// keyboard both go this way because Wayland has no pointer-injection protocol
// and COSMIC exposes no virtual-pointer.
//
// enter/leave/over are not injectable events: the compositor generates them by
// hit-testing the pointer position. Calling `mousemove` to place the pointer in
// or out of a window is what produces enter / leave / motion.

/// Logical screen size in pixels, used as the absolute pointer range.
fn screen_size() -> Result<(i32, i32), Box<dyn Error>> {
    let (_conn, _queue, st) = state::connect(Duration::from_millis(250))?;
    let s = st.logical_screen().ok_or(i18n::m().no_screen)?;
    if std::env::var_os("CWCTRL_DEBUG").is_some() {
        eprintln!(
            "debug: output_mode={:?} scale={:?} -> {:?}",
            st.output_mode, st.output_scale, s
        );
    }
    Ok(s)
}

enum KeyMode {
    Down,
    Up,
    Tap,
}

fn parse_i32(s: &str) -> Result<i32, Box<dyn Error>> {
    s.parse()
        .map_err(|_| i18n::fill1(i18n::m().bad_number, s).into())
}

/// Parse one of the shared move options (`--from`, `--duration`, `--ease`).
/// Returns how many arguments were consumed, or `None` if `rest[i]` is not one.
fn take_move_opt(
    rest: &[String],
    i: usize,
    from: &mut Option<(i32, i32)>,
    duration: &mut Option<u64>,
    ease: &mut input::Ease,
) -> Result<Option<usize>, Box<dyn Error>> {
    match rest[i].as_str() {
        "--from" => {
            let x = rest.get(i + 1).ok_or(i18n::m().from_arity)?;
            let y = rest.get(i + 2).ok_or(i18n::m().from_arity)?;
            *from = Some((parse_i32(x)?, parse_i32(y)?));
            Ok(Some(3))
        }
        "--duration" | "--ms" | "-d" => {
            let v = rest.get(i + 1).ok_or(i18n::m().duration_arity)?;
            *duration = Some(
                v.parse()
                    .map_err(|_| i18n::fill1(i18n::m().bad_number, v))?,
            );
            Ok(Some(2))
        }
        "--ease" => {
            let v = rest.get(i + 1).ok_or(i18n::m().ease_arity)?;
            *ease = input::Ease::parse(v).ok_or_else(|| i18n::fill1(i18n::m().bad_ease, v))?;
            Ok(Some(2))
        }
        _ => Ok(None),
    }
}

fn cmd_mousemove(rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let mut relative = false;
    let mut nums: Vec<i32> = Vec::new();
    let mut from: Option<(i32, i32)> = None;
    let mut duration: Option<u64> = None;
    let mut ease = input::Ease::InOut;
    let mut i = 0;
    while i < rest.len() {
        if let Some(n) = take_move_opt(rest, i, &mut from, &mut duration, &mut ease)? {
            i += n;
            continue;
        }
        match rest[i].as_str() {
            "-r" | "--relative" => relative = true,
            other => nums.push(parse_i32(other)?),
        }
        i += 1;
    }
    if nums.len() != 2 {
        return Err(i18n::m().mousemove_arity.into());
    }
    if relative {
        let m = Mouse::create(1, 1)?;
        match duration {
            Some(ms) => m.move_smooth_rel(nums[0], nums[1], ms, ease),
            None => m.move_rel(nums[0], nums[1]),
        }
    } else {
        let (sw, sh) = screen_size()?;
        let m = Mouse::create(sw, sh)?;
        match from {
            Some((fx, fy)) => m.move_smooth_abs(
                (fx, fy),
                (nums[0], nums[1]),
                sw,
                sh,
                duration.unwrap_or(DEFAULT_DURATION_MS),
                ease,
            ),
            None => m.move_abs(nums[0], nums[1], sw, sh),
        }
    }
    Ok(0)
}

fn cmd_mouse_button(down: bool, rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let name = rest
        .first()
        .map(String::as_str)
        .ok_or(i18n::m().button_required)?;
    let btn = Button::parse(name).ok_or_else(|| i18n::fill1(i18n::m().bad_button, name))?;
    Mouse::create(1, 1)?.button(btn, down);
    Ok(0)
}

fn cmd_click(rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let mut btn = Button::Left;
    let mut count: u32 = 1;
    let mut at: Option<(i32, i32)> = None;
    let mut from: Option<(i32, i32)> = None;
    let mut duration: Option<u64> = None;
    let mut ease = input::Ease::InOut;
    let mut i = 0;
    while i < rest.len() {
        if let Some(n) = take_move_opt(rest, i, &mut from, &mut duration, &mut ease)? {
            i += n;
            continue;
        }
        match rest[i].as_str() {
            "--at" | "-a" => {
                let x = rest.get(i + 1).ok_or(i18n::m().click_at_arity)?;
                let y = rest.get(i + 2).ok_or(i18n::m().click_at_arity)?;
                at = Some((parse_i32(x)?, parse_i32(y)?));
                i += 3;
            }
            "--count" | "-c" => {
                let n = rest.get(i + 1).ok_or(i18n::m().click_count_arity)?;
                count = n
                    .parse()
                    .map_err(|_| i18n::fill1(i18n::m().bad_number, n))?;
                i += 2;
            }
            other => {
                btn =
                    Button::parse(other).ok_or_else(|| i18n::fill1(i18n::m().bad_button, other))?;
                i += 1;
            }
        }
    }

    // Only query the screen size when positioning is needed; a plain click can
    // use a 1x1 placeholder range.
    let (sw, sh) = match at {
        Some(_) => screen_size()?,
        None => (1, 1),
    };
    let m = Mouse::create(sw, sh)?;
    if let Some((x, y)) = at {
        match from {
            Some((fx, fy)) => {
                m.move_smooth_abs(
                    (fx, fy),
                    (x, y),
                    sw,
                    sh,
                    duration.unwrap_or(DEFAULT_DURATION_MS),
                    ease,
                );
                std::thread::sleep(Duration::from_millis(80));
            }
            None => {
                m.move_abs(x, y, sw, sh);
                std::thread::sleep(Duration::from_millis(40));
            }
        }
    }
    m.click(btn, count);
    Ok(0)
}

fn cmd_scroll(rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let name = rest
        .first()
        .map(String::as_str)
        .ok_or(i18n::m().scroll_arity)?;
    let axis = ScrollAxis::parse(name).ok_or_else(|| i18n::fill1(i18n::m().bad_axis, name))?;
    let n = match rest.get(1) {
        Some(s) => parse_i32(s)?,
        None => 1,
    };
    Mouse::create(1, 1)?.scroll(axis, n);
    Ok(0)
}

fn cmd_key(mode: KeyMode, rest: &[String]) -> Result<i32, Box<dyn Error>> {
    let spec = rest
        .first()
        .map(String::as_str)
        .ok_or(i18n::m().key_required)?;
    let (mods, code) =
        input::parse_chord(spec).ok_or_else(|| i18n::fill1(i18n::m().bad_key, spec))?;
    let kb = Keyboard::create()?;
    match mode {
        KeyMode::Tap => kb.chord(&mods, code),
        KeyMode::Down => {
            for m in &mods {
                kb.key(*m, true);
            }
            kb.key(code, true);
        }
        KeyMode::Up => {
            kb.key(code, false);
            for m in mods.iter().rev() {
                kb.key(*m, false);
            }
        }
    }
    Ok(0)
}
