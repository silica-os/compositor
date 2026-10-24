//! Compositor state: Wayland protocols, window space, seat.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::desktop::utils::send_frames_surface_tree;
use smithay::desktop::{PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output};
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, LoopSignal, Mode, PostAction};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::ext_data_control::DataControlState;
use smithay::wayland::shell::wlr_layer::{Layer, WlrLayerShellState};
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use tracing::{info, warn};

use silica_control::Shortcut;

use crate::backend::Backend;

/// Desktop background color until there is a wallpaper (linear RGBA).
pub const CLEAR_COLOR: [f32; 4] = [0.11, 0.12, 0.16, 1.0];

/// Event loop data: the state and the `Display` it serves.
pub struct Compositor {
    pub state: State,
    pub display: Display<State>,
}

pub struct State {
    pub start_time: Instant,
    pub socket_name: OsString,
    /// Address of the private session bus (`--session-bus`), passed to the clients.
    pub session_bus: Option<String>,
    /// The `system.*` commands towards silica-daemon.
    pub system: crate::system::SystemLink,
    /// The settings of the Settings service (`settings_service.rs`).
    pub service_settings: crate::settings_service::ServiceSettings,
    /// The appearance forced with `--appearance` (tests, sessions without the daemon): it wins
    /// over silica-daemon's state.
    pub appearance: Option<bool>,
    /// The icon of a drag between apps (`wl_data_device.start_drag`): it follows the pointer
    /// until the drop.
    pub drag_icon: Option<WlSurface>,
    /// Where the drag icon sits from the pointer: the offsets of its commits added up
    /// (`wl_surface.attach` dx/dy or `wl_surface.offset`), so a client keeps the point it
    /// grabbed under the pointer.
    pub drag_icon_offset: Point<i32, Logical>,
    /// The cursor asked by the focused client: drawn by the compositor only in the real session.
    pub cursor_status: smithay::input::pointer::CursorImageStatus,
    /// The window screenshot mode (K-05): the frame of the window under the pointer is veiled.
    pub window_veil: bool,
    /// A shape the compositor itself imposes (resize edges, window bar), over the client's.
    pub cursor_override: Option<smithay::input::pointer::CursorIcon>,
    /// The XCursor theme (Adwaita by default), loaded on demand.
    pub cursor_theme: crate::cursor_theme::CursorTheme,
    /// wp_cursor_shape_v1: clients ask for a named shape instead of drawing a surface.
    pub cursor_shape_state: smithay::wayland::cursor_shape::CursorShapeManagerState,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Compositor>,
    pub loop_signal: LoopSignal,

    pub space: Space<Window>,
    pub popups: PopupManager,
    pub output: Output,
    pub backend: Backend,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    /// ext-data-control: the clipboard history in silica-daemon (and only there, see
    /// `clipboard_manager_client`).
    pub data_control_state: DataControlState,
    pub layer_shell_state: WlrLayerShellState,
    /// wp_viewporter: scaled or cropped surfaces (iced needs it for HiDPI).
    pub viewporter_state: ViewporterState,
    /// linux-dmabuf: buffers from the clients' GPU (drm only, see `dmabuf.rs`).
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub xdg_decoration_state: XdgDecorationState,
    /// xdg-foreign v2: an app's window becomes the parent of another app's window (the Open
    /// and Save dialog of the portal is attached to the app that asked for it).
    pub xdg_foreign_state: smithay::wayland::xdg_foreign::XdgForeignState,
    /// xdg-dialog: modal dialogs (the sheets of Vetro apps) are centred on their parent,
    /// which cannot take the focus while one shows.
    pub xdg_dialog_state: smithay::wayland::shell::xdg::dialog::XdgDialogState,
    pub glass_manager_state: crate::glass::GlassManagerState,
    pub window_manager_state: crate::window_appearance::WindowManagerState,
    /// The windows for the shell (`ext-foreign-toplevel-list`, only to its components).
    pub foreign_toplevel_list: smithay::wayland::foreign_toplevel_list::ForeignToplevelListState,
    pub shell_manager_state: crate::shell::ShellManagerState,
    /// The Dock's control of windows and layers, gestures and hot corners (`dock_control.rs`).
    pub dock_manager_state: crate::dock_control::DockManagerState,
    pub dock: crate::dock_control::DockControl,
    /// Windows the Dock took off the screen (another space), with the place they come back to.
    pub dock_hidden: crate::app_hide::HiddenApps,
    /// The shell components bound to `silica_shell_manager_v1`.
    pub shell_managers:
        Vec<silica_protocols::shell::server::silica_shell_manager_v1::SilicaShellManagerV1>,
    /// The states announced by the shell components (`broadcast`), by topic.
    pub shell_states: std::collections::BTreeMap<String, String>,
    /// The last focused window announced to the shell.
    pub announced_focus: Option<String>,
    /// The menu bar's surfaces: the frontmost app's titles, the status items (#12).
    pub menu_bar: crate::menu_bar::MenuBar,
    pub idle_notifications: crate::idle_notify::IdleNotifications,
    pub seat: Seat<State>,

    /// Number of windows opened so far, to cascade them.
    pub mapped: u32,
    /// Last id assigned to a window.
    pub next_window_id: u64,
    pub shortcuts: Vec<(Shortcut, &'static str)>,
    /// Settings is recording a shortcut (`shortcuts.json` `capture`): see `handle_key`.
    pub shortcuts_capture: bool,
    /// `dock.json`, and the last press on a title bar (window, time in ms, place) for the
    /// double click.
    pub dock_config: crate::config::DockConfig,
    /// The centers of the Dock's app icons in its surface, by app id (`dock.icons`).
    pub dock_icons: Vec<(String, (f64, f64))>,
    pub bar_press: Option<(smithay::desktop::Window, u32, Point<f64, Logical>)>,
    /// Keycodes for the synthetic keys (`input.key`, `input.type`).
    pub keys: crate::keys::KeyLookup,
    /// What `keys` was built for (the keyboard settings and the active input source): it is
    /// built again when they change.
    pub keys_for: Option<(String, String, String, String, u32)>,
    /// The user's settings folder (`config.rs`), watched; `None` without a home.
    pub config_dir: Option<std::path::PathBuf>,
    /// The keyboard settings applied (`keyboard.json`).
    pub keyboard_config: crate::config::KeyboardConfig,
    /// The input sources and the active one last told to the shell (`keyboard.sources`,
    /// `keyboard.source`).
    pub announced_sources: Option<(String, u32)>,
    /// The night light's colour temperature in kelvin (6500: off), `output.color_temperature`.
    pub color_temperature: u32,
    /// Energy (`power.json`, `idle.rs`): the last input, whether the display is off, whether
    /// this idle period already asked for sleep.
    pub power_config: crate::config::PowerConfig,
    /// `theme.json`: the glass level.
    pub theme_config: crate::config::ThemeConfig,
    /// The corner the pointer is in now.
    pub hot_corner_inside: Option<crate::config::Corner>,
    pub last_activity: Instant,
    /// The last input; `last_activity` also restarts while an idle inhibitor holds.
    pub last_input: Instant,
    pub display_off: bool,
    /// Something happened that may change the screen (a client request, input, a command):
    /// the next frame tick draws even if the output is idle (`State::wake`, drm.rs).
    pub redraw: bool,
    /// A frame is to be built once the current batch of events is handled (`State::wake`).
    pub render_scheduled: bool,
    pub idle_slept: bool,
    /// The session is locked (`lock.rs`), the window that had the focus, the lock and screen
    /// saver settings, and since when the saver or the display off began (the password delay
    /// counts from there).
    pub locked: bool,
    pub locked_focus: Option<smithay::desktop::Window>,
    /// ext-session-lock: the manager's state and the client that holds the lock.
    pub session_lock_state: smithay::wayland::session_lock::SessionLockManagerState,
    /// `accessibility.*` and the keys it holds back (accessibility.rs).
    pub accessibility: crate::accessibility::AccessibilityConfig,
    pub key_access: crate::accessibility::KeyAccess,
    /// The standard protocols beyond the core (protocols.rs).
    pub protocols: crate::protocols::Protocols,
    pub lock_client: crate::lock::LockClient,
    pub lock_config: crate::config::LockConfig,
    pub saver_config: crate::config::SaverConfig,
    pub saver_active: bool,
    pub lock_started: Option<Instant>,
    /// The mouse and touchpad settings applied (`input.json`).
    pub input_config: crate::config::InputConfig,
    /// Frame pace, startup and memory of the clients (`perf.*`).
    pub perf: crate::perf::Perf,
    /// Desktop color until there is a wallpaper (`--background`).
    pub clear_color: [f32; 4],
    /// Minimized windows, with the position they go back to.
    pub minimized: Vec<crate::windows::Minimized>,
    /// Windows hidden by their app (its last window closed, the app still running, #142),
    /// with the position they come back to.
    pub hidden: Vec<crate::windows::Minimized>,
    /// The windows of hidden apps (Hide, Hide Others; `app_hide.rs`).
    pub hidden_apps: crate::app_hide::HiddenApps,
    /// The app that stays frontmost after closing its last window (#142, APP-LC): no window
    /// has the focus, the menu bar shows that app and Cmd+Q quits it. Cleared when a window
    /// takes the focus.
    pub windowless_front: Option<String>,
    /// The windowless apps that were frontmost before another app's window took the front,
    /// most recent last: when that window goes and nothing else is in front, the last of them
    /// still running comes back (a document app whose Open panel, another process's window,
    /// was cancelled; macOS activates the previous app).
    pub windowless_history: Vec<String>,
    /// Where each app's window last closed: its next window opens there (APP-LC).
    pub app_frames: crate::app_frames::AppFrames,
    /// Shell components kept running (`--keep-shell`, `keep.rs`).
    pub keeper: crate::keep::Keeper,
    /// Open menus, panels and grabbed popups (`transient.rs`, #149).
    pub transients: Vec<crate::transient::Transient>,
    /// A press outside a transient was consumed: its release does not go to the client.
    pub transient_press: bool,
    /// The app switcher is open: releasing the Logo key confirms it.
    pub switcher_active: bool,
    /// The key acting as Logo (`--logo-key`).
    pub logo_key: crate::keys::LogoKey,
    /// Trackpad gesture in progress (`gestures.rs`).
    pub gesture: Option<crate::gestures::Gesture>,
    /// Window with the pointer on the bar's button group.
    pub titlebar_hover: Option<Window>,
    /// Bar button pressed: the action starts on release over the same button.
    pub titlebar_press: Option<(Window, compositor_render::titlebar::Part)>,
    /// The pointer is still on the pressed button (leaving it cancels the action).
    pub titlebar_press_inside: bool,
    /// How many button presses there have been: the long-press timer on the green button
    /// acts only if the press is still the one that started it.
    pub titlebar_press_count: u64,
    /// The pointer resting on the green button (window and the id of the timer armed on
    /// entering): after `WINDOW_MENU_HOVER_DELAY` the arrangement popover opens (TL-GLASS, #94).
    pub zoom_hover: Option<(Window, u64)>,
    /// How many screenshots were written (the temporary file name).
    pub screenshot_count: u64,
    /// The settings of the screenshot shortcuts (`screenshot.settings`): the pointer drawn in.
    pub screenshot_settings: silica_control::ScreenshotSettings,
    /// Button symbols: when the pointer entered the group and when it left it.
    pub buttons_hover: Option<ButtonsHover>,
    /// Window dragged by the bar, with the grab point relative to the frame.
    pub moving: Option<(Window, Point<f64, Logical>)>,
    /// Window resized from an edge (MU-08) or at the client's request.
    pub resizing: Option<crate::resize::Resize>,
    /// A resize just released, until the client answers its last configure (the anchored
    /// sides stay still until then).
    pub resize_settling: Option<crate::resize::Resize>,
    /// The serial of the last mouse button press, until it is released:
    /// `xdg_toplevel.move` applies only with this serial.
    pub press_serial: Option<smithay::utils::Serial>,
    /// Multiplier of the animation durations; 0 turns them off (`--animation-scale`,
    /// `ui.animation_scale`).
    pub animation_speed: f64,
    /// The speed to go back to when Reduce Motion is turned off.
    pub saved_animation_speed: f64,
    /// Where the first window opens, from the top left of the usable area (`--window-origin`).
    pub window_origin: i32,
    /// Distance between a window and the next one in the cascade.
    pub cascade_step: i32,
    /// Closed windows fading out.
    pub closing: Vec<crate::render::Closing>,
    /// Windows minimizing into the Dock or coming out of it (genie).
    pub genies: Vec<crate::render::Genie>,
    /// How windows go into the Dock: genie or scale (the Dock's setting, SH-DK-09).
    pub minimize_effect: crate::render::MinimizeEffect,
    /// Chosen glass quality (`--glass`, `ui.glass_quality`).
    pub glass_setting: GlassSetting,
    /// The renderer is GLES on a software rasterizer (llvmpipe): the automatic glass is
    /// frosted. The CPU renderer gets the full glass.
    pub software_renderer: bool,
}

/// Requested glass quality: a fixed one or the automatic choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlassSetting {
    #[default]
    Auto,
    Fixed(compositor_render::glass::GlassQuality),
}

impl GlassSetting {
    pub fn parse(text: &str) -> Option<Self> {
        use compositor_render::glass::GlassQuality;
        Some(match text {
            "auto" => GlassSetting::Auto,
            "full" => GlassSetting::Fixed(GlassQuality::Full),
            "frosted" => GlassSetting::Fixed(GlassQuality::Frosted),
            "opaque" => GlassSetting::Fixed(GlassQuality::Opaque),
            _ => return None,
        })
    }
}

impl State {
    /// The dark appearance: the one forced with `--appearance`, otherwise silica-daemon's
    /// (`appearance.dark`, light if unknown). It is read on every frame, so a daemon change
    /// shows on the next frame.
    pub fn dark(&self) -> bool {
        self.appearance.unwrap_or_else(|| self.system.dark())
    }

    /// Something may have changed the screen: the next frame tick draws (on DRM an output
    /// with nothing to draw stops building frames until this, see `drm.rs`). On DRM that tick
    /// comes as soon as the events at hand are handled: a client's commit shows on the next
    /// vblank, not one frame tick later.
    pub fn wake(&mut self) {
        self.redraw = true;
        if !self.render_scheduled && matches!(self.backend, Backend::Drm(_)) {
            self.render_scheduled = true;
            self.loop_handle.insert_idle(|data| {
                data.state.render_scheduled = false;
                data.state.drm_tick();
            });
        }
    }

    /// Whether the compositor itself is animating (not a client): a window closing or minimizing,
    /// an animated pointer. Frames are drawn on every tick then.
    pub fn compositor_animating(&mut self) -> bool {
        // Windows held for the Dock to take over end by themselves.
        if self.dock_tick() {
            return true;
        }
        if !self.closing.is_empty()
            || !self.genies.is_empty()
            || self.resizing.is_some()
            || self.resize_settling.is_some()
        {
            return true;
        }
        let scale = self.output.current_scale().fractional_scale();
        match self.effective_cursor() {
            smithay::input::pointer::CursorImageStatus::Named(icon) => {
                self.cursor_theme.is_animated(icon, scale)
            }
            _ => false,
        }
    }
}

impl State {
    /// The glass quality in use.
    pub fn glass_quality(&self) -> compositor_render::glass::GlassQuality {
        use compositor_render::glass::GlassQuality;
        // Reduce Transparency (Settings › Accessibility › Display): no blur, the opaque glass.
        if self.theme_config.reduce_transparency {
            return GlassQuality::Opaque;
        }
        match self.glass_setting {
            GlassSetting::Fixed(quality) => quality,
            GlassSetting::Auto if self.software_renderer => GlassQuality::Frosted,
            GlassSetting::Auto => GlassQuality::Full,
        }
    }
}

impl State {
    /// Creates the state, opens the Wayland socket and registers the `Display` in the loop.
    pub fn new(
        display: &mut Display<State>,
        loop_handle: LoopHandle<'static, Compositor>,
        loop_signal: LoopSignal,
        output: Output,
        backend: Backend,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let dh = display.handle();
        let protocols = crate::protocols::Protocols::new(&dh);
        let compositor_state = CompositorState::new::<State>(&dh);
        let xdg_shell_state = XdgShellState::new::<State>(&dh);
        let shm_state = ShmState::new::<State>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(&dh);
        let data_device_state = DataDeviceState::new::<State>(&dh);
        let data_control_state =
            DataControlState::new::<State, _>(&dh, None, crate::handlers::clipboard_manager_client);
        let layer_shell_state =
            WlrLayerShellState::new_with_filter::<State, _>(&dh, crate::security::unsandboxed);
        let viewporter_state = ViewporterState::new::<State>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<State>(&dh);
        let xdg_foreign_state = smithay::wayland::xdg_foreign::XdgForeignState::new::<State>(&dh);
        let xdg_dialog_state =
            smithay::wayland::shell::xdg::dialog::XdgDialogState::new::<State>(&dh);
        let glass_manager_state = crate::glass::GlassManagerState::new(&dh);
        let window_manager_state = crate::window_appearance::WindowManagerState::new(&dh);
        let menu_bar = crate::menu_bar::MenuBar::new(&dh);
        let idle_notifications = crate::idle_notify::IdleNotifications::new(&dh);
        crate::image_copy::create_globals(&dh);
        let foreign_toplevel_list = crate::shell::toplevel_list(&dh);
        let shell_manager_state = crate::shell::ShellManagerState::new(&dh);
        let dock_manager_state = crate::dock_control::DockManagerState::new(&dh);
        // Only the shell's locker may lock the session.
        let session_lock_state = smithay::wayland::session_lock::SessionLockManagerState::new::<
            State,
            _,
        >(&dh, crate::shell::is_shell_client);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        let (delay, rate) = key_repeat();
        seat.add_keyboard(Default::default(), delay, rate)?;
        seat.add_pointer();
        seat.add_touch();

        let _global = output.create_global::<State>(&dh);
        let mut space = Space::default();
        space.map_output(&output, (0, 0));

        let socket = ListeningSocketSource::new_auto()?;
        let socket_name = socket.socket_name().to_os_string();
        loop_handle.insert_source(socket, |stream, _, data| {
            // The process on the other side of the socket tells whether it is a shell
            // component: it is read here, once, because the global filters cannot query the
            // display while it is locked by the dispatch.
            let pid = rustix::net::sockopt::socket_peercred(&stream)
                .ok()
                .map(|credentials| credentials.pid.as_raw_nonzero().get());
            let shell = crate::shell::stream_is_shell(&stream)
                && pid.is_some_and(crate::security::system_binary);
            if let Err(err) = data.state.display_handle.insert_client(
                stream,
                Arc::new(ClientState {
                    shell,
                    pid,
                    ..ClientState::default()
                }),
            ) {
                warn!("Wayland client refused: {err}");
            }
        })?;

        let poll_fd = display.backend().poll_fd().try_clone_to_owned()?;
        loop_handle.insert_source(
            Generic::new(poll_fd, Interest::READ, Mode::Level),
            |_, _, data| {
                data.state.wake();
                if let Err(err) = data.display.dispatch_clients(&mut data.state) {
                    warn!("client dispatch failed: {err}");
                }
                Ok(PostAction::Continue)
            },
        )?;
        info!("socket Wayland: {}", socket_name.to_string_lossy());

        Ok(Self {
            start_time: Instant::now(),
            socket_name,
            session_bus: None,
            system: crate::system::SystemLink::disconnected(),
            service_settings: crate::settings_service::ServiceSettings::default(),
            appearance: None,
            drag_icon: None,
            drag_icon_offset: Point::default(),
            cursor_status: smithay::input::pointer::CursorImageStatus::default_named(),
            cursor_override: None,
            window_veil: false,
            cursor_theme: crate::cursor_theme::CursorTheme::load(),
            cursor_shape_state: smithay::wayland::cursor_shape::CursorShapeManagerState::new::<State>(
                &dh,
            ),
            display_handle: dh,
            loop_handle,
            loop_signal,
            space,
            popups: PopupManager::default(),
            output,
            backend,
            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            data_control_state,
            layer_shell_state,
            viewporter_state,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            xdg_decoration_state,
            xdg_foreign_state,
            xdg_dialog_state,
            glass_manager_state,
            window_manager_state,
            foreign_toplevel_list,
            shell_manager_state,
            dock_manager_state,
            dock: crate::dock_control::DockControl::default(),
            dock_hidden: crate::app_hide::HiddenApps::default(),
            shell_managers: Vec::new(),
            transients: Vec::new(),
            transient_press: false,
            shell_states: std::collections::BTreeMap::new(),
            announced_focus: None,
            menu_bar,
            idle_notifications,
            seat,
            mapped: 0,
            next_window_id: 0,
            shortcuts: crate::commands::shortcuts(),
            shortcuts_capture: false,
            dock_config: crate::config::DockConfig::default(),
            dock_icons: Vec::new(),
            bar_press: None,
            keys: crate::keys::KeyLookup::new()?,
            keys_for: None,
            config_dir: None,
            keyboard_config: crate::config::KeyboardConfig::parse(None, key_repeat()),
            announced_sources: None,
            input_config: crate::config::InputConfig::default(),
            power_config: crate::config::PowerConfig::default(),
            theme_config: crate::config::ThemeConfig::default(),
            hot_corner_inside: None,
            last_activity: Instant::now(),
            last_input: Instant::now(),
            display_off: false,
            redraw: true,
            render_scheduled: false,
            idle_slept: false,
            locked: false,
            locked_focus: None,
            session_lock_state,
            protocols,
            accessibility: Default::default(),
            key_access: Default::default(),
            lock_client: crate::lock::LockClient::default(),
            lock_config: crate::config::LockConfig::default(),
            saver_config: crate::config::SaverConfig::default(),
            saver_active: false,
            lock_started: None,
            color_temperature: crate::displays::NEUTRAL_KELVIN,
            perf: crate::perf::Perf::default(),
            clear_color: CLEAR_COLOR,
            minimized: Vec::new(),
            hidden: Vec::new(),
            hidden_apps: crate::app_hide::HiddenApps::default(),
            windowless_front: None,
            windowless_history: Vec::new(),
            app_frames: crate::app_frames::AppFrames::default(),
            keeper: crate::keep::Keeper::default(),
            switcher_active: false,
            logo_key: crate::keys::LogoKey::None,
            gesture: None,
            titlebar_hover: None,
            titlebar_press: None,
            titlebar_press_inside: false,
            titlebar_press_count: 0,
            zoom_hover: None,
            screenshot_count: 0,
            screenshot_settings: silica_control::ScreenshotSettings::default(),
            buttons_hover: None,
            moving: None,
            resizing: None,
            resize_settling: None,
            press_serial: None,
            animation_speed: 1.0,
            saved_animation_speed: 1.0,
            window_origin: crate::handlers::CASCADE_ORIGIN,
            cascade_step: crate::handlers::CASCADE_STEP,
            closing: Vec::new(),
            genies: Vec::new(),
            minimize_effect: crate::render::MinimizeEffect::Genie,
            glass_setting: GlassSetting::Auto,
            software_renderer: false,
        })
    }

    /// The surface under a point of the desktop, with its origin: first the layers above
    /// the windows, then the windows, then the layers below.
    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let in_layer = |levels: &[Layer]| {
            self.layer_under(levels, pos).and_then(|(layer, location)| {
                layer
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, offset)| (surface, (offset + location).to_f64()))
            })
        };
        in_layer(&[Layer::Overlay])
            // The menu bar and its menus (#12), over the Dock and the other bars.
            .or_else(|| self.menu_bar_under(pos))
            .or_else(|| in_layer(&[Layer::Top]))
            .or_else(|| {
                self.space
                    .element_under(pos)
                    .and_then(|(window, location)| {
                        window
                            .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                            .map(|(surface, offset)| (surface, (offset + location).to_f64()))
                    })
            })
            .or_else(|| in_layer(&[Layer::Bottom, Layer::Background]))
    }

    /// Sends the frame callbacks to every visible window, and to hidden ones (#142).
    pub fn send_frames(&self) {
        let output = &self.output;
        let now = self.start_time.elapsed();
        for window in self.space.elements() {
            window.send_frame(output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        // A windowless app (#142) draws its next window while hidden: its frame callbacks keep
        // coming, so its renderer never waits on a surface that is not on screen.
        for (window, _) in &self.hidden {
            window.send_frame(output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        for layer in layer_map_for_output(output).layers() {
            layer.send_frame(output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        if let Some(icon) = &self.drag_icon {
            send_frames_surface_tree(icon, output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        // The menu bar's surfaces and their menus (#12).
        for surface in self.menu_bar.surfaces() {
            send_frames_surface_tree(&surface, output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
            for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(&surface) {
                send_frames_surface_tree(
                    popup.wl_surface(),
                    output,
                    now,
                    Some(Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            }
        }
    }

    /// The drag icon and where to draw it: its corner on the pointer, moved by the offsets
    /// the client gave it.
    pub fn drag_icon_view(&self) -> Option<(WlSurface, Point<f64, Logical>)> {
        let icon = self.drag_icon.as_ref()?;
        if !smithay::reexports::wayland_server::Resource::is_alive(icon) {
            return None;
        }
        let pointer = self.seat.get_pointer()?;
        Some((
            icon.clone(),
            pointer.current_location() + self.drag_icon_offset.to_f64(),
        ))
    }

    /// A commit of the drag icon: its offset moves it from the pointer.
    pub fn drag_icon_committed(&mut self, surface: &WlSurface) {
        if self.drag_icon.as_ref() != Some(surface) {
            return;
        }
        let delta = smithay::wayland::compositor::with_states(surface, |states| {
            states
                .cached_state
                .get::<smithay::wayland::compositor::SurfaceAttributes>()
                .current()
                .buffer_delta
                .take()
        });
        if let Some(delta) = delta {
            self.drag_icon_offset = add_icon_offset(self.drag_icon_offset, delta);
        }
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// Shell component (`shell.rs`): sees the shell protocols.
    pub shell: bool,
    /// The client process (SO_PEERCRED), for force quit.
    pub pid: Option<i32>,
    /// The sandbox it came through (wp-security-context), if any (`security.rs`).
    pub security_context: Option<smithay::wayland::security_context::SecurityContext>,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// The pointer on a window's button group.
#[derive(Clone)]
pub struct ButtonsHover {
    pub window: Window,
    pub entered: std::time::Instant,
    pub left: Option<std::time::Instant>,
}

/// Default key repeat, sent to the clients in `wl_keyboard.repeat_info` (they run the timer).
/// Provisional: the reference only exposes the Keyboard sliders' positions (K-03), not
/// milliseconds; these are its system defaults (initial delay 375 ms, one repeat every 90 ms).
/// Override with `SILICA_COMPOSITOR_KEY_REPEAT=<delay ms>,<rate per second>`.
const KEY_REPEAT_DELAY_MS: i32 = 375;
const KEY_REPEAT_RATE: i32 = 11;

pub fn key_repeat() -> (i32, i32) {
    parse_key_repeat(
        std::env::var("SILICA_COMPOSITOR_KEY_REPEAT")
            .ok()
            .as_deref(),
    )
}

fn parse_key_repeat(value: Option<&str>) -> (i32, i32) {
    let parsed = value.and_then(|v| {
        let (delay, rate) = v.split_once(',')?;
        let delay: i32 = delay.trim().parse().ok()?;
        let rate: i32 = rate.trim().parse().ok()?;
        (delay > 0 && (0..=200).contains(&rate)).then_some((delay, rate))
    });
    parsed.unwrap_or((KEY_REPEAT_DELAY_MS, KEY_REPEAT_RATE))
}

#[cfg(test)]
mod key_repeat_tests {
    use super::*;

    #[test]
    fn key_repeat_defaults_and_override() {
        assert_eq!(parse_key_repeat(None), (375, 11));
        assert_eq!(parse_key_repeat(Some("250, 30")), (250, 30));
        // Rate 0 disables repeat (Wayland semantics).
        assert_eq!(parse_key_repeat(Some("500,0")), (500, 0));
        assert_eq!(parse_key_repeat(Some("fast")), (375, 11));
        assert_eq!(parse_key_repeat(Some("-1,30")), (375, 11));
    }
}

/// The drag icon's offset plus a commit's delta, bounded: a hostile client cannot overflow it,
/// and an icon farther than this from the pointer is off every screen anyway.
pub fn add_icon_offset(
    offset: Point<i32, Logical>,
    delta: Point<i32, Logical>,
) -> Point<i32, Logical> {
    const LIMIT: i32 = 1 << 16;
    Point::from((
        offset.x.saturating_add(delta.x).clamp(-LIMIT, LIMIT),
        offset.y.saturating_add(delta.y).clamp(-LIMIT, LIMIT),
    ))
}

#[cfg(test)]
mod drag_icon_tests {
    use super::add_icon_offset;
    use smithay::utils::Point;

    #[test]
    fn the_icon_offset_adds_up_and_stays_bounded() {
        let grabbed = add_icon_offset(Point::default(), Point::from((-40, -12)));
        assert_eq!(grabbed, Point::from((-40, -12)));
        assert_eq!(
            add_icon_offset(grabbed, Point::from((5, 2))),
            Point::from((-35, -10))
        );
        let hostile = add_icon_offset(
            Point::from((i32::MAX, 0)),
            Point::from((i32::MAX, i32::MIN)),
        );
        assert_eq!(hostile, Point::from((1 << 16, -(1 << 16))));
    }
}
