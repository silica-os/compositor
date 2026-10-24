//! The glass material on the CPU (`material.frag`), in two stages: the geometry of every pixel
//! (distances, lens, rims, coverage), which depends only on the shapes, and its color over a
//! blurred backdrop, redone when the backdrop changes.

use compositor_raster::fearless_simd::prelude::*;
use compositor_raster::fearless_simd::{Simd, dispatch, f32x16, i32x16, u32x16};
use compositor_raster::{CpuRenderer, Image};
use smithay::backend::renderer::gles::Uniform;

use super::{Uniforms, clamp01, length, mix, unpack, zip_with};

/// How many regions a group holds (`MAX_SHAPES`).
const SHAPES: usize = 8;

type Vec2 = [f32; 2];
type Vec4 = [f32; 4];

fn add(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] + b[0], a[1] + b[1]]
}

fn sub(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] - b[0], a[1] - b[1]]
}

fn mul(a: Vec2, k: f32) -> Vec2 {
    [a[0] * k, a[1] * k]
}

fn dot(a: Vec2, b: Vec2) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

fn len(a: Vec2) -> f32 {
    length(a[0], a[1])
}

fn normalize(a: Vec2) -> Vec2 {
    let l = len(a);
    if l > 0.0 { mul(a, 1.0 / l) } else { a }
}

/// A screen vector in the frame of a region rotated by `cs`: turned back by the rotation.
fn to_frame(v: Vec2, cs: Vec2) -> Vec2 {
    [cs[0] * v[0] + cs[1] * v[1], -cs[1] * v[0] + cs[0] * v[1]]
}

/// A point in the frame of the region `rect` rotated by `cs` about its centre.
fn to_region(p: Vec2, rect: Vec4, cs: Vec2) -> Vec2 {
    let c = [rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5];
    add(c, to_frame(sub(p, c), cs))
}

fn segment_distance(q: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = sub(b, a);
    let h = clamp01(dot(sub(q, a), ab) / dot(ab, ab).max(1e-6));
    len(sub(sub(q, a), mul(ab, h)))
}

/// Distance from the arc of `c`, radius `r`, between the directions `a0` and `a1`; far away
/// outside that sector.
fn arc_distance(q: Vec2, c: Vec2, r: f32, a0: Vec2, a1: Vec2) -> f32 {
    let v = sub(q, c);
    let turn = a0[0] * a1[1] - a0[1] * a1[0];
    let inside =
        (a0[0] * v[1] - a0[1] * v[0]) * turn >= 0.0 && (v[0] * a1[1] - v[1] * a1[0]) * turn >= 0.0;
    if inside { (len(v) - r).abs() } else { 1.0e6 }
}

/// Smooth (polynomial) minimum: the bridge between two nearby shapes.
fn smooth_min(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

/// A lens curve: (t, s) points in pixels, ended by a huge t.
fn lens_source(curve: &[f32; 16], t: f32) -> f32 {
    let mut previous = [-1.0, 0.0];
    for q in curve.as_chunks::<2>().0 {
        if q[0] > 1.0e5 {
            break;
        }
        if t <= q[0] {
            if previous[0] < 0.0 {
                return q[1];
            }
            return mix(
                previous[1],
                q[1],
                (t - previous[0]) / (q[0] - previous[0]).max(1e-3),
            );
        }
        previous = *q;
    }
    t
}

/// A measured edge profile at position `s` (points from the edge).
fn profile_at(m: &[f32; 16], s: f32) -> f32 {
    let s = s.max(0.0);
    let i0 = s.floor();
    let at = |i: f32| {
        if (0.0..12.0).contains(&i) {
            m.get(i as usize).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    };
    mix(at(i0), at(i0 + 1.0), s - i0)
}

/// The dark outline just outside: alpha at position `u` (points from the edge).
fn outline_at(m: &[f32; 16], u: f32) -> f32 {
    let u = u.max(0.0);
    if u < 1.0 {
        mix(m[12], m[13], u)
    } else {
        mix(m[13], 0.0, clamp01(u - 1.0))
    }
}

/// What the three orientations share where none of them owns the curve.
fn shared_value(a: f32, b: f32, c: f32) -> f32 {
    if a >= 0.0 && b >= 0.0 && c >= 0.0 {
        a.min(b.min(c))
    } else if a <= 0.0 && b <= 0.0 && c <= 0.0 {
        a.max(b.max(c))
    } else {
        0.0
    }
}

/// One region of the group.
#[derive(Debug, Clone, Copy, Default)]
struct Region {
    rect: Vec4,
    /// The corner radius; negative: a tip (−1 down, −2 up, −3 left, −4 right).
    radius: f32,
    rotation: Vec2,
    /// The tip's outline, or the smoothing of a rounded region in `x`.
    tip: Vec4,
    lens_width: f32,
}

/// The material's uniforms, read once.
#[derive(Debug, Clone)]
pub(crate) struct Material {
    regions: Vec<Region>,
    merge: f32,
    exponent: f32,
    rows: [[f32; 3]; 3],
    offset: [f32; 3],
    curve2: [f32; 3],
    curve3: [f32; 3],
    scale_px: f32,
    lens: [[f32; 16]; 3],
    lens_reach: f32,
    edges: [[f32; 16]; 3],
    fill_gray: f32,
    edge_power: f32,
    refraction: f32,
    chromatic: f32,
    brightness: f32,
    rim: f32,
    line: f32,
    saturation: f32,
    tint: Vec4,
    shadow: [f32; 3],
    edge_on: bool,
    inside_rim: Vec4,
    inside_rim_width: f32,
    rim_light: Vec4,
    rim_light_shape: Vec2,
    fallback: Vec4,
}

/// What a pixel takes from the shapes (`material.frag` up to the backdrop sample, and its rims).
#[derive(Debug, Clone, Copy, Default)]
struct Texel {
    /// Where the backdrop is sampled (texture pixels), and the red/blue split from there.
    sample: Vec2,
    split: Vec2,
    look: Look,
}

/// What the shapes do to a pixel's color: the inputs of `shade` besides the backdrop.
#[derive(Debug, Clone, Copy, Default)]
struct Look {
    coverage: f32,
    /// The black outline's alpha outside the edge.
    outline: f32,
    /// The edge offset (rim or line already applied).
    offset_in: f32,
    /// The share of the inside rim and of the rim light.
    in_band: f32,
    lit: f32,
    /// The shadow's alpha.
    shadow: f32,
}

impl Material {
    pub fn new(uniforms: &[Uniform<'static>]) -> Self {
        let u = Uniforms(uniforms);
        let shapes = [u.mat4("shapes", 0), u.mat4("shapes", 1)];
        let radii = u.mat4("radii", 0);
        let rotations = u.mat4("rotations", 0);
        let tips = [u.mat4("tips", 0), u.mat4("tips", 1)];
        let widths = u.mat4("lens_widths", 0);
        let count = (u.f1("shape_count").max(0.0) as usize).min(SHAPES);
        // One column per region across the matrices, and one (cos, sin) pair.
        let columns = |m: &[[f32; 16]; 2]| -> Vec<Vec4> {
            m.iter()
                .flat_map(|m| m.as_chunks::<4>().0.iter().copied())
                .collect()
        };
        let (rects, tips) = (columns(&shapes), columns(&tips));
        let regions = (0..count)
            .map(|i| Region {
                rect: rects.get(i).copied().unwrap_or_default(),
                radius: radii.get(i).copied().unwrap_or(0.0),
                rotation: rotations
                    .as_chunks::<2>()
                    .0
                    .get(i)
                    .copied()
                    .unwrap_or([1.0, 0.0]),
                tip: tips.get(i).copied().unwrap_or_default(),
                lens_width: widths.get(i).copied().unwrap_or(1.0),
            })
            .collect();
        let shadow = u.f3("shadow");
        Self {
            regions,
            merge: u.f1("merge"),
            exponent: u.f1("exponent"),
            rows: [u.f3("row_r"), u.f3("row_g"), u.f3("row_b")],
            offset: u.f3("offset"),
            curve2: u.f3("curve2"),
            curve3: u.f3("curve3"),
            scale_px: u.f1("scale_px"),
            lens: [u.mat4("lens", 0), u.mat4("lens", 1), u.mat4("lens", 2)],
            lens_reach: u.f1("lens_reach"),
            edges: [u.mat4("edges", 0), u.mat4("edges", 1), u.mat4("edges", 2)],
            fill_gray: u.f1("fill_gray"),
            edge_power: u.f1("edge_power"),
            refraction: u.f1("refraction"),
            chromatic: u.f1("chromatic"),
            brightness: u.f1("brightness"),
            rim: u.f1("rim"),
            line: u.f1("line"),
            saturation: u.f1("saturation"),
            tint: u.f4("tint_color"),
            shadow,
            edge_on: u.f1("edge_on") > 0.5,
            inside_rim: u.f4("inside_rim"),
            inside_rim_width: u.f1("inside_rim_width"),
            rim_light: u.f4("rim_light"),
            rim_light_shape: u.f2("rim_light_shape"),
            fallback: u.f4("fallback"),
        }
    }

    fn rounded_distance(&self, p: Vec2, rect: Vec4, r: f32) -> f32 {
        let half = [rect[2] * 0.5, rect[3] * 0.5];
        let r = r.min(half[0].min(half[1]));
        let d = [
            (p[0] - (rect[0] + half[0])).abs() - (half[0] - r),
            (p[1] - (rect[1] + half[1])).abs() - (half[1] - r),
        ];
        if r > 0.0 && d[0] > 0.0 && d[1] > 0.0 {
            let e = self.exponent;
            let q = [d[0] / r, d[1] / r];
            let f = (q[0].powf(e) + q[1].powf(e)).powf(1.0 / e);
            return (f - 1.0) * r;
        }
        d[0].max(d[1]) - r.max(0.0)
    }

    fn continuous_distance(p: Vec2, rect: Vec4, r: f32, s: f32) -> f32 {
        let half = [rect[2] * 0.5, rect[3] * 0.5];
        let a = (r * (1.0 + s)).min(half[0].min(half[1]));
        let d = [
            (p[0] - (rect[0] + half[0])).abs() - (half[0] - a),
            (p[1] - (rect[1] + half[1])).abs() - (half[1] - a),
        ];
        if a > 0.0 && d[0] > 0.0 && d[1] > 0.0 {
            let n = 2.0 + 2.55 * s;
            let q = [d[0] / a, d[1] / a];
            let f = (q[0].powf(n) + q[1].powf(n)).powf(1.0 / n);
            let grad =
                (q[0].powf(2.0 * n - 2.0) + q[1].powf(2.0 * n - 2.0)).sqrt() / f.powf(n - 1.0);
            return (f - 1.0) * a / grad.max(1e-3);
        }
        d[0].max(d[1]) - a
    }

    fn region_distance(&self, p: Vec2, rect: Vec4, r: f32, s: f32) -> f32 {
        if s > 0.0 {
            Self::continuous_distance(p, rect, r, s)
        } else {
            self.rounded_distance(p, rect, r)
        }
    }

    /// The tip of TIP-01 in its own frame (`tip_distance` of the shader).
    fn tip_distance(&self, p: Vec2, size: Vec2, k: Vec4) -> f32 {
        let [hb, ta, ra, rf] = k;
        let ca = 1.0 / (1.0 + ta * ta).sqrt();
        let sa = ta * ca;
        let th = sa / (1.0 + ca);
        let q = [(p[0] - 0.5 * size[0]).abs(), p[1]];
        let u = [-ca, sa];
        let b = [hb, 0.0];
        let w = hb + rf * th;
        let t2 = add(b, mul(u, rf * th));
        let apex = [0.0, hb * ta];
        let t3 = sub(apex, mul(u, ra * ta));
        let cf = [w, rf];
        let cr = [0.0, apex[1] - ra / ca];
        let mut d = if q[0] >= w {
            q[1].abs()
        } else {
            len(sub(q, [w, 0.0]))
        };
        d = d.min(arc_distance(
            q,
            cf,
            rf,
            [0.0, -1.0],
            normalize(add(sub(t2, cf), [1e-6, 0.0])),
        ));
        d = d.min(segment_distance(q, t2, t3));
        d = d.min(arc_distance(
            q,
            cr,
            ra,
            normalize(add(sub(t3, cr), [1e-6, 0.0])),
            [0.0, 1.0],
        ));
        let edge = if q[0] >= w {
            0.0
        } else if q[0] >= t2[0] {
            rf - (rf * rf - (q[0] - w) * (q[0] - w)).max(0.0).sqrt()
        } else if q[0] >= t3[0] {
            (hb - q[0]) * ta
        } else {
            cr[1] + (ra * ra - q[0] * q[0]).max(0.0).sqrt()
        };
        let sd = if q[1] < edge { -d } else { d };
        let overlap = 2.0 * self.scale_px;
        sd.max((-p[1] - overlap).max(q[0] - w))
    }

    /// The lens band's width of the region nearest to `p`, and that region's rotation.
    fn lens_width_at(&self, p: Vec2) -> (f32, Vec2) {
        let mut best = 1.0e6_f32;
        let mut width = 1.0;
        let mut frame = [1.0, 0.0];
        for region in &self.regions {
            let d = self.region_distance(
                to_region(p, region.rect, region.rotation),
                region.rect,
                region.radius.max(0.0),
                region.tip[0],
            );
            if d < best {
                best = d;
                width = region.lens_width;
                frame = region.rotation;
            }
        }
        (width.max(0.05), frame)
    }

    /// Signed distance from the union of the regions, corners at least `min_radius`, each
    /// region moved by `shift` in its own frame.
    fn union_distance(&self, screen: Vec2, min_radius: f32, shift: Vec2) -> f32 {
        let mut d = 1.0e6_f32;
        for region in &self.regions {
            let rect = region.rect;
            let r = region.radius;
            let p = sub(to_region(screen, rect, region.rotation), shift);
            let k = region.tip;
            let near = sub(p, [rect[0], rect[1]]);
            let far = sub([rect[0] + rect[2], rect[1] + rect[3]], p);
            let size = [rect[2], rect[3]];
            let flipped = [rect[3], rect[2]];
            let shape = if r > -0.5 {
                self.region_distance(p, rect, r.max(min_radius), k[0])
            } else if r > -1.5 {
                self.tip_distance(near, size, k)
            } else if r > -2.5 {
                self.tip_distance([near[0], far[1]], size, k)
            } else if r > -3.5 {
                self.tip_distance([near[1], far[0]], flipped, k)
            } else {
                self.tip_distance([near[1], near[0]], flipped, k)
            };
            d = if r < 0.0 {
                d.min(shape)
            } else {
                smooth_min(d, shape, self.merge)
            };
        }
        d
    }

    /// The gradient of `union_distance` by central differences, normalized (or `fallback`).
    fn normal(&self, p: Vec2, min_radius: f32) -> Option<Vec2> {
        let none = [0.0; 2];
        let n = [
            self.union_distance(add(p, [0.5, 0.0]), min_radius, none)
                - self.union_distance(sub(p, [0.5, 0.0]), min_radius, none),
            self.union_distance(add(p, [0.0, 0.5]), min_radius, none)
                - self.union_distance(sub(p, [0.0, 0.5]), min_radius, none),
        ];
        (len(n) > 0.0).then(|| normalize(n))
    }

    /// The geometry of a row of pixels at `py`, from `x0` on (texture pixels), compiled for
    /// the SIMD level of `S`.
    #[inline(always)]
    fn texel_row<S: Simd>(&self, _: S, x0: f32, py: f32, row: &mut [Texel]) {
        for (x, texel) in row.iter_mut().enumerate() {
            *texel = self.texel([x0 + x as f32 + 0.5, py]);
        }
    }

    /// The geometry of the pixel at `p` (texture pixels).
    #[inline(always)]
    fn texel(&self, p: Vec2) -> Texel {
        let none = [0.0; 2];
        let sd = self.union_distance(p, 0.0, none);
        let t = -sd;
        let coverage = clamp01(0.5 - sd);
        let shadow = if self.shadow[0] > 0.0 {
            let ds = self.union_distance(p, 0.0, [0.0, self.shadow[2]]);
            let a = self.shadow[0] / (1.0 + (1.702 * ds / self.shadow[1].max(0.5)).exp());
            a * clamp01(sd + 0.5)
        } else {
            0.0
        };
        let mut texel = Texel {
            sample: p,
            look: Look {
                coverage,
                shadow,
                ..Look::default()
            },
            ..Texel::default()
        };
        // Far outside the shape and its outline: only the shadow (which reads nothing).
        if coverage <= 0.0 && (!self.edge_on || sd / self.scale_px.max(1e-3) - 0.5 >= 2.0) {
            return texel;
        }
        let (band, frame) = self.lens_width_at(p);
        if self.refraction > 0.0 {
            let n = self.normal(p, self.lens_reach).unwrap_or([0.0, -1.0]);
            let nf = to_frame(n, frame);
            let vertical = nf[1] * nf[1];
            let tb = t / band;
            let side = if nf[1] < 0.0 {
                &self.lens[1]
            } else {
                &self.lens[2]
            };
            let measured = mix(
                lens_source(&self.lens[0], tb),
                lens_source(side, tb),
                vertical,
            );
            let source = t + self.refraction * band * (measured - tb);
            texel.sample = add(p, mul(n, t - source));
            let rim_ness = clamp01(1.0 - tb / self.lens_reach.max(1.0));
            texel.split = mul(n, self.chromatic * band * self.lens_reach * rim_ness);
        }
        if self.edge_on {
            let m = self
                .normal(p, 0.0)
                .map_or([0.0, -1.0], |m| to_frame(m, frame));
            let power = self.edge_power;
            let w_top = (-m[1]).max(0.0).powf(power);
            let w_bottom = m[1].max(0.0).powf(power);
            let w_side = m[0].abs().powf(power);
            let w_shared = (1.0 - w_top - w_bottom - w_side).max(0.0);
            let s_in = t / self.scale_px - 0.5;
            let [top, side, bottom] = self.edges.each_ref().map(|m| profile_at(m, s_in));
            let offset_in = w_top * top
                + w_side * side
                + w_bottom * bottom
                + w_shared * shared_value(top, side, bottom);
            texel.look.offset_in = offset_in * if offset_in > 0.0 { self.rim } else { self.line };
            let u_out = sd / self.scale_px - 0.5;
            let [top, side, bottom] = self.edges.each_ref().map(|m| outline_at(m, u_out));
            let outline = w_top * top
                + w_side * side
                + w_bottom * bottom
                + w_shared * top.min(side.min(bottom));
            texel.look.outline = clamp01(outline * self.line);
        }
        texel.look.in_band = clamp01(self.inside_rim_width + 0.5 - t);
        if self.rim_light[3] > 0.0 {
            let lit_distance = self.union_distance(p, 0.0, [0.0, self.rim_light_shape[0]]);
            texel.look.lit =
                1.0 / (1.0 + (-1.702 * lit_distance / self.rim_light_shape[1].max(0.25)).exp());
        }
        texel
    }

    /// Whether the red and blue channels sample apart from green.
    fn splits(&self) -> bool {
        self.chromatic > 0.0 && self.refraction > 0.0
    }

    /// The opaque fallback, which replaces the backdrop.
    fn fallback(&self) -> Option<[f32; 3]> {
        (self.fallback[3] > 0.5).then_some([self.fallback[0], self.fallback[1], self.fallback[2]])
    }

    /// Whether a pixel of this look reads the backdrop: inside the shape, without the
    /// fallback.
    fn reads(&self, look: &Look) -> bool {
        look.coverage > 0.0 && self.fallback().is_none()
    }

    /// Whether a pixel of this look takes its color from the backdrop alone: inside the shape
    /// and past its rims, where `shade::<true>` gives the same color. A rim light this faint
    /// moves no color by half a unit in the last place.
    fn plain(&self, look: &Look) -> bool {
        look.coverage >= 1.0
            && look.offset_in == 0.0
            && self.inside_rim[3] * look.in_band == 0.0
            && self.rim_light[3] * look.lit <= f32::EPSILON / 4.0
    }

    /// Row of `Plan::base` for the pixels `texels`: those that read nothing, final, and 0
    /// for the others.
    #[inline(always)]
    fn base_row<S: Simd>(&self, simd: S, texels: &[Texel], row: &mut [u32]) {
        let fallback = self.fallback();
        let mut run = Run::default();
        for (out, texels) in row.chunks_mut(RUN).zip(texels.chunks(RUN)) {
            for (i, texel) in texels.iter().enumerate() {
                let color = match fallback {
                    Some(color) if texel.look.coverage > 0.0 => color,
                    _ => [0.0; 3],
                };
                run.color(i, color);
                run.look(i..i + 1, &texel.look);
            }
            self.shade::<S, false>(simd, run.background(simd), &run, out);
            for (out, texel) in out.iter_mut().zip(texels) {
                if self.reads(&texel.look) {
                    *out = 0;
                }
            }
        }
    }

    /// The pixels of `run` over the backdrop `[r, g, b]` (`material.frag` from the backdrop
    /// sample on), the first `out.len()` of them, sixteen at once. Outside the shape the
    /// coverage is 0 and the backdrop black, which leaves the outline and the shadow, black:
    /// one formula for every pixel. `PLAIN`: every pixel plain, which leaves out the rims and
    /// the lanes of `run`.
    #[inline(always)]
    fn shade<S: Simd, const PLAIN: bool>(
        &self,
        simd: S,
        background: [f32x16<S>; 3],
        run: &Run,
        out: &mut [u32],
    ) {
        let [r, g, b] = background;
        let (zero, one) = (splat(simd, 0.0), splat(simd, 1.0));
        let opaque = self.fallback[3] > 0.5;
        let saturation = splat(simd, if opaque { 1.0 } else { self.saturation });
        let mut color = [zero; 3];
        for ((c, offset), row) in color.iter_mut().zip(self.offset).zip(self.rows) {
            *c = splat(simd, offset) + dot3(simd, row, background);
        }
        let luminance = (r + g + b) / splat(simd, 3.0);
        for ((c, q2), q3) in color.iter_mut().zip(self.curve2).zip(self.curve3) {
            *c += (splat(simd, q2) + splat(simd, q3) * luminance) * luminance * luminance;
        }
        let rec709 = dot3(simd, [0.2126, 0.7152, 0.0722], color);
        for c in &mut color {
            *c = mix16(rec709, *c, saturation);
        }
        if !opaque {
            let brightness = splat(simd, self.brightness);
            for c in &mut color {
                *c = clamp16(*c * brightness);
            }
        }
        let tint = splat(simd, self.tint[3]);
        for (c, t) in color.iter_mut().zip(self.tint) {
            *c = mix16(*c, splat(simd, t), tint);
        }
        let mut alpha = one;
        if !PLAIN {
            if self.edge_on {
                let offset_in = f32x16::load_array(simd, run.offset_in);
                let room = splat(simd, (1.0 - self.fill_gray).max(0.05));
                let scale = one + offset_in / splat(simd, self.fill_gray.max(0.05));
                let lighter = offset_in.simd_gt(zero);
                for c in &mut color {
                    let lit = *c + offset_in * ((one - *c) / room).min(one);
                    *c = lighter.select(lit, *c * scale);
                }
            }
            let rim = splat(simd, self.inside_rim[3]) * f32x16::load_array(simd, run.in_band);
            for (c, v) in color.iter_mut().zip(self.inside_rim) {
                *c = mix16(*c, splat(simd, v), rim);
            }
            if self.rim_light[3] > 0.0 {
                let light = splat(simd, self.rim_light[3]) * f32x16::load_array(simd, run.lit);
                for (c, v) in color.iter_mut().zip(self.rim_light) {
                    *c = mix16(*c, splat(simd, v), light);
                }
            }
            let coverage = f32x16::load_array(simd, run.coverage);
            for c in &mut color {
                *c = clamp16(*c) * coverage;
            }
            let a = coverage + f32x16::load_array(simd, run.outline) * (one - coverage);
            alpha = a + f32x16::load_array(simd, run.shadow) * (one - a);
        }
        let [r, g, b] = color;
        let packed = (unorm16(alpha) << 24) | (unorm16(r) << 16) | (unorm16(g) << 8) | unorm16(b);
        let packed: [i32; RUN] = packed.into();
        for (o, p) in out.iter_mut().zip(packed) {
            *o = p as u32;
        }
    }
}

/// Pixels shaded at once: long enough for the vector units, short enough to stay in
/// registers and L1.
const RUN: usize = 16;

// Lane-wise helpers of `shade`. Named functions, not closures: only these are compiled
// for the caller's SIMD level.

#[inline(always)]
fn splat<S: Simd>(simd: S, x: f32) -> f32x16<S> {
    f32x16::splat(simd, x)
}

#[inline(always)]
fn clamp16<S: Simd>(x: f32x16<S>) -> f32x16<S> {
    x.max(0.0).min(1.0)
}

#[inline(always)]
fn mix16<S: Simd>(a: f32x16<S>, b: f32x16<S>, t: f32x16<S>) -> f32x16<S> {
    a + (b - a) * t
}

#[inline(always)]
fn dot3<S: Simd>(simd: S, a: [f32; 3], [x, y, z]: [f32x16<S>; 3]) -> f32x16<S> {
    splat(simd, a[0]) * x + splat(simd, a[1]) * y + splat(simd, a[2]) * z
}

/// A channel of `pack`.
#[inline(always)]
fn unorm16<S: Simd>(v: f32x16<S>) -> i32x16<S> {
    (clamp16(v) * 255.0 + 0.5).to_int()
}

/// Sixteen pixels of the backdrop as `[r, g, b]` (`unpack`).
#[inline(always)]
fn unpack16<S: Simd>(simd: S, pixels: [u32; RUN]) -> [f32x16<S>; 3] {
    let p = u32x16::load_array(simd, pixels);
    let unit = splat(simd, 1.0 / 255.0);
    [
        f32x16::float_from((p >> 16) & 0xff) * unit,
        f32x16::float_from((p >> 8) & 0xff) * unit,
        f32x16::float_from(p & 0xff) * unit,
    ]
}

/// A run of pixels: the backdrop sampled, and the geometry of each.
#[derive(Default)]
struct Run {
    r: [f32; RUN],
    g: [f32; RUN],
    b: [f32; RUN],
    coverage: [f32; RUN],
    outline: [f32; RUN],
    offset_in: [f32; RUN],
    in_band: [f32; RUN],
    lit: [f32; RUN],
    shadow: [f32; RUN],
}

impl Run {
    /// The backdrop of lane `i`.
    #[inline(always)]
    fn color(&mut self, i: usize, [r, g, b]: [f32; 3]) {
        for (lane, value) in [(&mut self.r, r), (&mut self.g, g), (&mut self.b, b)] {
            if let Some(lane) = lane.get_mut(i) {
                *lane = value;
            }
        }
    }

    /// The backdrop of the lanes.
    #[inline(always)]
    fn background<S: Simd>(&self, simd: S) -> [f32x16<S>; 3] {
        [
            f32x16::load_array(simd, self.r),
            f32x16::load_array(simd, self.g),
            f32x16::load_array(simd, self.b),
        ]
    }

    /// `look` in the lanes of `lanes`.
    #[inline(always)]
    fn look(&mut self, lanes: std::ops::Range<usize>, look: &Look) {
        for (lane, value) in [
            (&mut self.coverage, look.coverage),
            (&mut self.outline, look.outline),
            (&mut self.offset_in, look.offset_in),
            (&mut self.in_band, look.in_band),
            (&mut self.lit, look.lit),
            (&mut self.shadow, look.shadow),
        ] {
            lane.get_mut(lanes.clone()).unwrap_or_default().fill(value);
        }
    }
}

/// Where a pixel reads the backdrop: `bilinear` at a point, as the texel there, the steps to
/// the next texel across and down (0 at the edge) and the weights of those.
#[derive(Debug, Clone, Copy, Default)]
struct Tap {
    index: u32,
    across: u32,
    down: u32,
    fx: f32,
    fy: f32,
}

impl Tap {
    /// `bilinear` at `(u, v)`, texel coordinates of a `w` × `h` texture.
    fn new([u, v]: Vec2, (w, h): (usize, usize)) -> Self {
        let (Some(right), Some(bottom)) = (w.checked_sub(1), h.checked_sub(1)) else {
            return Self::default();
        };
        let (x, y) = (u - 0.5, v - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        // `as usize` takes what lies before the first texel (and NaN) to it.
        let (x1, y1) = (
            ((x0 + 1.0) as usize).min(right),
            ((y0 + 1.0) as usize).min(bottom),
        );
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = ((x0 as usize).min(right), (y0 as usize).min(bottom));
        let narrow = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        Self {
            index: narrow(y0 * w + x0),
            across: narrow(x1 - x0),
            down: narrow((y1 - y0) * w),
            fx,
            fy,
        }
    }

    /// `reads` grown by the texels this tap reads, in a texture `width` wide.
    fn reach(self, width: usize, reads: Option<[usize; 4]>) -> Option<[usize; 4]> {
        let index = self.index as usize;
        let (Some(x), Some(y)) = (index.checked_rem(width), index.checked_div(width)) else {
            return reads;
        };
        let x1 = x + 1 + usize::from(self.across > 0);
        let y1 = y + 1 + usize::from(self.down > 0);
        Some(match reads {
            Some([a, b, c, d]) => [a.min(x), b.min(y), c.max(x1), d.max(y1)],
            None => [x, y, x1, y1],
        })
    }

    /// The backdrop there, premultiplied RGBA in 0–1: `bilinear` read through the steps.
    #[inline(always)]
    fn read(self, pixels: &[u32]) -> [f32; 4] {
        let at = |i: u32| unpack(pixels.get(i as usize).copied().unwrap_or(0));
        let a = at(self.index);
        if self.fx == 0.0 && self.fy == 0.0 {
            return a;
        }
        let b = at(self.index.saturating_add(self.across));
        let c = at(self.index.saturating_add(self.down));
        let d = at(self
            .index
            .saturating_add(self.down)
            .saturating_add(self.across));
        let top = zip_with(a, b, |a, b| mix(a, b, self.fx));
        let bottom = zip_with(c, d, |c, d| mix(c, d, self.fx));
        zip_with(top, bottom, |t, b| mix(t, b, self.fy))
    }

    /// One channel of `read`, the one `shift` bits up in a pixel.
    #[inline(always)]
    fn channel(self, pixels: &[u32], shift: u32) -> f32 {
        let at = |i: u32| {
            let p = pixels.get(i as usize).copied().unwrap_or(0);
            ((p >> shift) & 0xff) as f32 * (1.0 / 255.0)
        };
        let a = at(self.index);
        if self.fx == 0.0 && self.fy == 0.0 {
            return a;
        }
        let b = at(self.index.saturating_add(self.across));
        let c = at(self.index.saturating_add(self.down));
        let d = at(self
            .index
            .saturating_add(self.down)
            .saturating_add(self.across));
        mix(mix(a, b, self.fx), mix(c, d, self.fx), self.fy)
    }
}

/// A pixel near the edge, which reads the backdrop: what it does to the color, and where it
/// reads.
#[derive(Debug, Clone, Copy, Default)]
struct Edge {
    look: Look,
    tap: Tap,
    /// Where red and blue read apart, in `split_taps`; `u32::MAX` if they do not.
    split: u32,
}

/// Pixels `x0..x1` of a row that read the backdrop.
#[derive(Debug, Clone, Copy)]
enum Span {
    /// Inside the shape and past its rims: one texel each, from `texel` on.
    Plain { x0: u32, x1: u32, texel: u32 },
    /// Near the edge: their geometry in `edges`, from `edge` on.
    Edge { x0: u32, x1: u32, edge: u32 },
}

/// The geometry of a material over a part of its texture.
#[derive(Debug)]
pub(crate) struct Plan {
    material: Material,
    width: usize,
    height: usize,
    /// The size of the texture the taps read.
    tex_size: (usize, usize),
    /// The pixels that read nothing (outside the shape, or with the opaque fallback), final;
    /// 0 under the spans.
    base: Vec<u32>,
    /// The spans of row `y`: `spans[rows[y]..rows[y + 1]]`.
    spans: Vec<Span>,
    rows: Vec<usize>,
    edges: Vec<Edge>,
    split_taps: Vec<[Tap; 2]>,
    /// The texels the paint reads, `[x0, y0, x1, y1)`.
    reads: Option<[usize; 4]>,
}

impl Plan {
    /// The geometry of the `size` pixels at `origin` of the material's texture, which is
    /// `tex_size` large.
    pub fn new(
        renderer: &CpuRenderer,
        material: Material,
        origin: [i32; 2],
        size: (usize, usize),
        tex_size: (usize, usize),
    ) -> Self {
        let (width, height) = size;
        let mut texels = vec![Texel::default(); width.saturating_mul(height)];
        let level = renderer.level();
        renderer.rows_costly(&mut texels, width, |y, row| {
            let py = (origin[1] as f32) + y as f32 + 0.5;
            dispatch!(level, simd => material.texel_row(simd, origin[0] as f32, py, row));
        });
        let mut base = vec![0; texels.len()];
        renderer.rows_costly(&mut base, width, |y, row| {
            let texels = texels.get(y * width..(y + 1) * width).unwrap_or_default();
            dispatch!(level, simd => material.base_row(simd, texels, row));
        });
        let splits = material.splits();
        let narrow = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        let mut spans: Vec<Span> = Vec::new();
        let mut rows = Vec::with_capacity(height + 1);
        let mut edges = Vec::new();
        let mut split_taps = Vec::new();
        let mut reads = None;
        for row in texels.chunks(width.max(1)) {
            let start = spans.len();
            rows.push(start);
            for (x, texel) in (0u32..).zip(row) {
                if !material.reads(&texel.look) {
                    continue;
                }
                let tap = Tap::new(texel.sample, tex_size);
                reads = tap.reach(tex_size.0, reads);
                let split = splits && texel.split != [0.0, 0.0];
                let last = if spans.len() > start {
                    spans.last_mut()
                } else {
                    None
                };
                if !split && material.plain(&texel.look) && tap.fx == 0.0 && tap.fy == 0.0 {
                    if let Some(Span::Plain { x0, x1, texel }) = last
                        && *x1 == x
                        && texel.checked_add(x - *x0) == Some(tap.index)
                    {
                        *x1 += 1;
                    } else {
                        spans.push(Span::Plain {
                            x0: x,
                            x1: x + 1,
                            texel: tap.index,
                        });
                    }
                    continue;
                }
                let mut index = u32::MAX;
                if split {
                    let [x, y] = texel.sample;
                    let [dx, dy] = texel.split;
                    let pair = [
                        Tap::new([x + dx, y + dy], tex_size),
                        Tap::new([x - dx, y - dy], tex_size),
                    ];
                    for tap in pair {
                        reads = tap.reach(tex_size.0, reads);
                    }
                    index = narrow(split_taps.len());
                    split_taps.push(pair);
                }
                edges.push(Edge {
                    look: texel.look,
                    tap,
                    split: index,
                });
                if let Some(Span::Edge { x1, .. }) = last
                    && *x1 == x
                {
                    *x1 += 1;
                } else {
                    spans.push(Span::Edge {
                        x0: x,
                        x1: x + 1,
                        edge: narrow(edges.len() - 1),
                    });
                }
            }
        }
        rows.push(spans.len());
        Self {
            material,
            width,
            height,
            tex_size,
            base,
            spans,
            rows,
            edges,
            split_taps,
            reads,
        }
    }

    /// The texels of the backdrop the paint reads, `[x0, y0, x1, y1)`: the rest does not
    /// matter.
    pub fn reads(&self) -> Option<[usize; 4]> {
        self.reads
    }

    /// The size of the texture the plan reads.
    pub fn tex_size(&self) -> (usize, usize) {
        self.tex_size
    }

    /// The material over `backdrop` (`tex_size` large), at alpha 1.
    pub fn paint(&self, renderer: &CpuRenderer, backdrop: &Image) -> Image {
        let mut out = Image::new(self.width, self.height);
        // A backdrop of another size than the taps were made for reads nothing.
        let pixels = if (backdrop.width(), backdrop.height()) == self.tex_size {
            backdrop.pixels()
        } else {
            &[]
        };
        let level = renderer.level();
        renderer.rows_costly(out.pixels_mut(), self.width, |y, row| {
            dispatch!(level, simd => self.paint_row(simd, pixels, y, row));
        });
        out
    }

    /// Row `y` of `paint`, compiled for the SIMD level of `S`.
    #[inline(always)]
    fn paint_row<S: Simd>(&self, simd: S, pixels: &[u32], y: usize, row: &mut [u32]) {
        let material = &self.material;
        let base = self.base.get(y * self.width..(y + 1) * self.width);
        for (out, base) in row.iter_mut().zip(base.unwrap_or_default()) {
            *out = *base;
        }
        let spans = match (self.rows.get(y), self.rows.get(y + 1)) {
            (Some(&first), Some(&end)) => self.spans.get(first..end).unwrap_or_default(),
            _ => &[],
        };
        let mut run = Run::default();
        for span in spans {
            match *span {
                Span::Plain { x0, x1, texel } => {
                    let (x0, x1, texel) = (x0 as usize, x1 as usize, texel as usize);
                    let Some(out) = row.get_mut(x0..x1) else {
                        continue;
                    };
                    let src = pixels.get(texel..).unwrap_or_default();
                    for (k, out) in out.chunks_mut(RUN).enumerate() {
                        let mut texels = [0; RUN];
                        let src = src.get(k * RUN..).unwrap_or_default();
                        for (texel, src) in texels.iter_mut().zip(src) {
                            *texel = *src;
                        }
                        material.shade::<S, true>(simd, unpack16(simd, texels), &run, out);
                    }
                }
                Span::Edge { x0, x1, edge } => {
                    let (x0, x1, edge) = (x0 as usize, x1 as usize, edge as usize);
                    let (Some(out), Some(edges)) =
                        (row.get_mut(x0..x1), self.edges.get(edge..edge + (x1 - x0)))
                    else {
                        continue;
                    };
                    for (out, edges) in out.chunks_mut(RUN).zip(edges.chunks(RUN)) {
                        for (i, edge) in edges.iter().enumerate() {
                            let color = match self.split_taps.get(edge.split as usize) {
                                Some([red, blue]) => [
                                    red.channel(pixels, 16),
                                    edge.tap.channel(pixels, 8),
                                    blue.channel(pixels, 0),
                                ],
                                None => {
                                    let [r, g, b, _] = edge.tap.read(pixels);
                                    [r, g, b]
                                }
                            };
                            run.color(i, color);
                            run.look(i..i + 1, &edge.look);
                        }
                        material.shade::<S, false>(simd, run.background(simd), &run, out);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_min_bridges_only_within_k() {
        assert_eq!(smooth_min(1.0, 5.0, 2.0), 1.0);
        assert!(smooth_min(1.0, 1.0, 2.0) < 1.0);
        assert_eq!(smooth_min(3.0, 2.0, 0.0), 2.0);
    }

    #[test]
    fn lens_source_interpolates_and_stops() {
        let mut curve = [1.0e6_f32; 16];
        curve[..4].copy_from_slice(&[0.0, -2.0, 10.0, 10.0]);
        assert_eq!(lens_source(&curve, 0.0), -2.0);
        assert_eq!(lens_source(&curve, 5.0), 4.0);
        assert_eq!(lens_source(&curve, 20.0), 20.0);
    }
}
