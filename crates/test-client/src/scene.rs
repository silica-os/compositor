//! `--scene`: the desktop that the CPU frame budget is measured on, as
//! `crates/compositor/tests/frame_budget.rs` builds it on the host, so the VM checks the same
//! gate (`perf.render` p99 under 2.42 ms with no GPU).
//!
//! It maps a wallpaper with detail, a menu bar and a Dock on Regular glass, and a decorated
//! 640×400 window. Then it drags the window at 60 Hz through the control socket, along the
//! bench's path: each step moves the glass's backdrop and damages a large part of the screen.
//! It prints `scene ready` once everything is mapped and warmed up, then the `perf.render`
//! answer for the dragged frames as one line, `perf {…}`.

use std::io::Write;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use silica_control::Client;
use silica_control::commands::{PERF_RENDER, WINDOW_LIST, WINDOW_MOVE};
use silica_protocols::glass::client::silica_glass_manager_v1::SilicaGlassManagerV1;
use silica_protocols::glass::client::silica_glass_v1::{Material, SilicaGlassV1};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{
    Mode, ZxdgToplevelDecorationV1,
};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, ZwlrLayerSurfaceV1,
};

const MENU_BAR: u32 = 24;
const DOCK: (u32, u32) = (640, 64);
const WINDOW: (u32, u32) = (640, 400);
const TITLE: &str = "Silica scene";
const FRAME: Duration = Duration::from_micros(16_667);

#[derive(Default)]
struct Scene {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    layer_shell: Option<ZwlrLayerShellV1>,
    decorations: Option<ZxdgDecorationManagerV1>,
    glass: Option<SilicaGlassManagerV1>,
    /// The size each layer was configured to: wallpaper, menu bar, Dock.
    layers: [Option<(u32, u32)>; 3],
    window_configured: bool,
    closed: bool,
}

/// Maps the scene, drags the window for `seconds` and prints what `perf.render` measured.
pub fn run(seconds: u64) -> Result<(), String> {
    let frames = seconds
        .checked_mul(60)
        .and_then(|f| u32::try_from(f).ok())
        .ok_or(format!("invalid seconds: {seconds}"))?;
    let connection =
        Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut scene = Scene::default();
    queue.roundtrip(&mut scene).map_err(|e| e.to_string())?;
    let missing = |name: &str| format!("the compositor lacks {name}");
    let compositor = scene.compositor.clone().ok_or(missing("wl_compositor"))?;
    let shm = scene.shm.clone().ok_or(missing("wl_shm"))?;
    let wm_base = scene.wm_base.clone().ok_or(missing("xdg_wm_base"))?;
    let layer_shell = scene
        .layer_shell
        .clone()
        .ok_or(missing("zwlr_layer_shell_v1"))?;
    let decorations = scene
        .decorations
        .clone()
        .ok_or(missing("zxdg_decoration_manager_v1"))?;
    let glass = scene
        .glass
        .clone()
        .ok_or(missing("silica_glass_manager_v1"))?;

    let all = Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right;
    let specs = [
        ("wallpaper", Layer::Background, all, (0, 0)),
        (
            "menu-bar",
            Layer::Top,
            Anchor::Top | Anchor::Left | Anchor::Right,
            (0, MENU_BAR),
        ),
        ("dock", Layer::Top, Anchor::Bottom, DOCK),
    ];
    let mut layers = Vec::new();
    for (index, (namespace, level, anchor, size)) in specs.into_iter().enumerate() {
        let surface = compositor.create_surface(&qh, ());
        let layer =
            layer_shell.get_layer_surface(&surface, None, level, namespace.into(), &qh, index);
        layer.set_anchor(anchor);
        layer.set_size(size.0, size.1);
        layer.set_exclusive_zone(-1);
        surface.commit();
        layers.push((surface, layer));
    }
    let window = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&window, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title(TITLE.into());
    toplevel.set_app_id("dev.frascella.Silica.TestClient".into());
    decorations
        .get_toplevel_decoration(&toplevel, &qh, ())
        .set_mode(Mode::ServerSide);
    window.commit();
    while !(scene.window_configured && scene.layers.iter().all(Option::is_some)) {
        queue
            .blocking_dispatch(&mut scene)
            .map_err(|e| e.to_string())?;
        if scene.closed {
            return Err("the compositor closed the scene".into());
        }
    }

    let buffers = Buffers {
        compositor: &compositor,
        shm: &shm,
        qh: &qh,
    };
    for (index, ((surface, _), size)) in layers.iter().zip(scene.layers).enumerate() {
        let size = size.unwrap_or((1, 1));
        if index == 0 {
            // A wallpaper with detail, so the glass has something to blur.
            buffers.attach(surface, size, |x, y, (w, h)| {
                let r = (x * 255 / w) as u8;
                let g = (y * 255 / h) as u8;
                let b = if (x / 37 + y / 23) % 2 == 0 { 90 } else { 170 };
                [b, g, r, 255]
            })?;
        } else {
            // The bars are all glass: their own pixels are transparent.
            let radius = if index == 1 { 0.0 } else { 18.0 };
            let (w, h) = (to_i32(size.0)?, to_i32(size.1)?);
            glass
                .get_glass(surface, &qh, ())
                .add_region(0, 0, w, h, radius, Material::Regular);
            buffers.attach(surface, size, |_, _, _| [0; 4])?;
        }
    }
    buffers.attach(&window, WINDOW, |_, _, _| [0xec, 0xe8, 0xe8, 0xff])?;
    queue.roundtrip(&mut scene).map_err(|e| e.to_string())?;

    let path = silica_control::default_socket_path().ok_or("XDG_RUNTIME_DIR is not set")?;
    let mut control = Client::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let id = window_id(&mut control)?;
    // Warm up: the first frames fill the caches and the band workers.
    call(
        &mut control,
        WINDOW_MOVE,
        json!({ "id": id, "x": 320, "y": 160 }),
    )?;
    drag(&mut control, &mut queue, &mut scene, id, 60)?;
    println!("scene ready");
    let _ = std::io::stdout().flush();
    call(&mut control, PERF_RENDER, json!({ "reset": true }))?;
    drag(&mut control, &mut queue, &mut scene, id, frames)?;
    // The last frames still on their way.
    std::thread::sleep(FRAME * 4);
    let perf = call(&mut control, PERF_RENDER, json!({ "reset": true }))?;
    println!("perf {perf}");
    Ok(())
}

/// Moves the window once per frame along the bench's path, answering the compositor's pings
/// now and then.
fn drag(
    control: &mut Client,
    queue: &mut EventQueue<Scene>,
    scene: &mut Scene,
    id: u64,
    frames: u32,
) -> Result<(), String> {
    let start = Instant::now();
    for i in 0..frames {
        let t = f64::from(i) / 40.0;
        let x = 320.0 + 260.0 * t.sin();
        let y = 220.0 + 240.0 * (t * 0.7).sin();
        call(
            control,
            WINDOW_MOVE,
            json!({ "id": id, "x": x as i32, "y": y as i32 }),
        )?;
        if i % 30 == 29 {
            queue.roundtrip(scene).map_err(|e| e.to_string())?;
        }
        let next = start + FRAME * (i + 1);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    Ok(())
}

/// The id of the scene's window, once `window.list` shows it.
fn window_id(control: &mut Client) -> Result<u64, String> {
    for _ in 0..50 {
        let list = call(control, WINDOW_LIST, Value::Null)?;
        let id = list
            .as_array()
            .into_iter()
            .flatten()
            .find(|w| w.get("title").and_then(Value::as_str) == Some(TITLE))
            .and_then(|w| w.get("id")?.as_u64());
        if let Some(id) = id {
            return Ok(id);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("window.list does not show the scene's window".into())
}

fn call(control: &mut Client, command: &str, params: Value) -> Result<Value, String> {
    control
        .call(command, params)
        .map_err(|e| format!("{command}: {e}"))
}

fn to_i32(value: u32) -> Result<i32, String> {
    i32::try_from(value).map_err(|e| e.to_string())
}

struct Buffers<'a> {
    compositor: &'a WlCompositor,
    shm: &'a WlShm,
    qh: &'a QueueHandle<Scene>,
}

impl Buffers<'_> {
    /// Attaches a buffer of `size` painted by `paint(x, y, size)` (BGRA, premultiplied) and
    /// commits. An opaque buffer says so, as a well-behaved client does.
    fn attach(
        &self,
        surface: &WlSurface,
        size: (u32, u32),
        paint: impl Fn(u32, u32, (u32, u32)) -> [u8; 4],
    ) -> Result<(), String> {
        let (w, h) = (size.0.max(1), size.1.max(1));
        let pixels: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| paint(x, y, (w, h)))
            .collect();
        let (width, height) = (to_i32(w)?, to_i32(h)?);
        let stride = width.checked_mul(4).ok_or("buffer too wide")?;
        let len = stride.checked_mul(height).ok_or("buffer too large")?;
        let fd = rustix::fs::memfd_create("silica-test-scene", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| e.to_string())?;
        let mut file = std::fs::File::from(fd);
        file.write_all(&pixels).map_err(|e| e.to_string())?;
        let pool = self.shm.create_pool(file.as_fd(), len, self.qh, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            stride,
            wl_shm::Format::Argb8888,
            self.qh,
            (),
        );
        pool.destroy();
        if pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255) {
            let region = self.compositor.create_region(self.qh, ());
            region.add(0, 0, width, height);
            surface.set_opaque_region(Some(&region));
            region.destroy();
        }
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        Ok(())
    }
}

impl Dispatch<WlRegistry, ()> for Scene {
    fn event(
        scene: &mut Self,
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
                "wl_compositor" => scene.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => scene.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => scene.wm_base = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => scene.layer_shell = Some(registry.bind(name, 1, qh, ())),
                "zxdg_decoration_manager_v1" => {
                    scene.decorations = Some(registry.bind(name, 1, qh, ()));
                }
                "silica_glass_manager_v1" => {
                    scene.glass = Some(registry.bind(name, version.min(9), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, usize> for Scene {
    fn event(
        scene: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                if let Some(size) = scene.layers.get_mut(*index) {
                    *size = Some((width, height));
                }
            }
            zwlr_layer_surface_v1::Event::Closed => scene.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Scene {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Scene {
    fn event(
        scene: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            scene.window_configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Scene {
    fn event(
        scene: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            scene.closed = true;
        }
    }
}

delegate_noop!(Scene: ignore WlCompositor);
delegate_noop!(Scene: ignore WlShm);
delegate_noop!(Scene: ignore WlShmPool);
delegate_noop!(Scene: ignore WlBuffer);
delegate_noop!(Scene: ignore WlSurface);
delegate_noop!(Scene: ignore WlRegion);
delegate_noop!(Scene: ignore ZwlrLayerShellV1);
delegate_noop!(Scene: ignore ZxdgDecorationManagerV1);
delegate_noop!(Scene: ignore ZxdgToplevelDecorationV1);
delegate_noop!(Scene: ignore SilicaGlassManagerV1);
delegate_noop!(Scene: ignore SilicaGlassV1);
