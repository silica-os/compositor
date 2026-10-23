//! The glass blur on the CPU: `glass::gaussian` followed by `Blurred::back` (`downsample.frag`,
//! `blur.frag`, `upsample.frag`), in f32 between the passes.

use compositor_raster::{CpuRenderer, Image};

use super::{pack, unpack, zip_with};
use crate::glass::{BlurEdge, effective_factor, reduced_sigma};

/// RGBA in 0–1, `w` × `h`.
struct Plane {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Plane {
    fn of(image: &Image) -> Self {
        Self {
            w: image.width(),
            h: image.height(),
            px: image.pixels().iter().map(|p| unpack(*p)).collect(),
        }
    }

    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![[0.0; 4]; w.saturating_mul(h)],
        }
    }

    fn at(&self, x: usize, y: usize) -> [f32; 4] {
        self.px.get(y * self.w + x).copied().unwrap_or_default()
    }
}

/// The Gaussian weights within 3σ, from −reach to +reach, summing to 1.
fn kernel(sigma: f32) -> Vec<f32> {
    let reach = (3.0 * sigma).min(256.0).floor() as i32;
    let weights: Vec<f32> = (-reach..=reach)
        .map(|x| (-((x * x) as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = weights.iter().sum::<f32>().max(1e-6);
    weights.into_iter().map(|w| w / total).collect()
}

/// One pass of `blur.frag` along x or y, the edge repeated; `quantize` rounds to 8 bits as an
/// 8-bit target does.
fn pass(renderer: &CpuRenderer, src: &Plane, sigma: f32, vertical: bool, quantize: bool) -> Plane {
    let mut out = Plane::new(src.w, src.h);
    if src.w == 0 || src.h == 0 {
        return out;
    }
    let weights = kernel(sigma.max(1e-3));
    let reach = (weights.len() / 2) as isize;
    let (w, h) = (src.w as isize, src.h as isize);
    let finish = |v: f32| {
        if quantize {
            (v * 255.0).round() / 255.0
        } else {
            v
        }
    };
    renderer.rows(&mut out.px, src.w, |y, row| {
        if vertical {
            for (k, weight) in weights.iter().enumerate() {
                let sy = (y as isize + k as isize - reach).clamp(0, h - 1) as usize;
                let line = src.px.get(sy * src.w..(sy + 1) * src.w).unwrap_or_default();
                for (o, s) in row.iter_mut().zip(line) {
                    *o = zip_with(*o, *s, |o, s| o + s * weight);
                }
            }
        } else {
            let line = src.px.get(y * src.w..(y + 1) * src.w).unwrap_or_default();
            for (x, o) in row.iter_mut().enumerate() {
                for (k, weight) in weights.iter().enumerate() {
                    let sx = (x as isize + k as isize - reach).clamp(0, w - 1) as usize;
                    if let Some(s) = line.get(sx) {
                        *o = zip_with(*o, *s, |o, s| o + s * weight);
                    }
                }
            }
        }
        for o in row.iter_mut() {
            *o = o.map(finish);
        }
    });
    out
}

/// One source coordinate (a pixel centre) kept inside the source, as `downsample.frag`.
fn inside(p: f32, size: f32, mirror: bool) -> usize {
    let mut p = p;
    if mirror {
        p = p.abs();
        p = size - (size - p).abs();
    }
    p.clamp(0.5, size - 0.5).floor() as usize
}

/// `downsample.frag`: the box mean of `factor` × `factor` source pixels per output pixel,
/// with a border of `pad` output pixels holding the source extended past its edge.
fn downsample(
    renderer: &CpuRenderer,
    src: &Plane,
    small: (usize, usize),
    factor: usize,
    pad: usize,
    mirror: bool,
) -> Plane {
    let mut out = Plane::new(small.0, small.1);
    let (sw, sh) = (src.w as f32, src.h as f32);
    let norm = 1.0 / (factor * factor) as f32;
    renderer.rows(&mut out.px, small.0, |y, row| {
        let oy = (y as f32 - pad as f32) * factor as f32;
        let ys: Vec<usize> = (0..factor)
            .map(|j| inside(oy + j as f32 + 0.5, sh, mirror))
            .collect();
        for (x, o) in row.iter_mut().enumerate() {
            let ox = (x as f32 - pad as f32) * factor as f32;
            let mut sum = [0.0f32; 4];
            for i in 0..factor {
                let sx = inside(ox + i as f32 + 0.5, sw, mirror);
                for &sy in &ys {
                    sum = zip_with(sum, src.at(sx, sy), |a, b| a + b);
                }
            }
            *o = sum.map(|v| v * norm);
        }
    });
    out
}

/// Bilinear at texel coordinates kept half a texel inside (`upsample.frag`).
fn sample(plane: &Plane, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (plane.w as f32, plane.h as f32);
    let (x, y) = (u.clamp(0.5, w - 0.5) - 0.5, v.clamp(0.5, h - 0.5) - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as usize, y0 as usize);
    let (x1, y1) = ((x0 + 1).min(plane.w - 1), (y0 + 1).min(plane.h - 1));
    let (a, b, c, d) = (
        plane.at(x0, y0),
        plane.at(x1, y0),
        plane.at(x0, y1),
        plane.at(x1, y1),
    );
    let top = zip_with(a, b, |a, b| a + (b - a) * fx);
    let bottom = zip_with(c, d, |c, d| c + (d - c) * fx);
    zip_with(top, bottom, |t, b| t + (b - t) * fy)
}

/// A separable Gaussian of σ `sigma` pixels over `sharp` at 1/`factor` of the resolution, back
/// at full size with the sharp share `sharp_share` under it: `glass::blur` on the CPU.
pub(crate) fn blur(
    renderer: &CpuRenderer,
    sharp: &Image,
    sigma: f64,
    factor: u32,
    sharp_share: f32,
    edge: BlurEdge,
) -> Image {
    let source = Plane::of(sharp);
    let (w, h) = (source.w, source.h);
    let mut out = Image::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    let factor = effective_factor(sigma, factor);
    let mirror = edge == BlurEdge::Mirror;
    // The blurred plane and where the source lies in it: offset and texels per pixel.
    let (blurred, offset, step) = if factor > 1 {
        let f = factor as usize;
        let sigma_small = reduced_sigma(sigma, factor);
        let pad = (3.0 * sigma_small).ceil() as usize + 1;
        let small = (w.div_ceil(f) + 2 * pad, h.div_ceil(f) + 2 * pad);
        let prefiltered = downsample(renderer, &source, small, f, pad, mirror);
        let across = pass(renderer, &prefiltered, sigma_small as f32, false, false);
        let blurred = pass(renderer, &across, sigma_small as f32, true, false);
        (blurred, pad as f32, 1.0 / factor as f32)
    } else {
        let across = pass(renderer, &source, sigma as f32, false, true);
        let blurred = pass(renderer, &across, sigma as f32, true, true);
        (blurred, 0.0, 1.0)
    };
    let alpha = 1.0 - sharp_share;
    renderer.rows(out.pixels_mut(), w, |y, row| {
        let v = offset + (y as f32 + 0.5) * step;
        for (x, o) in row.iter_mut().enumerate() {
            let b = if factor > 1 {
                sample(&blurred, offset + (x as f32 + 0.5) * step, v)
            } else {
                blurred.at(x, y)
            };
            let s = source.at(x, y);
            let k = 1.0 - b[3] * alpha;
            *o = pack(zip_with(b, s, |b, s| b * alpha + s * k));
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_image_stays_flat() {
        let mut image = Image::new(37, 23);
        image.pixels_mut().fill(0xff40_8020);
        let renderer = CpuRenderer::with_threads(1);
        for factor in [1, 2, 4] {
            let out = blur(&renderer, &image, 12.0, factor, 0.0, BlurEdge::Clamp);
            assert!(out.pixels().iter().all(|p| *p == 0xff40_8020), "{factor}");
        }
    }

    #[test]
    fn the_kernel_sums_to_one() {
        for sigma in [0.5, 3.0, 40.0, 200.0] {
            let sum: f32 = kernel(sigma).iter().sum();
            assert!((sum - 1.0).abs() < 1e-4);
        }
    }
}
