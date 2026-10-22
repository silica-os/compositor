//! The owner of the desktop surface (`desktop-icons:<app id>`, #147) is the frontmost app
//! whenever no other app is, as the Finder on macOS: at the start of the session and after
//! the last window of the frontmost app closes. A window in use is never displaced.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::os::unix::net::UnixStream;
use std::process::Command;

use common::Headless;
use common::client::{APP_ID, TestWindow};
use silica_protocols::shell::client::silica_shell_manager_v1::{self, SilicaShellManagerV1};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{self, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, ZwlrLayerSurfaceV1,
};

const CHILD_SOCKET: &str = "SILICA_TEST_FRONT_SOCKET";
const DESKTOP_APP: &str = "dev.test.Desktop";
const OTHER_APP: &str = "dev.test.Other";
const DOC_APP: &str = "dev.test.Document";

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    manager: Option<SilicaShellManagerV1>,
    frontmost: Option<String>,
    focused: Option<String>,
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        client: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => client.compositor = Some(registry.bind(name, 4, qh, ())),
                "zwlr_layer_shell_v1" => {
                    client.layer_shell = Some(registry.bind(name, 1, qh, ()));
                }
                "silica_shell_manager_v1" => {
                    client.manager = Some(registry.bind(name, 4, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<SilicaShellManagerV1, ()> for Client {
    fn event(
        client: &mut Self,
        _: &SilicaShellManagerV1,
        event: silica_shell_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            silica_shell_manager_v1::Event::Broadcast { topic, value }
                if topic == "app.frontmost" =>
            {
                client.frontmost = Some(value);
            }
            silica_shell_manager_v1::Event::Focused { identifier } => {
                client.focused = Some(identifier);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            surface.ack_configure(serial);
        }
    }
}

delegate_noop!(Client: ignore WlCompositor);
delegate_noop!(Client: ignore WlSurface);
delegate_noop!(Client: ignore ZwlrLayerShellV1);

/// The child process (a shell component): prints the `app.frontmost` it was told on
/// connection. Runs only when the parent test launches it.
#[test]
fn front_child() {
    let Ok(socket) = std::env::var(CHILD_SOCKET) else {
        return;
    };
    let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut client).unwrap();
    queue.roundtrip(&mut client).unwrap();
    println!("FRONTMOST {}", client.frontmost.as_deref().unwrap_or("-"));
    println!("FOCUSED {}", client.focused.as_deref().unwrap_or("-"));
}

/// What a shell component connecting now is told is the frontmost app (`-` if never told).
fn frontmost(compositor: &Headless) -> String {
    told(compositor, "FRONTMOST ")
}

/// What a shell component connecting now is told is the focused window (empty: none).
fn focused(compositor: &Headless) -> String {
    told(compositor, "FOCUSED ")
}

fn told(compositor: &Headless, key: &str) -> String {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "front_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, &compositor.wayland)
        .env("SILICA_SHELL", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.split_once(key).map(|(_, value)| value))
        .unwrap_or_else(|| {
            panic!(
                "the child printed no state: {stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .to_owned()
}

/// A desktop surface of `app` on the bottom layer; it stays mapped while this value lives.
struct Desktop {
    _conn: Connection,
    _layer: ZwlrLayerSurfaceV1,
    _surface: WlSurface,
}

impl Desktop {
    fn map(compositor: &Headless, app: &str) -> Self {
        Self::map_layer(
            compositor,
            zwlr_layer_shell_v1::Layer::Bottom,
            &format!("desktop-icons:{app}"),
            false,
        )
    }

    /// A shell layer covering the output; with `exclusive` it takes the keyboard (an open
    /// menu or panel).
    fn map_layer(
        compositor: &Headless,
        level: zwlr_layer_shell_v1::Layer,
        namespace: &str,
        exclusive: bool,
    ) -> Self {
        let conn =
            Connection::from_socket(UnixStream::connect(&compositor.wayland).unwrap()).unwrap();
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut client = Client::default();
        conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut client).unwrap();
        let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
        let layer = client.layer_shell.as_ref().unwrap().get_layer_surface(
            &surface,
            None,
            level,
            namespace.to_owned(),
            &qh,
            (),
        );
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        if exclusive {
            layer.set_keyboard_interactivity(
                zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive,
            );
        }
        surface.commit();
        queue.roundtrip(&mut client).unwrap();
        Self {
            _conn: conn,
            _layer: layer,
            _surface: surface,
        }
    }
}

#[test]
fn the_desktop_owner_is_frontmost_when_the_desktop_maps() {
    let compositor = Headless::start("front-map");
    assert_ne!(frontmost(&compositor), DESKTOP_APP);
    let _desktop = Desktop::map(&compositor, DESKTOP_APP);
    assert_eq!(frontmost(&compositor), DESKTOP_APP);
}

#[test]
fn the_desktop_does_not_take_the_front_from_a_window_in_use() {
    let compositor = Headless::start("front-no-steal");
    APP_ID.with(|id| *id.borrow_mut() = OTHER_APP.to_owned());
    let _window = TestWindow::open(&compositor.wayland, "in use");
    let _desktop = Desktop::map(&compositor, DESKTOP_APP);
    assert_ne!(frontmost(&compositor), DESKTOP_APP);
}

#[test]
fn closing_the_last_window_falls_back_to_the_desktop_owner() {
    let compositor = Headless::start("front-close");
    let _desktop = Desktop::map(&compositor, DESKTOP_APP);
    APP_ID.with(|id| *id.borrow_mut() = OTHER_APP.to_owned());
    let mut window = TestWindow::open(&compositor.wayland, "closing");
    assert_ne!(frontmost(&compositor), DESKTOP_APP);
    window.destroy_toplevel();
    assert_eq!(frontmost(&compositor), DESKTOP_APP);
}

/// A document app with no window (its Open panel, preview#24): the panel is another app's
/// window; when it goes, the windowless app is frontmost again, not the desktop's owner.
#[test]
fn closing_another_apps_window_gives_the_front_back_to_the_windowless_app() {
    let compositor = Headless::start("front-back");
    let _desktop = Desktop::map(&compositor, DESKTOP_APP);
    APP_ID.with(|id| *id.borrow_mut() = DOC_APP.to_owned());
    let mut document = TestWindow::open(&compositor.wayland, "document");
    document.set_hidden(true);
    assert_eq!(frontmost(&compositor), DOC_APP);
    APP_ID.with(|id| *id.borrow_mut() = OTHER_APP.to_owned());
    let mut panel = TestWindow::open(&compositor.wayland, "Open");
    assert_ne!(frontmost(&compositor), DOC_APP);
    panel.destroy_toplevel();
    assert_eq!(frontmost(&compositor), DOC_APP);
}

#[test]
fn a_window_opened_after_a_desktop_click_is_frontmost_and_focused() {
    let compositor = Headless::start("front-after-desktop");
    let _desktop = Desktop::map(&compositor, DESKTOP_APP);
    // A click on the empty desktop: its owner is frontmost, with the keyboard.
    compositor
        .client()
        .call(
            silica_control::commands::INPUT_CLICK,
            serde_json::json!({"x": 300.0, "y": 190.0}),
        )
        .unwrap();
    assert_eq!(frontmost(&compositor), DESKTOP_APP);
    APP_ID.with(|id| *id.borrow_mut() = OTHER_APP.to_owned());
    let _window = TestWindow::open(&compositor.wayland, "opened");
    assert_eq!(frontmost(&compositor), "");
    assert_ne!(
        focused(&compositor),
        "",
        "the new window does not have the keyboard"
    );
}

#[test]
fn a_window_opened_while_the_desktop_owner_is_frontmost_takes_the_front() {
    let compositor = Headless::start("front-then-window");
    let _desktop = Desktop::map(&compositor, DESKTOP_APP);
    assert_eq!(frontmost(&compositor), DESKTOP_APP);
    APP_ID.with(|id| *id.borrow_mut() = OTHER_APP.to_owned());
    let _window = TestWindow::open(&compositor.wayland, "opened");
    assert_eq!(frontmost(&compositor), "");
    assert_ne!(
        focused(&compositor),
        "",
        "the new window does not have the keyboard"
    );
}

#[test]
fn with_a_shell_layer_holding_the_keyboard_the_window_on_top_stays_the_focused_one() {
    let compositor = Headless::start("front-layer-keyboard");
    APP_ID.with(|id| *id.borrow_mut() = OTHER_APP.to_owned());
    let _window = TestWindow::open(&compositor.wayland, "in use");
    let window = focused(&compositor);
    assert_ne!(window, "");
    // An open menu takes the keyboard: the frontmost app does not change (INPUT-01).
    let _menu = Desktop::map_layer(
        &compositor,
        zwlr_layer_shell_v1::Layer::Top,
        "menubar",
        true,
    );
    assert_eq!(focused(&compositor), window);
    // A window opened while it keeps the keyboard is the frontmost one.
    let _second = TestWindow::open(&compositor.wayland, "second");
    let second = focused(&compositor);
    assert_ne!(second, "");
    assert_ne!(second, window);
}

#[test]
fn with_the_desktop_owner_in_front_and_a_menu_open_no_window_is_focused() {
    let compositor = Headless::start("front-desktop-menu");
    let _desktop = Desktop::map(&compositor, DESKTOP_APP);
    APP_ID.with(|id| *id.borrow_mut() = OTHER_APP.to_owned());
    let _window = TestWindow::open(&compositor.wayland, "behind");
    // A click on the empty desktop: its owner is frontmost.
    compositor
        .client()
        .call(
            silica_control::commands::INPUT_CLICK,
            serde_json::json!({"x": 300.0, "y": 190.0}),
        )
        .unwrap();
    let _menu = Desktop::map_layer(
        &compositor,
        zwlr_layer_shell_v1::Layer::Top,
        "menubar",
        true,
    );
    assert_eq!(frontmost(&compositor), DESKTOP_APP);
    assert_eq!(focused(&compositor), "");
}
