//! ext-image-capture-source-v1 and ext-image-copy-capture-v1: the screen or one window, copied
//! into the client's shm buffer. Only the shell sees them (screenshots, screen recording);
//! apps will capture through the portal, which asks the user. Smithay 0.7 has neither protocol.
//!
//! A frame is drawn when the client asks for it, with the renderer of `ui.screenshot`, without
//! the cursor (cursor sessions stop at once).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::{
    ext_foreign_toplevel_image_capture_source_manager_v1::{
        self as toplevel_sources, ExtForeignToplevelImageCaptureSourceManagerV1,
    },
    ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
    ext_output_image_capture_source_manager_v1::{
        self as output_sources, ExtOutputImageCaptureSourceManagerV1,
    },
};
use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::{
    ext_image_copy_capture_cursor_session_v1::{
        self as cursor_session, ExtImageCopyCaptureCursorSessionV1,
    },
    ext_image_copy_capture_frame_v1::{self as frame, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self as manager, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self as session, ExtImageCopyCaptureSessionV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::Transform;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle;

use crate::capture::Capture;
use crate::state::State;

/// What a source captures.
#[derive(Clone)]
pub enum Source {
    Output(Output),
    /// A window, by its identifier in the shell's toplevel list.
    Toplevel(String),
    /// The output or the window was already gone.
    Gone,
}

pub struct SessionData {
    source: Source,
    /// The size last sent with `buffer_size`.
    size: Mutex<(u32, u32)>,
    /// A frame object exists (one at a time).
    has_frame: AtomicBool,
}

pub struct FrameData {
    session: ExtImageCopyCaptureSessionV1,
    buffer: Mutex<Option<WlBuffer>>,
    captured: AtomicBool,
}

pub fn create_globals(dh: &DisplayHandle) {
    dh.create_global::<State, ExtOutputImageCaptureSourceManagerV1, _>(1, ());
    dh.create_global::<State, ExtForeignToplevelImageCaptureSourceManagerV1, _>(1, ());
    dh.create_global::<State, ExtImageCopyCaptureManagerV1, _>(1, ());
}

impl State {
    fn capture_source(&mut self, source: &Source) -> Result<Capture, String> {
        match source {
            Source::Output(output) if *output == self.output => self.screenshot(),
            Source::Toplevel(identifier) => {
                let window = self
                    .space
                    .elements()
                    .find(|window| {
                        crate::shell::toplevel_handle(window)
                            .is_some_and(|handle| handle.identifier() == *identifier)
                    })
                    .cloned()
                    .ok_or("the window is gone")?;
                self.window_capture(&window, false)
                    .map(|(capture, _)| capture)
            }
            _ => Err("the source is gone".to_owned()),
        }
    }

    /// A new session: the buffer constraints, or `stopped` when the source cannot be drawn.
    fn announce(&mut self, session: &ExtImageCopyCaptureSessionV1, data: &SessionData) {
        match self.capture_source(&data.source) {
            Ok(capture) => send_constraints(session, data, (capture.width, capture.height)),
            Err(_) => session.stopped(),
        }
    }

    fn capture_frame(&mut self, frame: &ExtImageCopyCaptureFrameV1, data: &FrameData) {
        let Some(session_data) = data.session.data::<SessionData>() else {
            frame.failed(frame::FailureReason::Unknown);
            return;
        };
        let capture = match self.capture_source(&session_data.source) {
            Ok(capture) => capture,
            Err(_) => {
                frame.failed(frame::FailureReason::Stopped);
                data.session.stopped();
                return;
            }
        };
        let size = (capture.width, capture.height);
        if session_data.size.lock().is_ok_and(|s| *s != size) {
            send_constraints(&data.session, session_data, size);
            frame.failed(frame::FailureReason::BufferConstraints);
            return;
        }
        let buffer = data.buffer.lock().ok().and_then(|b| b.clone());
        if buffer.is_none_or(|buffer| copy_into(&buffer, &capture).is_err()) {
            frame.failed(frame::FailureReason::BufferConstraints);
            return;
        }
        let (width, height) = (
            i32::try_from(capture.width).unwrap_or(i32::MAX),
            i32::try_from(capture.height).unwrap_or(i32::MAX),
        );
        frame.transform(Transform::Normal);
        frame.damage(0, 0, width, height);
        let now = Duration::from(self.protocols.clock.now());
        let secs = now.as_secs();
        frame.presentation_time((secs >> 32) as u32, secs as u32, now.subsec_nanos());
        frame.ready();
    }
}

fn send_constraints(session: &ExtImageCopyCaptureSessionV1, data: &SessionData, size: (u32, u32)) {
    if let Ok(mut current) = data.size.lock() {
        *current = size;
    }
    session.buffer_size(size.0, size.1);
    session.shm_format(wl_shm::Format::Argb8888);
    session.shm_format(wl_shm::Format::Xrgb8888);
    session.done();
}

/// Copies the RGBA capture into an ARGB8888 or XRGB8888 shm buffer of its size.
#[allow(unsafe_code)]
fn copy_into(buffer: &WlBuffer, capture: &Capture) -> Result<(), String> {
    smithay::wayland::shm::with_buffer_contents_mut(buffer, |ptr, len, data| {
        let row = capture.width as usize * 4;
        let (Ok(offset), Ok(stride), Ok(width), Ok(height)) = (
            usize::try_from(data.offset),
            usize::try_from(data.stride),
            u32::try_from(data.width),
            u32::try_from(data.height),
        ) else {
            return Err("negative buffer layout".to_owned());
        };
        if (width, height) != (capture.width, capture.height)
            || !matches!(
                data.format,
                wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888
            )
            || stride < row
        {
            return Err("the buffer does not match the constraints".to_owned());
        }
        // SAFETY: Smithay keeps the pool mapped, `len` bytes long, while the closure runs; every
        // access below is bounds-checked against `len`.
        let memory = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
        for (y, source) in capture.rgba.chunks_exact(row).enumerate() {
            let start = offset + y * stride;
            let target = memory
                .get_mut(start..start + row)
                .ok_or("the buffer is too small")?;
            let (to, _) = target.as_chunks_mut::<4>();
            let (from, _) = source.as_chunks::<4>();
            for (to, &[r, g, b, a]) in to.iter_mut().zip(from) {
                // ARGB8888 is B, G, R, A in memory.
                *to = [b, g, r, a];
            }
        }
        Ok(())
    })
    .map_err(|e| format!("not an shm buffer: {e:?}"))?
}

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::shell::is_shell_client(&client)
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ExtOutputImageCaptureSourceManagerV1,
        request: output_sources::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let output_sources::Request::CreateSource { source, output } = request {
            let target = Output::from_resource(&output).map_or(Source::Gone, Source::Output);
            data_init.init(source, target);
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::shell::is_shell_client(&client)
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: toplevel_sources::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let toplevel_sources::Request::CreateSource {
            source,
            toplevel_handle,
        } = request
        {
            let target = ForeignToplevelHandle::from_resource(&toplevel_handle)
                .map_or(Source::Gone, |handle| Source::Toplevel(handle.identifier()));
            data_init.init(source, target);
        }
    }
}

impl Dispatch<ExtImageCaptureSourceV1, Source> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _source: &ExtImageCaptureSourceV1,
        _request: ext_image_capture_source_v1::Request,
        _data: &Source,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::shell::is_shell_client(&client)
    }
}

fn new_session(source: Source) -> SessionData {
    SessionData {
        source,
        size: Mutex::new((0, 0)),
        has_frame: AtomicBool::new(false),
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ExtImageCopyCaptureManagerV1,
        request: manager::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            manager::Request::CreateSession {
                session, source, ..
            } => {
                let target = source.data::<Source>().cloned().unwrap_or(Source::Gone);
                let session = data_init.init(session, new_session(target));
                if let Some(data) = session.data::<SessionData>() {
                    state.announce(&session, data);
                }
            }
            manager::Request::CreatePointerCursorSession { session, .. } => {
                data_init.init(session, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _cursor: &ExtImageCopyCaptureCursorSessionV1,
        request: cursor_session::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let cursor_session::Request::GetCaptureSession { session } = request {
            data_init.init(session, new_session(Source::Gone)).stopped();
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, SessionData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        session: &ExtImageCopyCaptureSessionV1,
        request: session::Request,
        data: &SessionData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let session::Request::CreateFrame { frame } = request {
            if data.has_frame.swap(true, Ordering::Relaxed) {
                session.post_error(session::Error::DuplicateFrame, "a frame already exists");
                return;
            }
            data_init.init(
                frame,
                FrameData {
                    session: session.clone(),
                    buffer: Mutex::new(None),
                    captured: AtomicBool::new(false),
                },
            );
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ExtImageCopyCaptureFrameV1,
        request: frame::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            frame::Request::AttachBuffer { buffer } => {
                if let Ok(mut current) = data.buffer.lock() {
                    *current = Some(buffer);
                }
            }
            frame::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    frame.post_error(frame::Error::InvalidBufferDamage, "invalid damage");
                }
            }
            frame::Request::Capture => {
                if data.captured.swap(true, Ordering::Relaxed) {
                    frame.post_error(frame::Error::AlreadyCaptured, "the frame was captured");
                } else if data.buffer.lock().is_ok_and(|b| b.is_none()) {
                    frame.post_error(frame::Error::NoBuffer, "no buffer attached");
                } else {
                    state.capture_frame(frame, data);
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _frame: &ExtImageCopyCaptureFrameV1,
        data: &FrameData,
    ) {
        if let Some(session) = data.session.data::<SessionData>() {
            session.has_frame.store(false, Ordering::Relaxed);
        }
    }
}
