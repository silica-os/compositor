//! The effects on the CPU renderer: the GLSL programs of `shaders/` ported to Rust. They read
//! the same uniforms as the GLES programs, so that both renderers draw the same pixels.
//!
//! Where an effect's pixels do not change from frame to frame (title bars, shadows, the glass
//! material) the CPU draws them once into a texture and composites it: the GLSL runs once per
//! change, not once per frame.

pub(crate) mod blur;
pub(crate) mod material;
pub(crate) mod pixel;
pub(crate) mod shade;

use compositor_raster::Image;
use smithay::backend::renderer::gles::{Uniform, UniformValue};

/// The uniforms of a program, read by name; a missing or mistyped one reads as zeros.
pub(crate) struct Uniforms<'a>(pub &'a [Uniform<'static>]);

impl Uniforms<'_> {
    fn value(&self, name: &str) -> Option<&UniformValue> {
        self.0.iter().find(|u| u.name == name).map(|u| &u.value)
    }

    pub fn f1(&self, name: &str) -> f32 {
        match self.value(name) {
            Some(UniformValue::_1f(x)) => *x,
            _ => 0.0,
        }
    }

    pub fn f2(&self, name: &str) -> [f32; 2] {
        match self.value(name) {
            Some(UniformValue::_2f(x, y)) => [*x, *y],
            _ => [0.0; 2],
        }
    }

    pub fn f3(&self, name: &str) -> [f32; 3] {
        match self.value(name) {
            Some(UniformValue::_3f(x, y, z)) => [*x, *y, *z],
            _ => [0.0; 3],
        }
    }

    pub fn f4(&self, name: &str) -> [f32; 4] {
        match self.value(name) {
            Some(UniformValue::_4f(x, y, z, w)) => [*x, *y, *z, *w],
            _ => [0.0; 4],
        }
    }

    /// Matrix `i` of a `mat4` array, column-major as GLSL reads it.
    pub fn mat4(&self, name: &str, i: usize) -> [f32; 16] {
        match self.value(name) {
            Some(UniformValue::Matrix4x4 { matrices, .. }) => {
                matrices.get(i).copied().unwrap_or([0.0; 16])
            }
            _ => [0.0; 16],
        }
    }
}

/// A premultiplied pixel as RGBA in 0–1.
#[inline]
pub(crate) fn unpack(p: u32) -> [f32; 4] {
    // A product, not a division: four divisions per texel were most of a material's cost.
    let c = |shift: u32| ((p >> shift) & 0xff) as f32 * (1.0 / 255.0);
    [c(16), c(8), c(0), c(24)]
}

/// RGBA in 0–1 (premultiplied) as a pixel, rounded as a unorm8 target stores it.
#[inline]
pub(crate) fn pack([r, g, b, a]: [f32; 4]) -> u32 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as i32 as u32;
    (c(a) << 24) | (c(r) << 16) | (c(g) << 8) | c(b)
}

/// GLSL's `mix`.
#[inline]
pub(crate) fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub(crate) fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

#[inline]
pub(crate) fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

#[inline]
pub(crate) fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp01((x - e0) / (e1 - e0));
    t * t * (3.0 - 2.0 * t)
}

#[inline]
pub(crate) fn length(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

/// GLSL `texture2D` with `LINEAR` filtering at `(u, v)` texel coordinates (texel centres at
/// .5), clamped to the edge: premultiplied RGBA in 0–1.
pub(crate) fn bilinear(image: &Image, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return [0.0; 4];
    }
    let (x, y) = (u - 0.5, v - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    // `as usize` takes what lies before the first texel (and NaN) to it.
    let (x1, y1) = (
        ((x0 + 1.0) as usize).min(w - 1),
        ((y0 + 1.0) as usize).min(h - 1),
    );
    let (x0, y0) = ((x0 as usize).min(w - 1), (y0 as usize).min(h - 1));
    let pixels = image.pixels();
    let at = |x: usize, y: usize| unpack(pixels.get(y * w + x).copied().unwrap_or(0));
    // On a texel centre: that texel, as the weights below would give it.
    if fx == 0.0 && fy == 0.0 {
        return at(x0, y0);
    }
    let (a, b) = (at(x0, y0), at(x1, y0));
    let (c, d) = (at(x0, y1), at(x1, y1));
    let top = zip_with(a, b, |a, b| mix(a, b, fx));
    let bottom = zip_with(c, d, |c, d| mix(c, d, fx));
    zip_with(top, bottom, |t, b| mix(t, b, fy))
}

/// `f` over the matching channels of `a` and `b`.
#[inline]
pub(crate) fn zip_with<const N: usize>(
    a: [f32; N],
    b: [f32; N],
    f: impl Fn(f32, f32) -> f32,
) -> [f32; N] {
    let mut out = a;
    for (o, b) in out.iter_mut().zip(b) {
        *o = f(*o, b);
    }
    out
}

/// Signed distance (positive outside) from a rectangle `[x, y, w, h]` with superellipse
/// corners of `radius` and `exponent` (the shadow, border and title bar programs).
#[inline]
pub(crate) fn superellipse_distance(
    p: [f32; 2],
    rect: [f32; 4],
    radius: f32,
    exponent: f32,
) -> f32 {
    let (hw, hh) = (rect[2] * 0.5, rect[3] * 0.5);
    let dx = (p[0] - (rect[0] + hw)).abs() - (hw - radius);
    let dy = (p[1] - (rect[1] + hh)).abs() - (hh - radius);
    if dx > 0.0 && dy > 0.0 && radius > 0.0 {
        let (qx, qy) = (dx / radius, dy / radius);
        let f = (qx.powf(exponent) + qy.powf(exponent)).powf(1.0 / exponent);
        return (f - 1.0) * radius;
    }
    dx.max(dy) - radius
}
