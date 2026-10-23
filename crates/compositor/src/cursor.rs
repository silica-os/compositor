//! The pointer drawn by the compositor in the real session (`drm`): nested, the host compositor draws
//! it. A client's cursor surface when it sets one, otherwise a default arrow generated here.
//! The arrow is only the fallback when the XCursor theme (Adwaita by default, see
//! `cursor_theme.rs`) is missing.

use compositor_render::renderer::{SilicaRenderer, SilicaTexture};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::{ImportMem, Renderer, Texture};
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::utils::{Logical, Point, Transform};
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

/// The default cursor as a texture, generated in code ([`arrow`]).
pub fn default_texture(renderer: &mut SilicaRenderer) -> Option<SilicaTexture> {
    let (w, h, pixels) = arrow();
    match renderer.import_memory(&pixels, Fourcc::Abgr8888, (w, h).into(), false) {
        Ok(texture) => Some(texture),
        Err(err) => {
            tracing::warn!("default cursor: {err}");
            None
        }
    }
}

/// The elements of the pointer at `location` (logical, relative to the output), in front.
/// `named` is the texture of a named shape (theme frame or built-in arrow) with its hotspot in
/// physical pixels and, for a theme frame, the element id of that frame (the built-in arrow
/// has none).
pub fn elements(
    renderer: &mut SilicaRenderer,
    status: &CursorImageStatus,
    named: Option<(&SilicaTexture, (i32, i32), Option<Id>)>,
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
            let Some((texture, hotspot, id)) = named else {
                return Vec::new();
            };
            let at = location.to_physical(scale) - Point::from((hotspot.0, hotspot.1)).to_f64();
            let element = TextureRenderElement::from_static_texture(
                id.unwrap_or_else(default_id),
                renderer.context_id(),
                at,
                texture.clone(),
                1,
                Transform::Normal,
                None,
                None,
                Some(texture.size().to_logical(1, Transform::Normal)),
                None,
                Kind::Cursor,
            );
            vec![OutputElement::Texture(RescaleRenderElement::from_element(
                element,
                at.to_i32_round(),
                1.0,
            ))]
        }
    }
}

/// One id for the named cursor, so the damage tracker sees it move instead of a new element.
fn default_id() -> Id {
    thread_local! {
        static ID: Id = Id::new();
    }
    ID.with(Id::clone)
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
