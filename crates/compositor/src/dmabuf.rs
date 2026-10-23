//! linux-dmabuf: clients draw with the GPU and pass the buffer without copies.
//!
//! The feedback (version 4) tells clients the render node of the compositor's GPU: without it,
//! Mesa does not know which device to use for a Wayland surface and falls back to llvmpipe.
//! Only with the drm backend; in headless the protocol is not there.

use std::os::unix::fs::MetadataExt;

use compositor_render::renderer::SilicaRenderer;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::ImportDma;
use smithay::delegate_dmabuf;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use tracing::{info, warn};

use crate::backend::Backend;
use crate::state::State;

/// The device (`dev_t`) of the render node the compositor's renderer runs on.
fn main_device(renderer: &SilicaRenderer) -> Result<u64, String> {
    let renderer = renderer
        .as_gles()
        .ok_or("the CPU renderer has no render node")?;
    let device = EGLDevice::device_for_display(renderer.egl_context().display())
        .map_err(|e| format!("EGL device not found: {e}"))?;
    let path = device
        .render_device_path()
        .map_err(|e| format!("render node not found: {e}"))?;
    let metadata = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    info!("render node for dmabuf: {}", path.display());
    Ok(metadata.rdev())
}

/// Creates the `zwp_linux_dmabuf_v1` global with the formats the renderer can import.
/// Without a render node (a driver that does not expose it) it offers version 3, without feedback.
pub fn create_global(
    state: &mut DmabufState,
    dh: &DisplayHandle,
    renderer: &SilicaRenderer,
) -> DmabufGlobal {
    let formats = renderer.dmabuf_formats();
    let feedback = main_device(renderer).and_then(|device| {
        DmabufFeedbackBuilder::new(device, formats.clone())
            .build()
            .map_err(|e| format!("feedback dmabuf: {e}"))
    });
    match feedback {
        Ok(feedback) => state.create_global_with_default_feedback::<State>(dh, &feedback),
        Err(err) => {
            warn!("dmabuf without feedback: {err}");
            state.create_global::<State>(dh, formats)
        }
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let imported = match &mut self.backend {
            Backend::Headless(_) => false,
            backend => backend.renderer().import_dmabuf(&dmabuf, None).is_ok(),
        };
        if imported {
            // If the client is already gone there is nobody to notify.
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

delegate_dmabuf!(State);
