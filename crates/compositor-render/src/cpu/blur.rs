//! The glass blur on the CPU: `glass::gaussian` followed by `Blurred::back` (`downsample.frag`,
//! `blur.frag`, `upsample.frag`), in f32 between the passes. Every loop runs along whole rows of
//! channels, so that it vectorizes.

use std::cell::RefCell;
use std::ops::Range;

use compositor_raster::fearless_simd::{Simd, dispatch};
use compositor_raster::{CpuRenderer, Image, Walk};

use super::{pack, unpack};
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
    /// A row of the blurred texture upsampled, per thread.
    static UPSAMPLED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
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
#[inline(always)]
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

/// `convolve` compiled for the SIMD level of `S`.
#[inline(always)]
fn convolve_row<'a, S: Simd>(
    _: S,
    out: &mut [f32],
    weights: &[f32],
    quantize: bool,
    taps: impl Fn(usize) -> (&'a [f32], &'a [f32]),
) {
    convolve(out, weights, quantize, taps);
}

/// One pass of `blur.frag` along x over rows of `w` pixels, the edge repeated; `load` writes
/// source row `y` into the channels it is given. Only `rows` are made; the others stay 0.
fn across(
    renderer: &CpuRenderer,
    (w, h): (usize, usize),
    weights: &[f32],
    quantize: bool,
    rows: Range<usize>,
    load: impl Fn(usize, &mut [f32]) + Sync,
) -> Plane {
    let mut out = Plane::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    let reach = weights.len() / 2;
    let level = renderer.level();
    let band = out.px.get_mut(rows.start * w * 4..rows.end * w * 4);
    renderer.rows_costly(band.unwrap_or_default(), w * 4, |y, out| {
        let y = y + rows.start;
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
            let taps = |k: usize| (at(reach - k), at(reach + k));
            dispatch!(level, simd => convolve_row(simd, out, weights, quantize, taps));
        });
    });
    out
}

/// One pass of `blur.frag` along y, the edge repeated, into 8 bits per channel. Only `rows`
/// are made; the others stay transparent.
fn down(
    renderer: &CpuRenderer,
    src: &Plane,
    weights: &[f32],
    quantize: bool,
    rows: Range<usize>,
) -> Image {
    let mut out = Image::new(src.w, src.h);
    if src.w == 0 || src.h == 0 {
        return out;
    }
    let level = renderer.level();
    let band = out
        .pixels_mut()
        .get_mut(rows.start * src.w..rows.end * src.w);
    renderer.rows_costly(band.unwrap_or_default(), src.w, |y, out| {
        let y = (y + rows.start) as isize;
        let taps = |k: usize| (src.row(y - k as isize), src.row(y + k as isize));
        SCRATCH.with_borrow_mut(|line| {
            line.clear();
            line.resize(src.w * 4, 0.0);
            dispatch!(level, simd => down_row(simd, out, line, weights, quantize, taps));
        });
    });
    out
}

/// One row of `down`, compiled for the SIMD level of `S`.
#[inline(always)]
fn down_row<'a, S: Simd>(
    _: S,
    out: &mut [u32],
    line: &mut [f32],
    weights: &[f32],
    quantize: bool,
    taps: impl Fn(usize) -> (&'a [f32], &'a [f32]),
) {
    convolve(line, weights, quantize, taps);
    for (o, c) in out.iter_mut().zip(line.as_chunks::<4>().0) {
        *o = pack(*c);
    }
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
/// with a border of `pad` output pixels holding the source extended past its edge. Only `rows`
/// are made; the others stay 0.
fn downsample(
    renderer: &CpuRenderer,
    src: &Image,
    (small, factor, pad): ((usize, usize), usize, usize),
    mirror: bool,
    rows: Range<usize>,
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
    let level = renderer.level();
    let band = out
        .px
        .get_mut(rows.start * small.0 * 4..rows.end * small.0 * 4);
    renderer.rows_costly(band.unwrap_or_default(), small.0 * 4, |y, out| {
        let y = y + rows.start;
        SCRATCH.with_borrow_mut(|sum| {
            let rows = (0..factor).map(|j| {
                let sy = inside(start(y) + j as f32 + 0.5, sh as f32, mirror);
                pixels.get(sy * sw..(sy + 1) * sw).unwrap_or_default()
            });
            let columns = (&columns[..], factor, norm);
            dispatch!(level, simd => downsample_row(simd, out, sum, rows, columns));
        });
    });
    out
}

/// One output row of `downsample` from its `factor` source `rows`, compiled for the SIMD level
/// of `S`: `columns` holds the source column of each sample, and `norm` takes the sum to 0–1.
#[inline(always)]
fn downsample_row<'a, S: Simd>(
    _: S,
    out: &mut [f32],
    sum: &mut Vec<f32>,
    rows: impl Iterator<Item = &'a [u32]>,
    (columns, factor, norm): (&[usize], usize, f32),
) {
    // The source rows summed, in 0–255.
    let mut rows = rows.peekable();
    sum.clear();
    sum.resize(rows.peek().map_or(0, |row| row.len() * 4), 0.0);
    for row in rows {
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
}

/// A separable Gaussian of σ `sigma` pixels over `sharp` at 1/`factor` of the resolution, back
/// at full size with the sharp share `sharp_share` under it: `glass::blur` on the CPU.
#[cfg(test)]
pub(crate) fn blur(
    renderer: &CpuRenderer,
    sharp: &Image,
    sigma: f64,
    factor: u32,
    sharp_share: f32,
    edge: BlurEdge,
) -> Image {
    let all = [0, 0, sharp.width(), sharp.height()];
    blur_area(renderer, sharp, (sigma, factor, sharp_share), edge, all)
}

/// `blur`, made only over `area` (`[x0, y0, x1, y1)`), transparent elsewhere: the pixels
/// someone reads.
pub(crate) fn blur_area(
    renderer: &CpuRenderer,
    sharp: &Image,
    (sigma, factor, sharp_share): (f64, u32, f32),
    edge: BlurEdge,
    area: [usize; 4],
) -> Image {
    let (w, h) = (sharp.width(), sharp.height());
    let mut out = Image::new(w, h);
    let [x0, y0, x1, y1] = area;
    let (x0, y0) = (x0.min(w), y0.min(h));
    let (x1, y1) = (x1.clamp(x0, w), y1.clamp(y0, h));
    if x0 == x1 || y0 == y1 {
        return out;
    }
    let pixels = sharp.pixels();
    let factor = effective_factor(sigma, factor);
    // The blurred texture's size, where the source lies in it (offset and texels per pixel),
    // and the Gaussian over it.
    let (f, pad, sigma) = if factor > 1 {
        let sigma = reduced_sigma(sigma, factor);
        (factor as usize, (3.0 * sigma).ceil() as usize + 1, sigma)
    } else {
        (1, 0, sigma)
    };
    let size = (w.div_ceil(f) + 2 * pad, h.div_ceil(f) + 2 * pad);
    let (offset, step) = (pad as f64, 1.0 / f as f64);
    let weights = kernel((sigma as f32).max(1e-3));
    // The texture rows the mix reads, and the rows within the Gaussian's reach of them: no
    // pass makes any other.
    let texel = |y: usize| (offset + (y as f64 + 0.5) * step - 0.5).floor().max(0.0) as usize;
    let read = texel(y0).min(size.1 - 1)..(texel(y1 - 1) + 2).min(size.1);
    let reach = weights.len() / 2;
    let near = read.start.saturating_sub(reach)..(read.end + reach).min(size.1);
    let blurred = if f > 1 {
        let mirror = edge == BlurEdge::Mirror;
        let prefiltered = downsample(renderer, sharp, (size, f, pad), mirror, near.clone());
        let across = across(renderer, size, &weights, false, near, |y, row| {
            for (o, s) in row.iter_mut().zip(prefiltered.row(y as isize)) {
                *o = *s;
            }
        });
        down(renderer, &across, &weights, false, read)
    } else {
        let across = across(renderer, size, &weights, true, near, |y, row| {
            let line = pixels.get(y * w..(y + 1) * w).unwrap_or_default();
            for (o, p) in row.as_chunks_mut::<4>().0.iter_mut().zip(line) {
                o.copy_from_slice(&unpack(*p));
            }
        });
        down(renderer, &across, &weights, true, read)
    };
    // The blurred texture upsampled along each row (`upsample.frag`: bilinear, kept inside
    // the texture, which the sampler's clamp to the edge gives) and drawn over the sharp share.
    let alpha = ((1.0 - sharp_share).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    let u = offset + (x0 as f64 + 0.5) * step;
    let kernels = renderer.kernels();
    let band = out.pixels_mut().get_mut(y0 * w..y1 * w).unwrap_or_default();
    renderer.rows_costly(band, w, |y, row| {
        let y = y + y0;
        let (Some(row), Some(sharp)) = (row.get_mut(x0..x1), pixels.get(y * w + x0..y * w + x1))
        else {
            return;
        };
        row.copy_from_slice(sharp);
        UPSAMPLED.with_borrow_mut(|line| {
            line.clear();
            line.resize(row.len(), 0);
            let v = offset + (y as f64 + 0.5) * step;
            kernels.bilinear(&blurred, Walk::new([u, v], [step, 0.0]), line);
            kernels.over(row, line, alpha);
        });
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_area_is_the_whole_blur_there() {
        let (w, h) = (61, 47);
        let mut image = Image::new(w, h);
        for (i, p) in image.pixels_mut().iter_mut().enumerate() {
            *p = 0xff00_0000 | ((i as u32).wrapping_mul(2_654_435_761) & 0x00ff_ffff);
        }
        let renderer = CpuRenderer::with_threads(1);
        let (xs, ys) = (7..40, 11..30);
        for factor in [1, 2, 4] {
            let all = blur(&renderer, &image, 6.0, factor, 0.1, BlurEdge::Clamp);
            let area = [xs.start, ys.start, xs.end, ys.end];
            let part = blur_area(&renderer, &image, (6.0, factor, 0.1), BlurEdge::Clamp, area);
            for (i, (p, a)) in part.pixels().iter().zip(all.pixels()).enumerate() {
                let (x, y) = (i % w, i / w);
                let expected = if xs.contains(&x) && ys.contains(&y) {
                    *a
                } else {
                    0
                };
                assert_eq!(*p, expected, "factor {factor} at {x},{y}");
            }
        }
    }

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
