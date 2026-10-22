//! Capture of the output into memory, for `ui.screenshot`.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::{ExportMem, ImportAll, Offscreen, Renderer, Texture};
use smithay::desktop::space::space_render_elements;
use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::utils::{Rectangle, Transform};

/// An 8-bit RGBA frame, rows from top to bottom.
pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Capture {
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&self.rgba)
            .map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        Ok(out)
    }
}

/// The transparent shadow margin around a window in its capture file, in points (SCREENSHOT-2,
/// #87): left, top, right, bottom. The file is the window frame plus these margins, whatever
/// the shadow on the screen reaches.
pub const WINDOW_MARGIN_LEFT: f64 = 22.0;
pub const WINDOW_MARGIN_TOP: f64 = 15.0;
pub const WINDOW_MARGIN_RIGHT: f64 = 23.0;
pub const WINDOW_MARGIN_BOTTOM: f64 = 30.0;

/// The margins at `scale`, in physical pixels: left, top, right, bottom.
pub fn window_margins(scale: f64) -> (i32, i32, i32, i32) {
    let px = |points: f64| (points * scale).round() as i32;
    (
        px(WINDOW_MARGIN_LEFT),
        px(WINDOW_MARGIN_TOP),
        px(WINDOW_MARGIN_RIGHT),
        px(WINDOW_MARGIN_BOTTOM),
    )
}

/// A transparent `width` × `height` canvas with `part` pasted at `(x, y)` (what falls outside
/// is left out).
pub fn paste_on_transparent(part: &Capture, x: i32, y: i32, width: u32, height: u32) -> Capture {
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for row in 0..part.height as i64 {
        let ty = i64::from(y) + row;
        if ty < 0 || ty >= i64::from(height) {
            continue;
        }
        let first = (-i64::from(x)).max(0);
        let last = (i64::from(width) - i64::from(x)).min(i64::from(part.width));
        if first >= last {
            continue;
        }
        let from = ((row * i64::from(part.width) + first) * 4) as usize;
        let to = ((row * i64::from(part.width) + last) * 4) as usize;
        let at = ((ty * i64::from(width) + i64::from(x) + first) * 4) as usize;
        if let (Some(src), Some(dst)) =
            (part.rgba.get(from..to), rgba.get_mut(at..at + (to - from)))
        {
            dst.copy_from_slice(src);
        }
    }
    Capture {
        width,
        height,
        rgba,
    }
}

/// The given rectangle (pixels, rounded and clipped to the edges) of a capture; None if empty.
pub fn crop(capture: &Capture, x: f64, y: f64, width: f64, height: f64) -> Option<Capture> {
    if ![x, y, width, height].iter().all(|v| v.is_finite()) {
        return None;
    }
    let clamp = |v: f64, max: u32| v.round().clamp(0.0, f64::from(max)) as u32;
    let (x0, y0) = (clamp(x, capture.width), clamp(y, capture.height));
    let (x1, y1) = (
        clamp(x + width, capture.width),
        clamp(y + height, capture.height),
    );
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let stride = capture.width as usize * 4;
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for row in y0..y1 {
        let start = row as usize * stride + x0 as usize * 4;
        rgba.extend_from_slice(capture.rgba.get(start..start + w as usize * 4)?);
    }
    Some(Capture {
        width: w,
        height: h,
        rgba,
    })
}

/// Draws `image` (premultiplied RGBA, `w × h` at scale 1, hotspot at its top-left pixel) over
/// the capture with its hotspot at `at` (physical pixels, relative to the capture), scaled to
/// `scale` (nearest pixel). Parts outside the capture are left out.
pub fn draw_over(
    capture: &mut Capture,
    at: (f64, f64),
    (w, h, image): (i32, i32, &[u8]),
    scale: f64,
) {
    if !at.0.is_finite() || !at.1.is_finite() || !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let (Ok(w), Ok(h)) = (usize::try_from(w), usize::try_from(h)) else {
        return;
    };
    let out_w = (w as f64 * scale).round() as i64;
    let out_h = (h as f64 * scale).round() as i64;
    let (x0, y0) = (at.0.round() as i64, at.1.round() as i64);
    let (cw, ch) = (i64::from(capture.width), i64::from(capture.height));
    for dy in 0..out_h {
        let y = y0 + dy;
        if !(0..ch).contains(&y) {
            continue;
        }
        let sy = ((dy as f64 / scale) as usize).min(h.saturating_sub(1));
        for dx in 0..out_w {
            let x = x0 + dx;
            if !(0..cw).contains(&x) {
                continue;
            }
            let sx = ((dx as f64 / scale) as usize).min(w.saturating_sub(1));
            let from = (sy * w + sx) * 4;
            let Some(src) = image.get(from..from + 4) else {
                continue;
            };
            let alpha = src.get(3).copied().map(u32::from).unwrap_or(0);
            if alpha == 0 {
                continue;
            }
            let i = ((y * cw + x) * 4) as usize;
            let Some(dst) = capture.rgba.get_mut(i..i + 4) else {
                continue;
            };
            // Source over, both premultiplied.
            for (d, s) in dst.iter_mut().zip(src) {
                let under = u32::from(*d) * (255 - alpha) / 255;
                *d = (u32::from(*s) + under).min(255) as u8;
            }
        }
    }
}

/// Draws the space into an offscreen buffer and reads it back (renderer without effects).
pub fn capture<R, T>(
    renderer: &mut R,
    output: &Output,
    space: &Space<Window>,
    clear_color: [f32; 4],
) -> Result<Capture, String>
where
    R: Renderer + ImportAll + Offscreen<T> + ExportMem,
    R::TextureId: Clone + Texture + 'static,
{
    let elements = space_render_elements::<_, Window, _>(renderer, [space], output, 1.0)
        .map_err(|e| format!("{e:?}"))?;
    capture_elements(renderer, output, &elements, clear_color)
}

/// Draws the given elements into an offscreen buffer and reads it back.
pub fn capture_elements<R, T, E>(
    renderer: &mut R,
    output: &Output,
    elements: &[E],
    clear_color: [f32; 4],
) -> Result<Capture, String>
where
    R: Renderer + Offscreen<T> + ExportMem,
    E: RenderElement<R>,
{
    let mode = output.current_mode().ok_or("output without a mode")?;
    let size = mode.size;
    let width = u32::try_from(size.w).map_err(|_| "invalid width")?;
    let height = u32::try_from(size.h).map_err(|_| "invalid height")?;
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);

    let mut target = renderer
        .create_buffer(Fourcc::Abgr8888, buffer_size)
        .map_err(|e| format!("offscreen buffer: {e:?}"))?;
    let mut framebuffer = renderer
        .bind(&mut target)
        .map_err(|e| format!("bind: {e:?}"))?;
    let scale = output.current_scale().fractional_scale();
    let mut damage_tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
    damage_tracker
        .render_output(renderer, &mut framebuffer, 0, elements, clear_color)
        .map_err(|e| format!("render: {e:?}"))?;
    let mapping = renderer
        .copy_framebuffer(
            &framebuffer,
            Rectangle::from_size(buffer_size),
            Fourcc::Abgr8888,
        )
        .map_err(|e| format!("copy: {e:?}"))?;
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|e| format!("readback: {e:?}"))?;

    let stride = (width as usize).checked_mul(4).ok_or("frame too large")?;
    let expected = stride
        .checked_mul(height as usize)
        .ok_or("frame too large")?;
    if bytes.len() < expected {
        return Err(format!(
            "the renderer returned {} bytes, {expected} are needed",
            bytes.len()
        ));
    }
    let rgba = bytes.get(..expected).unwrap_or_default().to_vec();
    Ok(Capture {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(width: u32, height: u32) -> Capture {
        Capture {
            width,
            height,
            rgba: [128, 128, 128, 255].repeat((width * height) as usize),
        }
    }

    fn at(capture: &Capture, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * capture.width + x) * 4) as usize;
        capture.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn the_pointer_is_drawn_at_its_hotspot_and_clipped() {
        let mut shot = grey(8, 8);
        // A 2 × 2 image: white, transparent / black, half black.
        let image = [
            255, 255, 255, 255, 0, 0, 0, 0, //
            0, 0, 0, 255, 0, 0, 0, 128,
        ];
        draw_over(&mut shot, (6.0, 6.0), (2, 2, &image), 1.0);
        assert_eq!(at(&shot, 6, 6), [255, 255, 255, 255]);
        assert_eq!(at(&shot, 7, 6), [128, 128, 128, 255]);
        assert_eq!(at(&shot, 6, 7), [0, 0, 0, 255]);
        assert_eq!(at(&shot, 7, 7), [63, 63, 63, 255]);
        // Partly outside: no panic, the inside part drawn.
        draw_over(&mut shot, (-1.0, -1.0), (2, 2, &image), 1.0);
        assert_eq!(at(&shot, 0, 0), [63, 63, 63, 255]);
        // Scale 2: every pixel twice as large.
        let mut big = grey(8, 8);
        draw_over(&mut big, (0.0, 0.0), (2, 2, &image), 2.0);
        assert_eq!(at(&big, 1, 1), [255, 255, 255, 255]);
        assert_eq!(at(&big, 0, 3), [0, 0, 0, 255]);
        // Hostile values are ignored.
        draw_over(&mut big, (f64::NAN, 0.0), (2, 2, &image), 1.0);
        draw_over(&mut big, (0.0, 0.0), (-2, 2, &image), 1.0);
        draw_over(&mut big, (0.0, 0.0), (4, 4, &image), 1.0);
    }

    #[test]
    fn window_margins_follow_the_scale_and_pasting_clips() {
        assert_eq!(window_margins(1.0), (22, 15, 23, 30));
        assert_eq!(window_margins(2.0), (44, 30, 46, 60));
        let part = grey(2, 2);
        let out = paste_on_transparent(&part, 1, 1, 4, 3);
        assert_eq!((out.width, out.height), (4, 3));
        assert_eq!(at(&out, 0, 0), [0, 0, 0, 0]);
        assert_eq!(at(&out, 1, 1), [128, 128, 128, 255]);
        assert_eq!(at(&out, 2, 2), [128, 128, 128, 255]);
        assert_eq!(at(&out, 3, 2), [0, 0, 0, 0]);
        // Partly outside on every side: only the inside is kept.
        let out = paste_on_transparent(&part, -1, -1, 1, 1);
        assert_eq!(at(&out, 0, 0), [128, 128, 128, 255]);
        let out = paste_on_transparent(&part, 5, 5, 3, 3);
        assert!(out.rgba.iter().all(|&b| b == 0));
    }
}
