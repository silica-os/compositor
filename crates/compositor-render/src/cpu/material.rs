//! The glass material on the CPU (`material.frag`), in two stages: the geometry of every pixel
//! (distances, lens, rims, coverage), which depends only on the shapes, and its color over a
//! blurred backdrop, redone when the backdrop changes.

use compositor_raster::fearless_simd::{Simd, dispatch};
use compositor_raster::{CpuRenderer, Image};
use smithay::backend::renderer::gles::Uniform;

use super::{Uniforms, clamp01, length, mix, mix3, pack, unpack, zip_with};

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

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
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
            coverage,
            shadow,
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
            texel.offset_in = offset_in * if offset_in > 0.0 { self.rim } else { self.line };
            let u_out = sd / self.scale_px - 0.5;
            let [top, side, bottom] = self.edges.each_ref().map(|m| outline_at(m, u_out));
            let outline = w_top * top
                + w_side * side
                + w_bottom * bottom
                + w_shared * top.min(side.min(bottom));
            texel.outline = clamp01(outline * self.line);
        }
        texel.in_band = clamp01(self.inside_rim_width + 0.5 - t);
        if self.rim_light[3] > 0.0 {
            let lit_distance = self.union_distance(p, 0.0, [0.0, self.rim_light_shape[0]]);
            texel.lit =
                1.0 / (1.0 + (-1.702 * lit_distance / self.rim_light_shape[1].max(0.25)).exp());
        }
        texel
    }

    /// Whether the red and blue channels sample apart from green.
    fn splits(&self) -> bool {
        self.chromatic > 0.0 && self.refraction > 0.0
    }

    /// The pixels of `run` (`material.frag` from the backdrop sample on), the first `out.len()`
    /// of them. Outside the shape the coverage is 0 and the backdrop black, which leaves the
    /// outline and the shadow, black: one formula for every pixel, with no branch, so that
    /// the loop vectorizes.
    #[inline(always)]
    fn shade(&self, run: &Run, out: &mut [u32]) {
        let opaque = self.fallback[3] > 0.5;
        let saturation = if opaque { 1.0 } else { self.saturation };
        let brightness = if opaque { None } else { Some(self.brightness) };
        let tint = [self.tint[0], self.tint[1], self.tint[2]];
        let rim = [self.inside_rim[0], self.inside_rim[1], self.inside_rim[2]];
        let light = [self.rim_light[0], self.rim_light[1], self.rim_light[2]];
        let room = (1.0 - self.fill_gray).max(0.05);
        let fill = self.fill_gray.max(0.05);
        let mut pixels = [0u32; RUN];
        for (i, pixel) in pixels.iter_mut().enumerate() {
            let lane = |values: &[f32; RUN]| values.get(i).copied().unwrap_or(0.0);
            let background = [lane(&run.r), lane(&run.g), lane(&run.b)];
            let mut color = self.offset;
            for (o, row) in color.iter_mut().zip(self.rows) {
                *o += dot3(row, background);
            }
            let luminance = (background[0] + background[1] + background[2]) / 3.0;
            for ((c, q2), q3) in color.iter_mut().zip(self.curve2).zip(self.curve3) {
                *c += (q2 + q3 * luminance) * luminance * luminance;
            }
            let rec709 = dot3(color, [0.2126, 0.7152, 0.0722]);
            color = mix3([rec709; 3], color, saturation);
            if let Some(brightness) = brightness {
                color = color.map(|c| clamp01(c * brightness));
            }
            color = mix3(color, tint, self.tint[3]);
            if self.edge_on {
                let offset_in = lane(&run.offset_in);
                let k = 1.0 + offset_in / fill;
                color = color.map(|c| {
                    let lighter = c + offset_in * ((1.0 - c) / room).min(1.0);
                    if offset_in > 0.0 { lighter } else { c * k }
                });
            }
            color = mix3(color, rim, self.inside_rim[3] * lane(&run.in_band));
            if self.rim_light[3] > 0.0 {
                color = mix3(color, light, self.rim_light[3] * lane(&run.lit));
            }
            let coverage = lane(&run.coverage);
            let [r, g, b] = color.map(|c| clamp01(c) * coverage);
            let a = coverage + lane(&run.outline) * (1.0 - coverage);
            *pixel = pack([r, g, b, a + lane(&run.shadow) * (1.0 - a)]);
        }
        for (o, p) in out.iter_mut().zip(pixels) {
            *o = p;
        }
    }
}

/// Pixels shaded at once: long enough for the vector units, short enough to stay in
/// registers and L1.
const RUN: usize = 16;

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
}

/// The geometry of a material over a part of its texture.
#[derive(Debug)]
pub(crate) struct Plan {
    material: Material,
    width: usize,
    /// The size of the texture the taps read.
    tex_size: (usize, usize),
    texels: Vec<Texel>,
    /// Where each pixel reads the backdrop (green, or all three channels)…
    taps: Vec<Tap>,
    /// …and red and blue where they read apart, in pixel order, from `split_rows[y]` on in row
    /// `y`.
    split_taps: Vec<[Tap; 2]>,
    split_rows: Vec<usize>,
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
        let mut texels = vec![Texel::default(); size.0.saturating_mul(size.1)];
        let level = renderer.level();
        renderer.rows_costly(&mut texels, size.0, |y, row| {
            let py = (origin[1] as f32) + y as f32 + 0.5;
            dispatch!(level, simd => material.texel_row(simd, origin[0] as f32, py, row));
        });
        let splits = material.splits();
        let taps: Vec<Tap> = texels
            .iter()
            .map(|t| Tap::new(t.sample, tex_size))
            .collect();
        let mut split_taps = Vec::new();
        let mut split_rows = Vec::with_capacity(size.1);
        for row in texels.chunks(size.0.max(1)) {
            split_rows.push(split_taps.len());
            for texel in row.iter().filter(|t| splits && t.split != [0.0, 0.0]) {
                let [x, y] = texel.sample;
                let [dx, dy] = texel.split;
                split_taps.push([
                    Tap::new([x + dx, y + dy], tex_size),
                    Tap::new([x - dx, y - dy], tex_size),
                ]);
            }
        }
        // What the covered pixels read; an opaque fallback reads nothing.
        let mut reads = None;
        if material.fallback[3] <= 0.5 {
            let covered = texels.iter().zip(&taps).filter(|(t, _)| t.coverage > 0.0);
            for tap in covered
                .map(|(_, tap)| tap)
                .chain(split_taps.iter().flatten())
            {
                reads = tap.reach(tex_size.0, reads);
            }
        }
        Self {
            material,
            width: size.0,
            tex_size,
            texels,
            taps,
            split_taps,
            split_rows,
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
        let height = self.texels.len().checked_div(self.width).unwrap_or(0);
        let mut out = Image::new(self.width, height);
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
    fn paint_row<S: Simd>(&self, _: S, pixels: &[u32], y: usize, row: &mut [u32]) {
        let material = &self.material;
        let fallback = (material.fallback[3] > 0.5).then_some([
            material.fallback[0],
            material.fallback[1],
            material.fallback[2],
        ]);
        let splits = material.splits();
        let span = y * self.width..(y + 1) * self.width;
        let texels = self.texels.get(span.clone()).unwrap_or_default();
        let taps = self.taps.get(span).unwrap_or_default();
        let first = self.split_rows.get(y).copied().unwrap_or(usize::MAX);
        let mut split_taps = self.split_taps.get(first..).unwrap_or_default().iter();
        let mut run = Run::default();
        for (out, (texels, taps)) in row
            .chunks_mut(RUN)
            .zip(texels.chunks(RUN).zip(taps.chunks(RUN)))
        {
            for (i, (texel, tap)) in texels.iter().zip(taps).enumerate() {
                let split = splits && texel.split != [0.0, 0.0];
                let side = if split { split_taps.next() } else { None };
                let [r, g, b] = match (texel.coverage > 0.0, fallback, side) {
                    (false, ..) => [0.0; 3],
                    (true, Some(color), _) => color,
                    (true, None, Some([red, blue])) => [
                        red.read(pixels)[0],
                        tap.read(pixels)[1],
                        blue.read(pixels)[2],
                    ],
                    (true, None, None) => {
                        let [r, g, b, _] = tap.read(pixels);
                        [r, g, b]
                    }
                };
                let set = |lane: &mut [f32; RUN], value: f32| {
                    if let Some(lane) = lane.get_mut(i) {
                        *lane = value;
                    }
                };
                set(&mut run.r, r);
                set(&mut run.g, g);
                set(&mut run.b, b);
                set(&mut run.coverage, texel.coverage);
                set(&mut run.outline, texel.outline);
                set(&mut run.offset_in, texel.offset_in);
                set(&mut run.in_band, texel.in_band);
                set(&mut run.lit, texel.lit);
                set(&mut run.shadow, texel.shadow);
            }
            material.shade(&run, out);
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
