//! silica-test-client: maps an xdg-shell window filled with one color and redraws it on every
//! frame callback, for checks of the compositor in the VM.
//!
//! ```text
//! silica-test-client [--color RRGGBB] [--size WxH] [--damage WxH] [--scanout] [--seconds N]
//! silica-test-client --scene [--seconds N]
//! silica-test-client --jit
//! ```
//!
//! It prints `configured WxH` when the window is mapped, then `frames N` once per second, and
//! exits after `--seconds` (default: until the window is closed). Each frame damages the whole
//! window, or only a centered `--damage` rectangle, as a spinner would.
//!
//! `--scanout` asks for full screen and draws into a GBM buffer object shared as a dmabuf
//! (linear XRGB8888, on `/dev/dri/renderD128`): the case where the display scans the client's
//! buffer out directly.
//!
//! `--scene` maps the desktop that the CPU frame budget is measured on and drags a window
//! across it for `--seconds` (default 10), then prints what `perf.render` measured
//! (`scene.rs`).
//!
//! `--jit` maps no window: it runs code from anonymous memory, as a JIT does, and exits 0 if
//! the kernel allowed it.

mod scene;

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
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

struct Options {
    scene: bool,
    color: u32,
    size: (i32, i32),
    damage: Option<(i32, i32)>,
    scanout: bool,
    seconds: Option<u64>,
}

/// The render node `--scanout` allocates on: the VM's only GPU.
const RENDER_NODE: &str = "/dev/dri/renderD128";

#[derive(Default)]
struct State {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    dmabuf: Option<ZwpLinuxDmabufV1>,
    wm_base: Option<XdgWmBase>,
    surface: Option<WlSurface>,
    /// The window's buffer, attached again on each frame: a commit without a buffer brings
    /// no damage.
    buffer: Option<WlBuffer>,
    /// With `--scanout`, the GBM buffer object behind `buffer`.
    scanout: Option<gbm::BufferObject<()>>,
    size: (i32, i32),
    damage: Option<(i32, i32)>,
    configured: bool,
    closed: bool,
    frames: u64,
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--jit") {
        return match jit() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("silica-test-client: {err}");
                ExitCode::FAILURE
            }
        };
    }
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("silica-test-client: {err}");
            return ExitCode::from(2);
        }
    };
    let result = if options.scene {
        scene::run(options.seconds.unwrap_or(10))
    } else {
        run(&options)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("silica-test-client: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Writes `mov eax, 42; ret` to anonymous memory, makes it executable and calls it. IPE lets
/// this through; SELinux's `execmem` decides it per domain.
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
fn jit() -> Result<(), String> {
    use rustix::mm::{MapFlags, MprotectFlags, ProtFlags, mmap_anonymous, mprotect, munmap};
    const CODE: [u8; 6] = [0xb8, 42, 0, 0, 0, 0xc3];
    const LEN: usize = 4096;
    // SAFETY: a new private mapping, at an address the kernel picks.
    let page = unsafe {
        mmap_anonymous(
            std::ptr::null_mut(),
            LEN,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
    }
    .map_err(|e| format!("mmap: {e}"))?;
    // SAFETY: the mapping is writable and LEN bytes long, longer than CODE.
    unsafe { std::ptr::copy_nonoverlapping(CODE.as_ptr(), page.cast::<u8>(), CODE.len()) };
    // SAFETY: the mapping is ours and LEN bytes long.
    let result = unsafe { mprotect(page, LEN, MprotectFlags::READ | MprotectFlags::EXEC) }
        .map_err(|e| format!("mprotect to executable: {e}"))
        .map(|()| {
            // SAFETY: the page holds a whole C function that takes nothing and returns 42.
            let code = unsafe {
                std::mem::transmute::<*mut std::ffi::c_void, extern "C" fn() -> u32>(page)
            };
            code()
        });
    // SAFETY: nothing refers to the mapping any more.
    let _ = unsafe { munmap(page, LEN) };
    match result? {
        42 => Ok(()),
        other => Err(format!("the code returned {other}, not 42")),
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn jit() -> Result<(), String> {
    Err("the JIT probe knows only x86-64 code".into())
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        scene: false,
        color: 0xffd0_3030,
        size: (480, 320),
        damage: None,
        scanout: false,
        seconds: None,
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--scene" => options.scene = true,
            "--color" => {
                let text = value()?;
                let rgb = u32::from_str_radix(&text, 16)
                    .ok()
                    .filter(|_| text.len() == 6)
                    .ok_or(format!("invalid color: {text}"))?;
                options.color = 0xff00_0000 | rgb;
            }
            "--size" => options.size = size(&value()?)?,
            "--damage" => options.damage = Some(size(&value()?)?),
            "--scanout" => options.scanout = true,
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

fn size(text: &str) -> Result<(i32, i32), String> {
    text.split_once('x')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .filter(|&(w, h): &(i32, i32)| (1..=8192).contains(&w) && (1..=8192).contains(&h))
        .ok_or(format!("invalid size: {text}"))
}

fn run(options: &Options) -> Result<(), String> {
    let connection =
        Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut state = State {
        size: options.size,
        damage: options.damage,
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
    if options.scanout && state.dmabuf.is_none() {
        return Err("the compositor lacks zwp_linux_dmabuf_v1 version 2".into());
    }
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("Silica test client".into());
    toplevel.set_app_id("dev.frascella.Silica.TestClient".into());
    if options.scanout {
        toplevel.set_fullscreen(None);
    }
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
            if options.scanout {
                draw_dmabuf(&mut state, options.color, &qh)?;
            } else {
                draw(&mut state, options.color, &qh)?;
            }
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
fn draw(state: &mut State, color: u32, qh: &QueueHandle<State>) -> Result<(), String> {
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
    state.buffer = Some(buffer);
    Ok(())
}

/// As `draw`, into a GBM buffer object that the display can scan out, shared as a dmabuf.
fn draw_dmabuf(state: &mut State, color: u32, qh: &QueueHandle<State>) -> Result<(), String> {
    let (Some(dmabuf), Some(surface)) = (&state.dmabuf, &state.surface) else {
        return Ok(());
    };
    let node = std::fs::File::options()
        .read(true)
        .write(true)
        .open(RENDER_NODE)
        .map_err(|e| format!("{RENDER_NODE}: {e}"))?;
    let device = gbm::Device::new(node).map_err(|e| format!("GBM: {e}"))?;
    let (w, h) = state.size;
    let width = u32::try_from(w).map_err(|e| e.to_string())?;
    let height = u32::try_from(h).map_err(|e| e.to_string())?;
    let mut bo = device
        .create_buffer_object::<()>(
            width,
            height,
            gbm::Format::Xrgb8888,
            gbm::BufferObjectFlags::SCANOUT | gbm::BufferObjectFlags::LINEAR,
        )
        .map_err(|e| format!("GBM buffer: {e}"))?;
    bo.map_mut(0, 0, width, height, |map| {
        for pixel in map.buffer_mut().as_chunks_mut::<4>().0 {
            *pixel = color.to_le_bytes();
        }
    })
    .map_err(|e| format!("GBM map: {e}"))?;
    let fd = bo.fd_for_plane(0).map_err(|e| format!("dmabuf: {e}"))?;
    let modifier = u64::from(bo.modifier());
    let params = dmabuf.create_params(qh, ());
    params.add(
        fd.as_fd(),
        0,
        bo.offset(0),
        bo.stride_for_plane(0),
        u32::try_from(modifier >> 32).map_err(|e| e.to_string())?,
        u32::try_from(modifier & 0xffff_ffff).map_err(|e| e.to_string())?,
    );
    let buffer = params.create_immed(
        w,
        h,
        gbm::Format::Xrgb8888 as u32,
        zwp_linux_buffer_params_v1::Flags::empty(),
        qh,
        (),
    );
    params.destroy();
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    surface.frame(qh, ());
    surface.commit();
    state.buffer = Some(buffer);
    state.scanout = Some(bo);
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
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                // `create_immed` came with version 2.
                "zwp_linux_dmabuf_v1" if version >= 2 => {
                    state.dmabuf = Some(registry.bind(name, version.min(3), qh, ()));
                }
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
                let (w, h) = state.size;
                let (dw, dh) = state
                    .damage
                    .map_or((w, h), |(dw, dh)| (dw.min(w), dh.min(h)));
                surface.attach(state.buffer.as_ref(), 0, 0);
                surface.damage_buffer((w - dw) / 2, (h - dh) / 2, dw, dh);
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
delegate_noop!(State: ignore ZwpLinuxDmabufV1);
delegate_noop!(State: ignore ZwpLinuxBufferParamsV1);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlSurface);
