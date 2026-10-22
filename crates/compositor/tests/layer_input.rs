//! A layer with an empty input region (a hidden shell component, like Faro when closed) must
//! not take the clicks meant for the windows below.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;

use common::Headless;
use common::client::{FIRST_WINDOW_AT, TestWindow};
use serde_json::{Value, json};
use silica_control::commands::{INPUT_CLICK, WINDOW_LIST, WindowInfo};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{self, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, ZwlrLayerSurfaceV1,
};

#[derive(Default)]
struct App {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    size: Option<(u32, u32)>,
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
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
                "wl_compositor" => app.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => app.layer_shell = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
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
            app.size = Some((width, height));
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

delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlBuffer);
delegate_noop!(App: WlShmPool);
delegate_noop!(App: WlRegion);
delegate_noop!(App: ignore ZwlrLayerShellV1);

#[test]
fn a_layer_with_an_empty_input_region_lets_clicks_through() {
    let compositor = Headless::start("layer-input");
    let _first = TestWindow::open(&compositor.wayland, "first");
    let _second = TestWindow::open(&compositor.wayland, "second");

    // Full-screen overlay, transparent, with an empty input region.
    let conn = Connection::from_socket(UnixStream::connect(&compositor.wayland).unwrap()).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut app = App::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut app).unwrap();
    let surface = app.compositor.as_ref().unwrap().create_surface(&qh, ());
    let layer = app.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Overlay,
        "hidden".to_owned(),
        &qh,
        (),
    );
    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    surface.commit();
    while app.size.is_none() {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    let (w, h) = app.size.unwrap();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&vec![0u8; (w * h * 4) as usize]).unwrap();
    let pool = app
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), (w * h * 4) as i32, &qh, ());
    let buffer = pool.create_buffer(
        0,
        w as i32,
        h as i32,
        (w * 4) as i32,
        wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    let empty = app.compositor.as_ref().unwrap().create_region(&qh, ());
    surface.set_input_region(Some(&empty));
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    queue.roundtrip(&mut app).unwrap();

    // The click on the first window (only partly below the second) gives it the focus.
    let at = FIRST_WINDOW_AT as f64;
    compositor
        .client()
        .call(INPUT_CLICK, json!({"x": at + 5.0, "y": at + 30.0}))
        .unwrap();
    let list: Vec<WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    assert_eq!(list[0].title.as_deref(), Some("first"), "{list:?}");
    assert!(list[0].focused);

    // The layer asks for the keyboard after the first commit (Faro opening): it takes it...
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive);
    surface.commit();
    queue.roundtrip(&mut app).unwrap();
    let layers = compositor
        .client()
        .call(silica_control::commands::LAYER_LIST, Value::Null)
        .unwrap();
    assert_eq!(layers[0]["focused"], true, "{layers}");
    // ...and releases it: back to the top window.
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    surface.commit();
    queue.roundtrip(&mut app).unwrap();
    let list: Vec<WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    assert!(list[0].focused, "{list:?}");
}
