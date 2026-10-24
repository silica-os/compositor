//! The real-session backend: The compositor on a TTY (or in a virtual machine) without a host
//! compositor. libseat opens the devices and switches VT, udev finds the GPU and its
//! connectors, libinput feeds the input paths. Each display has Smithay's `DrmCompositor`: the
//! frame is drawn straight into a GBM swapchain buffer (GLES on a GPU, `compositor-raster`
//! without one; see [`drm_renderer`]), the cursor goes on its own plane and a fullscreen
//! client's buffer straight to the display when the hardware can.
//!
//! One output for now: the first connected connector with its preferred mode (`State` has
//! one `Output`). When connectors change (hotplug), the output moves to the first connected
//! one; with none connected nothing is drawn until one comes back.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use compositor_raster::CpuRenderer;
use compositor_render::renderer::SilicaRenderer;
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType};
use smithay::backend::egl::EGLDevice;
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::Bind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, LoopHandle, channel};
use smithay::reexports::drm::control::atomic::AtomicModeReq;
use smithay::reexports::drm::control::{
    AtomicCommitFlags, Device as ControlDevice, ModeTypeFlags, connector, crtc, plane, property,
};
use smithay::reexports::drm::{ClientCapability, Device as _};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{DeviceFd, Transform};
use tracing::{info, warn};

use crate::backend::Backend;
use crate::state::{Compositor, State};

/// When no page flip is pending, the interval at which the loop checks for a new frame
/// (animations, frame callbacks): a frame with no damage is not queued and brings no vblank.
const IDLE_FRAME: Duration = Duration::from_micros(16_667);
/// Ticks in a row with nothing drawn before the output counts as idle: from then on a tick
/// builds a frame only when something happened (`State::wake`), so a still desktop costs
/// nothing (#202).
const QUIET_TICKS: u32 = 3;
/// While idle, how often a tick still builds a frame: a change no event announced (a timed
/// layer placement) shows within this.
const IDLE_RECHECK: Duration = Duration::from_millis(250);

/// A queued frame whose vblank has not come after this is taken as lost: clients get their
/// frame callbacks from the ticks again.
const FLIP_STALL: Duration = Duration::from_millis(100);

/// What a frame tick knows to decide whether to build a frame.
#[derive(Debug, Clone, Copy)]
struct Tick {
    /// Something happened since the last tick (`State::redraw`).
    woken: bool,
    /// The compositor is animating (`State::compositor_animating`).
    animating: bool,
    /// The light or dark appearance changed since the last frame.
    appearance_changed: bool,
    /// Ticks in a row that drew nothing.
    quiet: u32,
    /// Since the last frame built.
    since_frame: Duration,
}

/// Whether a tick builds a frame: always while something moves or just changed; on an idle
/// output only when woken, plus a slow recheck.
fn frame_due(tick: Tick) -> bool {
    tick.woken
        || tick.animating
        || tick.appearance_changed
        || tick.quiet < QUIET_TICKS
        || tick.since_frame >= IDLE_RECHECK
}

pub struct DrmBackend {
    pub renderer: SilicaRenderer,
    session: LibSeatSession,
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    /// The `dev_t` of the GPU, to recognise its udev events.
    device_id: u64,
    /// The GPU's render node: client buffers from it can be scanned out directly.
    render_node: Option<DrmNode>,
    fences: FenceWaiter,
    libinput: Libinput,
    /// The pointer devices, to apply the `input.*` settings again when it changes.
    devices: Vec<smithay::reexports::input::Device>,
    surface: Option<OutputSurface>,
    /// The other connected displays, right of the primary one.
    extras: Vec<ExtraOutput>,
    displaced: Vec<Displaced>,
    /// A frame is queued for scanout and waits for its vblank.
    pending: bool,
    /// Frames built in a row with nothing to draw (see [`frame_due`]).
    quiet: u32,
    /// When the last frame was built, and with which appearance.
    last_frame: Instant,
    last_dark: bool,
    /// The default cursor, drawn when no client sets one and there is no theme.
    cursor: crate::cursor::NamedCursor,
    /// The GPU time of the frames, for `perf.render`, when the driver measures it.
    gpu_timer: Option<crate::egl::GpuTimer>,
    /// The frames of the theme already loaded, by name, scale (×100) and frame.
    cursors: std::collections::HashMap<crate::cursor_theme::FrameKey, crate::cursor::NamedCursor>,
}

/// A display beside the primary one: its own output, global and swapchain. The menu bar,
/// the Dock and the lock screen stay on the primary display.
struct ExtraOutput {
    output: Output,
    global: smithay::reexports::wayland_server::backend::GlobalId,
    surface: OutputSurface,
    /// A frame is queued for scanout and waits for its vblank.
    pending: bool,
}

/// A window moved off a display that went away: where it was, to go back when it returns.
struct Displaced {
    window: smithay::desktop::Window,
    display: String,
    location: smithay::utils::Point<i32, smithay::utils::Logical>,
}

/// Puts the frames of one display on screen: the swapchain drawn into, and the planes. The
/// user data of a queued frame is its presentation feedback, given back at vblank.
type OutputCompositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

/// The connector driven by the output, with its swapchain.
struct OutputSurface {
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: smithay::reexports::drm::control::Mode,
    compositor: OutputCompositor,
    /// The primary plane's damage count when it was last drawn: a frame that leaves it as is
    /// only changed planes (the cursor), or shows a client's buffer directly.
    drawn: Option<CommitCounter>,
    /// A frame drawn that waits for the GPU before it is queued ([`FenceWaiter`]): its number
    /// and its presentation feedback.
    waiting: Option<(u64, Option<OutputPresentationFeedback>)>,
    /// The cursor plane's hotspot, on a virtual GPU.
    hotspot: Option<CursorHotspot>,
}

/// The hotspot of a virtual GPU's cursor plane (virtio-gpu, …): its host draws the cursor
/// itself and needs to know where the pointer is in the image. The kernel shows such a plane
/// only to a client that sets it (`DRM_CLIENT_CAP_CURSOR_PLANE_HOTSPOT`); Smithay's commits
/// leave these properties alone, so they are set apart, when the image changes.
struct CursorHotspot {
    fd: DrmDeviceFd,
    plane: plane::Handle,
    x: property::Handle,
    y: property::Handle,
    set: Option<(i32, i32)>,
}

impl CursorHotspot {
    /// The hotspot properties of the surface's cursor plane, if it has them.
    fn of(drm: &DrmDevice, surface: &smithay::backend::drm::DrmSurface) -> Option<Self> {
        let plane = surface.planes().cursor.first()?.handle;
        let properties = drm.get_properties(plane).ok()?;
        let find = |name: &[u8]| {
            properties.iter().find_map(|(property, _)| {
                drm.get_property(*property)
                    .is_ok_and(|info| info.name().to_bytes() == name)
                    .then_some(*property)
            })
        };
        Some(Self {
            fd: drm.device_fd().clone(),
            plane,
            x: find(b"HOTSPOT_X")?,
            y: find(b"HOTSPOT_Y")?,
            set: None,
        })
    }

    /// Sets the hotspot, in the image's pixels, before a frame that shows it. No frame waits
    /// for its flip then, so the commit goes through at once.
    fn show(&mut self, (x, y): (i32, i32)) {
        if self.set == Some((x, y)) {
            return;
        }
        let mut request = AtomicModeReq::new();
        request.add_property(self.plane, self.x, property::Value::SignedRange(x.into()));
        request.add_property(self.plane, self.y, property::Value::SignedRange(y.into()));
        if let Err(err) = self.fd.atomic_commit(AtomicCommitFlags::empty(), request) {
            warn!("cursor hotspot: {err}");
        }
        // Not tried again on each frame: the cursor still works, a few pixels off on the host.
        self.set = Some((x, y));
    }
}

/// Waits for the GPU off the event loop, for the frames whose fence cannot go to the display
/// with them (a driver without native fences): when one is done, the loop queues it
/// (`State::drm_synced`). Nothing in `render_drm` blocks on the GPU.
struct FenceWaiter {
    sender: std::sync::mpsc::Sender<(crtc::Handle, u64, SyncPoint)>,
    /// The number of the last frame handed over.
    last: u64,
}

impl FenceWaiter {
    fn new(handle: &LoopHandle<'static, Compositor>) -> Result<Self, Box<dyn std::error::Error>> {
        let (done, ready) = channel::channel::<(crtc::Handle, u64)>();
        let (sender, frames) = std::sync::mpsc::channel::<(crtc::Handle, u64, SyncPoint)>();
        std::thread::Builder::new()
            .name("drm-fences".to_owned())
            .spawn(move || {
                for (crtc, frame, sync) in frames {
                    // Interrupted or not, the frame goes: a lost fence must not stall the
                    // display.
                    let _ = sync.wait();
                    if done.send((crtc, frame)).is_err() {
                        break;
                    }
                }
            })?;
        handle.insert_source(ready, |event, _, data| {
            if let channel::Event::Msg((crtc, frame)) = event {
                data.state.drm_synced(crtc, frame);
            }
        })?;
        Ok(Self { sender, last: 0 })
    }
}

/// How a frame went to the display.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Queued {
    /// The primary plane was drawn: the time spent drawing and handing it over.
    Composited { draw: Duration, submit: Duration },
    /// Only planes changed: the cursor moved, or (`scanout`) a client's buffer is on the
    /// primary plane.
    Planes { scanout: bool },
}

/// The renderer of the DRM backend, from `SILICA_RENDERER`: `gles`, `cpu` (compositor-raster),
/// or `auto` (the default), GLES on a GPU and the CPU renderer when GLES would run on a software
/// rasterizer (`software`): no shader interpreter, and no JIT, which the compositor is not
/// entitled to (see `main.rs`).
fn drm_renderer(gles: GlesRenderer, software: bool) -> SilicaRenderer {
    match std::env::var("SILICA_RENDERER").as_deref() {
        Ok("gles") => gles.into(),
        Ok("cpu") => CpuRenderer::new().into(),
        _ if software => CpuRenderer::new().into(),
        _ => gles.into(),
    }
}

/// A connected connector with the crtc that can drive it and the mode to use.
#[derive(Debug, Clone, PartialEq)]
struct Choice {
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: smithay::reexports::drm::control::Mode,
    name: String,
    size_mm: (i32, i32),
}

/// The GPU of the seat: the primary one (the boot VGA device), otherwise the first one.
fn gpu_path(seat: &str) -> Result<PathBuf, String> {
    if let Ok(Some(path)) = primary_gpu(seat) {
        return Ok(path);
    }
    all_gpus(seat)
        .map_err(|e| format!("udev: {e}"))?
        .into_iter()
        .next()
        .ok_or_else(|| format!("no GPU on seat {seat}"))
}

/// The preferred mode: the one flagged PREFERRED, otherwise the first (the largest).
fn preferred_mode(
    modes: &[smithay::reexports::drm::control::Mode],
) -> Option<smithay::reexports::drm::control::Mode> {
    modes
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| modes.first())
        .copied()
}

/// The first connected connector with a mode and a usable crtc.
fn choose(drm: &DrmDevice) -> Option<Choice> {
    connected(drm).into_iter().next()
}

/// Every connected connector with a mode, each with a crtc of its own.
fn connected(drm: &DrmDevice) -> Vec<Choice> {
    let device = drm.device_fd();
    let Ok(resources) = device.resource_handles() else {
        return Vec::new();
    };
    let mut list: Vec<Choice> = Vec::new();
    for &handle in resources.connectors() {
        let Ok(info) = device.get_connector(handle, false) else {
            continue;
        };
        if info.state() != connector::State::Connected {
            continue;
        }
        let Some(mode) = preferred_mode(info.modes()) else {
            continue;
        };
        let crtc = info
            .encoders()
            .iter()
            .filter_map(|&encoder| device.get_encoder(encoder).ok())
            .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
            .find(|crtc| drm.crtcs().contains(crtc) && !list.iter().any(|c| c.crtc == *crtc));
        let Some(crtc) = crtc else {
            continue;
        };
        let (w, h) = info.size().unwrap_or((0, 0));
        list.push(Choice {
            connector: handle,
            crtc,
            mode,
            name: format!("{}-{}", info.interface().as_str(), info.interface_id()),
            size_mm: (
                i32::try_from(w).unwrap_or_default(),
                i32::try_from(h).unwrap_or_default(),
            ),
        });
    }
    list
}

/// A Wayland output for a display, in its preferred mode.
fn display_output(choice: &Choice) -> Output {
    let output = Output::new(
        choice.name.clone(),
        PhysicalProperties {
            size: choice.size_mm.into(),
            subpixel: Subpixel::Unknown,
            make: "Silica".to_owned(),
            model: choice.name.clone(),
        },
    );
    let mode = Mode::from(choice.mode);
    output.change_current_state(Some(mode), Some(Transform::Normal), None, None);
    output.set_preferred(mode);
    output
}

/// Draws `elements` on `surface` and queues the frame for scanout: the damage since the
/// swapchain buffer's last frame is drawn straight into it, the cursor and a fullscreen
/// client's buffer may go on planes instead. Nothing when nothing changed.
///
/// A frame whose fence cannot go to the display with it is handed to `fences` and queued when
/// the GPU is done (`State::drm_synced`); the CPU renderer's frames are done already.
fn render_surface(
    renderer: &mut SilicaRenderer,
    surface: &mut OutputSurface,
    fences: &mut FenceWaiter,
    elements: &[crate::render::OutputElement],
    clear_color: [f32; 4],
    feedback: Option<OutputPresentationFeedback>,
) -> Result<Option<Queued>, String> {
    let start = Instant::now();
    let frame = surface
        .compositor
        .render_frame(renderer, elements, clear_color, FrameFlags::DEFAULT)
        .map_err(|e| format!("render: {e}"))?;
    if frame.is_empty {
        return Ok(None);
    }
    let needs_sync = frame.needs_sync();
    let (composited, scanout, sync) = match &frame.primary_element {
        PrimaryPlaneElement::Swapchain(element) => {
            let commit = element.damage.current_commit();
            let composited = surface.drawn != Some(commit);
            surface.drawn = Some(commit);
            (composited, false, needs_sync.then(|| element.sync.clone()))
        }
        PrimaryPlaneElement::Element(_) => {
            surface.drawn = None;
            (false, true, None)
        }
    };
    let drawn = Instant::now();
    let queued = |submit| {
        if composited {
            Queued::Composited {
                draw: drawn - start,
                submit,
            }
        } else {
            Queued::Planes { scanout }
        }
    };
    if let Some(sync) = sync.filter(|sync| !sync.is_reached()) {
        fences.last += 1;
        match fences.sender.send((surface.crtc, fences.last, sync)) {
            Ok(()) => {
                surface.waiting = Some((fences.last, feedback));
                return Ok(Some(queued(drawn.elapsed())));
            }
            // The waiter is gone: the wait happens here.
            Err(std::sync::mpsc::SendError((_, _, sync))) => {
                let _ = sync.wait();
            }
        }
    }
    surface
        .compositor
        .queue_frame(feedback)
        .map_err(|e| format!("queue: {e}"))?;
    Ok(Some(queued(drawn.elapsed())))
}

fn create_surface(
    drm: &mut DrmDevice,
    gbm: &GbmDevice<DrmDeviceFd>,
    render_node: Option<DrmNode>,
    renderer: &SilicaRenderer,
    output: &Output,
    choice: &Choice,
) -> Result<OutputSurface, String> {
    let surface = drm
        .create_surface(choice.crtc, choice.mode, &[choice.connector])
        .map_err(|e| format!("DRM surface on {}: {e}", choice.name))?;
    let hotspot = CursorHotspot::of(drm, &surface);
    // The CPU renderer draws linear buffers: where the display has no modifiers the swapchain
    // is allocated with an implicit one, which must then be linear.
    let mut flags = GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT;
    if renderer.as_gles().is_none() {
        flags |= GbmBufferFlags::LINEAR;
    }
    let allocator = GbmAllocator::new(gbm.clone(), flags);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), render_node);
    let formats = Bind::<Dmabuf>::supported_formats(renderer).unwrap_or_default();
    let compositor = DrmCompositor::new(
        output,
        surface,
        None,
        allocator,
        exporter,
        [Fourcc::Argb8888, Fourcc::Xrgb8888],
        formats,
        drm.cursor_size(),
        Some(gbm.clone()),
    )
    .map_err(|e| format!("DRM compositor on {}: {e}", choice.name))?;
    Ok(OutputSurface {
        connector: choice.connector,
        crtc: choice.crtc,
        mode: choice.mode,
        compositor,
        drawn: None,
        waiting: None,
        hotspot,
    })
}

pub fn init_drm(
    event_loop: &mut EventLoop<'static, Compositor>,
    mut display: Display<State>,
) -> Result<Compositor, Box<dyn std::error::Error>> {
    let (mut session, session_events) =
        LibSeatSession::new().map_err(|e| format!("libseat session: {e}"))?;
    let seat = session.seat();
    let path = gpu_path(&seat)?;
    let fd = session
        .open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    // A virtual GPU shows its cursor plane only to a compositor that sets its hotspot (see
    // `CursorHotspot`), which needs atomic modesetting first; other drivers refuse it and need
    // nothing. Before the device opens: Smithay maps the planes' properties then.
    if fd
        .set_client_capability(ClientCapability::Atomic, true)
        .is_ok()
    {
        let _ = fd.set_client_capability(ClientCapability::CursorPlaneHotspot, true);
    }
    let (mut drm, drm_events) =
        DrmDevice::new(fd.clone(), true).map_err(|e| format!("DRM device: {e}"))?;
    let syncobj_fd = fd.clone();
    let gbm = GbmDevice::new(fd).map_err(|e| format!("GBM device: {e}"))?;
    let mut gles = crate::egl::gbm_renderer(gbm.clone())?;
    let software = EGLDevice::device_for_display(gles.egl_context().display())
        .is_ok_and(|device| device.is_software());
    let gpu_timer = crate::egl::GpuTimer::new(&mut gles);
    let renderer = drm_renderer(gles, software);
    let gpu_timer = gpu_timer.filter(|_| renderer.as_gles().is_some());
    let render_node = DrmNode::from_dev_id(drm.device_id())
        .ok()
        .and_then(|node| node.node_with_type(NodeType::Render))
        .and_then(Result::ok);
    info!(
        "DRM on {} (seat {seat}, software GPU: {software}, {} renderer)",
        path.display(),
        if renderer.as_gles().is_some() {
            "GLES"
        } else {
            "CPU"
        }
    );

    let choice = choose(&drm);
    let (mode, name, size_mm) = match &choice {
        Some(choice) => (Mode::from(choice.mode), choice.name.clone(), choice.size_mm),
        None => {
            warn!("no connected output yet");
            (
                Mode {
                    size: (1280, 800).into(),
                    refresh: 60_000,
                },
                "drm".to_owned(),
                (0, 0),
            )
        }
    };
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: size_mm.into(),
            subpixel: Subpixel::Unknown,
            make: "Silica".to_owned(),
            model: name,
        },
    );
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    let surface = match &choice {
        Some(choice) => Some(create_surface(
            &mut drm,
            &gbm,
            render_node,
            &renderer,
            &output,
            choice,
        )?),
        None => None,
    };

    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    if libinput.udev_assign_seat(&seat).is_err() {
        return Err(format!("libinput: cannot assign seat {seat}").into());
    }
    let device_id = drm.device_id();
    let fences = FenceWaiter::new(&event_loop.handle())?;
    let backend = DrmBackend {
        renderer,
        session,
        drm,
        gbm,
        device_id,
        render_node,
        fences,
        libinput: libinput.clone(),
        devices: Vec::new(),
        surface,
        extras: Vec::new(),
        displaced: Vec::new(),
        pending: false,
        quiet: 0,
        last_frame: Instant::now(),
        last_dark: false,
        cursor: crate::cursor::NamedCursor::arrow(),
        gpu_timer,
        cursors: std::collections::HashMap::new(),
    };
    let mut state = State::new(
        &mut display,
        event_loop.handle(),
        event_loop.get_signal(),
        output,
        Backend::Drm(Box::new(backend)),
    )?;
    state.software_renderer = software;
    state.apply_system_keymap();
    // The other displays already connected.
    state.drm_changed_inner();
    // Explicit sync (linux-drm-syncobj), when the kernel driver can signal eventfds.
    if smithay::wayland::drm_syncobj::supports_syncobj_eventfd(&syncobj_fd) {
        let dh = display.handle();
        state.protocols.syncobj =
            Some(smithay::wayland::drm_syncobj::DrmSyncobjState::new::<State>(&dh, syncobj_fd));
        info!("explicit sync on");
    }
    if let Backend::Drm(backend) = &state.backend {
        let dh = display.handle();
        state.dmabuf_global = Some(crate::dmabuf::create_global(
            &mut state.dmabuf_state,
            &dh,
            &backend.renderer,
            backend.render_node,
        ));
    }

    let handle = event_loop.handle();
    handle.insert_source(session_events, |event, _, data| {
        data.state.wake();
        match event {
            SessionEvent::PauseSession => data.state.drm_pause(),
            SessionEvent::ActivateSession => data.state.drm_activate(),
        }
    })?;
    handle.insert_source(drm_events, |event, _, data| match event {
        DrmEvent::VBlank(crtc) => data.state.drm_vblank(crtc),
        DrmEvent::Error(err) => warn!("DRM: {err}"),
    })?;
    let udev = UdevBackend::new(&seat).map_err(|e| format!("udev: {e}"))?;
    handle.insert_source(udev, |event, _, data| {
        data.state.wake();
        if let UdevEvent::Changed { device_id } = event {
            data.state.drm_changed(device_id);
        }
    })?;
    handle.insert_source(LibinputInputBackend::new(libinput), |event, _, data| {
        data.state.wake();
        match &event {
            InputEvent::DeviceAdded { device } => {
                info!("input device: {}", device.name());
                data.state.input_device_added(device.clone());
            }
            InputEvent::DeviceRemoved { device } => {
                data.state.input_device_removed(device);
            }
            _ => {}
        }
        data.state.process_input_event(event);
    })?;
    handle.insert_source(Timer::from_duration(IDLE_FRAME), |_, _, data| {
        data.state.drm_tick();
        TimeoutAction::ToDuration(IDLE_FRAME)
    })?;
    info!("drm backend ready");
    Ok(Compositor { state, display })
}

/// The make, model and serial from the connector's EDID.
fn edid(
    device: &impl ControlDevice,
    handle: connector::Handle,
) -> Option<(String, String, String)> {
    let properties = device.get_properties(handle).ok()?;
    for (property, value) in &properties {
        let Ok(info) = device.get_property(*property) else {
            continue;
        };
        if info.name().to_bytes() == b"EDID" {
            let blob = device.get_property_blob(*value).ok()?;
            return crate::displays::edid_identity(&blob);
        }
    }
    None
}

/// Whether a libinput device is a touchpad (it can tap) or another pointer (a mouse).
fn is_touchpad(device: &smithay::reexports::input::Device) -> bool {
    device.config_tap_finger_count() > 0
}

fn is_pointer(device: &smithay::reexports::input::Device) -> bool {
    device.has_capability(smithay::reexports::input::DeviceCapability::Pointer)
}

/// Applies the `input.*` settings to one device; a setting left out goes back to libinput's default.
fn configure_device(
    device: &mut smithay::reexports::input::Device,
    config: &crate::config::InputConfig,
) {
    use smithay::reexports::input::ClickMethod as Click;
    let touchpad = is_touchpad(device);
    let settings = if touchpad {
        &config.touchpad
    } else {
        &config.mouse
    };
    if device.config_accel_is_available() {
        let speed = settings
            .speed
            .unwrap_or_else(|| device.config_accel_default_speed());
        let _ = device.config_accel_set_speed(speed);
    }
    if device.config_scroll_has_natural_scroll() {
        let natural = settings
            .natural_scroll
            .unwrap_or_else(|| device.config_scroll_default_natural_scroll_enabled());
        let _ = device.config_scroll_set_natural_scroll_enabled(natural);
    }
    if device.config_left_handed_is_available() {
        let left = settings
            .left_handed
            .unwrap_or_else(|| device.config_left_handed_default());
        let _ = device.config_left_handed_set(left);
    }
    if touchpad {
        let tap = settings
            .tap_to_click
            .unwrap_or_else(|| device.config_tap_default_enabled());
        let _ = device.config_tap_set_enabled(tap);
        let method = match settings.click_method {
            Some(crate::config::ClickMethod::TwoFinger) => Some(Click::Clickfinger),
            Some(_) => Some(Click::ButtonAreas),
            None => device.config_click_default_method(),
        };
        if let Some(method) = method
            && device.config_click_methods().contains(&method)
        {
            let _ = device.config_click_set_method(method);
        }
        if device.config_dwt_is_available() {
            let dwt = settings
                .disable_while_typing
                .unwrap_or_else(|| device.config_dwt_default_enabled());
            let _ = device.config_dwt_set_enabled(dwt);
        }
    }
}

impl State {
    /// A device appeared (at start or plugged in): the `input.*` settings applies to it at once.
    fn input_device_added(&mut self, mut device: smithay::reexports::input::Device) {
        if !is_pointer(&device) {
            return;
        }
        configure_device(&mut device, &self.input_config);
        if let Some(backend) = self.drm_backend() {
            backend.devices.push(device);
        }
        self.report_input_devices();
    }

    fn input_device_removed(&mut self, device: &smithay::reexports::input::Device) {
        if let Some(backend) = self.drm_backend() {
            backend.devices.retain(|d| d != device);
        }
        self.report_input_devices();
    }

    /// The `input.*` settings changed: every pointer device again.
    pub fn configure_input_devices(&mut self) {
        let config = self.input_config.clone();
        if let Some(backend) = self.drm_backend() {
            for device in &mut backend.devices {
                configure_device(device, &config);
            }
        }
    }

    /// Tells silica-daemon which kinds of pointer are connected (Settings shows its Trackpad
    /// pane only with a touchpad).
    fn report_input_devices(&mut self) {
        let (touchpad, mouse) = match self.drm_backend() {
            Some(backend) => (
                backend.devices.iter().any(is_touchpad),
                backend.devices.iter().any(|d| !is_touchpad(d)),
            ),
            None => (false, false),
        };
        self.system
            .report("input.touchpad", serde_json::Value::Bool(touchpad));
        self.system
            .report("input.mouse", serde_json::Value::Bool(mouse));
    }

    fn drm_backend(&mut self) -> Option<&mut DrmBackend> {
        match &mut self.backend {
            Backend::Drm(backend) => Some(backend),
            _ => None,
        }
    }

    /// The session goes away (VT switch): stop drawing and reading input.
    fn drm_pause(&mut self) {
        let Some(backend) = self.drm_backend() else {
            return;
        };
        info!("session paused");
        backend.libinput.suspend();
        backend.drm.pause();
        backend.pending = false;
        for extra in &mut backend.extras {
            extra.pending = false;
        }
    }

    /// The session is back: the device state may have changed, so the buffers start over.
    fn drm_activate(&mut self) {
        let Some(backend) = self.drm_backend() else {
            return;
        };
        info!("session active");
        if backend.libinput.resume().is_err() {
            warn!("libinput: resume failed");
        }
        if let Err(err) = backend.drm.activate(false) {
            warn!("DRM: activate failed: {err}");
        }
        let surfaces = backend
            .surface
            .iter_mut()
            .chain(backend.extras.iter_mut().map(|e| &mut e.surface));
        for surface in surfaces {
            // A frame waiting for the GPU is dropped: the next one is drawn whole.
            surface.waiting = None;
            // Whoever had the display may have moved the hotspot.
            if let Some(hotspot) = &mut surface.hotspot {
                hotspot.set = None;
            }
            if let Err(err) = surface.compositor.reset_state() {
                warn!("DRM: reset: {err}");
            }
        }
        backend.pending = false;
        for extra in &mut backend.extras {
            extra.pending = false;
        }
        self.drm_changed_inner();
        self.render_drm();
    }

    /// The previous frame is on screen: the next one can be queued.
    fn drm_vblank(&mut self, crtc: crtc::Handle) {
        let Some(backend) = self.drm_backend() else {
            return;
        };
        let Some(surface) = &mut backend.surface else {
            return;
        };
        if surface.crtc != crtc {
            if let Some(extra) = backend.extras.iter_mut().find(|e| e.surface.crtc == crtc) {
                if let Err(err) = extra.surface.compositor.frame_submitted() {
                    warn!("DRM: frame submitted on {}: {err}", extra.output.name());
                }
                extra.pending = false;
                self.render_drm();
            }
            return;
        }
        let feedback = match surface.compositor.frame_submitted() {
            Ok(feedback) => feedback.flatten(),
            Err(err) => {
                warn!("DRM: frame submitted: {err}");
                None
            }
        };
        backend.pending = false;
        if let Some(feedback) = feedback {
            self.presented(feedback);
        }
        self.render_drm();
    }

    /// The GPU is done with frame `frame` of the display on `crtc`: it goes to the display,
    /// unless the display started over since.
    fn drm_synced(&mut self, crtc: crtc::Handle, frame: u64) {
        let Some(backend) = self.drm_backend() else {
            return;
        };
        let (surface, pending) = if let Some(surface) = backend
            .surface
            .as_mut()
            .filter(|surface| surface.crtc == crtc)
        {
            (surface, &mut backend.pending)
        } else if let Some(extra) = backend.extras.iter_mut().find(|e| e.surface.crtc == crtc) {
            (&mut extra.surface, &mut extra.pending)
        } else {
            return;
        };
        let Some((_, feedback)) = surface.waiting.take_if(|(number, _)| *number == frame) else {
            return;
        };
        if let Err(err) = surface.compositor.queue_frame(feedback) {
            warn!("DRM: queue: {err}");
            *pending = false;
        }
    }

    /// A udev event: if it is our GPU, its connectors may have changed.
    fn drm_changed(&mut self, device_id: u64) {
        if self.drm_backend().is_some_and(|b| b.device_id == device_id) {
            self.drm_changed_inner();
        }
    }

    /// Follows the connectors: the primary output stays on its display while it is connected,
    /// otherwise it moves to the first one; the others become extra outputs.
    fn drm_changed_inner(&mut self) {
        let output = self.output.clone();
        let Some(backend) = self.drm_backend() else {
            return;
        };
        let mut all = connected(&backend.drm);
        let current = backend.surface.as_ref().map(|s| (s.connector, s.crtc));
        let index = all
            .iter()
            .position(|c| Some((c.connector, c.crtc)) == current)
            .unwrap_or(0);
        let choice = (index < all.len()).then(|| all.remove(index));
        // An extra display that becomes the primary one gives up its swapchain first.
        if let Some(choice) = &choice {
            backend
                .extras
                .retain(|e| e.surface.connector != choice.connector);
        }
        // The same display: it keeps the mode chosen in Settings.
        if choice.as_ref().map(|c| (c.connector, c.crtc)) == current {
            self.drm_sync_extras(all);
            return;
        }
        backend.surface = None;
        backend.pending = false;
        let Some(choice) = choice else {
            info!("no connected output");
            return;
        };
        info!(
            "output {} {}x{}",
            choice.name,
            choice.mode.size().0,
            choice.mode.size().1
        );
        let mode = Mode::from(choice.mode);
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);
        // The layers are arranged for the new size now (see `State::output_resized`).
        smithay::desktop::layer_map_for_output(&output).arrange();
        match create_surface(
            &mut backend.drm,
            &backend.gbm,
            backend.render_node,
            &backend.renderer,
            &output,
            &choice,
        ) {
            Ok(surface) => backend.surface = Some(surface),
            Err(err) => warn!("{err}"),
        }
        // A display plugged in: its saved mode, scale and rotation, and the night light.
        self.apply_displays_config();
        let _ = self.drm_apply_gamma();
        self.drm_sync_extras(all);
    }

    /// The extra displays follow the connectors: new ones get an output right of the others,
    /// the windows of a display that went away move to the primary one, and come back when it
    /// is plugged in again.
    fn drm_sync_extras(&mut self, connected: Vec<Choice>) {
        let dh = self.display_handle.clone();
        let Some(backend) = self.drm_backend() else {
            return;
        };
        let mut removed = Vec::new();
        backend.extras.retain(|extra| {
            let keep = connected
                .iter()
                .any(|c| c.connector == extra.surface.connector && c.crtc == extra.surface.crtc);
            if !keep {
                removed.push((extra.output.clone(), extra.global.clone()));
            }
            keep
        });
        let mut added = Vec::new();
        for choice in &connected {
            if backend
                .extras
                .iter()
                .any(|e| e.surface.connector == choice.connector)
            {
                continue;
            }
            let output = display_output(choice);
            match create_surface(
                &mut backend.drm,
                &backend.gbm,
                backend.render_node,
                &backend.renderer,
                &output,
                choice,
            ) {
                Ok(surface) => {
                    let global = output.create_global::<State>(&dh);
                    info!(
                        "display {} added: {}x{}",
                        choice.name,
                        choice.mode.size().0,
                        choice.mode.size().1
                    );
                    backend.extras.push(ExtraOutput {
                        output: output.clone(),
                        global,
                        surface,
                        pending: false,
                    });
                    added.push(output);
                }
                Err(err) => warn!("{err}"),
            }
        }
        if removed.is_empty() && added.is_empty() {
            return;
        }
        // Windows on a display that goes away: remembered, then moved to the primary one.
        for (output, _) in &removed {
            let Some(area) = self.space.output_geometry(output) else {
                continue;
            };
            let windows: Vec<_> = self
                .space
                .elements()
                .filter_map(|w| {
                    let location = self.space.element_location(w)?;
                    area.contains(location)
                        .then(|| (w.clone(), location - area.loc))
                })
                .collect();
            let primary = self.usable_area();
            for (window, offset) in windows {
                let x = primary.loc.x + offset.x.clamp(0, (primary.size.w - 100).max(0));
                let y = primary.loc.y + offset.y.clamp(0, (primary.size.h - 100).max(0));
                self.space.map_element(window.clone(), (x, y), false);
                if let Some(backend) = self.drm_backend() {
                    backend.displaced.push(Displaced {
                        window,
                        display: output.name(),
                        location: offset,
                    });
                }
            }
        }
        for (output, global) in removed {
            info!("display {} removed", output.name());
            self.space.unmap_output(&output);
            dh.remove_global::<State>(global);
        }
        self.layout_extra_outputs();
        // A display back: its windows go back where they were.
        for output in added {
            let Some(area) = self.space.output_geometry(&output) else {
                continue;
            };
            let Some(backend) = self.drm_backend() else {
                return;
            };
            let name = output.name();
            let (back, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut backend.displaced)
                .into_iter()
                .partition(|d| d.display == name);
            backend.displaced = rest;
            for displaced in back {
                if smithay::utils::IsAlive::alive(&displaced.window) {
                    self.space
                        .map_element(displaced.window, area.loc + displaced.location, false);
                }
            }
        }
        self.redraw = true;
    }

    /// The extra outputs side by side, right of the primary one, top edges aligned.
    fn layout_extra_outputs(&mut self) {
        let outputs: Vec<Output> = match self.drm_backend() {
            Some(backend) => backend.extras.iter().map(|e| e.output.clone()).collect(),
            None => return,
        };
        let mut x = self
            .space
            .output_geometry(&self.output)
            .map(|g| g.loc.x + g.size.w)
            .unwrap_or(0);
        for output in outputs {
            self.space.map_output(&output, (x, 0));
            x += self
                .space
                .output_geometry(&output)
                .map(|g| g.size.w)
                .unwrap_or(0);
        }
    }

    /// Every connected connector, the driven one enabled.
    pub fn drm_outputs(&mut self) -> Option<Vec<crate::displays::OutputInfo>> {
        let scale = self.output.current_scale().fractional_scale();
        let transform = self.output.current_transform();
        let backend = self.drm_backend()?;
        let device = backend.drm.device_fd();
        let resources = device.resource_handles().ok()?;
        let driven = backend.surface.as_ref().map(|s| (s.connector, s.mode));
        let extras: Vec<_> = backend
            .extras
            .iter()
            .map(|e| {
                (
                    e.surface.connector,
                    e.surface.mode,
                    e.output.current_scale().fractional_scale(),
                    e.output.current_transform(),
                )
            })
            .collect();
        let mut list = Vec::new();
        for &handle in resources.connectors() {
            let Ok(info) = device.get_connector(handle, false) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            let preferred = preferred_mode(info.modes());
            let modes: Vec<crate::displays::ModeInfo> = info
                .modes()
                .iter()
                .map(|m| crate::displays::mode_info(Mode::from(*m), Some(*m) == preferred))
                .collect();
            let (make, model, serial) = edid(device, handle).unwrap_or_default();
            let primary = driven.is_some_and(|(c, _)| c == handle);
            let extra = extras.iter().find(|(c, ..)| *c == handle);
            let enabled = primary || extra.is_some();
            let current = driven
                .filter(|(c, _)| *c == handle)
                .map(|(_, m)| m)
                .or(extra.map(|(_, m, ..)| *m))
                .map(|m| crate::displays::mode_info(Mode::from(m), Some(m) == preferred));
            let (scale, transform) = match extra {
                Some((_, _, scale, transform)) => (*scale, *transform),
                None => (scale, transform),
            };
            let (w, h) = info.size().unwrap_or((0, 0));
            list.push(crate::displays::OutputInfo {
                connector: format!("{}-{}", info.interface().as_str(), info.interface_id()),
                make,
                model,
                serial,
                modes,
                current,
                scale: if enabled { scale } else { 1.0 },
                transform: if enabled {
                    transform
                } else {
                    Transform::Normal
                },
                enabled,
                primary,
                size_mm: (
                    i32::try_from(w).unwrap_or_default(),
                    i32::try_from(h).unwrap_or_default(),
                ),
            });
        }
        Some(list)
    }

    /// The display in use switches to its mode number `index` (in the connector's list).
    pub fn drm_set_mode(&mut self, index: usize) -> Result<(), String> {
        let output = self.output.clone();
        let Some(backend) = self.drm_backend() else {
            return Err("no display".into());
        };
        let Some(surface) = &backend.surface else {
            return Err("no display in use".into());
        };
        let (handle, crtc) = (surface.connector, surface.crtc);
        let info = backend
            .drm
            .device_fd()
            .get_connector(handle, false)
            .map_err(|e| format!("connector: {e}"))?;
        let mode = *info.modes().get(index).ok_or("no such mode")?;
        let choice = Choice {
            connector: handle,
            crtc,
            mode,
            name: format!("{}-{}", info.interface().as_str(), info.interface_id()),
            size_mm: (0, 0),
        };
        backend.surface = None;
        backend.pending = false;
        let smithay_mode = Mode::from(mode);
        output.change_current_state(Some(smithay_mode), None, None, None);
        let created = create_surface(
            &mut backend.drm,
            &backend.gbm,
            backend.render_node,
            &backend.renderer,
            &output,
            &choice,
        );
        backend.surface = Some(created?);
        info!(
            "display {}: {}x{} @ {} mHz",
            choice.name, smithay_mode.size.w, smithay_mode.size.h, smithay_mode.refresh
        );
        self.output_resized();
        let _ = self.drm_apply_gamma();
        Ok(())
    }

    /// Display power: off sets DPMS off on the CRTC (and `render_drm` stops drawing); on draws a
    /// full frame again, which re-enables it.
    pub fn drm_display_power(&mut self, on: bool) {
        let Some(backend) = self.drm_backend() else {
            return;
        };
        let Some(surface) = &mut backend.surface else {
            return;
        };
        if on {
            surface.compositor.reset_buffers();
            backend.pending = false;
            self.render_drm();
        } else {
            surface.waiting = None;
            if let Err(err) = surface.compositor.clear() {
                warn!("display off: {err}");
            }
        }
    }

    /// The colour temperature on the CRTC's gamma ramp; `None` without a display, false if the
    /// driver has no gamma.
    pub fn drm_apply_gamma(&mut self) -> Option<bool> {
        let kelvin = self.color_temperature;
        let backend = self.drm_backend()?;
        let crtc = backend.surface.as_ref()?.crtc;
        let device = backend.drm.device_fd();
        let size = device.get_crtc(crtc).ok()?.gamma_length() as usize;
        if !(2..=4096).contains(&size) {
            return Some(false);
        }
        let (red, green, blue) = crate::displays::gamma_ramp(kelvin, size);
        match device.set_gamma(crtc, &red, &green, &blue) {
            Ok(()) => Some(true),
            Err(err) => {
                warn!("gamma: {err}");
                Some(false)
            }
        }
    }

    /// Switches to another virtual terminal.
    pub fn drm_change_vt(&mut self, vt: i32) {
        if let Some(backend) = self.drm_backend()
            && let Err(err) = backend.session.change_vt(vt)
        {
            warn!("VT {vt}: {err}");
        }
    }

    /// The frame tick: builds a frame only if one may have something to draw (see
    /// [`frame_due`]); an idle output with no event costs nothing.
    pub(crate) fn drm_tick(&mut self) {
        let woken = std::mem::take(&mut self.redraw);
        let animating = self.compositor_animating();
        let dark = self.dark();
        let Some(backend) = self.drm_backend() else {
            return;
        };
        let tick = Tick {
            woken,
            animating,
            appearance_changed: backend.last_dark != dark,
            quiet: backend.quiet,
            since_frame: backend.last_frame.elapsed(),
        };
        if frame_due(tick) {
            self.render_drm();
        }
    }

    /// Draws a frame and queues it for scanout, unless one is already waiting for its
    /// vblank or the session is not active.
    pub fn render_drm(&mut self) {
        let now = Instant::now();
        let display_off = self.display_off;
        let (primary_ready, extras_ready, flipping) = self
            .drm_backend()
            .filter(|backend| !display_off && backend.session.is_active())
            .map_or((false, false, false), |backend| {
                (
                    !backend.pending && backend.surface.is_some(),
                    backend.extras.iter().any(|e| !e.pending),
                    backend.pending && backend.last_frame.elapsed() < FLIP_STALL,
                )
            });
        if !primary_ready && !extras_ready {
            // A frame waits for its vblank, which builds the next one and calls the clients
            // back then: a callback now would only make them draw a frame nobody sees.
            // Nothing is drawn at all (display off, session away): they still get their
            // callbacks, at the tick rate.
            if !flipping {
                self.send_frames();
                self.signal_frame_barriers();
            }
            return;
        }
        // The same frame preparation as the headless backend.
        let speed = self.animation_speed;
        let feedback = self.take_presentation_feedback();
        let glass_quality = self.glass_quality();
        self.closing.retain(|closing| !closing.finished(now, speed));
        self.genies.retain(|genie| !genie.finished(now, speed));
        let buttons = self.buttons_view();
        let alt = self.alt_held();
        let dark = self.dark();
        let drag_icon = self.drag_icon_view();
        let window_veil = self.window_veil_view();
        let pointer = self
            .seat
            .get_pointer()
            .map(|pointer| pointer.current_location());
        let cursor_status = self.effective_cursor();
        let scale = self.output.current_scale().fractional_scale();
        let themed = match &cursor_status {
            smithay::input::pointer::CursorImageStatus::Named(icon) => {
                self.cursor_theme.image(*icon, scale, now)
            }
            _ => None,
        };
        let lock_surfaces = self.lock_wl_surfaces();
        let accessibility = self.accessibility;
        let Backend::Drm(backend) = &mut self.backend else {
            return;
        };
        // The CPU renderer's workers stay awake until the frame is queued.
        let _warm = backend.renderer.cpu().map(|cpu| cpu.warm());
        // Locked: black behind the lock surface, and the plain screen if the locker is gone.
        let clear_color = if lock_surfaces.is_some() {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            self.clear_color
        };
        let context = crate::render::RenderContext {
            buttons,
            alt,
            clear_color,
            animation_speed: speed,
            closing: &self.closing,
            genies: &self.genies,
            now,
            glass_quality,
            glass_level: self.theme_config.glass,
            dark,
            drag_icon,
            window_veil,
            lock_surfaces,
        };
        if let (Some(timer), Some(gles)) = (&mut backend.gpu_timer, backend.renderer.gles()) {
            timer.begin(gles);
        }
        let mut elements = crate::render::output_elements(
            &mut backend.renderer,
            &self.space,
            &self.output,
            &context,
        );
        if let Some((key, image)) = &themed
            && !backend.cursors.contains_key(key)
        {
            backend
                .cursors
                .insert(key.clone(), crate::cursor::NamedCursor::frame(image, scale));
        }
        let mut hotspot = None;
        if let Some(location) = pointer {
            let origin = self
                .space
                .output_geometry(&self.output)
                .map(|g| g.loc)
                .unwrap_or_default();
            let named = themed
                .as_ref()
                .and_then(|(key, _)| backend.cursors.get(key))
                .unwrap_or(&backend.cursor);
            hotspot = crate::cursor::hotspot(&cursor_status, Some(named), scale);
            let cursor = crate::cursor::elements(
                &mut backend.renderer,
                &cursor_status,
                Some(named),
                location - origin.to_f64(),
                scale,
            );
            elements.splice(0..0, cursor);
        }
        let origin = self
            .space
            .output_geometry(&self.output)
            .map(|g| g.loc.to_f64())
            .unwrap_or_default();
        let elements = crate::accessibility::post_process(
            &mut backend.renderer,
            &self.output,
            elements,
            pointer.map(|p| p - origin),
            clear_color,
            &accessibility,
        );
        let mut stages = crate::perf::RenderStages {
            build: now.elapsed(),
            glass: compositor_render::glass::take_times(),
            ..Default::default()
        };
        let mut drawn = false;
        let mut planes = None;
        let mut failed = false;
        // A drawn frame counts in `perf.render` with its stages, one that only changed planes
        // apart.
        let count = |queued: Option<Queued>,
                     drawn: &mut bool,
                     stages: &mut crate::perf::RenderStages,
                     planes: &mut Option<bool>| match queued {
            Some(Queued::Composited { draw, submit }) => {
                *drawn = true;
                stages.draw += draw;
                stages.submit += submit;
            }
            Some(Queued::Planes { scanout }) => {
                *planes = Some(planes.unwrap_or(false) || scanout);
            }
            None => {}
        };
        if primary_ready && let Some(surface) = &mut backend.surface {
            if let (Some(at), Some(plane)) = (hotspot, &mut surface.hotspot) {
                plane.show(at);
            }
            let queued = render_surface(
                &mut backend.renderer,
                surface,
                &mut backend.fences,
                &elements,
                clear_color,
                Some(feedback),
            );
            match queued {
                Ok(queued) => {
                    backend.pending = queued.is_some();
                    backend.quiet = if queued.is_some() {
                        0
                    } else {
                        backend.quiet.saturating_add(1)
                    };
                    count(queued, &mut drawn, &mut stages, &mut planes);
                }
                Err(err) => {
                    backend.quiet = 0;
                    failed = true;
                    warn!("DRM frame: {err}");
                }
            }
        }
        // The extra displays: the same scene from their place in the space, without the lock
        // surface (black while locked).
        let context = crate::render::RenderContext {
            lock_surfaces: context.lock_surfaces.as_ref().map(|_| Vec::new()),
            ..context
        };
        for extra in backend.extras.iter_mut().filter(|e| !e.pending) {
            let building = Instant::now();
            let mut elements = crate::render::output_elements(
                &mut backend.renderer,
                &self.space,
                &extra.output,
                &context,
            );
            if let Some(location) = pointer
                && let Some(area) = self.space.output_geometry(&extra.output)
                && area.to_f64().contains(location)
            {
                let named = themed
                    .as_ref()
                    .and_then(|(key, _)| backend.cursors.get(key))
                    .unwrap_or(&backend.cursor);
                let scale = extra.output.current_scale().fractional_scale();
                if let (Some(at), Some(plane)) = (
                    crate::cursor::hotspot(&cursor_status, Some(named), scale),
                    &mut extra.surface.hotspot,
                ) {
                    plane.show(at);
                }
                let cursor = crate::cursor::elements(
                    &mut backend.renderer,
                    &cursor_status,
                    Some(named),
                    location - area.loc.to_f64(),
                    scale,
                );
                elements.splice(0..0, cursor);
            }
            let origin = self
                .space
                .output_geometry(&extra.output)
                .map(|g| g.loc.to_f64())
                .unwrap_or_default();
            let elements = crate::accessibility::post_process(
                &mut backend.renderer,
                &extra.output,
                elements,
                pointer.map(|p| p - origin),
                clear_color,
                &accessibility,
            );
            stages.build += building.elapsed();
            match render_surface(
                &mut backend.renderer,
                &mut extra.surface,
                &mut backend.fences,
                &elements,
                clear_color,
                None,
            ) {
                Ok(queued) => {
                    extra.pending = queued.is_some();
                    count(queued, &mut drawn, &mut stages, &mut planes);
                }
                Err(err) => {
                    failed = true;
                    warn!("DRM frame on {}: {err}", extra.output.name());
                }
            }
        }
        if let (Some(timer), Some(gles)) = (&mut backend.gpu_timer, backend.renderer.gles()) {
            timer.end(gles, drawn);
            timer.collect(gles, |time| self.perf.gpu_rendered(time));
        }
        let since_frame = now.saturating_duration_since(backend.last_frame);
        backend.last_frame = now;
        backend.last_dark = dark;
        // Nothing reached the display: the clients' callbacks wait for the frame tick, or a
        // client drawing on each callback would spin with the compositor.
        if failed && !drawn && planes.is_none() && since_frame < IDLE_FRAME {
            return;
        }
        if drawn {
            self.perf.rendered(stages);
        } else if let Some(scanout) = planes {
            self.perf.shown_on_planes(scanout);
        }
        self.send_frames();
        self.signal_frame_barriers();
    }
}

#[cfg(test)]
mod tests {
    use super::{IDLE_RECHECK, QUIET_TICKS, Tick, frame_due, preferred_mode};
    use std::time::Duration;

    #[test]
    fn no_modes_no_preferred_mode() {
        assert_eq!(preferred_mode(&[]), None);
    }

    /// A still output: nothing happened, nothing moves, quiet for a while, checked just now.
    fn idle() -> Tick {
        Tick {
            woken: false,
            animating: false,
            appearance_changed: false,
            quiet: QUIET_TICKS,
            since_frame: Duration::from_millis(16),
        }
    }

    #[test]
    fn no_frame_is_built_when_nothing_happened() {
        assert!(!frame_due(idle()));
        assert!(!frame_due(Tick {
            quiet: u32::MAX,
            ..idle()
        }));
    }

    #[test]
    fn an_event_an_animation_or_a_new_appearance_build_a_frame() {
        assert!(frame_due(Tick {
            woken: true,
            ..idle()
        }));
        assert!(frame_due(Tick {
            animating: true,
            ..idle()
        }));
        assert!(frame_due(Tick {
            appearance_changed: true,
            ..idle()
        }));
    }

    #[test]
    fn frames_go_on_for_a_few_ticks_after_the_last_damage() {
        for quiet in 0..QUIET_TICKS {
            assert!(frame_due(Tick { quiet, ..idle() }), "{quiet}");
        }
    }

    #[test]
    fn an_idle_output_is_still_checked_now_and_then() {
        assert!(frame_due(Tick {
            since_frame: IDLE_RECHECK,
            ..idle()
        }));
    }
}
