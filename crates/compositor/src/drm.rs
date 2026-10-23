//! The real-session backend: The compositor on a TTY (or in a virtual machine) without a host
//! compositor. libseat opens the devices and switches VT, udev finds the GPU and its
//! connectors, DRM/KMS scans out GBM buffers drawn by the same GLES pipeline as the other
//! backends (`compositor-render`: glass, shadows, corners), libinput feeds the input paths.
//!
//! One output for now: the first connected connector with its preferred mode (`State` has
//! one `Output`). When connectors change (hotplug), the output moves to the first connected
//! one; with none connected nothing is drawn until one comes back.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use compositor_render::renderer::{SilicaFrame, SilicaRenderer, SilicaTarget, SilicaTexture};
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, GbmBufferedSurface};
use smithay::backend::egl::EGLDevice;
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::{
    Bind, BlitFrame, Frame, ImportMem, Offscreen, Renderer, Texture, TextureFilter,
};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::drm::control::{Device as ControlDevice, ModeTypeFlags, connector, crtc};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{Buffer as BufferCoords, DeviceFd, Physical, Rectangle, Size, Transform};
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
    cursor: Option<SilicaTexture>,
    /// The GPU time of the frames, for `perf.render`, when the driver measures it.
    gpu_timer: Option<crate::egl::GpuTimer>,
    /// The frames of the theme already uploaded, by name, scale (×100) and frame, with their
    /// hotspot and the element id the damage tracker follows.
    cursors: std::collections::HashMap<
        crate::cursor_theme::FrameKey,
        (
            SilicaTexture,
            (i32, i32),
            smithay::backend::renderer::element::Id,
        ),
    >,
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

/// The connector driven by the output, with its swapchain.
struct OutputSurface {
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: smithay::reexports::drm::control::Mode,
    /// The user data of a queued frame: its presentation feedback, given back at vblank.
    gbm: GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, Option<OutputPresentationFeedback>>,
    damage_tracker: OutputDamageTracker,
    /// What the damage tracker draws into, copied into each scanout buffer (`render_surface`).
    canvas: Option<SilicaTexture>,
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

/// Renders `elements` into the canvas of `surface` with the damage since the last frame, then
/// copies the canvas into the next scanout buffer and queues it. It gives the time spent
/// drawing and submitting, nothing when nothing changed.
///
/// The damage tracker draws into the canvas, a texture that keeps its contents from frame to
/// frame (age 1), so a frame repaints only what changed. The scanout buffers are not trusted
/// to keep theirs: on the dev VM's software GPU they do not survive as the swapchain age
/// promises (silica-os/silica-desktop#233), so each one gets the whole canvas, a plain copy.
fn render_surface(
    renderer: &mut SilicaRenderer,
    surface: &mut OutputSurface,
    elements: &[crate::render::OutputElement],
    clear_color: [f32; 4],
    feedback: Option<OutputPresentationFeedback>,
) -> Result<Option<(Duration, Duration)>, String> {
    let start = Instant::now();
    let (w, h) = surface.mode.size();
    let size = Size::<i32, BufferCoords>::from((i32::from(w), i32::from(h)));
    let format = surface.gbm.format();
    let fresh = surface
        .canvas
        .as_ref()
        .is_none_or(|c| c.size() != size || c.format() != Some(format));
    if fresh {
        let canvas: SilicaTexture = renderer
            .create_buffer(format, size)
            .map_err(|e| format!("canvas: {e}"))?;
        surface.canvas = Some(canvas);
    }
    let Some(canvas) = surface.canvas.as_mut() else {
        return Err("no canvas".to_string());
    };
    // Declared before the canvas is bound: both bindings live as long as the copy's frame.
    let mut dmabuf;
    let mut canvas = renderer
        .bind(canvas)
        .map_err(|e| format!("bind canvas: {e}"))?;
    let result = surface
        .damage_tracker
        .render_output(
            renderer,
            &mut canvas,
            usize::from(!fresh),
            elements,
            clear_color,
        )
        .map_err(|e| format!("render: {e:?}"))?;
    let Some(damage) = result.damage.cloned() else {
        return Ok(None);
    };
    let drawn = Instant::now();
    (dmabuf, _) = surface
        .gbm
        .next_buffer()
        .map_err(|e| format!("next buffer: {e}"))?;
    let mut target = renderer
        .bind(&mut dmabuf)
        .map_err(|e| format!("bind: {e}"))?;
    let whole = Rectangle::from_size(Size::<i32, Physical>::from((size.w, size.h)));
    let sync = {
        let mut frame = renderer
            .render(&mut canvas, whole.size, Transform::Normal)
            .map_err(|e| format!("copy frame: {e}"))?;
        match (&mut frame, &mut target) {
            (SilicaFrame::Gles(frame), SilicaTarget::Gles(target)) => frame
                .blit_to(target, whole, whole, TextureFilter::Nearest)
                .map_err(|e| format!("copy: {e}"))?,
            _ => return Err("copy: the canvas needs GLES".to_owned()),
        }
        frame.finish().map_err(|e| format!("copy finish: {e}"))?
    };
    surface
        .gbm
        .queue_buffer(Some(sync), Some(damage), feedback)
        .map_err(|e| format!("queue: {e}"))?;
    Ok(Some((drawn - start, drawn.elapsed())))
}

fn create_surface(
    drm: &mut DrmDevice,
    gbm: &GbmDevice<DrmDeviceFd>,
    renderer: &SilicaRenderer,
    output: &Output,
    choice: &Choice,
) -> Result<OutputSurface, String> {
    let surface = drm
        .create_surface(choice.crtc, choice.mode, &[choice.connector])
        .map_err(|e| format!("DRM surface on {}: {e}", choice.name))?;
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let formats = Bind::<Dmabuf>::supported_formats(renderer).unwrap_or_default();
    let gbm = GbmBufferedSurface::new(
        surface,
        allocator,
        &[Fourcc::Argb8888, Fourcc::Xrgb8888],
        formats,
    )
    .map_err(|e| format!("GBM swapchain on {}: {e}", choice.name))?;
    Ok(OutputSurface {
        connector: choice.connector,
        crtc: choice.crtc,
        mode: choice.mode,
        gbm,
        damage_tracker: OutputDamageTracker::from_output(output),
        canvas: None,
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
    let (mut drm, drm_events) =
        DrmDevice::new(fd.clone(), true).map_err(|e| format!("DRM device: {e}"))?;
    let syncobj_fd = fd.clone();
    let gbm = GbmDevice::new(fd).map_err(|e| format!("GBM device: {e}"))?;
    let mut gles = crate::egl::gbm_renderer(gbm.clone())?;
    let software = EGLDevice::device_for_display(gles.egl_context().display())
        .is_ok_and(|device| device.is_software());
    let gpu_timer = crate::egl::GpuTimer::new(&mut gles);
    let renderer = SilicaRenderer::from(gles);
    info!(
        "DRM on {} (seat {seat}, software rendering: {software})",
        path.display()
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
        Some(choice) => Some(create_surface(&mut drm, &gbm, &renderer, &output, choice)?),
        None => None,
    };

    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    if libinput.udev_assign_seat(&seat).is_err() {
        return Err(format!("libinput: cannot assign seat {seat}").into());
    }
    let device_id = drm.device_id();
    let backend = DrmBackend {
        renderer,
        session,
        drm,
        gbm,
        device_id,
        libinput: libinput.clone(),
        devices: Vec::new(),
        surface,
        extras: Vec::new(),
        displaced: Vec::new(),
        pending: false,
        quiet: 0,
        last_frame: Instant::now(),
        last_dark: false,
        cursor: None,
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
        if let Some(surface) = &mut backend.surface {
            surface.gbm.reset_buffers();
        }
        backend.pending = false;
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
                if let Err(err) = extra.surface.gbm.frame_submitted() {
                    warn!("DRM: frame submitted on {}: {err}", extra.output.name());
                }
                extra.pending = false;
                self.render_drm();
            }
            return;
        }
        let feedback = match surface.gbm.frame_submitted() {
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
        let output = self.output.clone();
        let Some(backend) = self.drm_backend() else {
            return;
        };
        let Some(surface) = &mut backend.surface else {
            return;
        };
        if on {
            surface.gbm.reset_buffers();
            surface.damage_tracker = OutputDamageTracker::from_output(&output);
            backend.pending = false;
            self.render_drm();
        } else if let Err(err) = surface.gbm.surface().clear() {
            warn!("display off: {err}");
        }
    }

    /// The output's scale or rotation changed: the damage tracker follows it.
    pub fn drm_output_changed(&mut self) {
        let output = self.output.clone();
        if let Some(backend) = self.drm_backend()
            && let Some(surface) = &mut backend.surface
        {
            surface.damage_tracker = OutputDamageTracker::from_output(&output);
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
        self.advance_spaces(now);
        self.advance_panorama(now);
        let space_shift = self.space_shift();
        let panorama = self.panorama_view();
        let spaces = (self.workspaces.count(), self.workspaces.active);
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
            space_shift,
            panorama,
            spaces,
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
        if backend.cursor.is_none() {
            backend.cursor = crate::cursor::default_texture(&mut backend.renderer);
        }
        if let Some((key, image)) = &themed
            && !backend.cursors.contains_key(key)
        {
            match backend.renderer.import_memory(
                &image.rgba,
                Fourcc::Abgr8888,
                (image.width as i32, image.height as i32).into(),
                false,
            ) {
                Ok(texture) => {
                    backend.cursors.insert(
                        key.clone(),
                        (
                            texture,
                            image.hotspot,
                            smithay::backend::renderer::element::Id::new(),
                        ),
                    );
                }
                Err(err) => warn!("cursor {}: {err}", key.0),
            }
        }
        if let Some(location) = pointer {
            let origin = self
                .space
                .output_geometry(&self.output)
                .map(|g| g.loc)
                .unwrap_or_default();
            let named = themed
                .as_ref()
                .and_then(|(key, _)| backend.cursors.get(key))
                .map(|(texture, hotspot, id)| (texture, *hotspot, Some(id.clone())))
                .or(backend
                    .cursor
                    .as_ref()
                    .map(|texture| (texture, (0, 0), None)));
            let cursor = crate::cursor::elements(
                &mut backend.renderer,
                &cursor_status,
                named,
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
            ..Default::default()
        };
        let mut drawn = false;
        if primary_ready && let Some(surface) = &mut backend.surface {
            let queued = render_surface(
                &mut backend.renderer,
                surface,
                &elements,
                clear_color,
                Some(feedback),
            );
            match queued {
                Ok(queued) => {
                    backend.pending = queued.is_some();
                    backend.quiet = if let Some((draw, submit)) = queued {
                        drawn = true;
                        stages.draw += draw;
                        stages.submit += submit;
                        0
                    } else {
                        backend.quiet.saturating_add(1)
                    };
                }
                Err(err) => {
                    backend.quiet = 0;
                    warn!("DRM frame: {err}");
                }
            }
        }
        // The extra displays: the same scene from their place in the space, without the lock
        // surface (black while locked) and without Panorama, which lives on the primary one.
        let context = crate::render::RenderContext {
            lock_surfaces: context.lock_surfaces.as_ref().map(|_| Vec::new()),
            panorama: None,
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
                    .map(|(texture, hotspot, id)| (texture, *hotspot, Some(id.clone())))
                    .or(backend
                        .cursor
                        .as_ref()
                        .map(|texture| (texture, (0, 0), None)));
                let cursor = crate::cursor::elements(
                    &mut backend.renderer,
                    &cursor_status,
                    named,
                    location - area.loc.to_f64(),
                    extra.output.current_scale().fractional_scale(),
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
                &elements,
                clear_color,
                None,
            ) {
                Ok(queued) => {
                    extra.pending = queued.is_some();
                    if let Some((draw, submit)) = queued {
                        drawn = true;
                        stages.draw += draw;
                        stages.submit += submit;
                    }
                }
                Err(err) => warn!("DRM frame on {}: {err}", extra.output.name()),
            }
        }
        if let (Some(timer), Some(gles)) = (&mut backend.gpu_timer, backend.renderer.gles()) {
            timer.end(gles, drawn);
            timer.collect(gles, |time| self.perf.gpu_rendered(time));
        }
        backend.last_frame = now;
        backend.last_dark = dark;
        if drawn {
            self.perf.rendered(stages);
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
