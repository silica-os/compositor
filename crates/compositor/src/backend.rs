//! The compositor's backends: headless (tests and CI) here and, in `drm.rs`, the real session.

use compositor_raster::CpuRenderer;
use compositor_render::renderer::{SilicaRenderer, SilicaTexture};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::{Bind, Offscreen, Texture};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use std::time::{Duration, Instant};

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::Display;
use smithay::utils::{Buffer as BufferCoords, Rectangle, Size, Transform};
use tracing::{info, warn};

use crate::capture::{Capture, capture_elements};
use crate::egl::GpuTimer;
use crate::perf::RenderStages;
use crate::render::{OutputElement, RenderContext, output_elements};
use crate::state::{Compositor, State};

pub enum Backend {
    /// No window: it draws only when a capture is needed (or on every tick, to measure), with
    /// GLES on an EGL device (the GPU or llvmpipe) if there is one, otherwise on the CPU.
    Headless(Headless),
    /// The real session: DRM/KMS, libinput, libseat (`drm.rs`).
    #[cfg(feature = "drm")]
    Drm(Box<crate::drm::DrmBackend>),
}

impl Backend {
    pub fn renderer(&mut self) -> &mut SilicaRenderer {
        match self {
            Backend::Headless(headless) => &mut headless.renderer,
            #[cfg(feature = "drm")]
            Backend::Drm(backend) => &mut backend.renderer,
        }
    }
}

pub struct Headless {
    pub renderer: SilicaRenderer,
    /// Present when every tick is drawn (`SILICA_COMPOSITOR_HEADLESS_FRAMES=1`).
    frames: Option<Box<HeadlessFrames>>,
}

/// Headless drawing on every tick, to measure the renderer without a display: the damage of
/// each frame goes into an offscreen canvas kept from frame to frame (DRM draws only the damage
/// too, into its swapchain buffers), and the frame counts in `perf.render`.
struct HeadlessFrames {
    damage_tracker: OutputDamageTracker,
    canvas: Option<SilicaTexture>,
    cursor: crate::cursor::NamedCursor,
    timer: Option<GpuTimer>,
}

/// The headless renderer, from `SILICA_COMPOSITOR_HEADLESS_RENDERER`: GLES on llvmpipe (default),
/// `gpu` for GLES on the GPU, `cpu` for the CPU renderer. If EGL is missing, the CPU.
fn headless_renderer() -> SilicaRenderer {
    let choice = std::env::var("SILICA_COMPOSITOR_HEADLESS_RENDERER").unwrap_or_default();
    if choice != "cpu" {
        match crate::egl::headless_renderer(choice == "gpu") {
            Ok(renderer) => return renderer.into(),
            Err(err) => warn!("headless without GLES, on the CPU: {err}"),
        }
    }
    CpuRenderer::new().into()
}

/// Refresh rate declared for the headless output, in millihertz.
const HEADLESS_REFRESH_MHZ: i32 = 60_000;
/// Interval of the frame callbacks in headless.
const HEADLESS_FRAME: Duration = Duration::from_micros(16_667);

fn new_output(name: &str, model: &str, mode: Mode, transform: Transform) -> Output {
    let output = Output::new(
        name.to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Silica".to_owned(),
            model: model.to_owned(),
        },
    );
    output.change_current_state(Some(mode), Some(transform), None, Some((0, 0).into()));
    output.set_preferred(mode);
    output
}

pub fn init_headless(
    event_loop: &mut EventLoop<'static, Compositor>,
    mut display: Display<State>,
    (width, height): (i32, i32),
) -> Result<Compositor, Box<dyn std::error::Error>> {
    let mut renderer = headless_renderer();
    let mode = Mode {
        size: (width, height).into(),
        refresh: HEADLESS_REFRESH_MHZ,
    };
    let output = new_output(
        "headless",
        "The compositor headless",
        mode,
        Transform::Normal,
    );
    let frames =
        (std::env::var("SILICA_COMPOSITOR_HEADLESS_FRAMES").as_deref() == Ok("1")).then(|| {
            Box::new(HeadlessFrames {
                damage_tracker: OutputDamageTracker::from_output(&output),
                canvas: None,
                cursor: crate::cursor::NamedCursor::arrow(),
                timer: renderer.gles().and_then(GpuTimer::new),
            })
        });
    let mut state = State::new(
        &mut display,
        event_loop.handle(),
        event_loop.get_signal(),
        output,
        Backend::Headless(Headless { renderer, frames }),
    )?;
    event_loop
        .handle()
        .insert_source(Timer::from_duration(HEADLESS_FRAME), |_, _, data| {
            data.state.render_headless_frame();
            data.state.send_frames();
            let feedback = data.state.take_presentation_feedback();
            data.state.presented(feedback);
            data.state.signal_frame_barriers();
            TimeoutAction::ToDuration(HEADLESS_FRAME)
        })?;
    state.software_renderer = is_software(state.backend.renderer());
    info!("headless backend ready ({width}x{height})");
    Ok(Compositor { state, display })
}

impl State {
    /// Advances the animations to `now` and gathers the scene of the frame: the closing
    /// windows and the genies are borrowed by the caller (`RenderContext { closing, genies,
    /// ..scene }`).
    fn scene_context(&mut self, now: Instant) -> RenderContext<'static> {
        let speed = self.animation_speed;
        self.genies.retain(|genie| !genie.finished(now, speed));
        let glass_quality = self.glass_quality();
        let lock_surfaces = self.lock_wl_surfaces();
        // Locked: black behind the lock surface, and the plain screen if the locker is gone.
        let clear_color = if lock_surfaces.is_some() {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            self.clear_color
        };
        RenderContext {
            buttons: self.buttons_view(),
            alt: self.alt_held(),
            clear_color,
            animation_speed: speed,
            closing: &[],
            genies: &[],
            dock: None,
            now,
            glass_quality,
            glass_level: self.theme_config.glass,
            dark: self.dark(),
            drag_icon: self.drag_icon_view(),
            window_veil: self.window_veil_view(),
            lock_surfaces,
            menu_bar: self.menu_bar_view(),
            dock_shown: self.dock_shown(),
        }
    }

    /// Draws the frame of this tick offscreen, as a display would, when headless frames are on
    /// (`SILICA_COMPOSITOR_HEADLESS_FRAMES=1`): the damage goes into a canvas kept from frame to
    /// frame, and a drawn frame counts in `perf.render`.
    pub fn render_headless_frame(&mut self) {
        if !matches!(
            &self.backend,
            Backend::Headless(Headless {
                frames: Some(_),
                ..
            })
        ) {
            return;
        }
        let now = Instant::now();
        let speed = self.animation_speed;
        self.closing.retain(|closing| !closing.finished(now, speed));
        let scene = self.scene_context(now);
        let clear = scene.clear_color;
        let pointer = self
            .seat
            .get_pointer()
            .map(|pointer| pointer.current_location());
        let cursor_status = self.effective_cursor();
        let scale = self.output.current_scale().fractional_scale();
        let accessibility = self.accessibility;
        let Backend::Headless(Headless {
            renderer,
            frames: Some(frames),
        }) = &mut self.backend
        else {
            return;
        };
        // The CPU renderer's workers stay awake until the frame is drawn.
        let _warm = renderer.cpu().map(|cpu| cpu.warm());
        if let (Some(timer), Some(gles)) = (&mut frames.timer, renderer.gles()) {
            timer.begin(gles);
        }
        let context = RenderContext {
            closing: &self.closing,
            genies: &self.genies,
            dock: Some(&self.dock),
            ..scene
        };
        let mut elements = output_elements(renderer, &self.space, &self.output, &context);
        let origin = self
            .space
            .output_geometry(&self.output)
            .map(|g| g.loc.to_f64())
            .unwrap_or_default();
        if let Some(location) = pointer {
            let cursor = crate::cursor::elements(
                renderer,
                &cursor_status,
                Some(&frames.cursor),
                location - origin,
                scale,
            );
            elements.splice(0..0, cursor);
        }
        let elements = crate::accessibility::post_process(
            renderer,
            &self.output,
            elements,
            pointer.map(|p| p - origin),
            clear,
            &accessibility,
        );
        let mut stages = RenderStages {
            build: now.elapsed(),
            glass: compositor_render::glass::take_times(),
            ..Default::default()
        };
        // The glass's offscreen draws are its own stages: from here on, the frame's.
        let _ = compositor_raster::take_times();
        let drawn = match draw_headless(renderer, frames, &self.output, &elements, clear) {
            Ok(Some((draw, wait))) => {
                stages.draw = draw;
                stages.wait = wait;
                stages.raster = compositor_raster::take_times();
                self.perf.rendered(stages);
                true
            }
            Ok(None) => false,
            Err(err) => {
                warn!("headless frame: {err}");
                false
            }
        };
        if let (Some(timer), Some(gles)) = (&mut frames.timer, renderer.gles()) {
            timer.end(gles, drawn);
            timer.collect(gles, |time| self.perf.gpu_rendered(time));
        }
    }

    /// Draws the current output offscreen and returns it as RGBA.
    pub fn screenshot(&mut self) -> Result<Capture, String> {
        let scene = self.scene_context(Instant::now());
        let clear = scene.clear_color;
        let renderer = self.backend.renderer();
        let context = RenderContext {
            closing: &self.closing,
            genies: &self.genies,
            dock: Some(&self.dock),
            ..scene
        };
        let elements = output_elements(renderer, &self.space, &self.output, &context);
        capture_elements::<_, SilicaTexture, _>(renderer, &self.output, &elements, clear)
    }
}

impl State {
    /// Switches to another virtual terminal (Ctrl+Alt+F1…F12): only the real session has
    /// VTs, the other backends ignore it.
    pub fn change_vt(&mut self, vt: i32) {
        #[cfg(feature = "drm")]
        self.drm_change_vt(vt);
        #[cfg(not(feature = "drm"))]
        let _ = vt;
    }

    /// The snapshot of the closing window, to fade it out.
    pub fn start_closing(&mut self, window: &smithay::desktop::Window) {
        if self.animation_speed <= 0.0 {
            return;
        }
        let dark = self.dark();
        let renderer = self.backend.renderer();
        let now = Instant::now();
        let speed = self.animation_speed;
        self.closing.retain(|closing| !closing.finished(now, speed));
        if let Some(closing) =
            crate::render::snapshot_window(renderer, &self.space, &self.output, window, dark)
        {
            self.closing.push(closing);
        }
    }
}

impl State {
    /// The genie of a window still in the space: towards the Dock (minimize) or from the Dock
    /// (restore, the window just remapped stays hidden until the genie ends).
    /// Nothing with animations off or without GLES.
    pub fn start_genie(&mut self, window: &smithay::desktop::Window, restoring: bool) {
        if self.animation_speed <= 0.0 {
            return;
        }
        let Some(target) = self.minimize_target(window) else {
            return;
        };
        let dark = self.dark();
        let renderer = self.backend.renderer();
        let now = Instant::now();
        let speed = self.animation_speed;
        // A window has only one genie: the new one replaces the old one.
        self.genies
            .retain(|genie| !genie.finished(now, speed) && !genie.hides(window));
        let effect = self.minimize_effect;
        if let Some(mut genie) = crate::render::snapshot_genie(
            renderer,
            &self.space,
            &self.output,
            window,
            target,
            restoring,
            dark,
        ) {
            genie.effect = effect;
            self.genies.push(genie);
        }
    }
}

/// The thumbnail of a minimized window, in its data: premultiplied RGBA, at most
/// `THUMBNAIL_MAX` pixels per side.
#[derive(Default)]
pub struct Thumbnail(pub std::sync::Mutex<Option<(u32, u32, Vec<u8>)>>);

pub const THUMBNAIL_MAX: u32 = 256;

impl State {
    /// Photographs the window frame (without shadow) and keeps it, reduced, in its data.
    pub fn capture_thumbnail(&mut self, window: &smithay::desktop::Window) {
        let capture = match self.window_capture(window, false) {
            Ok((capture, _)) => capture,
            Err(err) => {
                warn!("thumbnail not read: {err}");
                return;
            }
        };
        let small = shrink(&capture.rgba, capture.width, capture.height, THUMBNAIL_MAX);
        window
            .user_data()
            .insert_if_missing_threadsafe(Thumbnail::default);
        if let Some(slot) = window.user_data().get::<Thumbnail>()
            && let Ok(mut slot) = slot.0.lock()
        {
            *slot = small;
        }
    }

    /// The window on its own (frame and bar included), at full resolution; with `shadow`, its
    /// shadow too on a transparent background (what the screen shows of it). Also where the
    /// image's top-left pixel sits on the output, in physical pixels.
    pub fn window_capture(
        &mut self,
        window: &smithay::desktop::Window,
        shadow: bool,
    ) -> Result<(crate::capture::Capture, (f64, f64)), String> {
        use smithay::backend::renderer::ExportMem;
        let dark = self.dark();
        let renderer = self.backend.renderer();
        let snapshot =
            crate::render::snapshot_window(renderer, &self.space, &self.output, window, dark)
                .ok_or("window cannot be drawn")?;
        let scale = self.output.current_scale().fractional_scale();
        let (Some(geometry), Some(origin)) = (
            self.space.element_geometry(window),
            self.space.output_geometry(&self.output),
        ) else {
            return Err("window not on screen".to_owned());
        };
        let mut frame = crate::decorations::frame_of(window, geometry);
        frame.loc -= origin.loc;
        let frame = frame.to_physical_precise_round(scale);
        // With the shadow, the file is the frame plus fixed transparent margins (SCREENSHOT-2),
        // not the whole shadow of the screen; what the shadow does not cover stays clear.
        let (ml, mt, mr, mb) = crate::capture::window_margins(scale);
        let padded = Rectangle::<i32, smithay::utils::Physical>::new(
            (frame.loc.x - ml, frame.loc.y - mt).into(),
            (frame.size.w + ml + mr, frame.size.h + mt + mb).into(),
        );
        let visible = if shadow {
            padded
                .intersection(snapshot.bbox)
                .ok_or("window off screen")?
        } else {
            frame
                .intersection(snapshot.bbox)
                .ok_or("window off screen")?
        };
        let region = Rectangle::<i32, smithay::utils::Buffer>::new(
            (visible.loc - snapshot.bbox.loc).to_logical(1).to_buffer(
                1,
                Transform::Normal,
                &visible.size.to_logical(1),
            ),
            visible.size.to_logical(1).to_buffer(1, Transform::Normal),
        );
        let pixels = renderer
            .copy_texture(
                &snapshot.texture,
                region,
                smithay::backend::allocator::Fourcc::Abgr8888,
            )
            .and_then(|mapping| renderer.map_texture(&mapping).map(<[u8]>::to_vec))
            .map_err(|err| format!("readback: {err:?}"))?;
        let (Ok(width), Ok(height)) =
            (u32::try_from(visible.size.w), u32::try_from(visible.size.h))
        else {
            return Err("invalid size".to_owned());
        };
        let capture = crate::capture::Capture {
            width,
            height,
            rgba: pixels,
        };
        if !shadow {
            return Ok((
                capture,
                (f64::from(visible.loc.x), f64::from(visible.loc.y)),
            ));
        }
        let (Ok(full_w), Ok(full_h)) = (u32::try_from(padded.size.w), u32::try_from(padded.size.h))
        else {
            return Err("invalid size".to_owned());
        };
        let canvas = crate::capture::paste_on_transparent(
            &capture,
            visible.loc.x - padded.loc.x,
            visible.loc.y - padded.loc.y,
            full_w,
            full_h,
        );
        Ok((canvas, (f64::from(padded.loc.x), f64::from(padded.loc.y))))
    }
}

/// Reduces an RGBA image (average of the pixels of every box) to at most `max` per side.
pub fn shrink(pixels: &[u8], w: u32, h: u32, max: u32) -> Option<(u32, u32, Vec<u8>)> {
    if w == 0 || h == 0 || pixels.len() < (w as usize) * (h as usize) * 4 {
        return None;
    }
    let factor = (w.max(h) as f64 / max as f64).max(1.0);
    let (tw, th) = (
        ((w as f64 / factor).round() as u32).max(1),
        ((h as f64 / factor).round() as u32).max(1),
    );
    let mut out = vec![0u8; (tw as usize) * (th as usize) * 4];
    for ty in 0..th {
        let y0 = (ty as f64 * factor) as u32;
        let y1 = (((ty + 1) as f64 * factor) as u32).clamp(y0 + 1, h);
        for tx in 0..tw {
            let x0 = (tx as f64 * factor) as u32;
            let x1 = (((tx + 1) as f64 * factor) as u32).clamp(x0 + 1, w);
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y as usize) * (w as usize) + x as usize) * 4;
                    if let Some(px) = pixels.get(i..i + 4) {
                        for (s, &v) in sum.iter_mut().zip(px) {
                            *s += u64::from(v);
                        }
                        n += 1;
                    }
                }
            }
            let o = ((ty as usize) * (tw as usize) + tx as usize) * 4;
            if let Some(dst) = out.get_mut(o..o + 4) {
                for (d, s) in dst.iter_mut().zip(sum) {
                    *d = (s / n.max(1)) as u8;
                }
            }
        }
    }
    Some((tw, th, out))
}

/// The renderer is GLES on a software rasterizer (EGL_MESA_device_software). Not ours: the CPU
/// renderer draws the full glass within the frame budget.
pub(crate) fn is_software(renderer: &SilicaRenderer) -> bool {
    renderer.as_gles().is_some_and(|renderer| {
        smithay::backend::egl::EGLDevice::device_for_display(renderer.egl_context().display())
            .is_ok_and(|device| device.is_software())
    })
}

impl State {
    /// The bar buttons to draw in this frame (fading symbols, pressed).
    pub fn buttons_view(&self) -> Option<crate::render::ButtonsView> {
        let now = Instant::now();
        let ms = |t: Instant| now.saturating_duration_since(t).as_secs_f64() * 1000.0;
        let speed = self.animation_speed;
        let scaled = |t: Instant| {
            if speed <= 0.0 {
                f64::MAX
            } else {
                ms(t) / speed
            }
        };
        let pressed = self
            .titlebar_press
            .as_ref()
            .filter(|_| self.titlebar_press_inside);
        let hover = self.buttons_hover.as_ref();
        let window = hover.map(|h| &h.window).or(pressed.map(|(w, _)| w))?;
        let symbols = match hover.filter(|h| &h.window == window) {
            Some(h) => {
                compositor_render::titlebar::symbols_opacity(scaled(h.entered), h.left.map(scaled))
            }
            None => 0.0,
        };
        Some(crate::render::ButtonsView {
            window: window.clone(),
            symbols,
            pressed: pressed
                .filter(|(w, _)| w == window)
                .and_then(|(_, part)| part.button()),
        })
    }

    /// ⌥ held on the keyboard.
    pub fn alt_held(&self) -> bool {
        self.seat
            .get_keyboard()
            .is_some_and(|keyboard| keyboard.modifier_state().alt)
    }
}

/// Renders `elements` into the headless canvas with the damage since the last frame, and waits
/// for the renderer to finish. It gives the time spent drawing and waiting, nothing when
/// nothing changed.
fn draw_headless(
    renderer: &mut SilicaRenderer,
    frames: &mut HeadlessFrames,
    output: &Output,
    elements: &[OutputElement],
    clear: [f32; 4],
) -> Result<Option<(Duration, Duration)>, String> {
    let start = Instant::now();
    let mode = output
        .current_mode()
        .map(|mode| mode.size)
        .unwrap_or_default();
    let size = Size::<i32, BufferCoords>::from((mode.w, mode.h));
    let fresh = frames.canvas.as_ref().is_none_or(|c| c.size() != size);
    if fresh {
        let canvas: SilicaTexture = renderer
            .create_buffer(Fourcc::Abgr8888, size)
            .map_err(|e| format!("canvas: {e}"))?;
        frames.canvas = Some(canvas);
    }
    let Some(canvas) = frames.canvas.as_mut() else {
        return Err("no canvas".to_owned());
    };
    let mut canvas = renderer
        .bind(canvas)
        .map_err(|e| format!("bind canvas: {e}"))?;
    let result = frames
        .damage_tracker
        .render_output(renderer, &mut canvas, usize::from(!fresh), elements, clear)
        .map_err(|e| format!("render: {e:?}"))?;
    if result.damage.is_none() {
        return Ok(None);
    }
    let drawn = Instant::now();
    // The GPU time ends with the frame sent, not after the wait.
    if let (Some(timer), Some(gles)) = (&mut frames.timer, renderer.gles()) {
        timer.end(gles, true);
    }
    result.sync.wait().map_err(|e| format!("sync: {e:?}"))?;
    Ok(Some((drawn - start, drawn.elapsed())))
}
