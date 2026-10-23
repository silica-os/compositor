//! The only module of the crate with `unsafe`: views of memory that other processes share
//! with the compositor, a client's shm pool and a dmabuf mapping. Everything else works on
//! the safe slices handed out here.
//!
//! Shared memory can change under us while we read it (the client writes into a buffer it
//! already committed, the GPU into a dmabuf without the sync ioctl): the result is wrong
//! pixels in that frame, as with every renderer that reads client memory, never a crash.
#![allow(unsafe_code)]

use smithay::backend::allocator::dmabuf::DmabufMapping;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::wayland::shm::{BufferAccessError, BufferData, with_buffer_contents};

/// Runs `f` on the bytes of the shm pool of `buffer` and the buffer's layout in it.
pub(crate) fn with_shm<T>(
    buffer: &WlBuffer,
    f: impl FnOnce(&[u8], BufferData) -> T,
) -> Result<T, BufferAccessError> {
    with_buffer_contents(buffer, |ptr, len, data| {
        // SAFETY: smithay maps the whole pool, `len` bytes from `ptr`, for the duration of
        // this closure, and turns a SIGBUS from a pool shrunk by the client into an error.
        // The slice does not leave the closure.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        f(bytes, data)
    })
}

/// The bytes of a dmabuf mapping.
pub(crate) fn mapped(mapping: &DmabufMapping) -> &[u8] {
    // SAFETY: the mapping is `length()` bytes from `ptr()` and stays mapped as long as it
    // lives, which the borrow ties the slice to.
    unsafe { std::slice::from_raw_parts(mapping.ptr().cast::<u8>(), mapping.length()) }
}

/// The bytes of a dmabuf mapping, for writing: the caller maps it writable and holds it
/// mutably, so no other slice of this process sees it meanwhile.
pub(crate) fn mapped_mut(mapping: &mut DmabufMapping) -> &mut [u8] {
    // SAFETY: as in `mapped`; the exclusive borrow of the mapping makes this the only
    // slice over it in the process.
    unsafe { std::slice::from_raw_parts_mut(mapping.ptr().cast::<u8>(), mapping.length()) }
}
