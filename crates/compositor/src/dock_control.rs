//! The Dock's control (`silica-dock-v1`, silica-os/compositor#10): the Dock sees every window
//! and layer surface as a node, hides them, places them anywhere at any scale, and receives
//! the gestures and hot corners, while the compositor keeps drawing the live windows. The
//! compositor offers primitives; spaces, Mission Control, App Windows, Show Desktop, the hot
//! corner actions and the minimize effect are the Dock's (silica-os/dock).
//!
//! Only the Dock sees the global (a shell component), and one client at a time holds the
//! control. Everything the control did ends when it goes.

use compositor_render::renderer::SilicaTexture;
use silica_protocols::dock::server::silica_dock_control_v1::{self, SilicaDockControlV1};
use silica_protocols::dock::server::silica_dock_manager_v1::{self, SilicaDockManagerV1};
use silica_protocols::dock::server::silica_dock_node_v1::{self, SilicaDockNodeV1};
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::{LayerSurface, Window, layer_map_for_output};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::Physical;
use smithay::utils::{Logical, Point, Rectangle};
use tracing::debug;

use crate::state::State;

/// The version of `silica_dock_manager_v1` the compositor offers.
pub const VERSION: u32 = 2;

/// Most placements one node can have.
const MAX_PLACEMENTS: usize = 8;

pub struct DockManagerState;

impl DockManagerState {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, SilicaDockManagerV1, _>(
            VERSION,
            DockGlobal {
                filter: Box::new(crate::shell::is_shell_client),
            },
        );
        Self
    }
}

pub struct DockGlobal {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync>,
}

/// What a node shows, as the Dock asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Inherit,
    Hidden,
    Shown,
}

/// Where a node is drawn: one entry of its placements.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub index: u32,
    /// `above_top`: above the Top layer (inside the Dock's spaces bar), else in the window
    /// stack.
    pub above_top: bool,
    /// The frame's top-left corner, desktop coordinates.
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub opacity: f64,
    /// A clip rectangle (a positive size), desktop coordinates.
    pub clip: Option<Rectangle<f64, Logical>>,
}

/// The genie of a node, shaped by the Dock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Warp {
    pub target: Rectangle<f64, Logical>,
    pub bend: f64,
    pub top: f64,
    pub bottom: f64,
}

/// A request waiting for `commit`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    Visibility(Visibility),
    Place(Placement),
    Remove(u32),
    Warp(Warp),
    ClearWarp,
}

/// What the Dock decided for a node, applied.
#[derive(Debug, Clone, Default)]
pub struct Applied {
    pub visibility: Visibility,
    pub places: Vec<Placement>,
    pub warp: Option<Warp>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Window(Window),
    Layer(LayerSurface),
    /// The menu bar strip (version 2): the Dock fades it and moves it with a space.
    MenuBar,
}

/// What was last sent about a node.
#[derive(Debug, Clone, PartialEq, Default)]
struct Sent {
    frame: Option<Rectangle<i32, Logical>>,
    state: Option<u32>,
    output: Option<String>,
}

pub struct Node {
    id: u64,
    resource: SilicaDockNodeV1,
    pub target: Target,
    sent: Sent,
    pending: Vec<Pending>,
    pub applied: Applied,
}

/// A window the frame draws specially: placed, warped, or shown while minimized.
#[derive(Debug, Clone)]
pub struct Drawn {
    pub window: Window,
    /// Where it is when it is not in the space (minimized): its location.
    pub location: Option<Point<i32, Logical>>,
    pub places: Vec<Placement>,
    pub warp: Option<Warp>,
    pub shown: bool,
    /// Not drawn at all until the Dock says: a window just restored, which the Dock warps out
    /// of its cell.
    pub veil: bool,
}

/// How long the compositor keeps a window just minimized on screen, or a restored one off
/// it, for the Dock to take it over with its first commit.
const HOLD: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Debug, Clone)]
struct Hold {
    window: Window,
    /// A restored window kept off the screen (else a minimized one kept on it).
    veil: bool,
    since: std::time::Instant,
}

/// A window's picture, taken when the Dock began to warp it (or minimized it): the genie
/// deforms it frame by frame.
pub struct Snapshot {
    pub id: Id,
    pub texture: SilicaTexture,
    /// Where the picture is in the output.
    pub bbox: Rectangle<i32, Physical>,
    /// The window frame (without shadow) in the output.
    pub frame: Rectangle<f64, Physical>,
    commit: std::cell::Cell<CommitCounter>,
}

impl Snapshot {
    /// The commit counter, bumped: the shape changes on every frame.
    pub fn next_commit(&self) -> CommitCounter {
        let mut commit = self.commit.get();
        commit.increment();
        self.commit.set(commit);
        commit
    }
}

struct Bound {
    resource: SilicaDockControlV1,
    nodes: Vec<Node>,
    next_id: u64,
    swipe_fingers: u32,
}

#[derive(Default)]
pub struct DockControl {
    bound: Option<Bound>,
    /// What the frame draws specially, rebuilt on every commit and every change of the nodes.
    pub drawn: Vec<Drawn>,
    snapshots: Vec<(Window, Snapshot)>,
    holds: Vec<Hold>,
}

impl DockControl {
    /// What the Dock applied to the layer surface `surface`, if it is one of its nodes.
    pub fn layer_applied(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> Option<&Applied> {
        self.bound
            .iter()
            .flat_map(|b| b.nodes.iter())
            .find(|n| matches!(&n.target, Target::Layer(l) if l.wl_surface() == surface))
            .map(|n| &n.applied)
    }

    /// What the Dock applied to the menu bar strip.
    pub fn menu_bar_applied(&self) -> Option<&Applied> {
        self.bound
            .iter()
            .flat_map(|b| b.nodes.iter())
            .find(|n| n.target == Target::MenuBar)
            .map(|n| &n.applied)
    }

    /// The layer surfaces placed above the Top layer, with each placement, in node order.
    pub fn layer_places_above_top(&self) -> Vec<(LayerSurface, Placement)> {
        self.bound
            .iter()
            .flat_map(|b| b.nodes.iter())
            .filter(|n| n.applied.visibility != Visibility::Hidden)
            .filter_map(|n| match &n.target {
                Target::Layer(l) => Some((l, &n.applied.places)),
                _ => None,
            })
            .flat_map(|(l, places)| {
                places
                    .iter()
                    .filter(|p| p.above_top)
                    .map(move |p| (l.clone(), *p))
            })
            .collect()
    }

    /// Whether a Dock holds the control.
    pub fn is_bound(&self) -> bool {
        self.bound.as_ref().is_some_and(|b| b.resource.is_alive())
    }

    /// Whether the Dock takes swipes with this many fingers.
    pub fn takes_swipe(&self, fingers: u32) -> bool {
        self.bound
            .as_ref()
            .is_some_and(|b| b.resource.is_alive() && b.swipe_fingers == fingers && fingers > 0)
    }

    /// The placements to draw, for the window.
    pub fn drawn_for(&self, window: &Window) -> Option<&Drawn> {
        self.drawn.iter().find(|d| &d.window == window)
    }

    /// The warp of a window's node, if the Dock set one.
    pub fn warp_of(&self, window: &Window) -> Option<Warp> {
        self.drawn_for(window).and_then(|d| d.warp)
    }

    /// The picture a window's genie deforms.
    pub fn snapshot_of(&self, window: &Window) -> Option<&Snapshot> {
        self.snapshots
            .iter()
            .find(|(w, _)| w == window)
            .map(|(_, s)| s)
    }
}

fn corner_value(corner: crate::config::Corner) -> u32 {
    match corner {
        crate::config::Corner::TopLeft => 0,
        crate::config::Corner::TopRight => 1,
        crate::config::Corner::BottomLeft => 2,
        crate::config::Corner::BottomRight => 3,
    }
}

impl State {
    /// Every window the Dock may know: those with an entry in the shell's list.
    fn dock_windows(&self) -> Vec<Window> {
        self.space
            .elements()
            .chain(self.minimized.iter().map(|(w, _)| w))
            .chain(self.hidden_apps.windows())
            .chain(self.dock_hidden.windows())
            .filter(|w| crate::shell::toplevel_handle(w).is_some())
            .cloned()
            .collect()
    }

    fn dock_layers(&self) -> Vec<LayerSurface> {
        let map = layer_map_for_output(&self.output);
        map.layers()
            .filter(|layer| map.layer_geometry(layer).is_some())
            .cloned()
            .collect()
    }

    /// The frame of a window, wherever it is kept: in the space or waiting to come back.
    fn dock_window_frame(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        let content = match self.space.element_geometry(window) {
            Some(geometry) => geometry,
            None => {
                let location = self
                    .minimized
                    .iter()
                    .chain(self.hidden_apps.0.iter())
                    .chain(self.dock_hidden.0.iter())
                    .find(|(w, _)| w == window)
                    .map(|(_, at)| *at)?;
                Rectangle::new(location, window.geometry().size)
            }
        };
        Some(crate::decorations::frame_of(window, content))
    }

    fn dock_window_state(&self, window: &Window) -> u32 {
        let mut state = 0;
        if self.minimized.iter().any(|(w, _)| w == window) {
            state |= 1;
        }
        if crate::windows::is_fullscreen(window) {
            state |= 2;
        }
        if self.hidden_apps.contains(window) {
            state |= 4;
        }
        if self.focused_window().as_ref() == Some(window) {
            state |= 8;
        }
        if crate::windows::is_modal(window) {
            state |= 16;
        }
        state
    }

    /// Keeps the Dock up to date with the nodes: new windows and layers are announced, gone
    /// ones closed, and changes of frame, state and output sent. Called after every batch of
    /// events.
    pub fn dock_sync(&mut self) {
        if !self.dock.is_bound() {
            return;
        }
        let windows = self.dock_windows();
        let layers = self.dock_layers();
        let output_name = self.output.name();
        // What each existing node looks like now.
        struct Seen {
            frame: Option<Rectangle<i32, Logical>>,
            state: u32,
        }
        let mut seen: Vec<(u64, Option<Seen>)> = Vec::new();
        let mut announce: Vec<Target> = Vec::new();
        if let Some(bound) = &self.dock.bound {
            for node in &bound.nodes {
                let current = match &node.target {
                    Target::Window(w) => windows.contains(w).then(|| Seen {
                        frame: self.dock_window_frame(w),
                        state: self.dock_window_state(w),
                    }),
                    Target::Layer(l) => layers.contains(l).then(|| Seen {
                        frame: layer_map_for_output(&self.output).layer_geometry(l),
                        state: 0,
                    }),
                    Target::MenuBar => self.menu_bar_frame().map(|frame| Seen {
                        frame: Some(frame),
                        state: 0,
                    }),
                };
                seen.push((node.id, current));
            }
            for window in &windows {
                if !bound
                    .nodes
                    .iter()
                    .any(|n| n.target == Target::Window(window.clone()))
                {
                    announce.push(Target::Window(window.clone()));
                }
            }
            for layer in &layers {
                if !bound
                    .nodes
                    .iter()
                    .any(|n| n.target == Target::Layer(layer.clone()))
                {
                    announce.push(Target::Layer(layer.clone()));
                }
            }
            if bound.resource.version() >= 2
                && self.menu_bar_frame().is_some()
                && !bound.nodes.iter().any(|n| n.target == Target::MenuBar)
            {
                announce.push(Target::MenuBar);
            }
        }
        let mut changed = false;
        if let Some(bound) = self.dock.bound.as_mut() {
            // Closed nodes first.
            for (id, current) in &seen {
                if current.is_none()
                    && let Some(i) = bound.nodes.iter().position(|n| n.id == *id)
                {
                    let node = bound.nodes.remove(i);
                    node.resource.closed();
                    changed = true;
                }
            }
            for (id, current) in seen {
                let (Some(current), Some(node)) =
                    (current, bound.nodes.iter_mut().find(|n| n.id == id))
                else {
                    continue;
                };
                if let Some(frame) = current.frame
                    && node.sent.frame != Some(frame)
                {
                    node.resource
                        .frame(frame.loc.x, frame.loc.y, frame.size.w, frame.size.h);
                    node.sent.frame = Some(frame);
                }
                if node.sent.state != Some(current.state)
                    && matches!(node.target, Target::Window(_))
                {
                    node.resource
                        .state(silica_dock_node_v1::State::from_bits_truncate(
                            current.state,
                        ));
                    node.sent.state = Some(current.state);
                }
            }
        }
        for target in announce {
            self.dock_announce(target, &output_name);
        }
        if changed {
            self.dock_refresh();
        }
    }

    /// Announces a window or a layer surface to the Dock.
    fn dock_announce(&mut self, target: Target, output_name: &str) {
        let (kind, key, frame, state) = match &target {
            Target::Window(w) => {
                let Some(identifier) = crate::shell::toplevel_handle(w).map(|h| h.identifier())
                else {
                    return;
                };
                (
                    0u32,
                    identifier,
                    self.dock_window_frame(w),
                    self.dock_window_state(w),
                )
            }
            Target::Layer(l) => (
                1u32,
                l.namespace().to_owned(),
                layer_map_for_output(&self.output).layer_geometry(l),
                0,
            ),
            Target::MenuBar => (2u32, "menu-bar".to_owned(), self.menu_bar_frame(), 0),
        };
        let dh = self.display_handle.clone();
        let Some(bound) = self.dock.bound.as_mut() else {
            return;
        };
        let Some(client) = bound.resource.client() else {
            return;
        };
        bound.next_id += 1;
        let id = bound.next_id;
        let Ok(resource) = client.create_resource::<SilicaDockNodeV1, u64, State>(
            &dh,
            bound.resource.version(),
            id,
        ) else {
            return;
        };
        bound.resource.node(
            &resource,
            match kind {
                0 => silica_dock_control_v1::Kind::Window,
                1 => silica_dock_control_v1::Kind::Layer,
                _ => silica_dock_control_v1::Kind::MenuBar,
            },
            key,
        );
        let mut sent = Sent::default();
        if let Some(frame) = frame {
            resource.frame(frame.loc.x, frame.loc.y, frame.size.w, frame.size.h);
            sent.frame = Some(frame);
        }
        if kind == 0 {
            resource.state(silica_dock_node_v1::State::from_bits_truncate(state));
            sent.state = Some(state);
        }
        resource.output(output_name.to_owned());
        sent.output = Some(output_name.to_owned());
        bound.nodes.push(Node {
            id,
            resource,
            target,
            sent,
            pending: Vec::new(),
            applied: Applied::default(),
        });
    }

    /// The Dock took the control: everything that exists is announced.
    fn dock_bound(&mut self) {
        self.dock_sync();
    }

    /// The Dock let go (or died): every override ends.
    pub fn dock_release(&mut self) {
        let Some(bound) = self.dock.bound.take() else {
            return;
        };
        drop(bound);
        self.dock.drawn.clear();
        self.dock.snapshots.clear();
        self.dock.holds.clear();
        for window in self
            .dock_hidden
            .0
            .iter()
            .map(|(w, _)| w.clone())
            .collect::<Vec<_>>()
        {
            self.dock_show_window(&window);
        }
        self.wake();
    }

    /// Takes a window off the screen for the Dock (another space): like an app hiding.
    fn dock_hide_window(&mut self, window: &Window) {
        if self.space.elements().all(|w| w != window) {
            return;
        }
        let location = self.space.element_location(window).unwrap_or_default();
        let was_focused = self.focused_window().as_ref() == Some(window);
        window.set_activated(false);
        self.space.unmap_elem(window);
        self.dock_hidden.0.push((window.clone(), location));
        if was_focused {
            let next = self.space.elements().last().cloned();
            self.focus_window(next);
            self.desktop_owner_fallback(None);
        }
    }

    /// Puts a window the Dock hid back, where it was.
    pub fn dock_show_window(&mut self, window: &Window) {
        let Some(i) = self.dock_hidden.0.iter().position(|(w, _)| w == window) else {
            return;
        };
        let (window, location) = self.dock_hidden.0.remove(i);
        // It kept committing while it was away: its geometry is brought up to date.
        window.on_commit();
        self.space.map_element(window, location, false);
    }

    /// A window closed: it is no longer waiting to come back.
    pub fn dock_forget_window(&mut self, window: &Window) {
        self.dock_hidden.0.retain(|(w, _)| w != window);
    }

    /// Rebuilds what the frame draws specially from the nodes.
    pub fn dock_refresh(&mut self) {
        let mut drawn = Vec::new();
        if let Some(bound) = &self.dock.bound {
            for node in &bound.nodes {
                let Target::Window(window) = &node.target else {
                    continue;
                };
                let shown = node.applied.visibility == Visibility::Shown;
                if node.applied.places.is_empty() && node.applied.warp.is_none() && !shown {
                    continue;
                }
                let in_space = self.space.elements().any(|w| w == window);
                let location = if in_space {
                    None
                } else {
                    self.minimized
                        .iter()
                        .chain(self.hidden_apps.0.iter())
                        .find(|(w, _)| w == window)
                        .map(|(_, at)| *at)
                };
                if !in_space && location.is_none() {
                    continue;
                }
                drawn.push(Drawn {
                    window: window.clone(),
                    location,
                    places: node.applied.places.clone(),
                    warp: node.applied.warp,
                    shown,
                    veil: false,
                });
            }
        }
        // Windows held for the Dock to take over.
        for hold in &self.dock.holds {
            if drawn.iter().any(|d| d.window == hold.window) {
                continue;
            }
            let in_space = self.space.elements().any(|w| *w == hold.window);
            let location = (!in_space)
                .then(|| {
                    self.minimized
                        .iter()
                        .find(|(w, _)| *w == hold.window)
                        .map(|(_, at)| *at)
                })
                .flatten();
            if in_space == hold.veil && (in_space || location.is_some()) {
                drawn.push(Drawn {
                    window: hold.window.clone(),
                    location,
                    places: Vec::new(),
                    warp: None,
                    shown: true,
                    veil: hold.veil,
                });
            }
        }
        self.dock.drawn = drawn;
        self.wake();
    }

    /// A window was just minimized or restored with a Dock in charge: it stays as it was on
    /// screen (minimized) or off it (restored) for a moment, for the Dock to start its effect
    /// on the first frame.
    pub fn dock_hold(&mut self, window: &Window, veil: bool) {
        if !self.dock.is_bound() {
            return;
        }
        self.dock.holds.retain(|h| &h.window != window);
        self.dock.holds.push(Hold {
            window: window.clone(),
            veil,
            since: std::time::Instant::now(),
        });
        self.dock_refresh();
    }

    /// Ends the holds that expired. Whether any is left (frames keep coming).
    pub fn dock_tick(&mut self) -> bool {
        if self.dock.holds.is_empty() {
            return false;
        }
        let before = self.dock.holds.len();
        self.dock.holds.retain(|h| h.since.elapsed() < HOLD);
        if self.dock.holds.len() != before {
            // The Dock never took them over: their pictures go too.
            let warped: Vec<Window> = self
                .dock
                .drawn
                .iter()
                .filter(|d| d.warp.is_some())
                .map(|d| d.window.clone())
                .collect();
            let held: Vec<Window> = self.dock.holds.iter().map(|h| h.window.clone()).collect();
            self.dock
                .snapshots
                .retain(|(w, _)| warped.contains(w) || held.contains(w));
            self.dock_refresh();
        }
        !self.dock.holds.is_empty()
    }

    /// Takes the picture a window's genie deforms, while the window is on screen.
    pub fn dock_capture(&mut self, window: &Window) {
        if self.dock.snapshots.iter().any(|(w, _)| w == window)
            || self.space.elements().all(|w| w != window)
        {
            return;
        }
        let dark = self.dark();
        let renderer = self.backend.renderer();
        let aim = Rectangle::new((0.0, 0.0).into(), (1.0, 1.0).into());
        if let Some(genie) = crate::render::snapshot_genie(
            renderer,
            &self.space,
            &self.output,
            window,
            aim,
            false,
            dark,
        ) {
            self.dock.snapshots.push((
                window.clone(),
                Snapshot {
                    id: genie.id.clone(),
                    texture: genie.texture.clone(),
                    bbox: genie.snapshot,
                    frame: genie.frame,
                    commit: std::cell::Cell::default(),
                },
            ));
        }
    }

    /// Applies the pending requests of every node, together.
    fn dock_commit(&mut self) {
        let mut hide: Vec<Window> = Vec::new();
        let mut show: Vec<Window> = Vec::new();
        let mut touched: Vec<Window> = Vec::new();
        let mut cleared: Vec<Window> = Vec::new();
        if let Some(bound) = self.dock.bound.as_mut() {
            for node in &mut bound.nodes {
                if node.pending.is_empty() {
                    continue;
                }
                let before = node.applied.visibility;
                if let Target::Window(window) = &node.target {
                    touched.push(window.clone());
                }
                for op in std::mem::take(&mut node.pending) {
                    match op {
                        Pending::Visibility(v) => node.applied.visibility = v,
                        Pending::Place(p) => {
                            if let Some(q) =
                                node.applied.places.iter_mut().find(|q| q.index == p.index)
                            {
                                *q = p;
                            } else if node.applied.places.len() < MAX_PLACEMENTS {
                                node.applied.places.push(p);
                            }
                        }
                        Pending::Remove(index) => node.applied.places.retain(|q| q.index != index),
                        Pending::Warp(w) => node.applied.warp = Some(w),
                        Pending::ClearWarp => {
                            node.applied.warp = None;
                            if let Target::Window(window) = &node.target {
                                cleared.push(window.clone());
                            }
                        }
                    }
                }
                if let Target::Window(window) = &node.target {
                    match (before, node.applied.visibility) {
                        (b, Visibility::Hidden) if b != Visibility::Hidden => {
                            hide.push(window.clone());
                        }
                        (Visibility::Hidden, v) if v != Visibility::Hidden => {
                            show.push(window.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        for window in show {
            self.dock_show_window(&window);
        }
        for window in hide {
            self.dock_hide_window(&window);
        }
        // The Dock took the windows it touched over from the compositor's holds.
        self.dock.holds.retain(|h| !touched.contains(&h.window));
        // A warp deforms the picture of the window: taken now unless it is already.
        let warped: Vec<Window> = self
            .dock
            .bound
            .iter()
            .flat_map(|b| b.nodes.iter())
            .filter(|n| n.applied.warp.is_some())
            .filter_map(|n| match &n.target {
                Target::Window(w) => Some(w.clone()),
                _ => None,
            })
            .collect();
        for window in &warped {
            self.dock_capture(window);
        }
        self.dock
            .snapshots
            .retain(|(w, _)| !cleared.contains(w) || warped.contains(w));
        self.dock_refresh();
    }

    /// The Dock asked to activate a window: shown first if the Dock hid it.
    fn dock_activate(&mut self, window: &Window) {
        self.dock_show_window(window);
        if let Some(bound) = self.dock.bound.as_mut()
            && let Some(node) = bound
                .nodes
                .iter_mut()
                .find(|n| n.target == Target::Window(window.clone()))
            && node.applied.visibility == Visibility::Hidden
        {
            node.applied.visibility = Visibility::Inherit;
        }
        if let Some(identifier) = crate::shell::toplevel_handle(window).map(|h| h.identifier()) {
            self.activate_identifier(&identifier, false);
        }
        self.dock_refresh();
    }

    /// Reports a gesture to the Dock: `true` if it took it.
    pub fn dock_gesture(
        &mut self,
        kind: crate::gestures::GestureKind,
        phase: u32,
        fingers: u32,
        dx: f64,
        dy: f64,
        scale: f64,
    ) -> bool {
        let Some(bound) = self.dock.bound.as_ref().filter(|b| b.resource.is_alive()) else {
            return false;
        };
        match kind {
            crate::gestures::GestureKind::Swipe => {
                if bound.swipe_fingers != fingers || fingers == 0 {
                    return false;
                }
                bound.resource.swipe(phase_value(phase), fingers, dx, dy);
            }
            crate::gestures::GestureKind::Pinch => {
                if fingers < 4 {
                    return false;
                }
                bound.resource.pinch(phase_value(phase), fingers, scale);
            }
        }
        true
    }

    /// Reports the pointer entering or leaving a corner. `true` if a Dock got it.
    pub fn dock_corner(&mut self, corner: crate::config::Corner, entered: bool) -> bool {
        let Some(bound) = self.dock.bound.as_ref().filter(|b| b.resource.is_alive()) else {
            return false;
        };
        let modifiers = self
            .seat
            .get_keyboard()
            .map(|k| k.modifier_state())
            .map_or(0, |m| {
                u32::from(m.shift)
                    | (u32::from(m.ctrl) << 1)
                    | (u32::from(m.alt) << 2)
                    | (u32::from(m.logo) << 3)
            });
        bound.resource.corner(
            self.output.name(),
            silica_dock_control_v1::Corner::try_from(corner_value(corner))
                .unwrap_or(silica_dock_control_v1::Corner::TopLeft),
            u32::from(entered),
            silica_dock_control_v1::Modifier::from_bits_truncate(modifiers),
        );
        true
    }
}

fn phase_value(phase: u32) -> silica_dock_control_v1::Phase {
    silica_dock_control_v1::Phase::try_from(phase).unwrap_or(silica_dock_control_v1::Phase::Cancel)
}

impl GlobalDispatch<SilicaDockManagerV1, DockGlobal> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<SilicaDockManagerV1>,
        _global_data: &DockGlobal,
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &DockGlobal) -> bool {
        (global_data.filter)(&client)
    }
}

impl Dispatch<SilicaDockManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &SilicaDockManagerV1,
        request: silica_dock_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            silica_dock_manager_v1::Request::GetControl { id } => {
                if state.dock.is_bound() {
                    manager.post_error(
                        silica_dock_manager_v1::Error::AlreadyBound,
                        "another client holds the Dock's control",
                    );
                    return;
                }
                let resource = data_init.init(id, ());
                state.dock.bound = Some(Bound {
                    resource,
                    nodes: Vec::new(),
                    next_id: 0,
                    swipe_fingers: 0,
                });
                state.dock_bound();
            }
            silica_dock_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<SilicaDockControlV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        control: &SilicaDockControlV1,
        request: silica_dock_control_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            silica_dock_control_v1::Request::Destroy => {}
            silica_dock_control_v1::Request::Commit => state.dock_commit(),
            silica_dock_control_v1::Request::SetSwipeFingers { fingers } => {
                if !matches!(fingers, 0 | 3 | 4) {
                    control.post_error(
                        silica_dock_control_v1::Error::InvalidFingers,
                        "swipe fingers must be 0, 3 or 4",
                    );
                    return;
                }
                if let Some(bound) = state.dock.bound.as_mut() {
                    bound.swipe_fingers = fingers;
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        _resource: &SilicaDockControlV1,
        _data: &(),
    ) {
        debug!("the Dock let go of the control");
        state.dock_release();
    }
}

impl Dispatch<SilicaDockNodeV1, u64> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _node: &SilicaDockNodeV1,
        request: silica_dock_node_v1::Request,
        id: &u64,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Some(bound) = state.dock.bound.as_mut() else {
            return;
        };
        let Some(node) = bound.nodes.iter_mut().find(|n| n.id == *id) else {
            return;
        };
        match request {
            silica_dock_node_v1::Request::SetVisibility { visibility } => {
                let v = match visibility {
                    WEnum::Value(silica_dock_node_v1::Visibility::Hidden) => Visibility::Hidden,
                    WEnum::Value(silica_dock_node_v1::Visibility::Shown) => Visibility::Shown,
                    _ => Visibility::Inherit,
                };
                node.pending.push(Pending::Visibility(v));
            }
            silica_dock_node_v1::Request::Place {
                index,
                level,
                x,
                y,
                scale,
                opacity,
                clip_x,
                clip_y,
                clip_width,
                clip_height,
            } => {
                let clip = (clip_width > 0.0 && clip_height > 0.0).then(|| {
                    Rectangle::new((clip_x, clip_y).into(), (clip_width, clip_height).into())
                });
                if !(scale.is_finite() && x.is_finite() && y.is_finite()) || scale <= 0.0 {
                    return;
                }
                node.pending.push(Pending::Place(Placement {
                    index,
                    above_top: matches!(level, WEnum::Value(silica_dock_node_v1::Level::AboveTop)),
                    x,
                    y,
                    scale: scale.min(16.0),
                    opacity: if opacity.is_finite() {
                        opacity.clamp(0.0, 1.0)
                    } else {
                        1.0
                    },
                    clip,
                }));
            }
            silica_dock_node_v1::Request::RemovePlacement { index } => {
                node.pending.push(Pending::Remove(index));
            }
            silica_dock_node_v1::Request::Warp {
                x,
                y,
                width,
                height,
                bend,
                top,
                bottom,
            } => {
                if [x, y, width, height, bend, top, bottom]
                    .iter()
                    .all(|v| v.is_finite())
                    && width > 0.0
                    && height > 0.0
                {
                    node.pending.push(Pending::Warp(Warp {
                        target: Rectangle::new((x, y).into(), (width, height).into()),
                        bend: bend.clamp(0.0, 1.0),
                        top: top.clamp(0.0, 1.0),
                        bottom: bottom.clamp(0.0, 1.0),
                    }));
                }
            }
            silica_dock_node_v1::Request::ClearWarp => node.pending.push(Pending::ClearWarp),
            silica_dock_node_v1::Request::Activate => {
                if let Target::Window(window) = node.target.clone() {
                    state.dock_activate(&window);
                }
            }
            silica_dock_node_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        _resource: &SilicaDockNodeV1,
        id: &u64,
    ) {
        // The node's overrides end.
        let mut show = None;
        if let Some(bound) = state.dock.bound.as_mut()
            && let Some(i) = bound.nodes.iter().position(|n| n.id == *id)
        {
            let node = bound.nodes.remove(i);
            if let (Target::Window(window), Visibility::Hidden) =
                (node.target, node.applied.visibility)
            {
                show = Some(window);
            }
        }
        if let Some(window) = show {
            state.dock_show_window(&window);
        }
        state.dock_refresh();
    }
}

/// Where the placement puts the frame's top-left corner and how it scales, as a rectangle
/// around the window's frame.
pub fn placement_rect(frame: Rectangle<f64, Logical>, p: &Placement) -> Rectangle<f64, Logical> {
    Rectangle::new(
        (p.x, p.y).into(),
        (frame.size.w * p.scale, frame.size.h * p.scale).into(),
    )
}
