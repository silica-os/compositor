//! Wayland protocol handlers: compositor, xdg-shell, shm, seat, output.

use std::os::fd::OwnedFd;

use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::{PopupKind, Window};
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::{Logical, Point, Rectangle, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    BufferAssignment, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    get_parent, is_sync_subsurface, with_states,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::wayland::selection::ext_data_control::{DataControlHandler, DataControlState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_viewporter, delegate_xdg_shell,
};
use tracing::{info, trace, warn};

use crate::state::{ClientState, State};

/// A new window opens 4 pt from the left and 4 pt under the top of the usable area, and the
/// next one cascades by 29 pt (PREVIEW-01; the cascade end is not measured, so it wraps).
pub const CASCADE_ORIGIN: i32 = 4;
/// Distance between a window and the next one in the cascade, in logical pixels.
pub const CASCADE_STEP: i32 = 29;
const CASCADE_SLOTS: u32 = 8;

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        match client.get_data::<ClientState>() {
            Some(data) => &data.compositor_state,
            // Every client is inserted with ClientState (state.rs): it does not happen.
            None => &FALLBACK_CLIENT_STATE,
        }
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        crate::protocols::add_buffer_blockers(surface);
    }

    fn commit(&mut self, surface: &WlSurface) {
        // It must be read before the buffer handler, which consumes the buffer just assigned.
        let new_buffer = with_states(surface, |states| {
            matches!(
                states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .buffer,
                Some(BufferAssignment::NewBuffer(_))
            )
        });
        if new_buffer {
            let pid = surface
                .client()
                .and_then(|client| client.get_credentials(&self.display_handle).ok())
                .and_then(|credentials| u32::try_from(credentials.pid).ok());
            self.perf.frame(surface, pid, std::time::Instant::now());
        }
        on_commit_buffer_handler::<Self>(surface);
        self.drag_icon_committed(surface);
        // The frame sequence of every surface, to check frame by frame what is on screen
        // (RUST_LOG=compositor::handlers=trace): a new buffer and the glass that comes with it.
        if tracing::enabled!(tracing::Level::TRACE) {
            let glass = crate::glass::current_regions(surface);
            let name = smithay::desktop::layer_map_for_output(&self.output)
                .layer_for_surface(surface, smithay::desktop::WindowSurfaceType::TOPLEVEL)
                .map_or_else(
                    || format!("{:?}", surface.id()),
                    |l| l.namespace().to_owned(),
                );
            trace!(
                "commit {name}: new buffer {new_buffer}, glass {} (opacity {:.2})",
                glass.len(),
                glass.iter().map(|r| r.opacity).fold(0.0, f64::max),
            );
        }
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for(&root) {
                window.on_commit();
            }
            // A live resize (#160): the anchored sides stay still, the next size can leave.
            self.resize_commit(&root, new_buffer && root == *surface);
        }
        // A dialog of another app is centered over its parent at its first buffer.
        if let Some(window) = self.window_for(surface) {
            self.center_on_parent(&window);
        }

        if let Some(toplevel) = self.window_for(surface).and_then(|w| w.toplevel().cloned())
            && !toplevel.is_initial_configure_sent()
        {
            toplevel.send_configure();
        }
        // With the compositor's bar the frame grows upwards: it stays in the usable area.
        if let Some(window) = self.window_for(surface)
            && crate::decorations::is_server_side(&window)
        {
            self.make_room_for_titlebar(&window);
        }

        self.layer_commit(surface);

        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
            && let Err(err) = popup.send_configure()
        {
            warn!("popup configure failed: {err}");
        }
    }
}

static FALLBACK_CLIENT_STATE: std::sync::LazyLock<CompositorClientState> =
    std::sync::LazyLock::new(CompositorClientState::default);

impl State {
    /// Where an app popup goes: the positioner's geometry moved back inside the output by its
    /// constraint adjustment (flip, slide, resize; xdg_positioner), relative to the parent
    /// surface's geometry, so a menu never leaves the screen.
    pub(crate) fn popup_geometry(
        &self,
        popup: &PopupKind,
        positioner: PositionerState,
    ) -> Rectangle<i32, Logical> {
        let unconstrained = (|| {
            let root = smithay::desktop::find_popup_root_surface(popup).ok()?;
            let coords = smithay::desktop::get_popup_toplevel_coords(popup);
            let mut target = if let Some(window) = self.window_for(&root) {
                let location = self.space.element_location(&window)?;
                let mut target = self.space.output_geometry(&self.output)?;
                target.loc -= location;
                target
            } else {
                // A popup of a shell layer (silica-os/silica-desktop#222): the layer's origin is where
                // its surface sits in the output.
                let map = smithay::desktop::layer_map_for_output(&self.output);
                let layer =
                    map.layer_for_surface(&root, smithay::desktop::WindowSurfaceType::TOPLEVEL)?;
                let at = map.layer_geometry(layer)?.loc;
                let output = self.space.output_geometry(&self.output)?;
                Rectangle::new(Point::from((-at.x, -at.y)), output.size)
            };
            target.loc -= coords;
            Some(positioner.get_unconstrained_geometry(target))
        })();
        unconstrained.unwrap_or_else(|| positioner.get_geometry())
    }

    pub(crate) fn window_for(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned()
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

/// A window whose app has not said its id yet (`app_id_changed` places it once).
struct FreshWindow(std::cell::Cell<bool>);

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        let slot = i32::try_from(self.mapped % CASCADE_SLOTS).unwrap_or(0);
        let offset = self
            .window_origin
            .saturating_add(self.cascade_step.saturating_mul(slot));
        self.mapped = self.mapped.wrapping_add(1);

        let window = Window::new_wayland_window(surface);
        // Until the app says its id, the window may still move to where that app's window
        // last closed (`app_id_changed`).
        window
            .user_data()
            .insert_if_missing(|| FreshWindow(std::cell::Cell::new(true)));
        let id = self.assign_window_id(&window);
        self.publish_toplevel(&window);
        info!("new window {id} at ({offset}, {offset})");
        self.space.map_element(
            window.clone(),
            self.usable_area().loc + Point::<i32, Logical>::from((offset, offset)),
            true,
        );
        self.focus_window(Some(window));
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let geometry = self.popup_geometry(&PopupKind::from(surface.clone()), positioner);
        surface.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            warn!("popup not tracked: {err}");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let geometry = self.popup_geometry(&PopupKind::from(surface.clone()), positioner);
        surface.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    /// An app popup is gone (a menu closed): its grab ends, and the keyboard goes back to the
    /// popup it hung from (a submenu's menu) or to its window, if the popup had it.
    fn popup_destroyed(&mut self, surface: PopupSurface) {
        let gone = surface.wl_surface().clone();
        self.end_popup_grab(&gone, surface.get_parent_surface());
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .is_some_and(|focus| &focus == surface.wl_surface());
        // The window fades out from its last image: the surface still has the buffer.
        if let Some(window) = self.window_for(surface.wl_surface()) {
            // Its app's next window opens here (APP-LC); a child window (Info, compose)
            // leaves the app's place alone.
            // The parent as last set: an xdg-foreign relationship may end before the window
            // (the dialog's process exits and its imported handle goes first).
            let parent = surface
                .parent()
                .or_else(|| crate::windows::last_parent(&window));
            if let (Some(app), Some(location), None) = (
                crate::windows::window_app(&window),
                self.space.element_location(&window),
                parent.as_ref(),
            ) {
                self.app_frames.remember(
                    &app,
                    crate::app_frames::Frame {
                        x: location.x,
                        y: location.y,
                    },
                );
            }
            self.start_closing(&window);
            // The keyboard was there: it goes to the window's parent, or to the app's next
            // window (closing an Info window gives the focus back to its browser window).
            if focused {
                let next = self.next_focus_after(&window, parent);
                if next.is_some() {
                    self.focus_window(next);
                }
            }
        }
        self.unpublish_toplevel(&surface);
        // The parent of a menu is gone (#237).
        self.dismiss_popup_menus_on(surface.wl_surface());
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        self.update_toplevel(&surface);
    }

    /// A child window (`set_parent`: Get Info, a compose window) opens over its parent,
    /// offset so the parent's bar stays visible, rather than in the cascade. A window of
    /// another app made a child through xdg-foreign (the portal's Open and Save dialog) is a
    /// dialog attached to its parent: it is centered over it once its size is known.
    fn parent_changed(&mut self, surface: ToplevelSurface) {
        let Some(window) = self.window_for(surface.wl_surface()) else {
            return;
        };
        let parent_surface = surface.parent();
        if let Some(parent) = &parent_surface {
            crate::windows::remember_parent(&window, parent);
        }
        let fresh = window
            .user_data()
            .get::<FreshWindow>()
            .is_some_and(|fresh| fresh.0.get());
        // Not on screen yet (no buffer): it may still be placed.
        let unshown = window.geometry().size.is_empty();
        let Some(parent) = parent_surface
            .as_ref()
            .and_then(|parent| self.window_for(parent))
        else {
            return;
        };
        let other_app = parent_surface
            .as_ref()
            .and_then(|parent| parent.client())
            .map(|client| client.id())
            != surface.wl_surface().client().map(|client| client.id());
        if other_app {
            crate::windows::center_at_first_buffer(&window);
            // The parent arrived after the window was shown: it moves over it now.
            if !unshown {
                self.center_on_parent(&window);
                return;
            }
        }
        if !fresh && !unshown {
            return;
        }
        let Some(parent_at) = self.space.element_location(&parent) else {
            return;
        };
        let at = crate::windows::child_location(parent_at, self.usable_area());
        self.space.map_element(window, at, true);
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        self.update_toplevel(&surface);
        // The app's first id: its window opens where the app's window last closed.
        let Some(window) = self.window_for(surface.wl_surface()) else {
            return;
        };
        let fresh = window
            .user_data()
            .get::<FreshWindow>()
            .is_some_and(|fresh| fresh.0.replace(false));
        // A child window stays over its parent (`parent_changed`).
        if !fresh || surface.parent().is_some() {
            return;
        }
        if let Some(app) = crate::windows::window_app(&window)
            && let Some(frame) = self.app_frames.get(&app)
        {
            let area = self.usable_area();
            // Kept on screen (the output may have changed since).
            let x = frame
                .x
                .clamp(area.loc.x, area.loc.x + (area.size.w - 64).max(0));
            let y = frame
                .y
                .clamp(area.loc.y, area.loc.y + (area.size.h - 64).max(0));
            self.space.map_element(window, (x, y), true);
        }
    }

    /// The app moves the window from its toolbar (full-size content): like dragging the compositor's
    /// bar, only with the button still pressed.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, serial: Serial) {
        if self.press_serial != Some(serial) || self.moving.is_some() {
            return;
        }
        let Some(window) = self.window_for(surface.wl_surface()) else {
            return;
        };
        let (Some(pointer), Some(frame)) = (self.seat.get_pointer(), self.window_frame(&window))
        else {
            return;
        };
        let grab = pointer.current_location() - frame.loc.to_f64();
        self.moving = Some((window, grab));
    }

    /// The app zooms its window (a double click on its toolbar, #169): it fills the usable
    /// area. The client always gets a configure back, even when nothing changed.
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let done = self
            .window_for(surface.wl_surface())
            .map(|window| self.zoom_window(&window, true));
        match done {
            Some(Ok(())) => {}
            Some(Err(err)) => {
                warn!("maximize: {err}");
                surface.send_pending_configure();
            }
            None => {
                surface.send_pending_configure();
            }
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let done = self
            .window_for(surface.wl_surface())
            .map(|window| self.zoom_window(&window, false));
        match done {
            Some(Ok(())) => {}
            Some(Err(err)) => {
                warn!("unmaximize: {err}");
                surface.send_pending_configure();
            }
            None => {
                surface.send_pending_configure();
            }
        }
    }

    /// The app asks for full screen (a slideshow, #173): the window fills the output.
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let done = self
            .window_for(surface.wl_surface())
            .map(|window| self.set_fullscreen_in_place(&window, true));
        if !matches!(done, Some(Ok(()))) {
            if let Some(Err(err)) = done {
                warn!("fullscreen: {err}");
            }
            surface.send_pending_configure();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let done = self
            .window_for(surface.wl_surface())
            .map(|window| self.set_fullscreen_in_place(&window, false));
        if !matches!(done, Some(Ok(()))) {
            if let Some(Err(err)) = done {
                warn!("unfullscreen: {err}");
            }
            surface.send_pending_configure();
        }
    }

    /// The app minimizes its window (its title bar double click, #169): like the yellow button.
    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for(surface.wl_surface())
            && let Err(err) = self.minimize_window(&window)
        {
            warn!("minimize: {err}");
        }
    }

    /// The client resizes the window from one of its edges (client-side decorations), with
    /// the button still pressed.
    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        if self.press_serial != Some(serial) || self.resizing.is_some() || self.moving.is_some() {
            return;
        }
        let edges = crate::resize::Edges::from_xdg(edges);
        let (Some(window), Some(pointer)) = (
            self.window_for(surface.wl_surface()),
            self.seat.get_pointer(),
        ) else {
            return;
        };
        if edges.any() {
            self.start_resize(window, edges, pointer.current_location());
        }
    }

    /// A popup with a grab (an app's menu): it closes on a press outside its app (#5, #149).
    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        self.grab_popup(PopupKind::from(surface));
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|s| self.display_handle.get_client(s.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client);
        self.announce_focus(focused);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl DataControlHandler for State {
    fn data_control_state(&self) -> &DataControlState {
        &self.data_control_state
    }
}

/// The only client that may read and set the selection without focus: silica-daemon, the
/// clipboard history (by its executable, from the pid read when the client connected: a
/// global filter must not query the display, which the dispatch holds locked). Every other
/// client keeps the usual rule (only the focused one reads the clipboard).
pub fn clipboard_manager_client(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .filter(|state| state.security_context.is_none())
        .and_then(|state| state.pid)
        .filter(|pid| crate::security::system_binary(*pid))
        .and_then(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok())
        .and_then(|exe| exe.file_name().map(|name| name == "silica-daemon"))
        .unwrap_or(false)
}

impl ClientDndGrabHandler for State {
    fn started(
        &mut self,
        _source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.drag_icon = icon;
        self.drag_icon_offset = Point::default();
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.drag_icon = None;
    }
}

impl ServerDndGrabHandler for State {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl OutputHandler for State {}

delegate_compositor!(State);
delegate_shm!(State);
delegate_xdg_shell!(State);
delegate_seat!(State);
delegate_data_device!(State);
smithay::delegate_ext_data_control!(State);
delegate_output!(State);
delegate_viewporter!(State);
smithay::delegate_cursor_shape!(State);
smithay::delegate_xdg_foreign!(State);
smithay::delegate_xdg_dialog!(State);

impl smithay::wayland::shell::xdg::dialog::XdgDialogHandler for State {
    /// A modal dialog (a sheet) is centred on its parent once its size is known, and keeps
    /// the focus from its parent while it shows.
    fn modal_changed(&mut self, toplevel: ToplevelSurface, is_modal: bool) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        crate::windows::set_modal(&window, is_modal);
        if is_modal {
            crate::windows::center_at_first_buffer(&window);
            self.center_on_parent(&window);
        }
    }
}

impl smithay::wayland::xdg_foreign::XdgForeignHandler for State {
    fn xdg_foreign_state(&mut self) -> &mut smithay::wayland::xdg_foreign::XdgForeignState {
        &mut self.xdg_foreign_state
    }
}

impl smithay::wayland::tablet_manager::TabletSeatHandler for State {}
