//! The shell protocols: `ext-foreign-toplevel-list` (the open windows, for the Dock) and
//! `silica-shell-v1` (activating a window, starting an app).
//!
//! Only the shell components see them: processes with `SILICA_SHELL=1` in the environment,
//! which the compositor sets when it launches them (`--shell`, `app.launch` with `shell: true`). It is
//! not a barrier against other processes of the same user, which can set the same variable:
//! that will come with security-context (app sandboxing).

use silica_protocols::shell::SHELL_ENV;
use silica_protocols::shell::server::silica_shell_manager_v1::{self, SilicaShellManagerV1};
use smithay::delegate_foreign_toplevel_list;
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_region::WlRegion;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::compositor::with_states;
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
};
use smithay::wayland::shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData};
use tracing::{info, warn};

use crate::state::State;

/// The process at the other end of the socket is a shell component (see the module comment):
/// pid from SO_PEERCRED, environment from `/proc/<pid>/environ`.
pub fn stream_is_shell(stream: &std::os::unix::net::UnixStream) -> bool {
    let Ok(credentials) = rustix::net::sockopt::socket_peercred(stream) else {
        return false;
    };
    let pid = credentials.pid.as_raw_nonzero();
    let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    let marker = format!("{SHELL_ENV}=1");
    environ
        .split(|byte| *byte == 0)
        .any(|entry| entry == marker.as_bytes())
}

/// The client is a shell component (decided on connection).
pub fn is_shell_client(client: &Client) -> bool {
    client
        .get_data::<crate::state::ClientState>()
        .is_some_and(|data| data.shell)
}

pub struct ShellManagerState;

/// The version of `silica_shell_manager_v1` the compositor offers: 10 splits `activate` (one window)
/// from `activate_app` (all the app's windows, silica-ui#101).
pub const SHELL_VERSION: u32 = 11;

impl ShellManagerState {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, SilicaShellManagerV1, _>(
            SHELL_VERSION,
            ShellGlobal {
                filter: Box::new(is_shell_client),
            },
        );
        Self
    }
}

pub struct ShellGlobal {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync>,
}

/// The window list for the shell, with the same filter.
pub fn toplevel_list(dh: &DisplayHandle) -> ForeignToplevelListState {
    ForeignToplevelListState::new_with_filter::<State>(dh, is_shell_client)
}

/// Title and app id of a toplevel.
fn title_and_app_id(toplevel: &ToplevelSurface) -> (String, String) {
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .map(|data| {
                (
                    data.title.clone().unwrap_or_default(),
                    data.app_id.clone().unwrap_or_default(),
                )
            })
            .unwrap_or_default()
    })
}

/// The window's entry in the shell's list (ext-foreign-toplevel). A window hidden by its app
/// leaves the list and comes back with a new entry, so the slot can be emptied.
#[derive(Default)]
pub struct ToplevelSlot(std::sync::Mutex<Option<ForeignToplevelHandle>>);

/// The window's current entry in the shell's list, if it has one.
pub fn toplevel_handle(window: &Window) -> Option<ForeignToplevelHandle> {
    window
        .user_data()
        .get::<ToplevelSlot>()?
        .0
        .lock()
        .ok()?
        .clone()
}

impl State {
    /// A new window enters the shell's list.
    pub fn publish_toplevel(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let (title, app_id) = title_and_app_id(toplevel);
        let handle = self
            .foreign_toplevel_list
            .new_toplevel::<State>(title, app_id);
        window.user_data().insert_if_missing(ToplevelSlot::default);
        if let Some(slot) = window.user_data().get::<ToplevelSlot>()
            && let Ok(mut slot) = slot.0.lock()
        {
            *slot = Some(handle);
        }
    }

    /// Title or app id changed: the shell receives them.
    pub fn update_toplevel(&mut self, surface: &ToplevelSurface) {
        let Some(window) = self
            .space
            .elements()
            .chain(self.minimized.iter().map(|(w, _)| w))
            .chain(self.hidden_apps.windows())
            .find(|w| w.toplevel() == Some(surface))
        else {
            return;
        };
        let Some(handle) = toplevel_handle(window) else {
            return;
        };
        let (title, app_id) = title_and_app_id(surface);
        handle.send_title(&title);
        handle.send_app_id(&app_id);
        handle.send_done();
        self.menu_bar_refresh();
    }

    /// The window is closed: it leaves the list.
    pub fn unpublish_toplevel(&mut self, surface: &ToplevelSurface) {
        let window = self
            .space
            .elements()
            .chain(self.minimized.iter().map(|(w, _)| w))
            .chain(self.hidden.iter().map(|(w, _)| w))
            .chain(self.hidden_apps.windows())
            .find(|w| w.toplevel() == Some(surface))
            .cloned();
        if let Some(window) = &window {
            self.withdraw_toplevel(window);
            self.forget_hidden_window(window);
        }
        self.minimized
            .retain(|(w, _)| w.toplevel() != Some(surface));
        self.hidden.retain(|(w, _)| w.toplevel() != Some(surface));
        // The windowless frontmost app quit (its hidden window went with it, #142).
        if let Some(window) = &window
            && self.windowless_front.is_some()
            && crate::windows::window_app(window) == self.windowless_front
            && !self
                .hidden
                .iter()
                .any(|(w, _)| crate::windows::window_app(w) == self.windowless_front)
        {
            self.set_windowless_front(None);
        }
        // Nothing else in front: the desktop's owner (#147).
        self.desktop_owner_fallback(window.as_ref());
    }

    /// The window leaves the shell's list but stays alive (hidden by its app, #142).
    pub fn withdraw_toplevel(&mut self, window: &Window) {
        let handle = window
            .user_data()
            .get::<ToplevelSlot>()
            .and_then(|slot| slot.0.lock().ok()?.take());
        if let Some(handle) = handle {
            self.foreign_toplevel_list.remove_toplevel(&handle);
        }
    }

    /// The identifier of the active window with this surface holding the keyboard: its
    /// window; with the keyboard on a shell layer (an open menu or panel, the desktop), the
    /// window on top, as `focused_window` (the frontmost app does not change while a menu or
    /// panel is open, INPUT-01), unless a windowless app is frontmost; empty otherwise.
    fn identifier_of(&self, focus: Option<&WlSurface>) -> String {
        let Some(focus) = focus else {
            return String::new();
        };
        // An open popup (a popover with the keyboard) belongs to the window it hangs from: the
        // app stays the one in front (silica-os/silica-desktop#238).
        let root = self.focus_root(focus);
        let focus = &root;
        let window = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == focus))
            .or_else(|| {
                let on_layer = smithay::desktop::layer_map_for_output(&self.output)
                    .layers()
                    .any(|layer| layer.wl_surface() == focus);
                (on_layer && self.windowless_front.is_none())
                    .then(|| self.space.elements().last())
                    .flatten()
            });
        window
            .and_then(toplevel_handle)
            .map(|h| h.identifier())
            .unwrap_or_default()
    }

    /// Announces the focused window again after a change that does not move the keyboard
    /// (a window raised under a shell layer that keeps it, a windowless app in front).
    pub fn reannounce_focus(&mut self) {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        self.announce_focus(focus.as_ref());
    }

    /// The identifier of the focused window, empty if none.
    pub fn focused_identifier(&self) -> String {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        self.identifier_of(focus.as_ref())
    }

    /// Sends the focused window to the shell components (if it changed). It is called from
    /// the focus change callback, with the surface: reading the focus from the keyboard in
    /// there would take its lock again.
    pub fn announce_focus(&mut self, focus: Option<&WlSurface>) {
        // The frontmost app's menu titles follow the focus (silica-os/compositor#12).
        self.menu_bar_follow_focus(focus);
        let identifier = self.identifier_of(focus);
        if self.announced_focus.as_deref() == Some(identifier.as_str()) {
            return;
        }
        self.shell_managers.retain(|manager| manager.is_alive());
        for manager in &self.shell_managers {
            manager.focused(identifier.clone());
        }
        self.announced_focus = Some(identifier);
    }

    /// Forwards a command to the shell components; how many received it.
    pub fn forward_to_shell(&mut self, id: &str) -> usize {
        self.shell_managers.retain(|manager| manager.is_alive());
        for manager in &self.shell_managers {
            manager.command(id.to_owned());
        }
        self.shell_managers.len()
    }

    /// Announces to the shell components that a window was minimized or restored.
    pub fn announce_minimized(&mut self, window: &Window, minimized: bool) {
        let Some(identifier) = toplevel_handle(window).map(|h| h.identifier()) else {
            return;
        };
        self.shell_managers.retain(|manager| manager.is_alive());
        for manager in &self.shell_managers {
            if manager.version() >= 2 {
                manager.minimized(identifier.clone(), u32::from(minimized));
            }
        }
    }

    /// A state announced by a shell component, sent back to all of them and remembered for
    /// whoever binds later (at most 64 topics, texts of at most 256 bytes).
    pub fn broadcast_to_shell(&mut self, topic: String, value: String) {
        if topic.len() > 256 || value.len() > 256 {
            warn!("shell: broadcast too long, ignored");
            return;
        }
        if !self.shell_states.contains_key(&topic) && self.shell_states.len() >= 64 {
            warn!("shell: too many broadcast topics, ignored {topic}");
            return;
        }
        self.shell_managers.retain(|manager| manager.is_alive());
        for manager in &self.shell_managers {
            if manager.version() >= 4 {
                manager.broadcast(topic.clone(), value.clone());
            }
        }
        self.shell_states.insert(topic, value);
    }

    /// Asks the shell components for the arrangement popover of the window, with the tip
    /// towards `point` (below the full-screen button). How many received it.
    pub fn announce_window_menu(&mut self, window: &Window, point: Point<i32, Logical>) -> usize {
        let Some(identifier) = toplevel_handle(window).map(|h| h.identifier()) else {
            return 0;
        };
        let appearance = window
            .toplevel()
            .and_then(|toplevel| crate::window_appearance::override_of(toplevel.wl_surface()))
            .map_or(0, |dark| if dark { 2 } else { 1 });
        self.shell_managers.retain(|manager| manager.is_alive());
        let mut sent = 0;
        for manager in &self.shell_managers {
            if manager.version() >= 3 {
                if manager.version() >= 8 {
                    manager.window_menu_appearance(identifier.clone(), appearance);
                }
                manager.window_menu(identifier.clone(), point.x, point.y);
                sent += 1;
            }
        }
        sent
    }

    /// Writes the thumbnail of the minimized window with this identifier into the file;
    /// returns its size (0 × 0 if there is none).
    fn write_thumbnail(&self, identifier: &str, fd: std::os::fd::OwnedFd) -> (u32, u32) {
        use std::io::{Seek, Write};
        let Some(window) =
            self.minimized.iter().map(|(w, _)| w).find(|w| {
                toplevel_handle(w).is_some_and(|handle| handle.identifier() == identifier)
            })
        else {
            return (0, 0);
        };
        let Some(thumbnail) = window.user_data().get::<crate::backend::Thumbnail>() else {
            return (0, 0);
        };
        let Ok(slot) = thumbnail.0.lock() else {
            return (0, 0);
        };
        let Some((w, h, pixels)) = slot.as_ref() else {
            return (0, 0);
        };
        let mut file = std::fs::File::from(fd);
        let written = file
            .seek(std::io::SeekFrom::Start(0))
            .and_then(|_| file.write_all(pixels))
            .and_then(|()| file.flush());
        match written {
            Ok(()) => (*w, *h),
            Err(err) => {
                warn!("thumbnail: {err}");
                (0, 0)
            }
        }
    }

    /// The window with this list identifier: focus, and it comes back if it was minimized.
    /// With `whole_app` its app activates: all of the app's windows on screen come forward
    /// (a Dock click, the app switcher, Bring All to Front); without, only this window (and
    /// its children) does, as a click on it (a window picked in a menu, silica-ui#101).
    pub fn activate_identifier(&mut self, identifier: &str, whole_app: bool) {
        let window = self
            .space
            .elements()
            .chain(self.minimized.iter().map(|(w, _)| w))
            .chain(self.hidden_apps.windows())
            .find(|w| toplevel_handle(w).is_some_and(|handle| handle.identifier() == identifier))
            .cloned();
        let Some(window) = window else {
            return;
        };
        // A window of a hidden app: the whole app comes back.
        self.unhide_for(&window);
        // Like a click on the app in the Dock (dock.md, D-03): all the app windows on screen
        // come forward in their order, with the requested one on top. A minimized window
        // comes back by itself (the Dock, if they are all minimized, asks for the last one).
        if whole_app
            && self.space.elements().any(|w| *w == window)
            && let Some(app) = crate::windows::window_app(&window)
        {
            let same: Vec<Window> = self
                .space
                .elements()
                .filter(|w| crate::windows::window_app(w).as_deref() == Some(app.as_str()))
                .cloned()
                .collect();
            for w in &same {
                self.space.raise_element(w, false);
            }
        }
        // The requested window on top (the Dock passes the app's most recently used one).
        if !self.space.elements().any(|w| *w == window) {
            self.restore_window(&window);
        }
        self.focus_window(Some(window));
    }
}

impl State {
    /// A transient's `wl_region` (surface coordinates) on the screen, logical.
    pub(crate) fn transient_region(
        &self,
        surface: &WlSurface,
        region: Option<WlRegion>,
    ) -> Vec<Rectangle<i32, Logical>> {
        let origin = self.surface_origin(surface);
        region
            .map(|region| {
                smithay::wayland::compositor::get_region_attributes(&region)
                    .rects
                    .into_iter()
                    .filter(|(kind, _)| {
                        matches!(kind, smithay::wayland::compositor::RectangleKind::Add)
                    })
                    .map(|(_, rect)| Rectangle::new(rect.loc + origin, rect.size))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl ForeignToplevelListHandler for State {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list
    }
}

delegate_foreign_toplevel_list!(State);

impl GlobalDispatch<SilicaShellManagerV1, ShellGlobal> for State {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<SilicaShellManagerV1>,
        _global_data: &ShellGlobal,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        manager.focused(state.focused_identifier());
        if manager.version() >= 2 {
            for (window, _) in &state.minimized {
                if let Some(handle) = toplevel_handle(window) {
                    manager.minimized(handle.identifier(), 1);
                }
            }
        }
        if manager.version() >= 4 {
            for (topic, value) in &state.shell_states {
                manager.broadcast(topic.clone(), value.clone());
            }
        }
        state.shell_managers.push(manager);
    }

    fn can_view(client: Client, global_data: &ShellGlobal) -> bool {
        (global_data.filter)(&client)
    }
}

impl Dispatch<SilicaShellManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &SilicaShellManagerV1,
        request: silica_shell_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // Before version 10 `activate` brought the whole app, as `activate_app` does now.
            silica_shell_manager_v1::Request::Activate { identifier } => {
                state.activate_identifier(&identifier, manager.version() < 10);
            }
            silica_shell_manager_v1::Request::ActivateApp { identifier } => {
                state.activate_identifier(&identifier, true);
            }
            silica_shell_manager_v1::Request::Launch { command } => match state.launch(&command) {
                Ok(pid) => info!("shell: started {command} (pid {pid})"),
                Err(err) => warn!("shell: {err}"),
            },
            silica_shell_manager_v1::Request::Execute { id } => {
                if let Err(err) = state.execute_command(&id, serde_json::Value::Null) {
                    warn!("shell: {id}: {err}");
                }
            }
            silica_shell_manager_v1::Request::ExecuteWith { id, params } => {
                let params = match serde_json::from_str::<serde_json::Value>(&params) {
                    Ok(value @ serde_json::Value::Object(_)) => value,
                    Ok(_) | Err(_) => {
                        warn!("shell: {id}: invalid parameters");
                        return;
                    }
                };
                if let Err(err) = state.execute_command(&id, params) {
                    warn!("shell: {id}: {err}");
                }
            }
            silica_shell_manager_v1::Request::Broadcast { topic, value } => {
                state.broadcast_to_shell(topic, value);
            }
            silica_shell_manager_v1::Request::Thumbnail { identifier, fd } => {
                let (width, height) = state.write_thumbnail(&identifier, fd);
                manager.thumbnail(identifier, width, height);
            }
            silica_shell_manager_v1::Request::SetTransient {
                surface,
                region,
                press,
                scroll,
            } => {
                let region = state.transient_region(&surface, region);
                let rule = crate::transient::Rule::from_outside(
                    press.into_result().ok(),
                    scroll.into_result().ok(),
                );
                state.set_shell_transient(
                    manager.clone(),
                    surface,
                    region,
                    crate::transient::OwnerApp::None,
                    (rule, rule),
                );
            }
            silica_shell_manager_v1::Request::SetOwnedTransient {
                surface,
                region,
                owner,
                owner_rule,
                other_rule,
            } => {
                let region = state.transient_region(&surface, region);
                let rule = |value: WEnum<silica_shell_manager_v1::Rule>| {
                    crate::transient::Rule::from_protocol(match value {
                        WEnum::Value(value) => value as u32,
                        WEnum::Unknown(value) => value,
                    })
                };
                state.set_shell_transient(
                    manager.clone(),
                    surface,
                    region,
                    crate::transient::OwnerApp::parse(&owner),
                    (rule(owner_rule), rule(other_rule)),
                );
            }
            silica_shell_manager_v1::Request::ClearTransient { surface } => {
                state.clear_shell_transient(&surface);
            }
            silica_shell_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
