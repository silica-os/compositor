//! The texture programs on the CPU: window corners (`corners.frag`) and display filters
//! (`filter.frag`) as shaders of the texels drawn, the genie (`genie.frag`) as an effect that
//! samples its snapshot, the bar tone (`tone.frag`) applied once to its texture.

use compositor_raster::fearless_simd::{Level, Simd, dispatch};
use compositor_raster::{Band, CpuRenderer, CpuTexture, Effect, Image, Rect, Shader, Walk};
use smithay::backend::renderer::gles::Uniform;

use super::{Uniforms, bilinear, clamp01, mix, pack, unpack};

/// `p` × `k` (0–1) on every channel, rounded.
#[inline]
fn scale(p: u32, k: f32) -> u32 {
    let k = (k * 256.0 + 0.5) as u32;
    let rb = ((p & 0x00ff_00ff) * k + 0x0080_0080) >> 8;
    let ag = (((p >> 8) & 0x00ff_00ff) * k + 0x0080_0080) >> 8;
    (rb & 0x00ff_00ff) | ((ag & 0x00ff_00ff) << 8)
}

/// The window corners: `corners.frag`'s coverage, in target pixels.
#[derive(Debug)]
pub(crate) struct Corners {
    /// The window's origin in the target and its size, in pixels.
    origin: [f64; 2],
    size: [f32; 2],
    radius: f32,
    exponent: f32,
}

impl Corners {
    pub fn new(origin: [f64; 2], size: [f32; 2], radius: f32, exponent: f32) -> Self {
        Self {
            origin,
            size,
            radius,
            exponent,
        }
    }

    fn coverage(&self, x: f32, y: f32) -> f32 {
        let [w, h] = self.size;
        if x < 0.0 || y < 0.0 || x > w || y > h {
            return 1.0;
        }
        let (ex, ey) = (x.min(w - x), y.min(h - y));
        let r = self.radius;
        if ex >= r || ey >= r {
            return 1.0;
        }
        let (qx, qy) = ((r - ex) / r, (r - ey) / r);
        let e = self.exponent;
        let f = (qx.powf(e) + qy.powf(e)).powf(1.0 / e);
        clamp01((1.0 - f) * r + 0.5)
    }
}

impl Shader for Corners {
    fn shade(&self, y: i32, x0: i32, row: &mut [u32]) {
        let py = (f64::from(y) + 0.5 - self.origin[1]) as f32;
        for (i, p) in row.iter_mut().enumerate() {
            let px = (f64::from(x0) + i as f64 + 0.5 - self.origin[0]) as f32;
            let k = self.coverage(px, py);
            if k < 1.0 {
                *p = scale(*p, k);
            }
        }
    }

    /// The four corner boxes, which never overlap (a rectangle shaded twice would be blended
    /// twice).
    fn region(&self) -> Option<Vec<Rect>> {
        if self.radius <= 0.0 {
            return Some(Vec::new());
        }
        let r = self.radius.ceil() as i32 + 1;
        let x0 = self.origin[0].floor() as i32;
        let y0 = self.origin[1].floor() as i32;
        let x1 = (self.origin[0] + f64::from(self.size[0])).ceil() as i32;
        let y1 = (self.origin[1] + f64::from(self.size[1])).ceil() as i32;
        let (mx, my) = (x0 + (x1 - x0) / 2, y0 + (y1 - y0) / 2);
        let (left, right) = ((x0, (x0 + r).min(mx)), ((x1 - r).max(mx), x1));
        let (top, bottom) = ((y0, (y0 + r).min(my)), ((y1 - r).max(my), y1));
        Some(
            [(left, top), (right, top), (left, bottom), (right, bottom)]
                .into_iter()
                .map(|((x0, x1), (y0, y1))| Rect::new(x0, y0, x1, y1))
                .filter(|r| !r.is_empty())
                .collect(),
        )
    }
}

/// The display filters: invert, grayscale, contrast (`filter.frag`).
#[derive(Debug)]
pub(crate) struct Filter {
    mode: f32,
    contrast: f32,
    level: Level,
}

impl Filter {
    pub fn new(uniforms: &[Uniform<'static>]) -> Self {
        let u = Uniforms(uniforms);
        Self {
            mode: u.f1("mode"),
            contrast: u.f1("contrast"),
            level: Level::new(),
        }
    }

    #[inline(always)]
    fn filter(&self, p: u32) -> u32 {
        let [r, g, b, a] = unpack(p);
        let mut rgb = if a > 0.0 {
            [r / a, g / a, b / a]
        } else {
            [r, g, b]
        };
        if self.mode >= 2.0 {
            let l = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
            rgb = [l; 3];
        }
        if self.mode == 1.0 || self.mode == 3.0 {
            rgb = rgb.map(|c| 1.0 - c);
        }
        let rgb = rgb.map(|c| clamp01((c - 0.5) * self.contrast + 0.5) * a);
        pack([rgb[0], rgb[1], rgb[2], a])
    }
}

impl Filter {
    /// `filter` over `row`, compiled for the SIMD level of `S`.
    #[inline(always)]
    fn filter_row<S: Simd>(&self, _: S, row: &mut [u32]) {
        for p in row {
            *p = self.filter(*p);
        }
    }
}

impl Shader for Filter {
    fn shade(&self, _y: i32, _x0: i32, row: &mut [u32]) {
        dispatch!(self.level, simd => self.filter_row(simd, row));
    }

    fn keeps_opaque(&self) -> bool {
        true
    }
}

/// The genie: the snapshot drawn through the funnel (`genie.frag`), in target pixels.
#[derive(Debug)]
pub(crate) struct Genie {
    snapshot: CpuTexture,
    /// The effect area's origin in the target.
    origin: [f64; 2],
    window: [f32; 4],
    target: [f32; 4],
    motion: [f32; 3],
    frame_uv: [f32; 4],
    alpha: f32,
}

impl Genie {
    pub fn new(
        snapshot: CpuTexture,
        origin: [f64; 2],
        uniforms: &[Uniform<'static>],
        alpha: f32,
    ) -> Self {
        let u = Uniforms(uniforms);
        Self {
            snapshot,
            origin,
            window: u.f4("window_rect"),
            target: u.f4("target_rect"),
            motion: u.f3("motion"),
            frame_uv: u.f4("frame_uv"),
            alpha,
        }
    }

    /// The rows of `rect` (`genie.frag`). What depends on the row alone is worked out once per
    /// row. Inside the window's edges the coverage is the row's and the texture coordinate moves
    /// by the same step from pixel to pixel: there the raster's bilinear sampler walks the
    /// snapshot. Only the pixel or two on each edge go through the shader's arithmetic.
    fn draw_rows(&self, image: &Image, band: &mut Band<'_>, rect: Rect) {
        let (window, target, motion) = (self.window, self.target, self.motion);
        let ease = |x: f32| {
            let x = clamp01(x);
            x * x * (3.0 - 2.0 * x)
        };
        let [u0, v0, du, dv] = self.frame_uv;
        let (w, h) = (image.width() as f32, image.height() as f32);
        let top = mix(window[1], target[1], motion[1]);
        let bottom = mix(window[3], target[3], motion[2]);
        let span = target[1] - window[1];
        let kernels = band.kernels();
        let mut row = vec![0u32; rect.width().max(0) as usize];
        // The centre of the rect's first column, from the effect's origin.
        let base = f64::from(rect.x0) + 0.5 - self.origin[0];
        for y in rect.y0..rect.y1 {
            let py = (f64::from(y) + 0.5 - self.origin[1]) as f32;
            let across = clamp01((py - top).min(bottom - py) + 0.5);
            if across <= 0.0 {
                continue;
            }
            let u = if span.abs() < 1.0 {
                1.0
            } else {
                (py - window[1]) / span
            };
            let k = motion[0] * ease(u);
            let left = mix(window[0], target[0], k);
            let right = mix(window[2], target[2], k);
            let width = (right - left).max(0.001);
            let qy = clamp01((py - top) / (bottom - top).max(0.001));
            let v = (v0 + qy * dv) * h;
            let (l, r) = (f64::from(left), f64::from(right));
            // The pixels the window can cover, a pixel wider on each side than its edges, and
            // within them the ones it covers whole.
            let at = |x: f64| x.max(0.0) as usize;
            let last = at((r + 1.0 - base).ceil()).min(row.len());
            let first = at((l - 1.0 - base).floor()).min(last);
            let inner_first = at((l + 0.5 - base).ceil()).clamp(first, last);
            let inner_last = at((r - 0.5 - base).floor() + 1.0).clamp(inner_first, last);
            let edge = |i: usize| {
                let px = (base + i as f64) as f32;
                let coverage = clamp01((px - left).min(right - px) + 0.5) * across;
                if coverage <= 0.0 {
                    return 0;
                }
                let qx = clamp01((px - left) / width);
                let texel = bilinear(image, (u0 + qx * du) * w, v);
                pack(texel.map(|c| c * self.alpha * coverage))
            };
            let segments = [
                (first, inner_first, false),
                (inner_first, inner_last, true),
                (inner_last, last, false),
            ];
            for (start, end, inner) in segments {
                let Some(out) = row.get_mut(start..end).filter(|out| !out.is_empty()) else {
                    continue;
                };
                let alpha = if inner {
                    let step = f64::from(du * w) / f64::from(width);
                    let x = f64::from(u0 * w) + (base + start as f64 - l) * step;
                    kernels.bilinear(image, Walk::new([x, f64::from(v)], [step, 0.0]), out);
                    (self.alpha * across * 255.0 + 0.5).min(255.0) as u32
                } else {
                    for (i, p) in (start..).zip(out.iter_mut()) {
                        *p = edge(i);
                    }
                    255
                };
                let Some(dst) = band.span_mut(y, rect.x0 + start as i32, rect.x0 + end as i32)
                else {
                    continue;
                };
                kernels.over(dst, out, alpha);
            }
        }
    }
}

impl Effect for Genie {
    fn draw(&self, band: &mut Band<'_>, rect: Rect) {
        // The snapshot is read while the frame draws: skipped if someone writes it.
        let Some(image) = self.snapshot.try_read() else {
            return;
        };
        self.draw_rows(&image, band, rect);
    }
}

/// The bar tone (`tone.frag`) applied to `image` in place: its rows are the first ones of the
/// output at `scale` pixels per point.
pub(crate) fn tone(renderer: &CpuRenderer, image: &mut Image, table: &[[f32; 11]; 7], scale: f64) {
    const ROWS: [f32; 7] = [0.0, 30.0, 45.0, 60.0, 80.0, 100.0, 120.0];
    let level = |c: usize| {
        if c >= 10 {
            255.0
        } else {
            96.0 + 16.0 * c as f32
        }
    };
    let at = |r: usize, c: usize| {
        table
            .get(r)
            .and_then(|row| row.get(c))
            .copied()
            .unwrap_or(0.0)
    };
    let mapped = |l: f32, y: f32| {
        if l <= 96.0 || y >= 120.0 {
            return l;
        }
        let c0 = (0..10).rev().find(|c| l >= level(*c)).unwrap_or(0);
        let fc = clamp01((l - level(c0)) / (level(c0 + 1) - level(c0)));
        let row = |r: usize| ROWS.get(r).copied().unwrap_or(120.0);
        let r0 = (0..6).rev().find(|r| y >= row(*r)).unwrap_or(0);
        let fr = clamp01((y - row(r0)) / (row(r0 + 1) - row(r0)));
        let a = mix(at(r0, c0), at(r0, c0 + 1), fc);
        let b = mix(at(r0 + 1, c0), at(r0 + 1, c0 + 1), fc);
        mix(a, b, fr)
    };
    let width = image.width();
    let scale = scale as f32;
    renderer.rows_costly(image.pixels_mut(), width, |y, row| {
        let y = (y as f32 + 0.5) / scale;
        for p in row {
            let [r, g, b, _] = unpack(*p);
            let l = (0.2126 * r + 0.7152 * g + 0.0722 * b) * 255.0;
            let shift = (mapped(l, y) - l) / 255.0;
            *p = pack([
                clamp01(r + shift),
                clamp01(g + shift),
                clamp01(b + shift),
                1.0,
            ]);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_by_one_and_zero_is_exact() {
        assert_eq!(scale(0x8040_2010, 1.0), 0x8040_2010);
        assert_eq!(scale(0xffff_ffff, 1.0), 0xffff_ffff);
        assert_eq!(scale(0xffff_ffff, 0.0), 0);
        assert_eq!(scale(0xff80_8080, 0.5), 0x8040_4040);
    }

    #[test]
    fn corner_boxes_never_overlap() {
        let corners = Corners::new([10.0, 10.0], [20.0, 12.0], 10.0, 2.5);
        let boxes = corners.region().unwrap_or_default();
        for (i, a) in boxes.iter().enumerate() {
            for b in boxes.iter().skip(i + 1) {
                assert!(a.intersect(*b).is_none(), "{a:?} {b:?}");
            }
        }
        // Inside the window, away from the corners: untouched.
        assert_eq!(corners.coverage(10.0, 6.0), 1.0);
        // The very corner is cut.
        assert_eq!(corners.coverage(0.2, 0.2), 0.0);
    }

    #[test]
    fn the_filter_inverts_and_keeps_alpha() {
        let filter = Filter {
            mode: 1.0,
            contrast: 1.0,
            level: Level::new(),
        };
        assert_eq!(filter.filter(0xff00_0000), 0xffff_ffff);
        assert_eq!(filter.filter(0x8080_8080) >> 24, 0x80);
    }
}
