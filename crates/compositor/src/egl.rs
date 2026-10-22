//! EGL/GLES initialization without a window: for the headless backend (the `compositor-render`
//! shaders run in the tests and in CI on llvmpipe too) and on a
//! GBM device for the real session (`drm`).
//!
//! The only the compositor module where `unsafe` is allowed (AGENTS.md): smithay's constructors for
//! display and renderer are `unsafe` for the conditions written next to every block.
#![allow(unsafe_code)]

use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::GlesRenderer;
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
        match renderer_for(device) {
            Ok(renderer) => {
                info!("headless su GLES (device software: {software})");
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
    unsafe { GlesRenderer::new(context) }.map_err(|e| format!("renderer: {e}"))
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
    unsafe { GlesRenderer::new(context) }.map_err(|e| format!("renderer: {e}"))
}
