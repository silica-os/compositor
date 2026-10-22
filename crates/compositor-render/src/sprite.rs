//! Small elements drawn by the compositor on the CPU: capsules, lines and text in Inter, uploaded
//! as textures and cached until the content changes. They serve the interface the
//! compositor draws by itself (Panorama's spaces bar), where there is no Flutter client.

use std::cell::RefCell;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Logical, Point, Size, Transform};
use tracing::warn;

const INTER_REGULAR: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../fonts/Inter-SemiBold.ttf");

/// Text weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular,
    SemiBold,
}

impl Weight {
    fn font(self) -> Option<FontRef<'static>> {
        FontRef::try_from_slice(match self {
            Weight::Regular => INTER_REGULAR,
            Weight::SemiBold => INTER_SEMIBOLD,
        })
        .ok()
    }
}

/// Width of a text in points.
pub fn text_width(text: &str, size: f32, weight: Weight) -> f32 {
    let Some(font) = weight.font() else {
        return 0.0;
    };
    let scaled = font.as_scaled(PxScale::from(size));
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
}

/// A premultiplied RGBA canvas, in pixels; the method coordinates are in points.
pub struct Canvas {
    width: u32,
    height: u32,
    scale: f32,
    pixels: Vec<[f32; 4]>,
}

/// Signed distance from a rectangle with corners of radius `r` (negative inside).
fn rounded_distance(px: f32, py: f32, rect: [f32; 4], r: f32) -> f32 {
    let [x, y, w, h] = rect;
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    let qx = (px - cx).abs() - (w / 2.0 - r);
    let qy = (py - cy).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

impl Canvas {
    fn new(size: Size<f64, Logical>, scale: f32) -> Option<Self> {
        let width = (size.w as f32 * scale).ceil().max(1.0) as u32;
        let height = (size.h as f32 * scale).ceil().max(1.0) as u32;
        // At most 4096 × 4096: an error in the data must not allocate gigabytes.
        if width > 4096 || height > 4096 {
            return None;
        }
        Some(Self {
            width,
            height,
            scale,
            pixels: vec![[0.0; 4]; (width as usize) * (height as usize)],
        })
    }

    /// Covers the pixel with `color` (RGBA 0–255) for a coverage 0–1 ("over").
    fn blend(&mut self, x: u32, y: u32, color: [u8; 4], coverage: f32) {
        if coverage <= 0.0 || x >= self.width || y >= self.height {
            return;
        }
        let a = f32::from(color[3]) / 255.0 * coverage.min(1.0);
        let index = y as usize * self.width as usize + x as usize;
        if let Some(px) = self.pixels.get_mut(index) {
            for (i, channel) in px.iter_mut().take(3).enumerate() {
                let c = color.get(i).copied().map_or(0.0, f32::from) / 255.0;
                *channel = c * a + *channel * (1.0 - a);
            }
            px[3] = a + px[3] * (1.0 - a);
        }
    }

    /// Fills a rounded rectangle (points).
    pub fn fill_rounded(&mut self, rect: [f32; 4], radius: f32, color: [u8; 4]) {
        self.shape(rect, radius, color, |d| (0.5 - d).clamp(0.0, 1.0));
    }

    /// Fills a straight rectangle (points): coverage by area, without distances.
    pub fn fill_rect(&mut self, rect: [f32; 4], color: [u8; 4]) {
        let s = self.scale;
        let (x0, y0) = (rect[0] * s, rect[1] * s);
        let (x1, y1) = (x0 + rect[2] * s, y0 + rect[3] * s);
        let cover = |a: f32, b: f32, i: u32| ((i as f32 + 1.0).min(b) - (i as f32).max(a)).max(0.0);
        let first_x = x0.floor().max(0.0) as u32;
        let last_x = (x1.ceil().max(0.0) as u32).min(self.width);
        let first_y = y0.floor().max(0.0) as u32;
        let last_y = (y1.ceil().max(0.0) as u32).min(self.height);
        for y in first_y..last_y {
            let cy = cover(y0, y1, y);
            for x in first_x..last_x {
                self.blend(x, y, color, cy * cover(x0, x1, x));
            }
        }
    }

    /// The outline of `width` points, centered on the rectangle edge.
    pub fn stroke_rounded(&mut self, rect: [f32; 4], radius: f32, width: f32, color: [u8; 4]) {
        let half = width * self.scale / 2.0;
        self.shape(rect, radius, color, |d| {
            (half + 0.5 - d.abs()).clamp(0.0, 1.0)
        });
    }

    fn shape(&mut self, rect: [f32; 4], radius: f32, color: [u8; 4], cover: impl Fn(f32) -> f32) {
        let s = self.scale;
        let rect_px = [rect[0] * s, rect[1] * s, rect[2] * s, rect[3] * s];
        let x0 = (rect_px[0] - 2.0).floor().max(0.0) as u32;
        let y0 = (rect_px[1] - 2.0).floor().max(0.0) as u32;
        let x1 = ((rect_px[0] + rect_px[2] + 2.0).ceil().max(0.0) as u32).min(self.width);
        let y1 = ((rect_px[1] + rect_px[3] + 2.0).ceil().max(0.0) as u32).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = rounded_distance(x as f32 + 0.5, y as f32 + 0.5, rect_px, radius * s);
                self.blend(x, y, color, cover(d));
            }
        }
    }

    /// A line of `width` points with rounded ends.
    pub fn line(&mut self, from: [f32; 2], to: [f32; 2], width: f32, color: [u8; 4]) {
        let s = self.scale;
        let (ax, ay, bx, by) = (from[0] * s, from[1] * s, to[0] * s, to[1] * s);
        let half = width * s / 2.0;
        let x0 = (ax.min(bx) - half - 2.0).floor().max(0.0) as u32;
        let y0 = (ay.min(by) - half - 2.0).floor().max(0.0) as u32;
        let x1 = ((ax.max(bx) + half + 2.0).ceil().max(0.0) as u32).min(self.width);
        let y1 = ((ay.max(by) + half + 2.0).ceil().max(0.0) as u32).min(self.height);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
                let (cx, cy) = (ax + t * dx, ay + t * dy);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - half;
                self.blend(x, y, color, (0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// Writes `text` with the baseline at `origin` (points); returns the width.
    pub fn text(
        &mut self,
        origin: [f32; 2],
        text: &str,
        size: f32,
        weight: Weight,
        color: [u8; 4],
    ) -> f32 {
        let Some(font) = weight.font() else {
            return 0.0;
        };
        let px = size * self.scale;
        let scaled = font.as_scaled(PxScale::from(px));
        let mut caret = origin[0] * self.scale;
        let baseline = origin[1] * self.scale;
        let start = caret;
        let mut previous = None;
        let mut glyphs = Vec::new();
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if let Some(previous) = previous {
                caret += scaled.kern(previous, id);
            }
            glyphs.push(id.with_scale_and_position(px, point(caret, baseline)));
            caret += scaled.h_advance(id);
            previous = Some(id);
        }
        for glyph in glyphs {
            let Some(outline) = font.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outline.px_bounds();
            let mut cells = Vec::new();
            outline.draw(|x, y, c| cells.push((x, y, c)));
            for (x, y, c) in cells {
                let gx = bounds.min.x as i64 + i64::from(x);
                let gy = bounds.min.y as i64 + i64::from(y);
                if gx >= 0 && gy >= 0 {
                    self.blend(gx as u32, gy as u32, color, c);
                }
            }
        }
        (caret - start) / self.scale
    }

    fn rgba8(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|px| px.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect()
    }
}

/// An element drawn on the canvas, redrawn only when the key changes.
#[derive(Default)]
pub struct Sprite {
    cached: RefCell<Option<(String, TextureBuffer<GlesTexture>)>>,
}

impl Sprite {
    /// The element `size` points large at `location` (points in the output), drawn by
    /// `draw` when `key` changes. `alpha` for fades.
    #[allow(clippy::too_many_arguments)]
    pub fn element(
        &self,
        renderer: &mut GlesRenderer,
        key: &str,
        size: Size<f64, Logical>,
        location: Point<f64, Logical>,
        scale: f64,
        alpha: f32,
        draw: impl FnOnce(&mut Canvas),
    ) -> Option<TextureRenderElement<GlesTexture>> {
        let buffer_scale = scale.round().max(1.0) as i32;
        let full_key = format!("{key}@{buffer_scale}:{}x{}", size.w, size.h);
        let mut cached = self.cached.borrow_mut();
        if cached.as_ref().is_none_or(|(k, _)| *k != full_key) {
            let mut canvas = Canvas::new(size, buffer_scale as f32)?;
            draw(&mut canvas);
            let buffer_size = Size::<i32, smithay::utils::Buffer>::from((
                i32::try_from(canvas.width).ok()?,
                i32::try_from(canvas.height).ok()?,
            ));
            let buffer = match TextureBuffer::from_memory(
                renderer,
                &canvas.rgba8(),
                Fourcc::Abgr8888,
                buffer_size,
                false,
                buffer_scale,
                Transform::Normal,
                None,
            ) {
                Ok(buffer) => buffer,
                Err(err) => {
                    warn!("drawn element not uploaded: {err:?}");
                    return None;
                }
            };
            *cached = Some((full_key, buffer));
        }
        let (_, buffer) = cached.as_ref()?;
        Some(TextureRenderElement::from_texture_buffer(
            location
                .to_physical_precise_round::<f64, i32>(scale)
                .to_f64(),
            buffer,
            Some(alpha),
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
    fn shapes_and_text_cover_the_right_pixels() {
        let mut canvas = Canvas::new(Size::from((40.0, 20.0)), 1.0).unwrap();
        canvas.fill_rounded([0.0, 0.0, 40.0, 20.0], 6.0, [255, 0, 0, 255]);
        // Full center, empty corner.
        let at = |c: &Canvas, x: u32, y: u32| c.pixels[(y * c.width + x) as usize];
        assert_eq!(at(&canvas, 20, 10), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(at(&canvas, 0, 0)[3], 0.0);
        // A rectangle at half a pixel covers the edge by half.
        canvas.fill_rect([0.0, 0.0, 2.5, 1.0], [0, 255, 0, 255]);
        assert_eq!(at(&canvas, 1, 0), [0.0, 1.0, 0.0, 1.0]);
        assert!((at(&canvas, 2, 0)[1] - 0.5).abs() < 1e-6);
        let width = canvas.text(
            [2.0, 15.0],
            "Desktop 1",
            13.0,
            Weight::Regular,
            [0, 0, 0, 255],
        );
        assert!((width - text_width("Desktop 1", 13.0, Weight::Regular)).abs() < 0.01);
        assert!(width > 40.0);
        // A huge canvas is refused.
        assert!(Canvas::new(Size::from((10000.0, 10.0)), 1.0).is_none());
    }
}
