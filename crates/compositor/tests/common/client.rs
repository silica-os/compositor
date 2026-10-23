//! A minimal Wayland client for the tests: an xdg-shell window with a solid color.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use silica_protocols::glass::client::silica_glass_manager_v1::SilicaGlassManagerV1;
use silica_protocols::glass::client::silica_glass_v1::{self, SilicaGlassV1};
use silica_protocols::menu::client::silica_menu_manager_v1::{self, SilicaMenuManagerV1};
use silica_protocols::menu::client::silica_menu_v1::{self, SilicaMenuV1};
use silica_protocols::menu::client::silica_popup_menu_v1::{self, SilicaPopupMenuV1};
use silica_protocols::window::client::silica_window_manager_v1::SilicaWindowManagerV1;
use silica_protocols::window::client::silica_window_v1::{self, SilicaWindowV1};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{self, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{
    self, ZxdgToplevelDecorationV1,
};
use wayland_protocols::xdg::dialog::v1::client::xdg_dialog_v1::XdgDialogV1;
use wayland_protocols::xdg::dialog::v1::client::xdg_wm_dialog_v1::XdgWmDialogV1;
use wayland_protocols::xdg::foreign::zv2::client::zxdg_exported_v2::{self, ZxdgExportedV2};
use wayland_protocols::xdg::foreign::zv2::client::zxdg_exporter_v2::ZxdgExporterV2;
use wayland_protocols::xdg::foreign::zv2::client::zxdg_imported_v2::ZxdgImportedV2;
use wayland_protocols::xdg::foreign::zv2::client::zxdg_importer_v2::ZxdgImporterV2;
use wayland_protocols::xdg::shell::client::xdg_popup::{self, XdgPopup};
use wayland_protocols::xdg::shell::client::xdg_positioner::{self, XdgPositioner};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{self, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, ZwlrLayerSurfaceV1,
};

/// Size and color of the client window (ARGB8888 in memory: B, G, R, A).
pub const WINDOW: (u32, u32) = (100, 60);

/// A popup for [`TestWindow::open_popup`].
#[derive(Clone, Copy, Default)]
pub struct PopupSpec {
    /// x, y, width, height in the parent's coordinates; the popup hangs from its bottom left.
    pub anchor_rect: (i32, i32, u32, u32),
    pub size: (u32, u32),
    /// `xdg_positioner.constraint_adjustment` bits.
    pub adjustment: u32,
    pub grab: bool,
    /// Hangs from the first popup instead of the window.
    pub nested: bool,
    /// Drawn transparent (to show the glass declared on it) instead of red.
    pub transparent: bool,
}
pub const RED_BGRA: [u8; 4] = [0, 0, 255, 255];
/// Position of the first window in the compositor's cascade (handlers.rs).
pub const FIRST_WINDOW_AT: u32 = 4;

#[derive(Default)]
struct App {
    compositor: Option<WlCompositor>,
    /// xdg-dialog (modal dialogs: a Vetro app's sheets).
    dialogs: Option<XdgWmDialogV1>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    decorations: Option<ZxdgDecorationManagerV1>,
    glass: Option<SilicaGlassManagerV1>,
    appearance: Option<SilicaWindowManagerV1>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    /// The keyboard events the window got (`TestWindow::key_log`).
    key_log: KeyLog,
    /// The serial of the last button press on the window.
    press_serial: Option<u32>,
    /// What the pointer did on the window (`TestWindow::pointer_log`).
    pointer_log: PointerLog,
    /// The compositor closed the window's popup (`xdg_popup.popup_done`).
    popup_done: bool,
    /// The `xdg_popup.configure` events, in order: x, y, width, height.
    popup_configures: Vec<(i32, i32, i32, i32)>,
    configured: bool,
    /// The last size requested by the compositor (`xdg_toplevel.configure`).
    configured_size: Option<(i32, i32)>,
    /// How many `xdg_toplevel.configure` arrived, and whether the last one had `resizing`.
    configure_count: u32,
    configured_resizing: bool,
    /// Whether the last configure carried the `maximized` state.
    configured_maximized: bool,
    /// Whether the last configure carried the `fullscreen` state.
    configured_fullscreen: bool,
    /// Configures are not acked until `TestWindow::ack_held` (a slow client).
    hold_acks: bool,
    held: Vec<u32>,
    close_requested: bool,
    /// Decoration mode chosen by the compositor.
    decoration_mode: Option<zxdg_toplevel_decoration_v1::Mode>,
    data_manager: Option<WlDataDeviceManager>,
    data_device: Option<WlDataDevice>,
    /// Drag leaving from here: the source and the data it sends.
    drag_source: Option<WlDataSource>,
    drag_payload: Vec<u8>,
    drag_finished: bool,
    drag_cancelled: bool,
    /// Incoming drag: the offer with its types, the enter and the drop.
    offer: Option<WlDataOffer>,
    offer_mimes: Vec<String>,
    drag_entered: Option<u32>,
    dropped: bool,
    layer_shell: Option<ZwlrLayerShellV1>,
    /// The size the compositor gave a layer surface (`zwlr_layer_surface_v1.configure`).
    layer_size: Option<(u32, u32)>,
    /// silica-menu-v1: the manager and the ids of the items chosen in the bar.
    menu_manager: Option<SilicaMenuManagerV1>,
    menu_activations: Vec<u32>,
    /// How the popup menus ended (silica-menu-v1 version 2).
    popup_results: Vec<PopupResult>,
    /// xdg-foreign v2: the globals and the handle of this window, once exported.
    exporter: Option<ZxdgExporterV2>,
    importer: Option<ZxdgImporterV2>,
    exported_handle: Option<String>,
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => state.wm_base = Some(registry.bind(name, 1, qh, ())),
                "silica_glass_manager_v1" => {
                    let version = GLASS_VERSION.with(std::cell::Cell::get).min(version);
                    state.glass = Some(registry.bind(name, version, qh, ()));
                }
                "wl_seat" => state.seat = Some(registry.bind(name, 1, qh, ())),
                "wl_data_device_manager" => {
                    state.data_manager = Some(registry.bind(name, 3.min(version), qh, ()));
                }
                "silica_window_manager_v1" => {
                    state.appearance = Some(registry.bind(name, 3.min(version), qh, ()));
                }
                "zxdg_decoration_manager_v1" => {
                    state.decorations = Some(registry.bind(name, 1, qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, 1, qh, ()));
                }
                "silica_menu_manager_v1" => {
                    state.menu_manager = Some(registry.bind(name, 2, qh, ()));
                }
                "zxdg_exporter_v2" => state.exporter = Some(registry.bind(name, 1, qh, ())),
                "zxdg_importer_v2" => state.importer = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_dialog_v1" => state.dialogs = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for App {
    fn event(
        state: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            if state.hold_acks && state.configured {
                state.held.push(serial);
            } else {
                surface.ack_configure(serial);
            }
            state.configured = true;
        }
    }
}

impl Dispatch<WlShm, ()> for App {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for App {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
            && caps.contains(wl_seat::Capability::Pointer)
            && state.pointer.is_none()
        {
            state.pointer = Some(seat.get_pointer(qh, ()));
        }
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
            && caps.contains(wl_seat::Capability::Keyboard)
            && state.keyboard.is_none()
        {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => {
                state.key_log.inside = true;
                state.key_log.enters += 1;
            }
            wl_keyboard::Event::Leave { .. } => state.key_log.inside = false,
            wl_keyboard::Event::Key { .. } => state.key_log.keys += 1,
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for App {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Button {
                serial,
                state: wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                state.press_serial = Some(serial);
                state.pointer_log.presses += 1;
            }
            wl_pointer::Event::Enter { .. } => {
                state.pointer_log.inside = true;
                state.pointer_log.enters += 1;
            }
            wl_pointer::Event::Leave { .. } => state.pointer_log.inside = false,
            wl_pointer::Event::Motion { .. } => state.pointer_log.motions += 1,
            wl_pointer::Event::Axis { .. } => state.pointer_log.axes += 1,
            _ => {}
        }
    }
}

delegate_noop!(App: ignore WlDataDeviceManager);

impl Dispatch<WlDataDevice, ()> for App {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                state.offer = Some(id);
                state.offer_mimes.clear();
            }
            wl_data_device::Event::Enter { serial, id, .. } => {
                state.drag_entered = Some(serial);
                if id.is_none() {
                    state.offer = None;
                }
            }
            wl_data_device::Event::Leave => state.drag_entered = None,
            wl_data_device::Event::Drop => state.dropped = true,
            _ => {}
        }
    }

    event_created_child!(App, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for App {
    fn event(
        state: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            state.offer_mimes.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, ()> for App {
    fn event(
        state: &mut Self,
        _: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let mut file = std::fs::File::from(fd);
                file.write_all(&state.drag_payload).unwrap();
            }
            wl_data_source::Event::DndFinished => state.drag_finished = true,
            wl_data_source::Event::Cancelled => state.drag_cancelled = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut Self,
        surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            surface.ack_configure(serial);
            state.layer_size = Some((width, height));
        }
    }
}

delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: WlRegion);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore XdgWmDialogV1);
delegate_noop!(App: ignore XdgDialogV1);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlBuffer);
delegate_noop!(App: WlShmPool);
delegate_noop!(App: ignore ZxdgDecorationManagerV1);
delegate_noop!(App: ignore SilicaGlassManagerV1);
delegate_noop!(App: ignore SilicaGlassV1);
delegate_noop!(App: ignore SilicaWindowManagerV1);
delegate_noop!(App: ignore SilicaWindowV1);
delegate_noop!(App: XdgPositioner);
delegate_noop!(App: ignore SilicaMenuManagerV1);

/// How a popup menu ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupResult {
    Selected(u32),
    Dismissed,
}

impl Dispatch<SilicaPopupMenuV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &SilicaPopupMenuV1,
        event: silica_popup_menu_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.popup_results.push(match event {
            silica_popup_menu_v1::Event::Selected { id } => PopupResult::Selected(id),
            silica_popup_menu_v1::Event::Dismissed => PopupResult::Dismissed,
            _ => return,
        });
    }
}

impl Dispatch<SilicaMenuV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &SilicaMenuV1,
        event: silica_menu_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let silica_menu_v1::Event::Activated { id } = event {
            state.menu_activations.push(id);
        }
    }
}

impl Dispatch<XdgPopup, ()> for App {
    fn event(
        state: &mut Self,
        _: &XdgPopup,
        event: xdg_popup::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_popup::Event::PopupDone => state.popup_done = true,
            xdg_popup::Event::Configure {
                x,
                y,
                width,
                height,
            } => state.popup_configures.push((x, y, width, height)),
            _ => {}
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure {
            mode: wayland_client::WEnum::Value(mode),
        } = event
        {
            state.decoration_mode = Some(mode);
        }
    }
}

impl Dispatch<ZxdgExportedV2, ()> for App {
    fn event(
        state: &mut Self,
        _: &ZxdgExportedV2,
        event: zxdg_exported_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_exported_v2::Event::Handle { handle } = event {
            state.exported_handle = Some(handle);
        }
    }
}

delegate_noop!(App: ignore ZxdgExporterV2);
delegate_noop!(App: ignore ZxdgImporterV2);
delegate_noop!(App: ignore ZxdgImportedV2);

impl Dispatch<XdgToplevel, ()> for App {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Close => state.close_requested = true,
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                state.configured_size = Some((width, height));
                state.configure_count += 1;
                let resizing = xdg_toplevel::State::Resizing as u32;
                state.configured_resizing = states
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|c| u32::from_ne_bytes(*c) == resizing);
                let maximized = xdg_toplevel::State::Maximized as u32;
                state.configured_maximized = states
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|c| u32::from_ne_bytes(*c) == maximized);
                let fullscreen = xdg_toplevel::State::Fullscreen as u32;
                state.configured_fullscreen = states
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|c| u32::from_ne_bytes(*c) == fullscreen);
            }
            _ => {}
        }
    }
}

/// The keyboard events a window received since it opened.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KeyLog {
    /// The window has the keyboard now.
    pub inside: bool,
    pub enters: u32,
    pub keys: u32,
}

/// The pointer events a window received since it opened.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PointerLog {
    /// The pointer is on the window now.
    pub inside: bool,
    pub enters: u32,
    pub motions: u32,
    pub presses: u32,
    pub axes: u32,
}

/// A window open on the compositor; it stays alive as long as this value exists.
pub struct TestWindow {
    queue: EventQueue<App>,
    app: App,
    surface: WlSurface,
    /// None for a layer surface ([`TestWindow::open_overlay`]).
    xdg_surface: Option<XdgSurface>,
    /// An open popup: its surface, xdg role objects and buffer.
    popup: Option<(WlSurface, XdgSurface, XdgPopup, WlBuffer)>,
    /// A second popup of the window, grabbed after `popup` (not hanging from it).
    sibling_popup: Option<(WlSurface, XdgSurface, XdgPopup, WlBuffer)>,
    /// A popup hanging from `popup` (a submenu).
    child_popup: Option<(WlSurface, XdgSurface, XdgPopup, WlBuffer)>,
    /// The glass object of `popup`.
    popup_glass: Option<SilicaGlassV1>,
    _toplevel: Option<XdgToplevel>,
    _layer: Option<ZwlrLayerSurfaceV1>,
    _decoration: Option<ZxdgToplevelDecorationV1>,
    glass: Option<SilicaGlassV1>,
    appearance: Option<SilicaWindowV1>,
    /// The app's menu object (silica-menu-v1), after the first `set_menu_model`.
    menu: Option<SilicaMenuV1>,
    /// xdg-foreign: this window exported, or another app's window imported as its parent.
    exported: Option<ZxdgExportedV2>,
    imported: Option<ZxdgImportedV2>,
    /// A second window of this client with the first as its parent ([`TestWindow::open_child`]).
    child: Option<(
        WlSurface,
        XdgSurface,
        XdgToplevel,
        Option<XdgDialogV1>,
        WlBuffer,
    )>,
    buffer: WlBuffer,
}

/// A glass region for `TestWindow::set_glass`: x, y, width, height, radius,
/// material.
pub type Region = (i32, i32, i32, i32, f64, silica_glass_v1::Material);

thread_local! {
    /// Version of silica-glass-v1 that the windows of this thread (a test) ask for.
    pub static GLASS_VERSION: std::cell::Cell<u32> = const { std::cell::Cell::new(4) };
    /// The app_id of the windows this thread opens (none if empty).
    pub static APP_ID: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    /// An xdg-foreign handle: the windows this thread opens import it as their parent before
    /// their title and app id (none if empty).
    pub static FOREIGN_PARENT: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

impl TestWindow {
    /// Opens a `WINDOW` window filled with `RED_BGRA` and waits for the compositor to receive it.
    pub fn open(socket: &Path, title: &str) -> Self {
        Self::open_sized(socket, title, WINDOW)
    }

    /// Like `open`, with a size of choice (in pixels, at scale 1).
    pub fn open_sized(socket: &Path, title: &str, size: (u32, u32)) -> Self {
        Self::open_with(socket, title, size, false)
    }

    /// A window asking the compositor for decorations (`xdg-decoration`).
    pub fn open_decorated(socket: &Path, title: &str, size: (u32, u32)) -> Self {
        Self::open_with(socket, title, size, true)
    }

    /// A window with pixels of choice (premultiplied BGRA, rows from the top).
    #[allow(dead_code)]
    pub fn open_pixels(socket: &Path, title: &str, size: (u32, u32), pixels: &[u8]) -> Self {
        Self::open_full(socket, title, size, false, pixels)
    }

    fn open_with(socket: &Path, title: &str, (w, h): (u32, u32), decorated: bool) -> Self {
        let pixels = RED_BGRA.repeat((w * h) as usize);
        Self::open_full(socket, title, (w, h), decorated, &pixels)
    }

    fn open_full(
        socket: &Path,
        title: &str,
        (w, h): (u32, u32),
        decorated: bool,
        pixels: &[u8],
    ) -> Self {
        let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut app = App::default();
        conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut app).unwrap();

        let compositor = app.compositor.clone().expect("wl_compositor missing");
        let shm = app.shm.clone().expect("wl_shm missing");
        let wm_base = app.wm_base.clone().expect("xdg_wm_base missing");

        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        let parent = FOREIGN_PARENT.with(|handle| handle.borrow().clone());
        let imported = (!parent.is_empty()).then(|| {
            let imported = app
                .importer
                .clone()
                .expect("zxdg_importer_v2 missing")
                .import_toplevel(parent, &qh, ());
            imported.set_parent_of(&surface);
            imported
        });
        toplevel.set_title(title.to_owned());
        let app_id = APP_ID.with(|id| id.borrow().clone());
        if !app_id.is_empty() {
            toplevel.set_app_id(app_id);
        }
        let decoration = decorated.then(|| {
            app.decorations
                .clone()
                .expect("zxdg_decoration_manager_v1 missing")
                .get_toplevel_decoration(&toplevel, &qh, ())
        });
        surface.commit();
        while !app.configured {
            queue.blocking_dispatch(&mut app).unwrap();
        }

        let stride = w * 4;
        let mut file = tempfile::tempfile().unwrap();
        assert_eq!(pixels.len(), (w * h * 4) as usize, "window pixels");
        file.write_all(pixels).unwrap();
        let pool = shm.create_pool(file.as_fd(), (stride * h) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.commit();
        queue.roundtrip(&mut app).unwrap();
        Self {
            queue,
            app,
            surface,
            xdg_surface: Some(xdg_surface),
            popup: None,
            child_popup: None,
            sibling_popup: None,
            popup_glass: None,
            _toplevel: Some(toplevel),
            _layer: None,
            _decoration: decoration,
            glass: None,
            appearance: None,
            menu: None,
            exported: None,
            imported,
            child: None,
            buffer,
        }
    }

    /// A full-screen overlay layer surface (like the shell's screenshot layer), transparent,
    /// that takes the pointer only in `input` (x, y, width, height, surface coordinates),
    /// filled there with `RED_BGRA`.
    #[allow(dead_code)]
    pub fn open_overlay(socket: &Path, input: (i32, i32, i32, i32)) -> Self {
        let (ix, iy, iw, ih) = input;
        Self::open_layer(
            socket,
            zwlr_layer_shell_v1::Layer::Overlay,
            "test-overlay",
            Some(input),
            |x, y, _, _| {
                let inside =
                    (ix..ix + iw).contains(&(x as i32)) && (iy..iy + ih).contains(&(y as i32));
                if inside { RED_BGRA } else { [0; 4] }
            },
        )
    }

    /// A full-screen layer surface on `level` with this namespace, painted by `paint(x, y,
    /// width, height)` (BGRA); `input` limits where it takes the pointer.
    #[allow(dead_code)]
    pub fn open_layer(
        socket: &Path,
        level: zwlr_layer_shell_v1::Layer,
        namespace: &str,
        input: Option<(i32, i32, i32, i32)>,
        paint: impl Fn(u32, u32, u32, u32) -> [u8; 4],
    ) -> Self {
        let all = zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Bottom
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right;
        Self::open_layer_at(socket, level, namespace, all, (0, 0), input, paint)
    }

    /// Like `open_layer`, anchored to `anchor` with `size` (0 stretches between the anchors),
    /// as a bar or a dock.
    #[allow(dead_code)]
    pub fn open_layer_at(
        socket: &Path,
        level: zwlr_layer_shell_v1::Layer,
        namespace: &str,
        anchor: zwlr_layer_surface_v1::Anchor,
        size: (u32, u32),
        input: Option<(i32, i32, i32, i32)>,
        paint: impl Fn(u32, u32, u32, u32) -> [u8; 4],
    ) -> Self {
        let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut app = App::default();
        conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut app).unwrap();
        let compositor = app.compositor.clone().expect("wl_compositor missing");
        let shm = app.shm.clone().expect("wl_shm missing");
        let surface = compositor.create_surface(&qh, ());
        let layer = app
            .layer_shell
            .clone()
            .expect("zwlr_layer_shell_v1 missing")
            .get_layer_surface(&surface, None, level, namespace.to_owned(), &qh, ());
        layer.set_anchor(anchor);
        layer.set_size(size.0, size.1);
        layer.set_exclusive_zone(-1);
        surface.commit();
        while app.layer_size.is_none() {
            queue.blocking_dispatch(&mut app).unwrap();
        }
        let (w, h) = app.layer_size.unwrap();
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&paint(x, y, w, h));
            }
        }
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&pixels).unwrap();
        let pool = shm.create_pool(file.as_fd(), (w * h * 4) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        if let Some((ix, iy, iw, ih)) = input {
            let region = compositor.create_region(&qh, ());
            region.add(ix, iy, iw, ih);
            surface.set_input_region(Some(&region));
        }
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.commit();
        queue.roundtrip(&mut app).unwrap();
        Self {
            queue,
            app,
            surface,
            xdg_surface: None,
            popup: None,
            child_popup: None,
            sibling_popup: None,
            popup_glass: None,
            _toplevel: None,
            _layer: Some(layer),
            _decoration: None,
            glass: None,
            appearance: None,
            menu: None,
            exported: None,
            imported: None,
            child: None,
            buffer,
        }
    }

    /// Opens a `size` popup with a grab (an app's menu) at `at` in the window, as after a
    /// press on it (the serial of the last press; the window must have been clicked).
    #[allow(dead_code)]
    pub fn open_grabbing_popup(&mut self, at: (i32, i32), (w, h): (u32, u32)) {
        self.queue.roundtrip(&mut self.app).unwrap();
        let qh = self.queue.handle();
        let compositor = self.app.compositor.clone().expect("wl_compositor");
        let shm = self.app.shm.clone().expect("wl_shm");
        let wm_base = self.app.wm_base.clone().expect("xdg_wm_base");
        let seat = self.app.seat.clone().expect("wl_seat");
        let serial = self.app.press_serial.expect("no press on the window");
        let positioner = wm_base.create_positioner(&qh, ());
        positioner.set_size(w as i32, h as i32);
        positioner.set_anchor_rect(at.0, at.1, 1, 1);
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let popup = xdg_surface.get_popup(self.xdg_surface.as_ref(), &positioner, &qh, ());
        popup.grab(&seat, serial);
        surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&RED_BGRA.repeat((w * h) as usize)).unwrap();
        let pool = shm.create_pool(file.as_fd(), (w * h * 4) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        self.popup = Some((surface, xdg_surface, popup, buffer));
    }

    /// Opens a second popup with a grab, beside the first one (it does not hang from it): an
    /// app opening another menu while one is open.
    #[allow(dead_code)]
    pub fn open_sibling_grabbing_popup(&mut self, at: (i32, i32), (w, h): (u32, u32)) {
        self.queue.roundtrip(&mut self.app).unwrap();
        let qh = self.queue.handle();
        let compositor = self.app.compositor.clone().expect("wl_compositor");
        let shm = self.app.shm.clone().expect("wl_shm");
        let wm_base = self.app.wm_base.clone().expect("xdg_wm_base");
        let seat = self.app.seat.clone().expect("wl_seat");
        let serial = self.app.press_serial.expect("no press on the window");
        let positioner = wm_base.create_positioner(&qh, ());
        positioner.set_size(w as i32, h as i32);
        positioner.set_anchor_rect(at.0, at.1, 1, 1);
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let popup = xdg_surface.get_popup(self.xdg_surface.as_ref(), &positioner, &qh, ());
        popup.grab(&seat, serial);
        surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&RED_BGRA.repeat((w * h) as usize)).unwrap();
        let pool = shm.create_pool(file.as_fd(), (w * h * 4) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        self.sibling_popup = Some((surface, xdg_surface, popup, buffer));
    }

    /// Opens a popup as `spec` says (the first one hangs from the window, the second from the
    /// first: a submenu), drawn red. See `popup_configures`.
    #[allow(dead_code)]
    pub fn open_popup(&mut self, spec: PopupSpec) {
        self.queue.roundtrip(&mut self.app).unwrap();
        let qh = self.queue.handle();
        let compositor = self.app.compositor.clone().expect("wl_compositor");
        let shm = self.app.shm.clone().expect("wl_shm");
        let wm_base = self.app.wm_base.clone().expect("xdg_wm_base");
        let seat = self.app.seat.clone().expect("wl_seat");
        let (w, h) = spec.size;
        let positioner = wm_base.create_positioner(&qh, ());
        positioner.set_size(w as i32, h as i32);
        let (ax, ay, aw, ah) = spec.anchor_rect;
        positioner.set_anchor_rect(ax, ay, aw as i32, ah as i32);
        positioner.set_anchor(xdg_positioner::Anchor::BottomLeft);
        positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
        positioner.set_constraint_adjustment(
            xdg_positioner::ConstraintAdjustment::from_bits_truncate(spec.adjustment),
        );
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let popup = match (&self.popup, &self._layer) {
            (Some((_, parent, _, _)), _) if spec.nested => {
                xdg_surface.get_popup(Some(parent), &positioner, &qh, ())
            }
            // The popup of a layer surface (silica-os/silica-desktop#222): no xdg parent, the layer
            // adopts it.
            (_, Some(layer)) => {
                let popup = xdg_surface.get_popup(None, &positioner, &qh, ());
                layer.get_popup(&popup);
                popup
            }
            _ => {
                let parent = self.xdg_surface.as_ref().expect("an xdg window");
                xdg_surface.get_popup(Some(parent), &positioner, &qh, ())
            }
        };
        if spec.grab {
            let serial = self.app.press_serial.expect("no press on the window");
            popup.grab(&seat, serial);
        }
        surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        let mut file = tempfile::tempfile().unwrap();
        let fill = if spec.transparent { [0; 4] } else { RED_BGRA };
        file.write_all(&fill.repeat((w * h) as usize)).unwrap();
        let pool = shm.create_pool(file.as_fd(), (w * h * 4) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        let opened = (surface, xdg_surface, popup, buffer);
        if spec.nested {
            self.child_popup = Some(opened);
        } else {
            self.popup = Some(opened);
        }
    }

    /// Declares the glass regions of the popup and commits (a menu on compositor glass).
    #[allow(dead_code)]
    pub fn set_popup_glass(&mut self, regions: &[Region]) -> Result<(), String> {
        let qh = self.queue.handle();
        let manager = self
            .app
            .glass
            .clone()
            .expect("silica_glass_manager_v1 missing");
        let surface = self.popup.as_ref().expect("no popup").0.clone();
        let glass = self
            .popup_glass
            .get_or_insert_with(|| manager.get_glass(&surface, &qh, ()));
        glass.clear();
        for &(x, y, w, h, radius, material) in regions {
            glass.add_region(x, y, w, h, radius, material);
        }
        surface.commit();
        self.queue
            .roundtrip(&mut self.app)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The `xdg_popup.configure` events so far (x, y, width, height), relative to the parent.
    #[allow(dead_code)]
    pub fn popup_configures(&mut self) -> Vec<(i32, i32, i32, i32)> {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.popup_configures.clone()
    }

    /// Destroys the nested popup (a submenu closing itself).
    #[allow(dead_code)]
    pub fn close_child_popup(&mut self) {
        if let Some((surface, xdg_surface, popup, _)) = self.child_popup.take() {
            popup.destroy();
            xdg_surface.destroy();
            surface.destroy();
        }
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// The compositor closed the popup.
    #[allow(dead_code)]
    pub fn popup_done(&mut self) -> bool {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.popup_done
    }

    /// The keyboard events received so far (after a roundtrip).
    #[allow(dead_code)]
    pub fn key_log(&mut self) -> KeyLog {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.key_log
    }

    /// The pointer events received so far (after a roundtrip).
    #[allow(dead_code)]
    pub fn pointer_log(&mut self) -> PointerLog {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.pointer_log
    }

    /// A new shm buffer with `pixels` (premultiplied BGRA, rows from the top, `WINDOW` size),
    /// committed with only `damage` (x, y, width, height in buffer pixels) as its damage: what
    /// the embedder does for a frame that differs from the one before in one rectangle.
    #[allow(dead_code)]
    pub fn repaint(&mut self, pixels: &[u8], damage: Option<(i32, i32, i32, i32)>) {
        let (w, h) = WINDOW;
        let stride = w * 4;
        let qh = self.queue.handle();
        let shm = self.app.shm.clone().expect("wl_shm missing");
        let mut file = tempfile::tempfile().unwrap();
        assert_eq!(pixels.len(), (w * h * 4) as usize, "window pixels");
        file.write_all(pixels).unwrap();
        let pool = shm.create_pool(file.as_fd(), (stride * h) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        self.surface.attach(Some(&buffer), 0, 0);
        if let Some((x, y, dw, dh)) = damage {
            self.surface.damage_buffer(x, y, dw, dh);
        }
        self.surface.commit();
        self.buffer = buffer;
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Recommits the same buffer `count` times, every `every`: frames at a known pace.
    #[allow(dead_code)]
    pub fn commit_frames(&mut self, count: u32, every: std::time::Duration) {
        let (w, h) = WINDOW;
        for _ in 0..count {
            std::thread::sleep(every);
            self.surface.attach(Some(&self.buffer), 0, 0);
            self.surface.damage_buffer(0, 0, w as i32, h as i32);
            self.surface.commit();
            self.queue.flush().unwrap();
        }
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Recommits the same buffer `count` times, every `every`, damaging only `(x, y, w, h)`: a
    /// spinner's frames.
    #[allow(dead_code)]
    pub fn commit_damaged(
        &mut self,
        count: u32,
        every: std::time::Duration,
        (x, y, w, h): (i32, i32, i32, i32),
    ) {
        for _ in 0..count {
            std::thread::sleep(every);
            self.surface.attach(Some(&self.buffer), 0, 0);
            self.surface.damage_buffer(x, y, w, h);
            self.surface.commit();
            self.queue.flush().unwrap();
        }
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Declares the glass regions and commits. Error if the compositor refused the requests
    /// (protocol error: the client is disconnected).
    #[allow(dead_code)]
    pub fn set_glass(&mut self, regions: &[Region]) -> Result<(), String> {
        let qh = self.queue.handle();
        let manager = self
            .app
            .glass
            .clone()
            .expect("silica_glass_manager_v1 missing");
        let glass = self
            .glass
            .get_or_insert_with(|| manager.get_glass(&self.surface, &qh, ()));
        glass.clear();
        for &(x, y, w, h, radius, material) in regions {
            glass.add_region(x, y, w, h, radius, material);
        }
        self.surface.commit();
        self.queue
            .roundtrip(&mut self.app)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Like `set_glass`, with an opacity per region (`add_translucent_region`, version 3).
    #[allow(dead_code)]
    pub fn set_translucent_glass(&mut self, regions: &[(Region, f64)]) -> Result<(), String> {
        let qh = self.queue.handle();
        let manager = self
            .app
            .glass
            .clone()
            .expect("silica_glass_manager_v1 missing");
        let glass = self
            .glass
            .get_or_insert_with(|| manager.get_glass(&self.surface, &qh, ()));
        glass.clear();
        for &((x, y, w, h, radius, material), opacity) in regions {
            glass.add_translucent_region(x, y, w, h, radius, material, opacity);
        }
        self.surface.commit();
        self.queue
            .roundtrip(&mut self.app)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Free glass requests (version 4: `separate`, `add_tip`) after a `clear`, then the
    /// commit.
    #[allow(dead_code)]
    pub fn set_glass_with(
        &mut self,
        requests: impl FnOnce(&silica_protocols::glass::client::silica_glass_v1::SilicaGlassV1),
    ) -> Result<(), String> {
        let qh = self.queue.handle();
        let manager = self
            .app
            .glass
            .clone()
            .expect("silica_glass_manager_v1 missing");
        let glass = self
            .glass
            .get_or_insert_with(|| manager.get_glass(&self.surface, &qh, ()));
        glass.clear();
        requests(glass);
        self.surface.commit();
        self.queue
            .roundtrip(&mut self.app)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Full-size content (`silica-window-v1`) with a toolbar `toolbar` tall (0: back to
    /// The compositor's bar), then the commit.
    #[allow(dead_code)]
    pub fn set_fixed_size(&mut self, (w, h): (i32, i32)) {
        self._toplevel.as_ref().unwrap().set_min_size(w, h);
        self._toplevel.as_ref().unwrap().set_max_size(w, h);
        self.surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// The client's minimum content size (`xdg_toplevel.set_min_size`), then the commit.
    #[allow(dead_code)]
    pub fn set_min_size(&mut self, (w, h): (i32, i32)) {
        self._toplevel.as_ref().unwrap().set_min_size(w, h);
        self.surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Asks for full-size content (no title bar from the compositor; the buttons stay over the app
    /// toolbar), then the commit.
    #[allow(dead_code)]
    pub fn set_full_size_content(&mut self, toolbar: u32) {
        let qh = self.queue.handle();
        let manager = self
            .app
            .appearance
            .clone()
            .expect("silica_window_manager_v1 missing");
        let window = self
            .appearance
            .get_or_insert_with(|| manager.get_window(&self.surface, &qh, ()));
        window.set_full_size_content(toolbar);
        self.surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Overrides the system appearance for this window (`silica_window_v1.set_appearance`:
    /// 0 system, 1 light, 2 dark), then the commit.
    #[allow(dead_code)]
    pub fn set_appearance(&mut self, appearance: silica_window_v1::Appearance) {
        let qh = self.queue.handle();
        let manager = self
            .app
            .appearance
            .clone()
            .expect("silica_window_manager_v1 missing");
        let window = self
            .appearance
            .get_or_insert_with(|| manager.get_window(&self.surface, &qh, ()));
        window.set_appearance(appearance);
        self.surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Hides the window as an app does when its last window closes (`silica_window_v1
    /// .set_hidden`, #142): the app keeps running with no window.
    #[allow(dead_code)]
    pub fn set_hidden(&mut self, hidden: bool) {
        let qh = self.queue.handle();
        let manager = self
            .app
            .appearance
            .clone()
            .expect("silica_window_manager_v1 missing");
        let window = self
            .appearance
            .get_or_insert_with(|| manager.get_window(&self.surface, &qh, ()));
        window.set_hidden(u32::from(hidden));
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Asks the compositor to move the window (`xdg_toplevel.move`) with the serial of the last press
    /// received, as the embedder does from the app's toolbar.
    #[allow(dead_code)]
    pub fn start_move(&mut self) {
        self.queue.roundtrip(&mut self.app).unwrap();
        let seat = self.app.seat.clone().expect("wl_seat missing");
        let serial = self.app.press_serial.expect("no press received");
        self._toplevel.as_ref().unwrap()._move(&seat, serial);
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    fn data_device(&mut self) -> WlDataDevice {
        self.queue.roundtrip(&mut self.app).unwrap();
        let qh = self.queue.handle();
        if self.app.data_device.is_none() {
            let manager = self
                .app
                .data_manager
                .clone()
                .expect("data_device_manager missing");
            let seat = self.app.seat.clone().expect("wl_seat missing");
            self.app.data_device = Some(manager.get_data_device(&seat, &qh, ()));
            self.queue.roundtrip(&mut self.app).unwrap();
        }
        self.app.data_device.clone().unwrap()
    }

    /// Gets ready to receive drags (creates the wl_data_device).
    #[allow(dead_code)]
    pub fn accept_drops(&mut self) {
        self.data_device();
    }

    /// Starts a drag (`start_drag`) with the serial of the last press: offers `mime` with
    /// `payload`, and a `side` × `side` icon of a solid color (BGRA).
    #[allow(dead_code)]
    pub fn start_drag(&mut self, mime: &str, payload: &[u8], side: u32, color: [u8; 4]) {
        self.start_drag_at(mime, payload, side, color, (0, 0));
    }

    /// Like `start_drag`, with the icon grabbed at `hotspot` (its pixel that stays under the
    /// pointer: the icon is attached at minus that offset).
    #[allow(dead_code)]
    pub fn start_drag_at(
        &mut self,
        mime: &str,
        payload: &[u8],
        side: u32,
        color: [u8; 4],
        hotspot: (i32, i32),
    ) {
        let device = self.data_device();
        let qh = self.queue.handle();
        let manager = self.app.data_manager.clone().unwrap();
        let source = manager.create_data_source(&qh, ());
        source.offer(mime.to_owned());
        source.set_actions(
            wl_data_device_manager::DndAction::Copy | wl_data_device_manager::DndAction::Move,
        );
        self.app.drag_payload = payload.to_vec();
        let compositor = self.app.compositor.clone().unwrap();
        let shm = self.app.shm.clone().unwrap();
        let icon = compositor.create_surface(&qh, ());
        let stride = side * 4;
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&color.repeat((side * side) as usize))
            .unwrap();
        let pool = shm.create_pool(file.as_fd(), (stride * side) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            side as i32,
            side as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        let serial = self.app.press_serial.expect("no press received");
        device.start_drag(Some(&source), &self.surface, Some(&icon), serial);
        icon.attach(Some(&buffer), -hotspot.0, -hotspot.1);
        icon.damage_buffer(0, 0, side as i32, side as i32);
        icon.commit();
        self.app.drag_source = Some(source);
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Reads the events (for the source: sends the requested data) and tells whether the drag
    /// finished (true) or was cancelled (false); None if it is still in progress.
    #[allow(dead_code)]
    pub fn drag_outcome(&mut self) -> Option<bool> {
        self.queue.roundtrip(&mut self.app).unwrap();
        if self.app.drag_finished {
            Some(true)
        } else if self.app.drag_cancelled {
            Some(false)
        } else {
            None
        }
    }

    /// The drag over this window: the offered types (None if it did not enter).
    #[allow(dead_code)]
    pub fn drag_over(&mut self) -> Option<Vec<String>> {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.drag_entered?;
        Some(self.app.offer_mimes.clone())
    }

    /// Accepts `mime` with copy, as an app does over a valid target.
    #[allow(dead_code)]
    pub fn accept_offer(&mut self, mime: &str) {
        let serial = self.app.drag_entered.expect("no drag");
        let offer = self.app.offer.clone().expect("no offer");
        offer.accept(serial, Some(mime.to_owned()));
        offer.set_actions(
            wl_data_device_manager::DndAction::Copy,
            wl_data_device_manager::DndAction::Copy,
        );
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// After the drop: asks for the data in `mime` (the source writes it on its side with
    /// `pump`, called in between) and closes the offer.
    #[allow(dead_code)]
    pub fn receive_drop(&mut self, mime: &str, pump: impl FnOnce()) -> Vec<u8> {
        self.queue.roundtrip(&mut self.app).unwrap();
        assert!(self.app.dropped, "no drop");
        let offer = self.app.offer.clone().expect("no offer");
        let (mut reader, writer) = std::io::pipe().unwrap();
        offer.receive(mime.to_owned(), std::os::fd::AsFd::as_fd(&writer));
        self.queue.roundtrip(&mut self.app).unwrap();
        drop(writer);
        pump();
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut data).unwrap();
        offer.finish();
        self.queue.roundtrip(&mut self.app).unwrap();
        data
    }

    /// Opens a second window of this client, filled with `RED_BGRA`, whose parent is this
    /// window (`xdg_toplevel.set_parent`); `modal` makes it a modal dialog (xdg-dialog), as
    /// a Vetro app's sheet.
    #[allow(dead_code)]
    pub fn open_child(&mut self, title: &str, (w, h): (u32, u32), modal: bool) {
        let qh = self.queue.handle();
        let compositor = self.app.compositor.clone().expect("wl_compositor missing");
        let shm = self.app.shm.clone().expect("wl_shm missing");
        let wm_base = self.app.wm_base.clone().expect("xdg_wm_base missing");
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_parent(self._toplevel.as_ref());
        toplevel.set_title(title.to_owned());
        let app_id = APP_ID.with(|id| id.borrow().clone());
        if !app_id.is_empty() {
            toplevel.set_app_id(app_id);
        }
        let dialog = modal.then(|| {
            let d = self
                .app
                .dialogs
                .clone()
                .expect("xdg_wm_dialog_v1 missing")
                .get_xdg_dialog(&toplevel, &qh, ());
            d.set_modal();
            d
        });
        surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        self.queue.roundtrip(&mut self.app).unwrap();
        let stride = w * 4;
        let pixels = RED_BGRA.repeat((w * h) as usize);
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&pixels).unwrap();
        let pool = shm.create_pool(file.as_fd(), (stride * h) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        self.child = Some((surface, xdg_surface, toplevel, dialog, buffer));
    }

    /// Exports the window (xdg-foreign v2) and returns its handle.
    #[allow(dead_code)]
    pub fn export(&mut self) -> String {
        let exporter = self.app.exporter.clone().expect("zxdg_exporter_v2 missing");
        let qh = self.queue.handle();
        self.exported = Some(exporter.export_toplevel(&self.surface, &qh, ()));
        while self.app.exported_handle.is_none() {
            self.queue.blocking_dispatch(&mut self.app).unwrap();
        }
        self.app.exported_handle.clone().unwrap()
    }

    /// Imports `handle` and makes it the parent of this window, after it is already on
    /// screen (a parent that arrives late).
    #[allow(dead_code)]
    pub fn set_foreign_parent(&mut self, handle: &str) {
        let importer = self.app.importer.clone().expect("zxdg_importer_v2 missing");
        let qh = self.queue.handle();
        let imported = importer.import_toplevel(handle.to_owned(), &qh, ());
        imported.set_parent_of(&self.surface);
        self.imported = Some(imported);
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Destroys the imported parent handle: the xdg-foreign relationship ends.
    #[allow(dead_code)]
    pub fn drop_imported(&mut self) {
        if let Some(imported) = self.imported.take() {
            imported.destroy();
        }
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Destroys the toplevel (the window closes) leaving the surface alive.
    #[allow(dead_code)]
    pub fn destroy_toplevel(&mut self) {
        self._toplevel.as_ref().unwrap().destroy();
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// The decoration mode the compositor chose, after reading the events.
    #[allow(dead_code)]
    pub fn decoration_mode(&mut self) -> Option<zxdg_toplevel_decoration_v1::Mode> {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.decoration_mode
    }

    /// The last content size requested by the compositor, after reading the events.
    #[allow(dead_code)]
    pub fn configured_size(&mut self) -> Option<(i32, i32)> {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.configured_size
    }

    /// How many toplevel configures arrived so far, after reading the events.
    #[allow(dead_code)]
    pub fn configure_count(&mut self) -> u32 {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.configure_count
    }

    /// Whether the last configure carried the `resizing` state.
    #[allow(dead_code)]
    pub fn resizing(&mut self) -> bool {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.configured_resizing
    }

    /// Whether the last configure carried the `maximized` state.
    #[allow(dead_code)]
    pub fn maximized(&mut self) -> bool {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.configured_maximized
    }

    /// Whether the last configure carried the `fullscreen` state.
    #[allow(dead_code)]
    pub fn fullscreen(&mut self) -> bool {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.configured_fullscreen
    }

    /// `xdg_toplevel.set_fullscreen` / `unset_fullscreen`.
    #[allow(dead_code)]
    pub fn request_fullscreen(&mut self, on: bool) {
        let toplevel = self._toplevel.as_ref().unwrap();
        if on {
            toplevel.set_fullscreen(None);
        } else {
            toplevel.unset_fullscreen();
        }
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// `xdg_toplevel.set_maximized` / `unset_maximized`.
    #[allow(dead_code)]
    pub fn request_maximized(&mut self, on: bool) {
        let toplevel = self._toplevel.as_ref().unwrap();
        if on {
            toplevel.set_maximized();
        } else {
            toplevel.unset_maximized();
        }
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// `xdg_toplevel.set_minimized`.
    #[allow(dead_code)]
    pub fn request_minimized(&mut self) {
        self._toplevel.as_ref().unwrap().set_minimized();
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// From now on the configures wait for `ack_held` (a client slow to answer).
    #[allow(dead_code)]
    pub fn hold_acks(&mut self, hold: bool) {
        self.app.hold_acks = hold;
    }

    /// Acks the configures held so far (the last one wins, as a real client does).
    #[allow(dead_code)]
    pub fn ack_held(&mut self) {
        self.queue.roundtrip(&mut self.app).unwrap();
        if let (Some(serial), Some(xdg)) = (self.app.held.pop(), &self.xdg_surface) {
            xdg.ack_configure(serial);
        }
        self.app.held.clear();
    }

    /// Commits a new red buffer of `w` × `h` (a frame drawn at that size) and waits for
    /// The compositor to process it.
    #[allow(dead_code)]
    pub fn commit_size(&mut self, (w, h): (i32, i32)) {
        let qh = self.queue.handle();
        let shm = self.app.shm.clone().expect("wl_shm missing");
        let (w, h) = (w.max(1), h.max(1));
        let stride = w * 4;
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&RED_BGRA.repeat((w * h) as usize)).unwrap();
        let pool = shm.create_pool(file.as_fd(), stride * h, &qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, w, h);
        self.surface.commit();
        self.buffer = buffer;
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Answers the last configure as a client does: acks it (already done unless held) and
    /// commits a buffer of the size asked. Returns that size.
    #[allow(dead_code)]
    pub fn answer(&mut self) -> (i32, i32) {
        self.ack_held();
        let size = self.app.configured_size.unwrap_or((1, 1));
        self.commit_size(size);
        size
    }

    /// Publishes the app's menus (silica-menu-v1) as this JSON, in a memfd.
    #[allow(dead_code)]
    pub fn set_menu_model(&mut self, json: &str) {
        let qh = self.queue.handle();
        let manager = self
            .app
            .menu_manager
            .clone()
            .expect("silica_menu_manager_v1 missing");
        let menu = self
            .menu
            .get_or_insert_with(|| manager.get_menu(&qh, ()))
            .clone();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(json.as_bytes()).unwrap();
        menu.set_model(file.as_fd(), json.len() as u32);
        // A bad model ends the connection: `disconnected` tells.
        let _ = self.queue.roundtrip(&mut self.app);
    }

    /// The app has no menus any more.
    #[allow(dead_code)]
    pub fn destroy_menu(&mut self) {
        if let Some(menu) = self.menu.take() {
            menu.destroy();
        }
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// The items chosen in the bar since the last call.
    #[allow(dead_code)]
    pub fn menu_activations(&mut self) -> Vec<u32> {
        self.queue.roundtrip(&mut self.app).unwrap();
        std::mem::take(&mut self.app.menu_activations)
    }

    /// Shows a context menu (kind 0) or a pop-up button's menu (1) on this window's surface:
    /// `rows` are built on the new menu object before `show`.
    #[allow(dead_code)]
    pub fn show_popup_menu(
        &mut self,
        kind: u32,
        anchor: (i32, i32, i32, i32),
        selected: u32,
        rows: impl FnOnce(&SilicaPopupMenuV1),
    ) -> SilicaPopupMenuV1 {
        let qh = self.queue.handle();
        let manager = self
            .app
            .menu_manager
            .clone()
            .expect("silica_menu_manager_v1 missing");
        let kind = if kind == 0 {
            silica_menu_manager_v1::PopupKind::Context
        } else {
            silica_menu_manager_v1::PopupKind::PopUp
        };
        let menu = manager.get_popup_menu(
            &self.surface,
            kind,
            anchor.0,
            anchor.1,
            anchor.2,
            anchor.3,
            selected,
            &qh,
            (),
        );
        rows(&menu);
        menu.show();
        // A bad menu ends the connection: `disconnected` tells.
        let _ = self.queue.roundtrip(&mut self.app);
        menu
    }

    /// How the popup menus ended since the last call.
    #[allow(dead_code)]
    pub fn popup_results(&mut self) -> Vec<PopupResult> {
        let _ = self.queue.roundtrip(&mut self.app);
        std::mem::take(&mut self.app.popup_results)
    }

    /// Whether the compositor closed the connection (a protocol error).
    #[allow(dead_code)]
    pub fn disconnected(&mut self) -> bool {
        self.queue.roundtrip(&mut self.app).is_err()
    }

    /// Reads the incoming events and tells whether the compositor asked to close.
    pub fn close_requested(&mut self) -> bool {
        self.queue.roundtrip(&mut self.app).unwrap();
        self.app.close_requested
    }
}
