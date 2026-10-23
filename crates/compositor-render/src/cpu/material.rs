//! The glass material on the CPU (`material.frag`), in two stages: the geometry of every pixel
//! (distances, lens, rims, coverage), which depends only on the shapes, and its color over a
//! blurred backdrop, redone when the backdrop changes.

use compositor_raster::{CpuRenderer, Image};
use smithay::backend::renderer::gles::Uniform;

use super::{Uniforms, bilinear, clamp01, length, mix, mix3, pack, zip_with};

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

    /// The geometry of the pixel at `p` (texture pixels).
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

    /// The pixel over the backdrop `tex` (premultiplied RGBA, alpha 1).
    fn color(&self, texel: &Texel, tex: &Image) -> [f32; 4] {
        if texel.coverage <= 0.0 {
            // Outside the shape: the outline and the shadow, black.
            let a = texel.outline;
            return [0.0, 0.0, 0.0, a + texel.shadow * (1.0 - a)];
        }
        let opaque = self.fallback[3] > 0.5;
        let background = if opaque {
            [self.fallback[0], self.fallback[1], self.fallback[2]]
        } else if self.chromatic > 0.0 && self.refraction > 0.0 {
            let [x, y] = texel.sample;
            let [r, ..] = bilinear(tex, x + texel.split[0], y + texel.split[1]);
            let [_, g, ..] = bilinear(tex, x, y);
            let [_, _, b, _] = bilinear(tex, x - texel.split[0], y - texel.split[1]);
            [r, g, b]
        } else {
            let [r, g, b, _] = bilinear(tex, texel.sample[0], texel.sample[1]);
            [r, g, b]
        };
        let mut color = self.offset;
        for (o, row) in color.iter_mut().zip(self.rows) {
            *o += dot3(row, background);
        }
        let luminance = (background[0] + background[1] + background[2]) / 3.0;
        let curve = zip_with(self.curve2, self.curve3, |q2, q3| {
            (q2 + q3 * luminance) * luminance * luminance
        });
        color = zip_with(color, curve, |c, q| c + q);
        let rec709 = dot3(color, [0.2126, 0.7152, 0.0722]);
        color = mix3(
            [rec709; 3],
            color,
            if opaque { 1.0 } else { self.saturation },
        );
        if !opaque {
            color = color.map(|c| clamp01(c * self.brightness));
        }
        let tint = [self.tint[0], self.tint[1], self.tint[2]];
        color = mix3(color, tint, self.tint[3]);
        if self.edge_on {
            let offset_in = texel.offset_in;
            if offset_in > 0.0 {
                let room = (1.0 - self.fill_gray).max(0.05);
                color = color.map(|c| c + offset_in * ((1.0 - c) / room).min(1.0));
            } else {
                let k = 1.0 + offset_in / self.fill_gray.max(0.05);
                color = color.map(|c| c * k);
            }
        }
        let rim = [self.inside_rim[0], self.inside_rim[1], self.inside_rim[2]];
        color = mix3(color, rim, self.inside_rim[3] * texel.in_band);
        if self.rim_light[3] > 0.0 {
            let light = [self.rim_light[0], self.rim_light[1], self.rim_light[2]];
            color = mix3(color, light, self.rim_light[3] * texel.lit);
        }
        let color = color.map(clamp01);
        let coverage = texel.coverage;
        let mut result = [
            color[0] * coverage,
            color[1] * coverage,
            color[2] * coverage,
            coverage + texel.outline * (1.0 - coverage),
        ];
        result[3] += texel.shadow * (1.0 - result[3]);
        result
    }
}

/// The geometry of a material over a part of its texture.
#[derive(Debug)]
pub(crate) struct Plan {
    material: Material,
    width: usize,
    texels: Vec<Texel>,
}

impl Plan {
    /// The geometry of the `size` pixels at `origin` of the material's texture.
    pub fn new(
        renderer: &CpuRenderer,
        material: Material,
        origin: [i32; 2],
        size: (usize, usize),
    ) -> Self {
        let mut texels = vec![Texel::default(); size.0.saturating_mul(size.1)];
        renderer.rows(&mut texels, size.0, |y, row| {
            let py = (origin[1] as f32) + y as f32 + 0.5;
            for (x, texel) in row.iter_mut().enumerate() {
                let px = (origin[0] as f32) + x as f32 + 0.5;
                *texel = material.texel([px, py]);
            }
        });
        Self {
            material,
            width: size.0,
            texels,
        }
    }

    /// The material over `backdrop` (as large as `tex_size`), at alpha 1.
    pub fn paint(&self, renderer: &CpuRenderer, backdrop: &Image) -> Image {
        let height = self.texels.len().checked_div(self.width).unwrap_or(0);
        let mut out = Image::new(self.width, height);
        let width = self.width;
        renderer.rows(out.pixels_mut(), width, |y, row| {
            let texels = self
                .texels
                .get(y * width..(y + 1) * width)
                .unwrap_or_default();
            for (p, texel) in row.iter_mut().zip(texels) {
                *p = pack(self.material.color(texel, backdrop));
            }
        });
        out
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
