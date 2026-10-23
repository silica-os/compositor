//! The glass blur on the CPU: `glass::gaussian` followed by `Blurred::back` (`downsample.frag`,
//! `blur.frag`, `upsample.frag`), in f32 between the passes. Every loop runs along whole rows of
//! channels, so that it vectorizes.

use std::cell::RefCell;

use compositor_raster::{CpuRenderer, Image};

use super::{pack, unpack, zip_with};
use crate::glass::{BlurEdge, effective_factor, reduced_sigma};

/// Premultiplied RGBA in 0–1, `w` × `h`, four channels per pixel.
struct Plane {
    w: usize,
    h: usize,
    px: Vec<f32>,
}

impl Plane {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![0.0; w.saturating_mul(h).saturating_mul(4)],
        }
    }

    /// Row `y`, clamped into the plane: the edge repeated.
    fn row(&self, y: isize) -> &[f32] {
        let Some(last) = self.h.checked_sub(1) else {
            return &[];
        };
        let y = y.clamp(0, last as isize) as usize;
        let n = self.w * 4;
        self.px.get(y * n..(y + 1) * n).unwrap_or_default()
    }
}

thread_local! {
    /// A row of channels, per thread.
    static SCRATCH: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
}

/// Pixel `x` of a row of channels.
#[inline]
fn texel(row: &[f32], x: usize) -> [f32; 4] {
    row.get(x * 4..x * 4 + 4)
        .and_then(|c| c.try_into().ok())
        .unwrap_or_default()
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

/// `out` = the symmetric `weights` over `taps(k)`, the pair of rows at −k and +k (the centre at
/// 0), rounded to 8 bits if `quantize`, as an 8-bit target stores it.
#[inline]
fn convolve<'a>(
    out: &mut [f32],
    weights: &[f32],
    quantize: bool,
    taps: impl Fn(usize) -> (&'a [f32], &'a [f32]),
) {
    let reach = weights.len() / 2;
    let centre = weights.get(reach).copied().unwrap_or(1.0);
    for (o, c) in out.iter_mut().zip(taps(0).0) {
        *o = c * centre;
    }
    for (k, weight) in weights.iter().skip(reach + 1).enumerate() {
        let (a, b) = taps(k + 1);
        for ((o, a), b) in out.iter_mut().zip(a).zip(b) {
            *o += weight * (a + b);
        }
    }
    if quantize {
        for o in out.iter_mut() {
            *o = (*o * 255.0).round() / 255.0;
        }
    }
}

/// One pass of `blur.frag` along x over rows of `w` pixels, the edge repeated; `load` writes
/// source row `y` into the channels it is given.
fn across(
    renderer: &CpuRenderer,
    (w, h): (usize, usize),
    weights: &[f32],
    quantize: bool,
    load: impl Fn(usize, &mut [f32]) + Sync,
) -> Plane {
    let mut out = Plane::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    let reach = weights.len() / 2;
    renderer.rows(&mut out.px, w * 4, |y, out| {
        SCRATCH.with_borrow_mut(|line| {
            line.clear();
            line.resize((w + 2 * reach) * 4, 0.0);
            let (left, rest) = line.split_at_mut(reach * 4);
            let (middle, right) = rest.split_at_mut(w * 4);
            load(y, middle);
            let (Some(first), Some(last)) = (
                middle.first_chunk::<4>().copied(),
                middle.last_chunk::<4>().copied(),
            ) else {
                return;
            };
            for c in left.as_chunks_mut::<4>().0.iter_mut() {
                c.copy_from_slice(&first);
            }
            for c in right.as_chunks_mut::<4>().0.iter_mut() {
                c.copy_from_slice(&last);
            }
            let line = &line[..];
            let at = |k: usize| line.get(k * 4..).unwrap_or_default();
            convolve(out, weights, quantize, |k| (at(reach - k), at(reach + k)));
        });
    });
    out
}

/// One pass of `blur.frag` along y, the edge repeated.
fn down(renderer: &CpuRenderer, src: &Plane, weights: &[f32], quantize: bool) -> Plane {
    let mut out = Plane::new(src.w, src.h);
    if src.w == 0 || src.h == 0 {
        return out;
    }
    renderer.rows(&mut out.px, src.w * 4, |y, out| {
        let y = y as isize;
        convolve(out, weights, quantize, |k| {
            (src.row(y - k as isize), src.row(y + k as isize))
        });
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
    src: &Image,
    small: (usize, usize),
    factor: usize,
    pad: usize,
    mirror: bool,
) -> Plane {
    let mut out = Plane::new(small.0, small.1);
    let (sw, sh) = (src.width(), src.height());
    if sw == 0 || sh == 0 || small.0 == 0 || factor == 0 {
        return out;
    }
    let norm = 1.0 / (255 * factor * factor) as f32;
    let start = |o: usize| (o as f32 - pad as f32) * factor as f32;
    // The source column of each of the `factor` samples of every output pixel.
    let columns: Vec<usize> = (0..small.0 * factor)
        .map(|i| {
            inside(
                start(i / factor) + (i % factor) as f32 + 0.5,
                sw as f32,
                mirror,
            )
        })
        .collect();
    let pixels = src.pixels();
    renderer.rows(&mut out.px, small.0 * 4, |y, out| {
        SCRATCH.with_borrow_mut(|sum| {
            // The `factor` source rows summed, in 0–255.
            sum.clear();
            sum.resize(sw * 4, 0.0);
            for j in 0..factor {
                let sy = inside(start(y) + j as f32 + 0.5, sh as f32, mirror);
                let row = pixels.get(sy * sw..(sy + 1) * sw).unwrap_or_default();
                for (s, p) in sum.as_chunks_mut::<4>().0.iter_mut().zip(row) {
                    s[0] += ((p >> 16) & 0xff) as f32;
                    s[1] += ((p >> 8) & 0xff) as f32;
                    s[2] += (p & 0xff) as f32;
                    s[3] += (p >> 24) as f32;
                }
            }
            for (o, columns) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(columns.chunks_exact(factor))
            {
                let mut total = [0.0f32; 4];
                for &x in columns {
                    for (t, s) in total.iter_mut().zip(texel(sum, x)) {
                        *t += s;
                    }
                }
                for (o, t) in o.iter_mut().zip(total) {
                    *o = t * norm;
                }
            }
        });
    });
    out
}

/// Where each pixel along an axis samples a plane `size` texels long, bilinear at the pixel
/// centre mapped to `offset + (x + 0.5) · step`, kept half a texel inside (`upsample.frag`):
/// the two texels and the weight of the second.
fn taps(pixels: usize, size: usize, offset: f32, step: f32) -> Vec<(usize, usize, f32)> {
    let last = size.saturating_sub(1);
    (0..pixels)
        .map(|x| {
            let t =
                (offset + (x as f32 + 0.5) * step).clamp(0.5, (size as f32 - 0.5).max(0.5)) - 0.5;
            let t0 = t.floor();
            let i = t0 as usize;
            (i.min(last), (i + 1).min(last), t - t0)
        })
        .collect()
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
    let (w, h) = (sharp.width(), sharp.height());
    let mut out = Image::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    let pixels = sharp.pixels();
    let factor = effective_factor(sigma, factor);
    // The blurred plane and where the source lies in it: offset and texels per pixel.
    let (blurred, offset, step) = if factor > 1 {
        let f = factor as usize;
        let sigma_small = reduced_sigma(sigma, factor);
        let pad = (3.0 * sigma_small).ceil() as usize + 1;
        let small = (w.div_ceil(f) + 2 * pad, h.div_ceil(f) + 2 * pad);
        let prefiltered = downsample(renderer, sharp, small, f, pad, edge == BlurEdge::Mirror);
        let weights = kernel((sigma_small as f32).max(1e-3));
        let across = across(renderer, small, &weights, false, |y, row| {
            for (o, s) in row.iter_mut().zip(prefiltered.row(y as isize)) {
                *o = *s;
            }
        });
        let blurred = down(renderer, &across, &weights, false);
        (blurred, pad as f32, 1.0 / factor as f32)
    } else {
        let weights = kernel((sigma as f32).max(1e-3));
        let across = across(renderer, (w, h), &weights, true, |y, row| {
            let line = pixels.get(y * w..(y + 1) * w).unwrap_or_default();
            for (o, p) in row.as_chunks_mut::<4>().0.iter_mut().zip(line) {
                o.copy_from_slice(&unpack(*p));
            }
        });
        let blurred = down(renderer, &across, &weights, true);
        (blurred, 0.0, 1.0)
    };
    let alpha = 1.0 - sharp_share;
    let columns = taps(w, blurred.w, offset, step);
    let rows = taps(h, blurred.h, offset, step);
    renderer.rows(out.pixels_mut(), w, |y, row| {
        let Some(&(y0, y1, fy)) = rows.get(y) else {
            return;
        };
        SCRATCH.with_borrow_mut(|line| {
            // The two rows around the pixel centre, mixed: the vertical half of the bilinear.
            line.clear();
            line.extend(
                blurred
                    .row(y0 as isize)
                    .iter()
                    .zip(blurred.row(y1 as isize))
                    .map(|(a, b)| a + (b - a) * fy),
            );
            let sharp = pixels.get(y * w..(y + 1) * w).unwrap_or_default();
            for ((o, &(x0, x1, fx)), s) in row.iter_mut().zip(&columns).zip(sharp) {
                let (a, b, s) = (texel(line, x0), texel(line, x1), unpack(*s));
                let mut p = zip_with(a, b, |a, b| (a + (b - a) * fx) * alpha);
                let k = 1.0 - p[3];
                for (p, s) in p.iter_mut().zip(s) {
                    *p += s * k;
                }
                *o = pack(p);
            }
        });
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
