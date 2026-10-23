//! The pointer drawn by the compositor in the real session (`drm`): nested, the host compositor draws
//! it. A client's cursor surface when it sets one, otherwise a default arrow generated here.
//! The arrow is only the fallback when the XCursor theme (Adwaita by default, see
//! `cursor_theme.rs`) is missing.
//!
//! The elements are `Kind::Cursor` with their pixels in memory (a named cursor) or in a shm
//! buffer (a client's), so `DrmCompositor` can copy them into the cursor plane: moving the
//! pointer then draws nothing. Where it cannot (no cursor plane, a scale the bitmap was not made
//! for), they are drawn like any other element.

use compositor_render::renderer::SilicaRenderer;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::utils::{Logical, Point, Size, Transform};
use smithay::wayland::compositor::with_states;

use crate::render::OutputElement;

/// The arrow, one row per line: `#` black, `o` white outline, `.` transparent. The hotspot is
/// the top-left pixel.
const ARROW: [&str; 19] = [
    "o...........",
    "oo..........",
    "o#o.........",
    "o##o........",
    "o###o.......",
    "o####o......",
    "o#####o.....",
    "o######o....",
    "o#######o...",
    "o########o..",
    "o#########o.",
    "o######oooo.",
    "o###o##o....",
    "o##oo##o....",
    "o#o..o##o...",
    "oo...o##o...",
    "o.....o##o..",
    "......o##o..",
    ".......oo...",
];

/// The default arrow: width, height and RGBA pixels (premultiplied).
pub fn arrow() -> (i32, i32, Vec<u8>) {
    let width = ARROW.iter().map(|row| row.len()).max().unwrap_or(0);
    let mut pixels = Vec::with_capacity(width * ARROW.len() * 4);
    for row in ARROW {
        for at in 0..width {
            pixels.extend_from_slice(match row.as_bytes().get(at) {
                Some(b'#') => &[0, 0, 0, 255],
                Some(b'o') => &[255, 255, 255, 255],
                _ => &[0, 0, 0, 0],
            });
        }
    }
    (
        i32::try_from(width).unwrap_or(0),
        i32::try_from(ARROW.len()).unwrap_or(0),
        pixels,
    )
}

/// A named cursor's image, ready to draw: its pixels as the cursor plane takes them
/// (premultiplied ARGB8888), its hotspot in pixels and its size in points.
#[derive(Debug, Clone)]
pub struct NamedCursor {
    buffer: MemoryRenderBuffer,
    hotspot: (i32, i32),
    size: Size<i32, Logical>,
}

impl NamedCursor {
    /// The built-in arrow ([`arrow`]), one pixel per point.
    pub fn arrow() -> Self {
        let (w, h, pixels) = arrow();
        Self::new(&pixels, (w, h), (0, 0), 1.0)
    }

    /// A theme frame, drawn at `scale` pixels per point.
    pub fn frame(image: &crate::cursor_theme::CursorImage, scale: f64) -> Self {
        let size = (image.width as i32, image.height as i32);
        Self::new(&image.rgba, size, image.hotspot, scale)
    }

    /// `rgba` (premultiplied) of `size` pixels, made for `scale` pixels per point.
    fn new(rgba: &[u8], size: (i32, i32), hotspot: (i32, i32), scale: f64) -> Self {
        let (pixels, _) = rgba.as_chunks::<4>();
        let argb: Vec<u8> = pixels
            .iter()
            .flat_map(|&[r, g, b, a]| [b, g, r, a])
            .collect();
        let buffer = MemoryRenderBuffer::from_slice(
            &argb,
            Fourcc::Argb8888,
            size,
            1,
            Transform::Normal,
            None,
        );
        let scale = scale.max(0.1);
        let points = |pixels: i32| ((f64::from(pixels) / scale).round() as i32).max(1);
        Self {
            buffer,
            hotspot,
            size: (points(size.0), points(size.1)).into(),
        }
    }
}

/// The elements of the pointer at `location` (logical, relative to the output), in front.
/// `named` is the image of a named shape (theme frame or built-in arrow).
pub fn elements(
    renderer: &mut SilicaRenderer,
    status: &CursorImageStatus,
    named: Option<&NamedCursor>,
    location: Point<f64, Logical>,
    scale: f64,
) -> Vec<OutputElement> {
    match status {
        CursorImageStatus::Hidden => Vec::new(),
        CursorImageStatus::Surface(surface) => {
            let hotspot = with_states(surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .and_then(|data| data.lock().ok().map(|data| data.hotspot))
            })
            .unwrap_or_default();
            let at = (location - hotspot.to_f64()).to_physical_precise_round(scale);
            render_elements_from_surface_tree(renderer, surface, at, scale, 1.0, Kind::Cursor)
                .into_iter()
                .map(OutputElement::Surface)
                .collect()
        }
        CursorImageStatus::Named(_) => {
            let Some(named) = named else {
                return Vec::new();
            };
            let at = location.to_physical(scale) - Point::from(named.hotspot).to_f64();
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                at,
                &named.buffer,
                None,
                None,
                Some(named.size),
                Kind::Cursor,
            ) {
                Ok(element) => vec![OutputElement::Memory(element)],
                Err(err) => {
                    tracing::warn!("cursor: {err}");
                    Vec::new()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ARROW, arrow};

    #[test]
    fn the_arrow_is_a_full_rectangle() {
        let (w, h, pixels) = arrow();
        assert_eq!((w, h), (12, 19));
        assert_eq!(pixels.len(), 12 * 19 * 4);
        assert!(ARROW.iter().all(|row| row.len() == 12));
        // The hotspot is drawn: the tip is the outline.
        assert_eq!(pixels.get(0..4), Some(&[255, 255, 255, 255][..]));
    }
}
