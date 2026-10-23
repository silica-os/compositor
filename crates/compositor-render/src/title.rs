//! The title in the server-side decorations bar.
//!
//! Amalfi title-only bar (kit `Titlebars/Bar/Title Only`): the title is centred on the window,
//! Inter semibold 13 pt on an 18 pt line (baseline 21 pt from the top of the 32 pt bar), and
//! the text box stops 84 pt from each side (the window controls and the matching spacer), so
//! a long title is shortened with an ellipsis there. In 52 pt toolbars the app draws the title
//! itself (the compositor draws none).

use std::cell::RefCell;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

use crate::renderer::{SilicaRenderer, SilicaTexture};
use tracing::warn;

const INTER_SEMIBOLD: &[u8] = include_bytes!("../fonts/Inter-SemiBold.ttf");

/// Title size in points (the kit's `Title` text: Inter 13, weight 8, line height 18).
pub const SIZE: f32 = 13.0;
/// Baseline from the top of the frame, in points: the 18 pt line starts at y 7 of the bar.
pub const BASELINE: f64 = 21.0;
/// The text box starts this far from each side (kit: x 84 in a 520 pt bar, 64 pt of controls
/// after 20 pt, mirrored by a spacer on the right).
const INSET: f64 = 84.0;
// Ink colors (the kit's `Title` text): `Text/Primary` when active, `Text/Tertiary` when not.
const ACTIVE: [u8; 3] = [0x1d, 0x1e, 0x21];
const INACTIVE: [u8; 3] = [0x9a, 0x9e, 0xa6];
const ACTIVE_DARK: [u8; 3] = [0xf4, 0xf5, 0xf7];
const INACTIVE_DARK: [u8; 3] = [0x7c, 0x80, 0x8a];

/// The title color according to state and appearance.
pub fn color(active: bool, dark: bool) -> [u8; 3] {
    match (active, dark) {
        (true, false) => ACTIVE,
        (false, false) => INACTIVE,
        (true, true) => ACTIVE_DARK,
        (false, true) => INACTIVE_DARK,
    }
}

/// The rasterized text, in premultiplied RGBA.
struct Raster {
    width: u32,
    height: u32,
    /// Distance of the baseline from the top of the raster, in pixels.
    ascent: f32,
    pixels: Vec<u8>,
}

/// Rasterizes `text` at `px` pixels in the given color, at most `max_width` pixels: beyond
/// that the text is shortened with an ellipsis.
fn rasterize(text: &str, px: f32, color: [u8; 3], max_width: f32) -> Option<Raster> {
    let font = FontRef::try_from_slice(INTER_SEMIBOLD).ok()?;
    let scaled = font.as_scaled(PxScale::from(px));
    let advance = |text: &str| {
        let mut width = 0.0;
        let mut previous = None;
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if let Some(previous) = previous {
                width += scaled.kern(previous, id);
            }
            width += scaled.h_advance(id);
            previous = Some(id);
        }
        width
    };
    let mut shown = text.to_owned();
    if advance(&shown) > max_width {
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let candidate: String = chars.iter().collect::<String>().trim_end().to_owned() + "…";
            if advance(&candidate) <= max_width {
                shown = candidate;
                break;
            }
        }
        if chars.is_empty() {
            return None;
        }
    }
    let ascent = scaled.ascent();
    let width = advance(&shown).ceil().max(1.0) as u32;
    let height = (ascent - scaled.descent()).ceil().max(1.0) as u32;
    let mut coverage = vec![0.0_f32; (width as usize).checked_mul(height as usize)?];
    let mut caret = 0.0;
    let mut previous = None;
    for c in shown.chars() {
        let id = scaled.glyph_id(c);
        if let Some(previous) = previous {
            caret += scaled.kern(previous, id);
        }
        let glyph = id.with_scale_and_position(px, point(caret, ascent));
        caret += scaled.h_advance(id);
        previous = Some(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|x, y, c| {
            let gx = bounds.min.x as i64 + i64::from(x);
            let gy = bounds.min.y as i64 + i64::from(y);
            if gx >= 0 && gy >= 0 && (gx as u32) < width && (gy as u32) < height {
                let index = gy as usize * width as usize + gx as usize;
                if let Some(cell) = coverage.get_mut(index) {
                    *cell = (*cell + c).min(1.0);
                }
            }
        });
    }
    let pixels = coverage
        .iter()
        .flat_map(|&c| {
            let a = (c * 255.0).round();
            let channel = |v: u8| (f32::from(v) * c).round() as u8;
            [
                channel(color[0]),
                channel(color[1]),
                channel(color[2]),
                a as u8,
            ]
        })
        .collect();
    Some(Raster {
        width,
        height,
        ascent,
        pixels,
    })
}

/// The title of a window, kept in its data: it is rasterized only when text, state, scale
/// or available width change.
#[derive(Default)]
pub struct Title {
    state: RefCell<Option<Cached>>,
}

struct Cached {
    key: (String, [u8; 3], u32, i32),
    buffer: TextureBuffer<SilicaTexture>,
    /// Position of the baseline in the raster, in points.
    ascent: f64,
    /// Width of the raster in pixels.
    width: u32,
}

impl Title {
    /// The title element for a frame (logical, in the output). Nothing if the text is
    /// empty or there is no room.
    pub fn element(
        &self,
        renderer: &mut SilicaRenderer,
        text: &str,
        frame: Rectangle<i32, Logical>,
        color: [u8; 3],
        scale: f64,
    ) -> Option<TextureRenderElement<SilicaTexture>> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let buffer_scale = scale.round().max(1.0) as i32;
        let available = f64::from(frame.size.w) - 2.0 * INSET;
        if available < f64::from(SIZE) {
            return None;
        }
        let key = (
            text.to_owned(),
            color,
            buffer_scale.unsigned_abs(),
            frame.size.w,
        );
        let mut state = self.state.borrow_mut();
        if state.as_ref().is_none_or(|cached| cached.key != key) {
            let px = SIZE * buffer_scale as f32;
            let raster = rasterize(
                text,
                px,
                color,
                (available * f64::from(buffer_scale)) as f32,
            )?;
            let size = Size::<i32, smithay::utils::Buffer>::from((
                i32::try_from(raster.width).ok()?,
                i32::try_from(raster.height).ok()?,
            ));
            let buffer = match TextureBuffer::from_memory(
                renderer,
                &raster.pixels,
                Fourcc::Abgr8888,
                size,
                false,
                buffer_scale,
                Transform::Normal,
                None,
            ) {
                Ok(buffer) => buffer,
                Err(err) => {
                    warn!("title not uploaded: {err:?}");
                    return None;
                }
            };
            *state = Some(Cached {
                key,
                buffer,
                ascent: f64::from(raster.ascent) / f64::from(buffer_scale),
                width: raster.width,
            });
        }
        let cached = state.as_ref()?;
        // Centred on the window (the box is symmetric), never closer to the sides than INSET.
        let width = f64::from(cached.width) / f64::from(buffer_scale);
        let x = f64::from(frame.loc.x) + ((f64::from(frame.size.w) - width) / 2.0).max(INSET);
        let y = f64::from(frame.loc.y) + BASELINE - cached.ascent;
        let location: Point<i32, smithay::utils::Physical> =
            Point::<f64, Logical>::from((x, y)).to_physical_precise_round(scale);
        Some(TextureRenderElement::from_texture_buffer(
            location.to_f64(),
            &cached.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_rasterize_and_shorten_to_fit() {
        let raster = rasterize("Documents", 15.0, ACTIVE, 1000.0).unwrap();
        assert!(
            raster.width > 40 && raster.height >= 15,
            "{}x{}",
            raster.width,
            raster.height
        );
        // A few full pixels: the text is there.
        assert!(raster.pixels.chunks(4).any(|p| p[3] > 200));
        let short = rasterize("A very long title for the bar", 15.0, ACTIVE, 80.0).unwrap();
        assert!(short.width <= 80, "{}", short.width);
        assert!(rasterize("Title", 15.0, ACTIVE, 1.0).is_none());
    }
}
