//! What a shell layer shows, frame by frame: never a buffer drawn for another size (the Dock
//! in the middle of the screen at session start), the old buffer in its place while the layer
//! resizes, and the glass leaving with the commit that removes it (no residue after a panel
//! closes).

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
use serde_json::Value;
use silica_control::commands::GLASS_LIST;
use silica_protocols::glass::client::silica_glass_manager_v1::SilicaGlassManagerV1;
use silica_protocols::glass::client::silica_glass_v1::{Material, SilicaGlassV1};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{self, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, ZwlrLayerSurfaceV1,
};

const SIZE: (u32, u32) = (320, 200);

/// A the headless compositor whose safety net for layers that never fit (500 ms) does not fire while
/// a test waits for its screenshot on a loaded machine.
fn start(name: &str, size: (u32, u32), extra: &[&str]) -> Headless {
    Headless::start_with_env(
        name,
        size,
        extra,
        &[("SILICA_COMPOSITOR_LAYER_FIT_GRACE_MS", "600000")],
    )
}
const RED: [u8; 4] = [0, 0, 255, 255];
const BLUE: [u8; 4] = [255, 0, 0, 255];

#[derive(Default)]
struct App {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    glass: Option<SilicaGlassManagerV1>,
    /// The last configure.
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
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => app.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => app.layer_shell = Some(registry.bind(name, 1, qh, ())),
                "silica_glass_manager_v1" => {
                    app.glass = Some(registry.bind(name, version.min(3), qh, ()));
                }
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
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore SilicaGlassManagerV1);
delegate_noop!(App: ignore SilicaGlassV1);

/// A layer like the Dock: anchored to the bottom, left and right, `height` tall.
struct Layer {
    app: App,
    queue: EventQueue<App>,
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    glass: Option<SilicaGlassV1>,
}

impl Layer {
    fn open(compositor: &Headless, height: u32) -> Self {
        let conn =
            Connection::from_socket(UnixStream::connect(&compositor.wayland).unwrap()).unwrap();
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut app = App::default();
        conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut app).unwrap();
        let surface = app.compositor.as_ref().unwrap().create_surface(&qh, ());
        let layer = app.layer_shell.as_ref().unwrap().get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Top,
            "dock".to_owned(),
            &qh,
            (),
        );
        layer.set_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_size(0, height);
        surface.commit();
        while app.size.is_none() {
            queue.blocking_dispatch(&mut app).unwrap();
        }
        Self {
            app,
            queue,
            surface,
            layer,
            glass: None,
        }
    }

    /// The size of the last configure.
    fn configured(&self) -> (u32, u32) {
        self.app.size.unwrap()
    }

    /// Attaches a `size` buffer of one color (BGRA bytes); with `glass`, also sets the
    /// regions (`Some(&[])` clears them) in the same commit.
    fn draw(&mut self, size: (u32, u32), color: [u8; 4], glass: Option<&[(i32, i32, i32, i32)]>) {
        let qh = self.queue.handle();
        let (w, h) = size;
        let mut file = tempfile::tempfile().unwrap();
        let pixels: Vec<u8> = color.repeat((w * h) as usize);
        file.write_all(&pixels).unwrap();
        let pool =
            self.app
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
        if let Some(regions) = glass {
            let manager = self.app.glass.clone().unwrap();
            let object = self
                .glass
                .get_or_insert_with(|| manager.get_glass(&self.surface, &qh, ()));
            object.clear();
            for &(x, y, width, height) in regions {
                object.add_region(x, y, width, height, 8.0, Material::Clear);
            }
        }
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, w as i32, h as i32);
        self.surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
    }

    /// Asks for another height, committing without a new buffer.
    fn resize(&mut self, height: u32) {
        self.layer.set_size(0, height);
        self.surface.commit();
        self.queue.roundtrip(&mut self.app).unwrap();
        self.queue.roundtrip(&mut self.app).unwrap();
    }
}

/// Rows of the screenshot with the color (BGRA bytes of the buffer: the PNG is RGBA).
fn rows_with(shot: &[u8], color: [u8; 4]) -> Vec<u32> {
    let rgba = [color[2], color[1], color[0], color[3]];
    (0..SIZE.1)
        .filter(|&y| common::pixel_in(shot, SIZE.0, SIZE.0 / 2, y) == rgba)
        .collect()
}

fn glass_list(compositor: &Headless) -> Value {
    compositor.client().call(GLASS_LIST, Value::Null).unwrap()
}

#[test]
fn a_layer_is_not_drawn_before_its_first_frame_at_the_configured_size() {
    let compositor = start("layer-first-frame", SIZE, &[]);
    let mut dock = Layer::open(&compositor, 50);
    assert_eq!(dock.configured(), (SIZE.0, 50));

    // The size the client guessed before the configure (the Dock's 1280 × 200): not drawn,
    // neither the buffer nor its glass, anywhere.
    dock.draw((200, 80), RED, Some(&[(10, 10, 100, 40)]));
    let shot = common::screenshot(&compositor);
    assert_eq!(rows_with(&shot, RED), Vec::<u32>::new());
    assert!((0..SIZE.0).all(|x| common::pixel(&shot, x, 120)[..3] != [255, 0, 0]));
    assert_eq!(glass_list(&compositor), serde_json::json!([]));

    // The first frame at the configured size: on the bottom 50 rows, with its glass.
    dock.draw((SIZE.0, 50), RED, Some(&[(10, 10, 100, 40)]));
    let shot = common::screenshot(&compositor);
    assert_eq!(rows_with(&shot, RED), (150..200).collect::<Vec<u32>>());
    let list = glass_list(&compositor);
    assert_eq!(list[0]["y"], 150, "{list}");
}

#[test]
fn a_resizing_layer_keeps_its_old_buffer_in_place_until_the_new_one() {
    let compositor = start("layer-resize", SIZE, &[]);
    let mut dock = Layer::open(&compositor, 50);
    dock.draw((SIZE.0, 50), RED, Some(&[(10, 10, 100, 30)]));

    // Taller (the Dock covering the screen for a menu): configured, but its buffer is still
    // the old one. It stays on the bottom rows, with its glass, not at the top of the new
    // geometry.
    dock.resize(120);
    assert_eq!(dock.configured(), (SIZE.0, 120));
    let shot = common::screenshot(&compositor);
    assert_eq!(rows_with(&shot, RED), (150..200).collect::<Vec<u32>>());
    let list = glass_list(&compositor);
    assert_eq!(
        (list[0]["y"].clone(), list[0]["regions"][0]["y"].clone()),
        (150.into(), 10.into()),
        "{list}"
    );

    // The frame at the new size takes its place.
    dock.draw((SIZE.0, 120), BLUE, Some(&[(10, 80, 100, 30)]));
    let shot = common::screenshot(&compositor);
    assert_eq!(rows_with(&shot, RED), Vec::<u32>::new());
    assert_eq!(rows_with(&shot, BLUE), (80..200).collect::<Vec<u32>>());
    let list = glass_list(&compositor);
    assert_eq!(list[0]["y"], 80, "{list}");
}

#[test]
fn the_glass_leaves_with_the_commit_of_the_closing_frame() {
    let compositor = start("layer-glass-close", SIZE, &[]);
    let mut panel = Layer::open(&compositor, 120);
    let clear_color = common::screenshot(&compositor);
    let background = common::pixel(&clear_color, 160, 150);

    // An open panel: content (transparent here) and its glass tile.
    panel.draw((SIZE.0, 120), [0, 0, 0, 0], Some(&[(60, 20, 200, 80)]));
    let shot = common::screenshot(&compositor);
    assert_ne!(
        common::pixel(&shot, 160, 140),
        background,
        "the glass is drawn"
    );
    assert_eq!(glass_list(&compositor).as_array().map(Vec::len), Some(1));

    // The last frame of the closing panel and the glass removal in one commit: on screen
    // nothing of the tile is left, outline and shadow included.
    panel.draw((SIZE.0, 120), [0, 0, 0, 0], Some(&[]));
    let shot = common::screenshot(&compositor);
    for y in 80..200 {
        for x in 0..SIZE.0 {
            assert_eq!(
                common::pixel(&shot, x, y),
                common::pixel(&clear_color, x, y),
                "({x}, {y}) still shows the glass"
            );
        }
    }
    assert_eq!(glass_list(&compositor), serde_json::json!([]));
}

#[test]
fn a_bottom_layer_that_never_draws_at_its_size_stays_at_the_bottom_with_its_glass() {
    // The Dock at session start: it asks for the whole screen height before it ever drew at
    // its configured size, then keeps committing its old height. Once the safety net shows it,
    // its buffer and glass sit against the bottom edge it is anchored to.
    let compositor = Headless::start_with_env(
        "layer-unfit-bottom",
        SIZE,
        &[],
        &[("SILICA_COMPOSITOR_LAYER_FIT_GRACE_MS", "50")],
    );
    let mut dock = Layer::open(&compositor, 50);
    dock.resize(SIZE.1);
    assert_eq!(dock.configured(), SIZE);
    dock.draw((SIZE.0, 50), RED, Some(&[(10, 10, 100, 30)]));
    std::thread::sleep(std::time::Duration::from_millis(200));
    // A commit after the grace: the layer is placed again.
    dock.draw((SIZE.0, 50), RED, Some(&[(10, 10, 100, 30)]));
    let shot = common::screenshot(&compositor);
    assert_eq!(rows_with(&shot, RED), (150..200).collect::<Vec<u32>>());
    let list = glass_list(&compositor);
    assert_eq!(list[0]["y"], 150, "{list}");
}
