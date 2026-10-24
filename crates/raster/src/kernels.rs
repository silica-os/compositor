//! The per-row kernels: fill, copy, source-over, alpha, sampling. Pixels are premultiplied
//! `0xAARRGGBB`; the arithmetic works on two 16-bit lanes per `u32` (red and blue, alpha and
//! green), which LLVM vectorizes across pixels once `dispatch!` compiles the loop for the
//! machine's SIMD level.
//!
//! Every division by 255 rounds to nearest, as a unorm8 GPU blend does.

use std::cell::RefCell;

use fearless_simd::{Level, Simd, dispatch};

use crate::image::Image;

const LANES: u32 = 0x00ff_00ff;

/// `x / 255` rounded, in each 16-bit lane of `x` (each lane at most 255 × 255).
#[inline(always)]
fn div255x2(x: u32) -> u32 {
    let t = x + 0x0080_0080;
    ((t + ((t >> 8) & LANES)) >> 8) & LANES
}

/// Each 16-bit lane of `x` (at most 510) clamped to 255.
#[inline(always)]
fn saturate(x: u32) -> u32 {
    (x | (((x >> 8) & 0x0001_0001) * 0xff)) & LANES
}

/// `p × f / 255` on every channel, `f` at most 255.
#[inline(always)]
pub(crate) fn scale(p: u32, f: u32) -> u32 {
    div255x2((p & LANES) * f) | (div255x2(((p >> 8) & LANES) * f) << 8)
}

/// `s` over `d`.
#[inline(always)]
pub(crate) fn over(s: u32, d: u32) -> u32 {
    let inv = 255 - (s >> 24);
    let rb = saturate(div255x2((d & LANES) * inv) + (s & LANES));
    let ag = saturate(div255x2(((d >> 8) & LANES) * inv) + ((s >> 8) & LANES));
    rb | (ag << 8)
}

/// `a × (256 − f) + b × f`, over 256 and rounded, on every channel; `f` at most 256.
#[inline(always)]
fn lerp(a: u32, b: u32, f: u32) -> u32 {
    let g = 256 - f;
    let rb = ((a & LANES) * g + (b & LANES) * f + 0x0080_0080) >> 8;
    let ag = (((a >> 8) & LANES) * g + ((b >> 8) & LANES) * f + 0x0080_0080) >> 8;
    (rb & LANES) | ((ag & LANES) << 8)
}

#[inline(always)]
fn fill_over_row<S: Simd>(_: S, dst: &mut [u32], color: u32) {
    for d in dst {
        *d = over(color, *d);
    }
}

#[inline(always)]
fn over_row<S: Simd>(_: S, dst: &mut [u32], src: &[u32]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = over(*s, *d);
    }
}

#[inline(always)]
fn over_alpha_row<S: Simd>(_: S, dst: &mut [u32], src: &[u32], alpha: u32) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = over(scale(*s, alpha), *d);
    }
}

#[inline(always)]
fn copy_alpha_row<S: Simd>(_: S, dst: &mut [u32], src: &[u32], alpha: u32) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = scale(*s, alpha);
    }
}

/// The SIMD level the kernels run at, found once.
#[derive(Clone, Copy, Debug)]
pub struct Kernels {
    level: Level,
}

impl Default for Kernels {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernels {
    pub fn new() -> Self {
        Self {
            level: Level::new(),
        }
    }

    /// The kernels at the baseline level (SSE2 on x86-64), for tests against the best one.
    #[cfg(test)]
    pub(crate) fn fallback() -> Self {
        Self {
            level: Level::baseline(),
        }
    }

    /// The SIMD level, for `fearless_simd::dispatch!`.
    pub fn level(self) -> Level {
        self.level
    }

    /// `dst` sampled from `src` along `walk`, bilinear (`sample_bilinear`).
    pub fn bilinear(self, src: &Image, walk: Walk, dst: &mut [u32]) {
        dispatch!(self.level, simd => bilinear_row(simd, src, walk, dst));
    }

    pub fn fill(self, dst: &mut [u32], color: u32) {
        dst.fill(color);
    }

    /// `color` over every pixel of `dst`.
    pub fn fill_over(self, dst: &mut [u32], color: u32) {
        match color >> 24 {
            0 if color == 0 => {}
            255 => dst.fill(color),
            _ => dispatch!(self.level, simd => fill_over_row(simd, dst, color)),
        }
    }

    /// `src` × `alpha` (0..=255) into `dst`, replacing it.
    pub fn copy(self, dst: &mut [u32], src: &[u32], alpha: u32) {
        if alpha >= 255 {
            let n = dst.len().min(src.len());
            if let (Some(d), Some(s)) = (dst.get_mut(..n), src.get(..n)) {
                d.copy_from_slice(s);
            }
        } else {
            dispatch!(self.level, simd => copy_alpha_row(simd, dst, src, alpha));
        }
    }

    /// `src` × `alpha` (0..=255) over `dst`.
    pub fn over(self, dst: &mut [u32], src: &[u32], alpha: u32) {
        if alpha >= 255 {
            dispatch!(self.level, simd => over_row(simd, dst, src));
        } else if alpha > 0 {
            dispatch!(self.level, simd => over_alpha_row(simd, dst, src, alpha));
        }
    }
}

/// A walk through a source image: the texel coordinates of the first pixel's center and
/// their steps along a row, in 16.16 fixed point.
#[derive(Clone, Copy, Debug)]
pub struct Walk {
    pub u: i64,
    pub v: i64,
    pub du: i64,
    pub dv: i64,
}

impl Walk {
    /// The walk from `(u, v)` by `(du, dv)` per pixel, in texels.
    pub fn new([u, v]: [f64; 2], [du, dv]: [f64; 2]) -> Self {
        Self {
            u: fixed(u),
            v: fixed(v),
            du: fixed(du),
            dv: fixed(dv),
        }
    }
}

/// 16.16 fixed point, saturating far outside any texture.
pub(crate) fn fixed(v: f64) -> i64 {
    (v * 65536.0)
        .round()
        .clamp(-(1i64 << 46) as f64, (1i64 << 46) as f64) as i64
}

/// One texel per pixel, the nearest, clamped to the image's edges.
pub fn sample_nearest(src: &Image, walk: Walk, dst: &mut [u32]) {
    let (w, h) = (src.width() as i64, src.height() as i64);
    if w == 0 || h == 0 {
        dst.fill(0);
        return;
    }
    let pixels = src.pixels();
    let (mut u, mut v) = (walk.u, walk.v);
    for d in dst {
        let x = (u >> 16).clamp(0, w - 1);
        let y = (v >> 16).clamp(0, h - 1);
        *d = pixels.get((y * w + x) as usize).copied().unwrap_or(0);
        u += walk.du;
        v += walk.dv;
    }
}

/// `sample_bilinear` compiled for the SIMD level of `S`.
#[inline(always)]
fn bilinear_row<S: Simd>(_: S, src: &Image, walk: Walk, dst: &mut [u32]) {
    if walk.dv == 0 {
        let done = MIXED.try_with(|mixed| {
            let Ok(mut mixed) = mixed.try_borrow_mut() else {
                return false;
            };
            sample_bilinear_across(src, walk, dst, &mut mixed);
            true
        });
        if done == Ok(true) {
            return;
        }
    }
    sample_bilinear(src, walk, dst);
}

thread_local! {
    /// The texture row that `sample_bilinear_across` mixes, kept between calls.
    static MIXED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// `sample_bilinear` along a texture row (`dv` 0, as for scaling): the two texture rows are
/// mixed once over the columns the walk reaches, and each pixel then mixes two neighbours of
/// that mix. The weights are the same; only the order of the steps differs.
#[inline(always)]
fn sample_bilinear_across(src: &Image, walk: Walk, dst: &mut [u32], mixed: &mut Vec<u32>) {
    let (w, h) = (src.width() as i64, src.height() as i64);
    let Some(last) = dst.len().checked_sub(1) else {
        return;
    };
    if w == 0 || h == 0 {
        dst.fill(0);
        return;
    }
    let pixels = src.pixels();
    let row = |y: i64| {
        let start = (y.clamp(0, h - 1) * w) as usize;
        pixels.get(start..start + w as usize).unwrap_or_default()
    };
    // The texel centers are at .5: weights from the position half a texel back.
    let (u, v) = (walk.u - 0x8000, walk.v - 0x8000);
    let (upper, lower) = (row(v >> 16), row((v >> 16) + 1));
    let fy = ((v >> 8) & 0xff) as u32;
    // The columns the walk reaches and the one after, as far as one past each edge: further
    // out a column reads as the edge's.
    let end = u + walk.du * last as i64;
    let first = (u.min(end) >> 16).clamp(-1, w - 1);
    let after = ((u.max(end) >> 16) + 1).clamp(first + 1, w);
    let edge = |x: usize| {
        let (a, b) = (upper.get(x), lower.get(x));
        lerp(a.copied().unwrap_or(0), b.copied().unwrap_or(0), fy)
    };
    mixed.clear();
    if first < 0 {
        mixed.push(edge(0));
    }
    let inner = first.max(0) as usize..after.min(w - 1) as usize + 1;
    let start = mixed.len();
    mixed.resize(start + inner.len(), 0);
    let (Some(out), Some(a), Some(b)) = (
        mixed.get_mut(start..),
        upper.get(inner.clone()),
        lower.get(inner),
    ) else {
        return;
    };
    if fy == 0 {
        out.copy_from_slice(a);
    } else {
        for ((m, a), b) in out.iter_mut().zip(a).zip(b) {
            *m = lerp(*a, *b, fy);
        }
    }
    if after == w {
        mixed.push(edge(w as usize - 1));
    }
    let mut u = u;
    for d in dst {
        let i = ((u >> 16).clamp(first, after - 1) - first) as usize;
        let fx = ((u >> 8) & 0xff) as u32;
        let a = mixed.get(i).copied().unwrap_or(0);
        let b = mixed.get(i + 1).copied().unwrap_or(0);
        *d = lerp(a, b, fx);
        u += walk.du;
    }
}

/// Four texels per pixel weighted by distance (GL's `LINEAR` with `CLAMP_TO_EDGE`), with
/// 8-bit weights.
#[inline(always)]
pub fn sample_bilinear(src: &Image, walk: Walk, dst: &mut [u32]) {
    let (w, h) = (src.width() as i64, src.height() as i64);
    if w == 0 || h == 0 {
        dst.fill(0);
        return;
    }
    let pixels = src.pixels();
    let at = |x: i64, y: i64| {
        let x = x.clamp(0, w - 1);
        let y = y.clamp(0, h - 1);
        pixels.get((y * w + x) as usize).copied().unwrap_or(0)
    };
    // The texel centers are at .5: weights from the position half a texel back.
    let (mut u, mut v) = (walk.u - 0x8000, walk.v - 0x8000);
    for d in dst {
        let (x, y) = (u >> 16, v >> 16);
        let fx = ((u >> 8) & 0xff) as u32;
        let fy = ((v >> 8) & 0xff) as u32;
        let top = lerp(at(x, y), at(x + 1, y), fx);
        let bottom = lerp(at(x, y + 1), at(x + 1, y + 1), fx);
        *d = lerp(top, bottom, fy);
        u += walk.du;
        v += walk.dv;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same operations, one channel at a time, in floating point.
    fn reference_over(s: u32, d: u32, alpha: u32) -> u32 {
        let channel = |p: u32, i: u32| f64::from((p >> (8 * i)) & 0xff);
        let a = f64::from(alpha) / 255.0;
        let sa = (channel(s, 3) * a).round() / 255.0;
        let mut out = 0;
        for i in 0..4 {
            let sc = (channel(s, i) * a).round();
            let v = (sc + (channel(d, i) * (1.0 - sa)).round()).min(255.0);
            out |= (v as u32) << (8 * i);
        }
        out
    }

    fn pixels(seed: u32, n: usize) -> Vec<u32> {
        // Premultiplied pixels from a small LCG, with the edge cases mixed in.
        let mut x = seed;
        (0..n)
            .map(|i| match i % 7 {
                0 => 0,
                1 => 0xff00_0000 | (x & 0xff_ffff),
                _ => {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let a = x >> 24;
                    let c = |shift: u32| ((x >> shift) & 0xff) * a / 255;
                    (a << 24) | (c(16) << 16) | (c(8) << 8) | c(0)
                }
            })
            .collect()
    }

    #[test]
    fn over_matches_the_reference() {
        let src = pixels(1, 1000);
        let dst = pixels(2, 1000);
        for alpha in [0, 1, 77, 128, 254, 255] {
            for kernels in [Kernels::new(), Kernels::fallback()] {
                let mut out = dst.clone();
                kernels.over(&mut out, &src, alpha);
                for ((o, s), d) in out.iter().zip(&src).zip(&dst) {
                    let expected = reference_over(*s, *d, alpha);
                    for i in 0..4 {
                        let (a, b) = ((o >> (8 * i)) & 0xff, (expected >> (8 * i)) & 0xff);
                        assert!(
                            a.abs_diff(b) <= 1,
                            "{s:08x} over {d:08x} × {alpha}: {o:08x}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn vector_and_scalar_kernels_agree_bit_for_bit() {
        // Odd lengths, so the vector loops have tails.
        for n in [0, 1, 3, 7, 8, 9, 31, 33, 1283] {
            let src = pixels(3, n);
            let dst = pixels(4, n);
            for alpha in [0, 100, 255] {
                let mut a = dst.clone();
                let mut b = dst.clone();
                Kernels::new().over(&mut a, &src, alpha);
                Kernels::fallback().over(&mut b, &src, alpha);
                assert_eq!(a, b);
                Kernels::new().copy(&mut a, &src, alpha);
                Kernels::fallback().copy(&mut b, &src, alpha);
                assert_eq!(a, b);
                Kernels::new().fill_over(&mut a, 0x8040_2010);
                Kernels::fallback().fill_over(&mut b, 0x8040_2010);
                assert_eq!(a, b);
            }
        }
    }

    #[test]
    fn invalid_premultiplied_pixels_saturate() {
        // Color above alpha, as a buggy client sends: no carry into the next channel.
        let mut dst = [0xffff_ffff];
        Kernels::new().over(&mut dst, &[0x80ff_ffff], 255);
        assert_eq!(dst, [0xffff_ffff]);
    }

    #[test]
    fn opaque_over_replaces_and_transparent_keeps() {
        let mut dst = [0x1234_5678, 0x1234_5678];
        Kernels::new().over(&mut dst, &[0xff00_ff00, 0], 255);
        assert_eq!(dst, [0xff00_ff00, 0x1234_5678]);
    }

    #[test]
    fn sampling_never_reads_outside() {
        let mut image = Image::new(2, 2);
        image.pixels_mut().copy_from_slice(&[1, 2, 3, 4]);
        let mut out = [0u32; 6];
        let walk = Walk {
            u: -5 << 16,
            v: 9 << 16,
            du: 3 << 16,
            dv: -(4 << 16),
        };
        sample_nearest(&image, walk, &mut out);
        assert_eq!(out, [3, 3, 4, 2, 2, 2]);
        sample_bilinear(&image, walk, &mut out);
        sample_bilinear(&Image::new(0, 0), walk, &mut out);
        assert_eq!(out, [0; 6]);
    }

    #[test]
    fn bilinear_along_a_row_matches_texel_by_texel() {
        let mut image = Image::new(7, 3);
        for (i, p) in image.pixels_mut().iter_mut().enumerate() {
            let c = (i as u32 * 37) & 0xff;
            *p = 0xff00_0000 | c << 16 | (255 - c) << 8 | (c * 3) & 0xff;
        }
        let walks = [
            (0x8000, 0x1_4000, 0x1_0000),
            (-0x3_2000, 0x2_c000, 0x5800),
            (0x2_7000, 0x9000, 0x2_8000),
            (0x6_0000, 0x1_0000, -0x1_3000),
            (0x40_0000, -0x8_0000, 0x1_0000),
            (-0x40_0000, 0x8_0000, 0x1_0000),
        ];
        for (u, v, du) in walks {
            let walk = Walk { u, v, du, dv: 0 };
            let (mut expected, mut out) = ([0u32; 11], [0u32; 11]);
            sample_bilinear(&image, walk, &mut expected);
            sample_bilinear_across(&image, walk, &mut out, &mut Vec::new());
            for (e, o) in expected.iter().zip(&out) {
                for shift in [0, 8, 16, 24] {
                    let (e, o) = ((e >> shift) & 0xff, (o >> shift) & 0xff);
                    assert!(e.abs_diff(o) <= 1, "{walk:?}: {e} against {o}");
                }
            }
        }
    }

    #[test]
    fn bilinear_at_texel_centers_is_exact() {
        let mut image = Image::new(3, 1);
        image
            .pixels_mut()
            .copy_from_slice(&[0xff00_0000, 0xffff_ffff, 0x8040_2010]);
        let mut out = [0u32; 3];
        let walk = Walk {
            u: 0x8000,
            v: 0x8000,
            du: 1 << 16,
            dv: 0,
        };
        sample_bilinear(&image, walk, &mut out);
        assert_eq!(out, [0xff00_0000, 0xffff_ffff, 0x8040_2010]);
        // Halfway between black and white: mid grey.
        let walk = Walk {
            u: 0x1_0000,
            ..walk
        };
        sample_bilinear(&image, walk, &mut out[..1]);
        assert_eq!(out[0], 0xff80_8080);
    }
}
