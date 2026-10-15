//! Wayland state collection: enumerate toplevels, workspaces and outputs, and
//! keep the proxies needed to send control requests.
//!
//! Key point: as of v2, COSMIC's `zcosmic_toplevel_info_v1` no longer emits the
//! `toplevel` event nor `title`/`app_id`. You must first obtain an
//! `ext_foreign_toplevel_handle_v1` from `ext_foreign_toplevel_list_v1`, then
//! call `get_cosmic_toplevel()` to exchange it for the enhanced handle that
//! carries state / geometry / workspace.

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use crate::i18n;
use crate::protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1::{Event as CosmicHandleEvent, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, Event as CosmicInfoEvent, ZcosmicToplevelInfoV1},
};
use crate::protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1::{
    Event as ManagerEvent, ZcosmicToplevelManagerV1,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::ObjectId,
    event_created_child,
    protocol::{wl_output, wl_registry, wl_seat},
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{Event as ForeignHandleEvent, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, Event as ForeignListEvent, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{Event as GroupEvent, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{Event as WorkspaceEvent, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, Event as WsManagerEvent, ExtWorkspaceManagerV1},
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{Event as XdgOutputEvent, ZxdgOutputV1},
};

// zcosmic_toplevel_handle_v1::State (same values as upstream cosmic-ext-window-helper)
pub const ST_MAXIMIZED: u32 = 0;
pub const ST_MINIMIZED: u32 = 1;
pub const ST_ACTIVATED: u32 = 2;
pub const ST_FULLSCREEN: u32 = 3;
pub const ST_STICKY: u32 = 4;

// ext_workspace_handle_v1::State
pub const WS_ACTIVE: u32 = 1;

#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub output: Option<ObjectId>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Default)]
pub struct Window {
    pub foreign: Option<ExtForeignToplevelHandleV1>,
    pub cosmic: Option<ZcosmicToplevelHandleV1>,
    pub identifier: Option<String>,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub states: Vec<u32>,
    pub geometry: Option<Geometry>,
    pub workspace: Option<ObjectId>,
    pub outputs: Vec<ObjectId>,
    pub closed: bool,
    /// Whether the `zcosmic_toplevel_handle_v1` state event has been received.
    /// COSMIC pushes state/geometry in the same ~150ms deferred batch, so while
    /// this is false, `states`/`is_active` and friends are not trustworthy.
    pub state_known: bool,
}

impl Window {
    #[inline]
    pub fn has(&self, bit: u32) -> bool {
        self.states.contains(&bit)
    }
}

pub struct WorkspaceEntry {
    pub handle: ExtWorkspaceHandleV1,
    pub name: Option<String>,
}

#[derive(Default)]
pub struct State {
    pub windows: Vec<Window>,
    by_foreign: HashMap<ObjectId, usize>,
    by_cosmic: HashMap<ObjectId, usize>,

    /// Every global the compositor exposes, as (interface name, version).
    pub globals: Vec<(String, u32)>,

    /// wl_output, in registry order.
    pub outputs: Vec<(wl_output::WlOutput, Option<String>)>,
    pub output_names: HashMap<ObjectId, String>,
    /// Physical mode and scale per output, used to derive the logical pixel size
    /// (needed by the absolute pointer).
    pub output_mode: HashMap<ObjectId, (i32, i32)>,
    pub output_scale: HashMap<ObjectId, i32>,
    /// Logical size from `xdg_output.logical_size` (authoritative, preferred).
    pub output_logical: HashMap<ObjectId, (i32, i32)>,
    by_xdg_output: HashMap<ObjectId, ObjectId>,

    pub workspaces: HashMap<ObjectId, WorkspaceEntry>,
    pub workspace_state: HashMap<ObjectId, u32>,

    pub seat: Option<wl_seat::WlSeat>,
    pub list: Option<ExtForeignToplevelListV1>,
    pub workspace_manager: Option<ExtWorkspaceManagerV1>,
    pub toplevel_info: Option<ZcosmicToplevelInfoV1>,
    pub toplevel_manager: Option<ZcosmicToplevelManagerV1>,
    pub xdg_output_manager: Option<ZxdgOutputManagerV1>,
}

// `as_chunks` needs Rust 1.88 while this project's MSRV is 1.86 (set by wayland-protocols).
#[allow(clippy::chunks_exact_to_as_chunks)]
fn parse_states(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

impl State {
    pub fn active(&self) -> Option<usize> {
        self.windows
            .iter()
            .position(|w| !w.closed && w.has(ST_ACTIVATED))
    }

    pub fn active_window(&self) -> Option<&Window> {
        self.windows.get(self.active()?)
    }

    pub fn active_output(&self) -> Option<ObjectId> {
        let a = self.active()?;
        self.windows[a]
            .geometry
            .as_ref()
            .and_then(|g| g.output.clone())
            .or_else(|| self.windows[a].outputs.first().cloned())
    }

    pub fn active_workspace(&self) -> Option<ObjectId> {
        self.windows.get(self.active()?)?.workspace.clone()
    }

    /// Pair each `ext_foreign_toplevel_handle_v1` with its `zcosmic_toplevel_handle_v1`.
    pub fn associate(&mut self, qh: &QueueHandle<Self>) {
        let Some(info) = self.toplevel_info.clone() else {
            return;
        };
        let tasks: Vec<(usize, ExtForeignToplevelHandleV1)> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.cosmic.is_none() && !w.closed)
            .filter_map(|(i, w)| w.foreign.clone().map(|f| (i, f)))
            .collect();
        for (idx, foreign) in tasks {
            let handle = info.get_cosmic_toplevel(&foreign, qh, ());
            self.by_cosmic.insert(handle.id(), idx);
            self.windows[idx].cosmic = Some(handle);
        }
    }

    pub fn output_name(&self, id: &ObjectId) -> Option<&str> {
        self.output_names.get(id).map(|s| s.as_str())
    }

    pub fn workspace_name(&self, id: &ObjectId) -> Option<&str> {
        self.workspaces.get(id).and_then(|e| e.name.as_deref())
    }

    pub fn workspace_is_visible(&self, id: &ObjectId) -> bool {
        self.workspace_state
            .get(id)
            .map(|s| s & WS_ACTIVE != 0)
            .unwrap_or(false)
    }

    /// Output name of a window (geometry first, falling back to output_enter).
    pub fn window_output(&self, w: &Window) -> Option<&str> {
        if let Some(o) = self.window_output_id(w) {
            return self.output_name(&o);
        }
        None
    }

    pub fn window_output_id(&self, w: &Window) -> Option<ObjectId> {
        w.geometry
            .as_ref()
            .and_then(|g| g.output.clone())
            .or_else(|| w.outputs.first().cloned())
    }

    /// Logical screen size in pixels. Prefers `xdg_output.logical_size`, falling
    /// back to `wl_output.mode / scale`. Used as the uinput absolute pointer range.
    ///
    /// Note: this is the size of a single output and only the first one is used;
    /// with multiple outputs the absolute coordinates may need an offset, since
    /// window geometry is relative to each output.
    pub fn logical_screen(&self) -> Option<(i32, i32)> {
        if let Some(size) = self.output_logical.values().next() {
            return Some(*size);
        }
        let (id, (w, h)) = self.output_mode.iter().next()?;
        let scale = self.output_scale.get(id).copied().unwrap_or(1).max(1);
        Some((w / scale, h / scale))
    }
}

/// Create an `xdg_output` for every bound `wl_output` to obtain the **logical**
/// size. COSMIC's `wl_output.mode` can be inaccurate, so this is authoritative.
fn setup_xdg_outputs(state: &mut State, qh: &QueueHandle<State>) {
    let Some(mgr) = state.xdg_output_manager.clone() else {
        return;
    };
    let outs: Vec<wl_output::WlOutput> = state
        .outputs
        .iter()
        .map(|(o, _)| o.clone())
        .filter(|o| !state.by_xdg_output.values().any(|v| *v == o.id()))
        .collect();
    for out in outs {
        let xo = mgr.get_xdg_output(&out, qh, ());
        state.by_xdg_output.insert(xo.id(), out.id());
    }
}

/// Connect to the compositor and collect state.
///
/// `wait` is how much longer to wait for geometry/workspace: COSMIC pushes
/// toplevel info as a **deferred batch** (measured ~150ms), whereas
/// title/app_id/identifier arrive immediately. Callers that only need
/// app_id/title for querying and controlling should pass `Duration::ZERO`.
pub fn connect(
    wait: Duration,
) -> Result<(Connection, EventQueue<State>, State), Box<dyn std::error::Error>> {
    let conn =
        Connection::connect_to_env().map_err(|e| i18n::fill1(i18n::m().connect_failed, e))?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let display = conn.display();
    let _registry = display.get_registry(&qh, ());

    let mut state = State::default();
    queue.roundtrip(&mut state)?; // globals, then bind what we need
    setup_xdg_outputs(&mut state, &qh);
    queue.roundtrip(&mut state)?; // xdg_output logical_size + first toplevel batch
    setup_xdg_outputs(&mut state, &qh); // catch outputs that appear in the second round
    queue.roundtrip(&mut state)?;
    sync_more(&mut queue, &mut state, wait)?;
    Ok((conn, queue, state))
}

/// Keep collecting on an existing connection, waiting at most `wait` longer.
/// `associate()` runs first to create the zcosmic handles immediately (control
/// requests need no server reply); only then do we wait for the deferred
/// state/geometry batch.
pub fn sync_more(
    queue: &mut EventQueue<State>,
    state: &mut State,
    wait: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    state.associate(&queue.handle());
    queue.roundtrip(state)?;
    if wait.is_zero() || state.windows.is_empty() {
        return Ok(());
    }
    let deadline = Instant::now() + wait;
    // Measured: the data arrives after ~150ms; a 1ms poll exits the moment it does.
    while Instant::now() < deadline {
        if state.windows.iter().all(|w| w.closed || w.state_known) {
            break;
        }
        thread::sleep(Duration::from_millis(1));
        queue.roundtrip(state)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- Dispatch

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        state.globals.push((interface.clone(), version));
        match &*interface {
            "wl_output" => {
                let out = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
                state.outputs.push((out, None));
            }
            "wl_seat" => {
                let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(9), qh, ());
                state.seat = Some(seat);
            }
            "ext_foreign_toplevel_list_v1" => {
                let list =
                    registry.bind::<ExtForeignToplevelListV1, _, _>(name, version.min(1), qh, ());
                state.list = Some(list);
            }
            "ext_workspace_manager_v1" => {
                let mgr =
                    registry.bind::<ExtWorkspaceManagerV1, _, _>(name, version.min(1), qh, ());
                state.workspace_manager = Some(mgr);
            }
            "zxdg_output_manager_v1" => {
                let mgr = registry.bind::<ZxdgOutputManagerV1, _, _>(name, version.min(3), qh, ());
                state.xdg_output_manager = Some(mgr);
            }
            "zcosmic_toplevel_info_v1" => {
                let info =
                    registry.bind::<ZcosmicToplevelInfoV1, _, _>(name, version.min(3), qh, ());
                state.toplevel_info = Some(info);
            }
            "zcosmic_toplevel_manager_v1" => {
                let mgr =
                    registry.bind::<ZcosmicToplevelManagerV1, _, _>(name, version.min(4), qh, ());
                state.toplevel_manager = Some(mgr);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = &event {
            let id = output.id();
            if let Some(slot) = state.outputs.iter_mut().find(|(o, _)| o.id() == id) {
                slot.1 = Some(name.clone());
            }
            state.output_names.insert(id, name.clone());
        }
        match event {
            wl_output::Event::Mode { width, height, .. } => {
                state.output_mode.insert(output.id(), (width, height));
            }
            wl_output::Event::Scale { factor } => {
                state.output_scale.insert(output.id(), factor);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZxdgOutputManagerV1,
        _: <ZxdgOutputManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgOutputV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZxdgOutputV1,
        event: XdgOutputEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let XdgOutputEvent::LogicalSize { width, height } = event {
            if let Some(out) = state.by_xdg_output.get(&handle.id()).cloned() {
                state.output_logical.insert(out, (width, height));
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// ---- ext_foreign_toplevel_list_v1 ----

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ForeignListEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ForeignListEvent::Toplevel { toplevel } = event {
            let idx = state.windows.len();
            state.by_foreign.insert(toplevel.id(), idx);
            state.windows.push(Window {
                foreign: Some(toplevel),
                ..Default::default()
            });
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ForeignHandleEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(&idx) = state.by_foreign.get(&handle.id()) else {
            return;
        };
        match event {
            ForeignHandleEvent::Identifier { identifier } => {
                state.windows[idx].identifier = Some(identifier)
            }
            ForeignHandleEvent::Title { title } => state.windows[idx].title = Some(title),
            ForeignHandleEvent::AppId { app_id } => state.windows[idx].app_id = Some(app_id),
            ForeignHandleEvent::Closed => state.windows[idx].closed = true,
            _ => {}
        }
    }
}

// ---- zcosmic_toplevel_info_v1 ----

impl Dispatch<ZcosmicToplevelInfoV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZcosmicToplevelInfoV1,
        event: CosmicInfoEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The Toplevel event is deprecated since v2; we pair handles via
        // associate() instead. The opcode mapping must still exist, otherwise
        // wayland-client panics on the unexpected new_id.
        let _ = matches!(event, CosmicInfoEvent::Toplevel { .. });
    }

    event_created_child!(State, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZcosmicToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZcosmicToplevelHandleV1,
        event: CosmicHandleEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(&idx) = state.by_cosmic.get(&handle.id()) else {
            return;
        };
        match event {
            CosmicHandleEvent::State { state: bytes } => {
                state.windows[idx].states = parse_states(&bytes);
                state.windows[idx].state_known = true;
            }
            CosmicHandleEvent::Geometry {
                output,
                x,
                y,
                width,
                height,
            } => {
                state.windows[idx].geometry = Some(Geometry {
                    output: Some(output.id()),
                    x,
                    y,
                    width,
                    height,
                })
            }
            CosmicHandleEvent::OutputEnter { output } => {
                let id = output.id();
                if !state.windows[idx].outputs.contains(&id) {
                    state.windows[idx].outputs.push(id);
                }
            }
            CosmicHandleEvent::OutputLeave { output } => {
                let id = output.id();
                state.windows[idx].outputs.retain(|o| *o != id);
            }
            CosmicHandleEvent::ExtWorkspaceEnter { workspace } => {
                state.windows[idx].workspace = Some(workspace.id())
            }
            CosmicHandleEvent::ExtWorkspaceLeave { .. } => state.windows[idx].workspace = None,
            // Deprecated in v2/v3, kept as a fallback.
            CosmicHandleEvent::Title { title } if state.windows[idx].title.is_none() => {
                state.windows[idx].title = Some(title)
            }
            CosmicHandleEvent::AppId { app_id } if state.windows[idx].app_id.is_none() => {
                state.windows[idx].app_id = Some(app_id)
            }
            CosmicHandleEvent::Closed => state.windows[idx].closed = true,
            _ => {}
        }
    }
}

// ---- zcosmic_toplevel_manager_v1 ----

impl Dispatch<ZcosmicToplevelManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZcosmicToplevelManagerV1,
        _: ManagerEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// ---- ext_workspace_manager_v1 ----

impl Dispatch<ExtWorkspaceManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: WsManagerEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let WsManagerEvent::Workspace { workspace } = event {
            state
                .workspaces
                .entry(workspace.id())
                .or_insert_with(|| WorkspaceEntry {
                    handle: workspace,
                    name: None,
                });
        }
    }

    event_created_child!(State, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtWorkspaceGroupHandleV1,
        event: GroupEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let GroupEvent::WorkspaceEnter { workspace } = event {
            state
                .workspaces
                .entry(workspace.id())
                .or_insert_with(|| WorkspaceEntry {
                    handle: workspace,
                    name: None,
                });
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        workspace: &ExtWorkspaceHandleV1,
        event: WorkspaceEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            WorkspaceEvent::Name { name } => {
                let e = state
                    .workspaces
                    .entry(workspace.id())
                    .or_insert_with(|| WorkspaceEntry {
                        handle: workspace.clone(),
                        name: None,
                    });
                e.name = Some(name);
            }
            WorkspaceEvent::State { state: ws } => {
                let bits = ws.into_result().map(|s| s.bits()).unwrap_or(0);
                state.workspace_state.insert(workspace.id(), bits);
            }
            _ => {}
        }
    }
}
