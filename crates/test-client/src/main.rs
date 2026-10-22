//! silica-test-client: maps an xdg-shell window filled with one color and redraws it on every
//! frame callback, for checks of the compositor in the VM.
//!
//! ```text
//! silica-test-client [--color RRGGBB] [--size WxH] [--seconds N]
//! ```
//!
//! It prints `configured WxH` when the window is mapped, then `frames N` once per second, and
//! exits after `--seconds` (default: until the window is closed).

use std::io::Write;
use std::os::fd::AsFd;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

struct Options {
    color: u32,
    size: (i32, i32),
    seconds: Option<u64>,
}

#[derive(Default)]
struct State {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    surface: Option<WlSurface>,
    size: (i32, i32),
    configured: bool,
    closed: bool,
    frames: u64,
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("silica-test-client: {err}");
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("silica-test-client: {err}");
            ExitCode::FAILURE
        }
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        color: 0xffd0_3030,
        size: (480, 320),
        seconds: None,
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--color" => {
                let text = value()?;
                let rgb = u32::from_str_radix(&text, 16)
                    .ok()
                    .filter(|_| text.len() == 6)
                    .ok_or(format!("invalid color: {text}"))?;
                options.color = 0xff00_0000 | rgb;
            }
            "--size" => {
                let text = value()?;
                options.size = text
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .filter(|&(w, h): &(i32, i32)| {
                        (1..=8192).contains(&w) && (1..=8192).contains(&h)
                    })
                    .ok_or(format!("invalid size: {text}"))?;
            }
            "--seconds" => {
                let text = value()?;
                options.seconds = Some(
                    text.parse()
                        .map_err(|_| format!("invalid seconds: {text}"))?,
                );
            }
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    Ok(options)
}

fn run(options: &Options) -> Result<(), String> {
    let connection =
        Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut state = State {
        size: options.size,
        ..State::default()
    };
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let (Some(compositor), Some(wm_base)) = (state.compositor.clone(), state.wm_base.clone())
    else {
        return Err("the compositor lacks wl_compositor or xdg_wm_base".into());
    };
    if state.shm.is_none() {
        return Err("the compositor lacks wl_shm".into());
    }
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("Silica test client".into());
    toplevel.set_app_id("dev.frascella.Silica.TestClient".into());
    surface.commit();
    state.surface = Some(surface);

    let start = Instant::now();
    let mut reported = Instant::now();
    let mut drawn = false;
    while !state.closed {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| e.to_string())?;
        if state.configured && !drawn {
            println!("configured {}x{}", state.size.0, state.size.1);
            draw(&state, options.color, &qh)?;
            drawn = true;
        }
        if reported.elapsed() >= Duration::from_secs(1) {
            println!("frames {}", state.frames);
            reported = Instant::now();
        }
        let _ = std::io::stdout().flush();
        if options
            .seconds
            .is_some_and(|s| start.elapsed() >= Duration::from_secs(s))
        {
            break;
        }
    }
    println!("frames {}", state.frames);
    Ok(())
}

/// Attaches a new buffer of the window's size, asks for the next frame and commits.
fn draw(state: &State, color: u32, qh: &QueueHandle<State>) -> Result<(), String> {
    let (Some(shm), Some(surface)) = (&state.shm, &state.surface) else {
        return Ok(());
    };
    let (w, h) = state.size;
    let stride = w * 4;
    let len = usize::try_from(stride * h).map_err(|e| e.to_string())?;
    let fd = rustix::fs::memfd_create("silica-test-client", rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|e| e.to_string())?;
    let mut file = std::fs::File::from(fd);
    let pixels: Vec<u8> = color
        .to_le_bytes()
        .iter()
        .copied()
        .cycle()
        .take(len)
        .collect();
    file.write_all(&pixels).map_err(|e| e.to_string())?;
    let pool = shm.create_pool(file.as_fd(), stride * h, qh, ());
    let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    surface.frame(qh, ());
    surface.commit();
    Ok(())
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
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
                "wl_compositor" => state.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => state.wm_base = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
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

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                state.size = (width, height);
            }
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frames += 1;
            // The next frame: the same surface, damaged again so the compositor keeps sending
            // frame callbacks.
            if let Some(surface) = &state.surface {
                surface.damage_buffer(0, 0, state.size.0, state.size.1);
                surface.frame(qh, ());
                surface.commit();
            }
        }
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlSurface);
