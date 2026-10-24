//! EGL/GLES initialization without a window: for the headless backend (the `compositor-render`
//! shaders run in the tests and in CI on llvmpipe too) and on a
//! GBM device for the real session (`drm`).
//!
//! The only the compositor module where `unsafe` is allowed (AGENTS.md): smithay's constructors for
//! display and renderer are `unsafe` for the conditions written next to every block.
#![allow(unsafe_code)]

use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::{Capability, GlesRenderer, ffi};
use std::collections::VecDeque;
use std::time::Duration;
use tracing::{info, warn};

/// A GLES renderer on an EGL device without a surface. With `prefer_gpu` the GPUs first,
/// otherwise Mesa's software device (llvmpipe) first: identical screenshots on every machine
/// and in CI. Error if no device initializes.
pub fn headless_renderer(prefer_gpu: bool) -> Result<GlesRenderer, String> {
    let mut devices: Vec<EGLDevice> = EGLDevice::enumerate()
        .map_err(|e| format!("cannot list EGL devices: {e}"))?
        .collect();
    devices.sort_by_key(|device| device.is_software() == prefer_gpu);
    let mut errors = Vec::new();
    for device in devices {
        let software = device.is_software();
        let node = device.render_device_path().ok();
        match renderer_for(device) {
            Ok(renderer) => {
                info!("headless on GLES (software device: {software}, node: {node:?})");
                return Ok(renderer);
            }
            Err(err) => {
                warn!("EGL device discarded: {err}");
                errors.push(err);
            }
        }
    }
    Err(format!("no usable EGL device: {}", errors.join("; ")))
}

fn renderer_for(device: EGLDevice) -> Result<GlesRenderer, String> {
    // SAFETY: smithay keeps track of the displays it creates; the condition is that no other
    // code in the process calls eglGetPlatformDisplay/eglTerminate on the same display.
    // The compositor creates EGL displays only through smithay.
    let display = unsafe { EGLDisplay::new(device) }.map_err(|e| format!("display: {e}"))?;
    let context = EGLContext::new(&display).map_err(|e| format!("context: {e}"))?;
    // SAFETY: the context is just created and not current on any other thread; the
    // renderer stays on the compositor thread, the only one using it.
    unsafe { renderer_on(context) }
}

/// A GLES renderer on `context`, without smithay's per-texture fences.
///
/// With `Capability::Fencing` smithay puts a `glFenceSync` behind every texture draw and
/// upload, to order them against other contexts sharing the textures. The compositor has one
/// context and no shared ones, where GL already orders its own commands; and on drivers without
/// deferred flushes (virgl) each fence is a submission to the host, about 60 µs per draw.
/// The frame's own fence (`Capability::ExportFence`) stays.
///
/// # Safety
///
/// As `GlesRenderer::new`: `context` is not current on another thread and the renderer
/// stays on the thread that creates it.
unsafe fn renderer_on(context: EGLContext) -> Result<GlesRenderer, String> {
    // SAFETY: the caller's conditions are smithay's.
    let capabilities = unsafe { GlesRenderer::supported_capabilities(&context) }
        .map_err(|e| format!("capabilities: {e}"))?
        .into_iter()
        .filter(|capability| *capability != Capability::Fencing);
    // SAFETY: as above.
    unsafe { GlesRenderer::with_capabilities(context, capabilities) }
        .map_err(|e| format!("renderer: {e}"))
}

/// A GLES renderer on a GBM device, for the real-session backend: it draws into the GBM
/// buffers that DRM scans out.
#[cfg(feature = "drm")]
pub fn gbm_renderer(
    gbm: smithay::backend::allocator::gbm::GbmDevice<smithay::backend::drm::DrmDeviceFd>,
) -> Result<GlesRenderer, String> {
    // SAFETY: as in `renderer_for`, the display is created only through smithay.
    let display = unsafe { EGLDisplay::new(gbm) }.map_err(|e| format!("GBM display: {e}"))?;
    let context = EGLContext::new(&display).map_err(|e| format!("context: {e}"))?;
    // SAFETY: as in `renderer_for`: a new context, used only on the compositor thread.
    unsafe { renderer_on(context) }
}

/// The GPU's own time on the frames, from `GL_EXT_disjoint_timer_query` on the GLES 3 queries: a
/// query brackets the GL work of a frame, and its result is read frames later when it is ready,
/// never waiting.
pub struct GpuTimer {
    /// Queries in flight, oldest first, each with whether its frame was drawn.
    pending: VecDeque<(u32, bool)>,
    free: Vec<u32>,
    active: Option<u32>,
}

const TIME_ELAPSED: u32 = 0x88BF;
const GPU_DISJOINT: u32 = 0x8FBB;
/// Queries in flight at most: past that, frames go unmeasured until the GPU catches up.
const MAX_PENDING: usize = 8;

impl GpuTimer {
    /// A timer for `renderer`, if its GL has GLES 3 queries and `GL_EXT_disjoint_timer_query`.
    pub fn new(renderer: &mut GlesRenderer) -> Option<GpuTimer> {
        renderer
            .with_context(|gl| {
                if !(gl.GenQueries.is_loaded()
                    && gl.BeginQuery.is_loaded()
                    && gl.EndQuery.is_loaded()
                    && gl.GetQueryObjectuiv.is_loaded())
                {
                    return Err("no GLES 3 queries");
                }
                // SAFETY: the context is current inside `with_context` and GL_EXTENSIONS is a
                // valid name.
                let extensions = unsafe { gl.GetString(ffi::EXTENSIONS) };
                if extensions.is_null() {
                    return Err("no GL extensions");
                }
                // SAFETY: not null, so a NUL-terminated string GL keeps with the context.
                let extensions = unsafe { std::ffi::CStr::from_ptr(extensions.cast()) };
                if extensions
                    .to_bytes()
                    .split(|b| *b == b' ')
                    .any(|name| name == b"GL_EXT_disjoint_timer_query")
                {
                    Ok(())
                } else {
                    Err("no GL_EXT_disjoint_timer_query")
                }
            })
            .map_err(|_| "no GL context")
            .flatten()
            .inspect_err(|reason| warn!("perf.render without GPU time: {reason}"))
            .ok()
            .map(|()| GpuTimer {
                pending: VecDeque::new(),
                free: Vec::new(),
                active: None,
            })
    }

    /// Starts timing the GL work that follows, up to `end`.
    pub fn begin(&mut self, renderer: &mut GlesRenderer) {
        if self.active.is_some() || self.pending.len() >= MAX_PENDING {
            return;
        }
        let free = &mut self.free;
        let started = renderer.with_context(|gl| {
            let query = free.pop().or_else(|| {
                let mut query = 0;
                // SAFETY: the context is current (GLES 3, checked in `new`); one name is
                // written into `query`.
                unsafe { gl.GenQueries(1, &mut query) };
                (query != 0).then_some(query)
            })?;
            // SAFETY: the context is current, `query` is one of its query names and no
            // TIME_ELAPSED query is active (`self.active` is None).
            unsafe { gl.BeginQuery(TIME_ELAPSED, query) };
            Some(query)
        });
        self.active = started.ok().flatten();
    }

    /// Ends the timing started by `begin`; `drawn` says whether the frame counts. Called as
    /// soon as the frame is sent: the end goes to the GPU at once, right behind the frame, so
    /// the time does not run on while the GPU waits for the next commands.
    pub fn end(&mut self, renderer: &mut GlesRenderer, drawn: bool) {
        let Some(query) = self.active.take() else {
            return;
        };
        let ended = renderer.with_context(|gl| {
            // SAFETY: the context is current and `query` is the active TIME_ELAPSED query.
            unsafe {
                gl.EndQuery(TIME_ELAPSED);
                gl.Flush();
            }
        });
        if ended.is_ok() {
            self.pending.push_back((query, drawn));
        }
    }

    /// Gives `record` the GPU time of each drawn frame whose result is ready, oldest first.
    /// Results across a disjoint event (the GPU clock jumped) are dropped.
    pub fn collect(&mut self, renderer: &mut GlesRenderer, mut record: impl FnMut(Duration)) {
        let pending = &mut self.pending;
        let free = &mut self.free;
        let _ = renderer.with_context(|gl| {
            let mut times = [Duration::ZERO; MAX_PENDING];
            let mut count = 0;
            while let Some(&(query, drawn)) = pending.front() {
                let mut available = 0;
                // SAFETY: the context is current and `query` names an ended query.
                unsafe { gl.GetQueryObjectuiv(query, ffi::QUERY_RESULT_AVAILABLE, &mut available) };
                if available == 0 {
                    break;
                }
                // 32 bits of nanoseconds: up to 4.2 s, far beyond any frame.
                let mut nanoseconds = 0;
                // SAFETY: as above, and the result is available, so this does not wait.
                unsafe { gl.GetQueryObjectuiv(query, ffi::QUERY_RESULT, &mut nanoseconds) };
                pending.pop_front();
                free.push(query);
                if drawn && let Some(slot) = times.get_mut(count) {
                    *slot = Duration::from_nanos(u64::from(nanoseconds));
                    count += 1;
                }
            }
            let mut disjoint = 0;
            // SAFETY: the context is current; GPU_DISJOINT is one integer.
            unsafe { gl.GetIntegerv(GPU_DISJOINT, &mut disjoint) };
            if disjoint == 0 {
                times.iter().take(count).copied().for_each(&mut record);
            }
        });
    }
}
