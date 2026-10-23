//! Glass: the background behind a surface, blurred and drawn as an Amalfi material in the
//! regions declared with `silica-glass-v1`.
//!
//! For every surface with glass: the elements behind it are redrawn into an offscreen
//! texture (regions plus the blur margin), blurred with a separable Gaussian and mixed with the
//! kind's sharp share, and a `GlassElement` per group of regions draws it: saturation, tint
//! fill, inside rim and rim light, the drop shadow outside, corners and merged shapes from the
//! signed distance. The textures are reused while the background does not change. The
//! materials are `docs/design/amalfi/materials.md` (`glass_profile`).

use std::cell::RefCell;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformType,
    UniformValue,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Bind, Frame, Offscreen, Renderer};
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::glass_profile::{GlassProfile, lens_width, refraction_band};

/// Parameters of a material: blur, sharp share and color (`out = M · in + b + q₂ · l² + q₃ · l³`
/// on the encoded sRGB values, 0–1, l the mean of the input channels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaterialParams {
    /// σ of the Gaussian in points.
    pub sigma: f64,
    /// Share of the sharp image mixed with the blurred one.
    pub sharp: f32,
    pub matrix: [[f32; 3]; 3],
    pub offset: [f32; 3],
    /// Quadratic and cubic terms on the input luminance (`q₂`, `q₃`): some transfers bend.
    pub curve: [[f32; 3]; 2],
    /// The edge lens.
    pub lens: Lens,
    /// The edge row and the inner glow.
    pub edge: Edge,
    /// Gap below which two regions of this material merge (0: never).
    pub merge_pt: f32,
}

/// A material's own edge, by orientation (top, sides, bottom): a profile of offsets inside the
/// shape and the darkening just outside it. Until Amalfi these were the edges measured on the
/// reference captures (`xtask/tools/glass_parity.py`); the Amalfi materials draw the rims of
/// their profile instead and have none ([`Edge::NONE`]). The shader picks the
/// orientation from the shape's own normal (SDF), so corners, tips and the bridges between
/// merged shapes blend continuously.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    pub top: EdgeProfile,
    pub side: EdgeProfile,
    pub bottom: EdgeProfile,
}

/// One orientation of the edge.
/// - `inner[i]`: offset in levels (0–255) of the i-th point inside the edge, relative to the
///   fill over the reference gray. Positive offsets are lights: they add the same amount over
///   darker backgrounds and less over lighter ones, scaled by min(1, (1 − c) / (1 − c_gray)).
///   Negative offsets are shadows: they multiply.
/// - `outer[i]`: alpha of black on the i-th point outside the edge (a dark outline drawn over
///   whatever is behind).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdgeProfile {
    pub inner: [f32; 12],
    pub outer: [f32; 2],
}

impl EdgeProfile {
    pub const NONE: Self = Self {
        inner: [0.0; 12],
        outer: [0.0; 2],
    };
}

impl Edge {
    /// No edge of the material's own (Glass/Regular draws the Amalfi rims of
    /// its profile instead).
    pub const NONE: Self = Self {
        top: EdgeProfile::NONE,
        side: EdgeProfile::NONE,
        bottom: EdgeProfile::NONE,
    };

    /// The three profiles as `mat4`s: inner 0–11 (0–1 levels), outer 0–1 in 12–13.
    fn uniform(&self) -> UniformValue {
        let pack = |p: &EdgeProfile| {
            let mut m = [0.0_f32; 16];
            for (slot, value) in m.iter_mut().zip(p.inner) {
                *slot = value / 255.0;
            }
            m[12] = p.outer[0];
            m[13] = p.outer[1];
            m
        };
        UniformValue::Matrix4x4 {
            matrices: vec![pack(&self.top), pack(&self.side), pack(&self.bottom)],
            transpose: false,
        }
    }
}

/// The edge lens: for every side, from which distance from the edge (s) the glass samples the
/// background for a pixel at distance t, in points, with (t, s) points at increasing t (at most
/// 8). s > t magnifies (samples further inside), s < 0 samples outside the edge. Before the
/// first point the first one applies, past the last one s = t.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lens {
    pub side: &'static [(f32, f32)],
    pub top: &'static [(f32, f32)],
    pub bottom: &'static [(f32, f32)],
}

impl Lens {
    /// No lens: the material does not refract.
    pub const NONE: Self = Self {
        side: &[],
        top: &[],
        bottom: &[],
    };

    /// How far outside the edge it can sample, in points.
    pub fn outward_reach(&self) -> f32 {
        [self.side, self.top, self.bottom]
            .iter()
            .flat_map(|curve| curve.iter().map(|(_, s)| -s))
            .fold(0.0, f32::max)
    }

    /// The largest displacement of the glass, in points (how far inside the edge the sampled
    /// background is from the pixel's own position), and the distance from the edge where it is.
    pub fn max_displacement(&self) -> (f32, f32) {
        [self.side, self.top, self.bottom]
            .iter()
            .flat_map(|curve| curve.iter().map(|(t, s)| (s - t, *t)))
            .fold(
                (0.0, 0.0),
                |best, now| if now.0 > best.0 { now } else { best },
            )
    }

    /// How far inside the edge it can sample, in points (the deepest source of the curves).
    pub fn inward_reach(&self) -> f32 {
        [self.side, self.top, self.bottom]
            .iter()
            .flat_map(|curve| curve.iter().map(|(t, s)| t.max(*s)))
            .fold(0.0, f32::max)
    }

    /// The three curves as arrays of `mat4` (columns: t0, s0, t1, s1), scaled to pixels.
    fn uniform(&self, px: f32) -> UniformValue {
        let pack = |curve: &[(f32, f32)]| {
            // Empty slots have a huge t: the shader stops there.
            let mut m = [1.0e6_f32; 16];
            for ([slot_t, slot_s], (t, s)) in m.as_chunks_mut::<2>().0.iter_mut().zip(curve) {
                *slot_t = t * px;
                *slot_s = s * px;
            }
            m
        };
        UniformValue::Matrix4x4 {
            matrices: vec![pack(self.side), pack(self.top), pack(self.bottom)],
            transpose: false,
        }
    }
}

impl MaterialParams {
    /// An Amalfi material in the glass pass: a blur of σ `sigma` points with the `sharp` share,
    /// a neutral color transfer (the profile's saturation, brightness and tint make the color),
    /// the `lens` of its refraction (`Lens::NONE`: none) and no edge of its own (the profile draws the rims), merging within `merge_pt`.
    pub const fn amalfi(sigma: f64, sharp: f32, merge_pt: f32, lens: Lens) -> Self {
        Self {
            sigma,
            sharp,
            matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            offset: [0.0; 3],
            curve: [[0.0; 3]; 2],
            lens,
            edge: Edge::NONE,
            merge_pt,
        }
    }
}

pub const BLUR_SHADER: &str = include_str!("shaders/blur.frag");
pub const BLUR_UNIFORMS: [(&str, UniformType); 2] =
    [("step", UniformType::_2f), ("sigma", UniformType::_1f)];
/// A reduced blur drawn back at full size.
pub const UPSAMPLE_SHADER: &str = include_str!("shaders/upsample.frag");
pub const UPSAMPLE_UNIFORMS: [(&str, UniformType); 1] = [("tex_size", UniformType::_2f)];
/// The box prefilter of a reduced-resolution blur.
pub const DOWNSAMPLE_SHADER: &str = include_str!("shaders/downsample.frag");
pub const DOWNSAMPLE_UNIFORMS: [(&str, UniformType); 5] = [
    ("src_size", UniformType::_2f),
    ("out_size", UniformType::_2f),
    ("factor", UniformType::_1f),
    ("pad", UniformType::_1f),
    ("mirror", UniformType::_1f),
];
pub const MATERIAL_SHADER: &str = include_str!("shaders/material.frag");
pub const MATERIAL_UNIFORMS: [(&str, UniformType); 35] = [
    ("tex_size", UniformType::_2f),
    ("shapes", UniformType::Matrix4x4),
    ("radii", UniformType::Matrix4x4),
    ("rotations", UniformType::Matrix4x4),
    ("tips", UniformType::Matrix4x4),
    ("shape_count", UniformType::_1f),
    ("merge", UniformType::_1f),
    ("exponent", UniformType::_1f),
    ("row_r", UniformType::_3f),
    ("row_g", UniformType::_3f),
    ("row_b", UniformType::_3f),
    ("offset", UniformType::_3f),
    ("curve2", UniformType::_3f),
    ("curve3", UniformType::_3f),
    ("scale_px", UniformType::_1f),
    ("lens", UniformType::Matrix4x4),
    ("lens_reach", UniformType::_1f),
    ("edges", UniformType::Matrix4x4),
    ("fill_gray", UniformType::_1f),
    ("edge_power", UniformType::_1f),
    ("lens_widths", UniformType::Matrix4x4),
    ("refraction", UniformType::_1f),
    ("chromatic", UniformType::_1f),
    ("brightness", UniformType::_1f),
    ("rim", UniformType::_1f),
    ("line", UniformType::_1f),
    ("saturation", UniformType::_1f),
    ("tint_color", UniformType::_4f),
    ("shadow", UniformType::_3f),
    ("edge_on", UniformType::_1f),
    ("inside_rim", UniformType::_4f),
    ("inside_rim_width", UniformType::_1f),
    ("rim_light", UniformType::_4f),
    ("rim_light_shape", UniformType::_2f),
    ("fallback", UniformType::_4f),
];

/// How fast each orientation's edge (top, sides, bottom) fades around a curve: its weight is
/// the matching component of the outward normal to this power. Fitted on the ends of the
/// Control Center modules over gray (glass.md, `gray-dark` capture): the dark side line is
/// ~0.7 of its strength 15° from the side, ~0.5 at 30° and gone by 45°; the light top row ~0.4
/// at 30° from the top. With the square of the component (the earlier weighting) the side
/// line covered the whole end of a capsule: the black crescents at the left and right ends
/// of modules, pills and menus (#168).
pub const EDGE_POWER: f32 = 5.0;

/// The weights of the top, side and bottom edges, and the shared part, for an outward normal
/// (the shader's `w_top`, `w_side`, `w_bottom`, `w_shared`). Where no orientation owns the
/// curve (a corner's diagonal) the shared part takes what the three agree on, so an outline
/// measured the same on every side (the menus') runs without a gap.
pub fn edge_weights(normal: (f32, f32)) -> [f32; 4] {
    let (x, y) = normal;
    let length = (x * x + y * y).sqrt();
    let (x, y) = if length > 0.0 {
        (x / length, y / length)
    } else {
        (0.0, -1.0)
    };
    let top = (-y).max(0.0).powf(EDGE_POWER);
    let bottom = y.max(0.0).powf(EDGE_POWER);
    let side = x.abs().powf(EDGE_POWER);
    [top, side, bottom, (1.0 - top - side - bottom).max(0.0)]
}

/// A screen direction or offset in the frame of a region rotated by `rotation` (radians,
/// clockwise on screen): turned back by the rotation (the shader's `to_frame`).
pub fn to_frame((x, y): (f32, f32), rotation: f32) -> (f32, f32) {
    let (sin, cos) = rotation.sin_cos();
    (cos * x + sin * y, -sin * x + cos * y)
}

/// The edge weights of a rotated region: the screen normal is read in the region's own frame,
/// so its top edge keeps the top light however the region turns (the shader's `w_top`, … on
/// the rotated normal).
pub fn edge_weights_in_frame(normal: (f32, f32), rotation: f32) -> [f32; 4] {
    edge_weights(to_frame(normal, rotation))
}

/// Signed distance (pixels, negative inside) of `p` from a rectangle `[x, y, width, height]`
/// with circular-arc corners of radius `r`, rotated by `rotation` (radians, clockwise on
/// screen) about its centre: the shader's `rounded_distance` on `to_region`.
pub fn rotated_rounded_distance(p: (f32, f32), rect: [f32; 4], r: f32, rotation: f32) -> f32 {
    let [x, y, w, h] = rect;
    let (cx, cy) = (x + w * 0.5, y + h * 0.5);
    let (lx, ly) = to_frame((p.0 - cx, p.1 - cy), rotation);
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    let dx = lx.abs() - (w * 0.5 - r);
    let dy = ly.abs() - (h * 0.5 - r);
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    outside + dx.max(dy).min(0.0) - r
}

/// The exponent of the superellipse that stands for a continuous corner of `smoothing`
/// (0–1): 2 (a circle) without smoothing, 3.53 at the body's 0.6 (the least-squares fit of the
/// Sketch body path, `tools/body.svg` of silica-icons: within 2.2 units of 824 of the curve),
/// linear in between.
pub fn continuous_exponent(smoothing: f32) -> f32 {
    2.0 + 2.55 * smoothing.clamp(0.0, 1.0)
}

/// Signed distance (pixels, negative inside) of `p` from a rectangle `[x, y, width, height]`
/// whose corners are continuous curves: the corner starts `r × (1 + smoothing)` from the
/// corner point along each edge and is a superellipse of [`continuous_exponent`] over that
/// extent (silica-glass-v1 `add_continuous_region`). The shader's `continuous_distance`, rotated
/// by `rotation` about the centre like [`rotated_rounded_distance`].
pub fn rotated_continuous_distance(
    p: (f32, f32),
    rect: [f32; 4],
    r: f32,
    smoothing: f32,
    rotation: f32,
) -> f32 {
    let [x, y, w, h] = rect;
    let (cx, cy) = (x + w * 0.5, y + h * 0.5);
    let (lx, ly) = to_frame((p.0 - cx, p.1 - cy), rotation);
    let smoothing = smoothing.clamp(0.0, 1.0);
    let a = (r.max(0.0) * (1.0 + smoothing)).min(w * 0.5).min(h * 0.5);
    let dx = lx.abs() - (w * 0.5 - a);
    let dy = ly.abs() - (h * 0.5 - a);
    if a > 0.0 && dx > 0.0 && dy > 0.0 {
        let n = continuous_exponent(smoothing);
        let (qx, qy) = (dx / a, dy / a);
        let f = (qx.powf(n) + qy.powf(n)).powf(1.0 / n);
        // The gradient of f in units of q: the distance is (f - 1) / |grad| in units of a.
        let grad = (qx.powf(2.0 * n - 2.0) + qy.powf(2.0 * n - 2.0)).sqrt() / f.powf(n - 1.0);
        return (f - 1.0) * a / grad.max(1e-3);
    }
    dx.max(dy).min(0.0) + dx.max(0.0).hypot(dy.max(0.0)) - a
}

impl MaterialParams {
    /// The fill over the reference gray (`#7a7b80`), 0–1 luminance: the level the edge
    /// offsets were measured against.
    pub fn fill_on_gray(&self) -> f32 {
        self.fill_on_gray_with(&GlassProfile::NEUTRAL)
    }

    /// The same with a profile's saturation and tint over the material.
    pub fn fill_on_gray_with(&self, profile: &GlassProfile) -> f32 {
        let [r, g, b] = profile_color(profile, self.transfer(REFERENCE_GRAY));
        ((r + g + b) / 3.0).clamp(0.01, 0.99)
    }

    /// The material's color transfer on an encoded sRGB color (0-1): what a blurred flat
    /// background of that color shows inside the glass, before the edge, the profile's
    /// saturation and tint (the shader's `out = M * in + b + q2 * l^2 + q3 * l^3`, clamped).
    pub fn transfer(&self, input: [f32; 3]) -> [f32; 3] {
        let [x, y, z] = input;
        let l = (x + y + z) / 3.0;
        let [q2, q3] = self.curve;
        let mut out = [0.0; 3];
        for ((((value, row), offset), q2), q3) in out
            .iter_mut()
            .zip(self.matrix)
            .zip(self.offset)
            .zip(q2)
            .zip(q3)
        {
            let [r0, r1, r2] = row;
            let v = r0 * x + r1 * y + r2 * z + offset + (q2 + q3 * l) * l * l;
            *value = v.clamp(0.0, 1.0);
        }
        out
    }
}

/// The reference gray (`#7a7b80`) the edge offsets and the tone checks are measured on.
pub const REFERENCE_GRAY: [f32; 3] = [122.0 / 255.0, 123.0 / 255.0, 128.0 / 255.0];

/// A profile's saturation and tint over a material color (the shader's, on the fill).
pub fn profile_color(profile: &GlassProfile, [r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let saturate = |c: f32| (l + (c - l) * profile.saturation) * profile.brightness;
    let [tr, tg, tb, amount] = profile.tint;
    let tint = |c: f32, t: f32| c + (t - c) * amount;
    [
        tint(saturate(r), tr),
        tint(saturate(g), tg),
        tint(saturate(b), tb),
    ]
}

/// The fill of a profile without blur (the opaque quality level, reduced transparency): its
/// tint over its fallback surface, `Surface/Window` (the shader's, with `fallback` on).
pub fn fallback_color(profile: &GlassProfile) -> [f32; 3] {
    let [tr, tg, tb, amount] = profile.tint;
    let [r, g, b] = profile.fallback;
    let tint = |c: f32, t: f32| c + (t - c) * amount;
    [tint(r, tr), tint(g, tg), tint(b, tb)]
}

/// How far the lens reaches inside the glass (the last point of the curves), in points: the
/// normal is computed on corners at least this wide, so that it does not jump on the diagonal
/// of a right angle.
fn lens_extent(lens: &Lens) -> f32 {
    [lens.side, lens.top, lens.bottom]
        .iter()
        .filter_map(|curve| curve.last().map(|(t, _)| *t))
        .fold(0.0, f32::max)
}

/// Glass quality levels: full (blurs everything behind), static frosted (only the desktop
/// wallpaper: recomputed rarely), opaque (no blur: the Amalfi fallback, each material's tint
/// over `Surface/Window`, rims and shadow kept). For weak GPUs (plan, "Risks") and for reduced
/// transparency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlassQuality {
    Full,
    Frosted,
    Opaque,
}

impl GlassQuality {
    pub fn name(self) -> &'static str {
        match self {
            GlassQuality::Full => "full",
            GlassQuality::Frosted => "frosted",
            GlassQuality::Opaque => "opaque",
        }
    }
}

/// Gap between two regions of the same material below which they join into a single shape,
/// like drops (smooth union of the distances, k = 2 · MERGE_PT: two edges at distance g touch
/// when g < k / 2). Measured limits: the search panel's round buttons 10 pt from the panel stay
/// separate (spotlight.md) and the Control Center modules, whose glasses are 10 pt apart
/// horizontally (the side edge sticks out by 1 pt), have no bridge or bulge on the edge
/// (controlcenter.md): k ≤ 10, that is at most 5. The exact value is not measured
/// (plan/reference/RICHIESTE.md, G-02).
pub const MERGE_PT: f32 = 5.0;
/// Regions the shader merges together at most: beyond that, separate groups.
const MAX_SHAPES: usize = 8;

/// The smallest σ, in pixels of the reduced texture, a reduced blur runs with: below it the
/// factor halves (a kind's factor is validated by the equivalence test, this only guards
/// against a σ the test did not see).
const MIN_REDUCED_SIGMA_PX: f64 = 1.5;

/// Where a tip points: its apex at the middle of that side of its rectangle, its base on the
/// opposite one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tip {
    Down,
    Up,
    Left,
    Right,
}

/// The outline of a tip, measured in TIP-01 (points). Not a triangle: two straight flanks at
/// `angle` from the body edge, whose sharp corners would sit `half_base` from the axis; a round
/// apex of `apex_radius` tangent to both flanks; concave fillets of `fillet_radius` tangent to
/// the body edge and to each flank. It is symmetric and fixed in points (a larger popover has
/// the same tip). The fit is within ~0.2 px of the reference outline at 1×.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TipProfile {
    pub half_base: f64,
    /// Flank angle from the body edge, in radians.
    pub angle: f64,
    pub apex_radius: f64,
    pub fillet_radius: f64,
}

/// A measured tip (TIP-01, points and degrees).
struct TipPreset {
    depth: f64,
    angle_deg: f64,
    apex_radius: f64,
    fillet_radius: f64,
}

/// The Dock icon menu's tip: 10.19 deep, flanks at 50.8°, apex radius 5.30, fillets 9.41
/// (15.29 from the axis to where the outline leaves the body edge).
const DOCK_MENU_TIP: TipPreset = TipPreset {
    depth: 10.19,
    angle_deg: 50.8,
    apex_radius: 5.30,
    fillet_radius: 9.41,
};
/// A popover's tip on its top or bottom edge (mean of the two): 10.8 deep, flanks at 46.2°,
/// apex radius 5.58, fillets 8.0.
const POPOVER_TIP: TipPreset = TipPreset {
    depth: 10.80,
    angle_deg: 46.2,
    apex_radius: 5.58,
    fillet_radius: 8.0,
};
/// A popover's tip on a side edge: one point deeper with longer fillets, the same flanks and
/// apex.
const POPOVER_SIDE_TIP: TipPreset = TipPreset {
    depth: 11.74,
    angle_deg: 46.1,
    apex_radius: 5.73,
    fillet_radius: 13.6,
};

impl TipPreset {
    fn profile(&self) -> TipProfile {
        let angle = self.angle_deg.to_radians();
        TipProfile {
            half_base: TipProfile::half_base_for(angle, self.apex_radius, self.depth),
            angle,
            apex_radius: self.apex_radius,
            fillet_radius: self.fillet_radius,
        }
    }
}

impl TipProfile {
    pub fn dock_menu() -> Self {
        DOCK_MENU_TIP.profile()
    }

    pub fn popover() -> Self {
        POPOVER_TIP.profile()
    }

    pub fn popover_side() -> Self {
        POPOVER_SIDE_TIP.profile()
    }

    /// The half base that gives `depth`: the sharp apex sits half_base · tan(angle) from the
    /// edge, and the round apex takes back apex_radius · (1 / cos(angle) − 1).
    fn half_base_for(angle: f64, apex_radius: f64, depth: f64) -> f64 {
        (depth + apex_radius * (1.0 / angle.cos() - 1.0)) / angle.tan()
    }

    /// Depth from the body edge to the apex.
    pub fn depth(&self) -> f64 {
        self.half_base * self.angle.tan() - self.apex_radius * (1.0 / self.angle.cos() - 1.0)
    }

    /// From the axis to where the outline leaves the body edge.
    pub fn half_width(&self) -> f64 {
        self.half_base + self.fillet_radius * (self.angle / 2.0).tan()
    }

    /// The tip drawn in a rectangle of `width` along the body edge and `depth` away from it
    /// (points): the measured preset closest to that size keeps its flank angle and apex,
    /// the half base gives the depth and the fillets give the width. The measured sizes give
    /// the measured outlines.
    pub fn fit(width: f64, depth: f64) -> Self {
        let half = width / 2.0;
        // The side does not choose: a size measured on one edge gives the same outline on
        // every edge (rotated), so the closest measured size wins.
        let off = |p: &Self| (p.half_width() - half).abs() + (p.depth() - depth).abs();
        let preset = [Self::dock_menu(), Self::popover(), Self::popover_side()]
            .into_iter()
            .fold(None::<Self>, |best, p| match best {
                Some(b) if off(&b) <= off(&p) => Some(b),
                _ => Some(p),
            })
            .unwrap_or_else(Self::dock_menu);
        // Protocol rectangles are whole points: a size that rounds the measured one draws the
        // measured outline itself (it may pass the rectangle by a fraction of a point).
        if (preset.depth() - depth).abs() <= 0.5 && (preset.half_width() - half).abs() <= 0.5 {
            return preset;
        }
        let depth = depth.max(1.0);
        let half = half.max(0.5);
        // The apex cannot be rounder than the tip is deep or wide.
        let apex_radius = preset.apex_radius.min(depth * 0.8).min(half * 0.8);
        let mut angle = preset.angle;
        let mut half_base = Self::half_base_for(angle, apex_radius, depth);
        if half_base > half {
            // Narrower than the measured flanks allow: steeper flanks from the rectangle's
            // corners, still reaching `depth`.
            half_base = half;
            angle = Self::angle_for(half_base, apex_radius, depth);
        }
        let fillet_radius = ((half - half_base) / (angle / 2.0).tan()).max(0.0);
        Self {
            half_base,
            angle,
            apex_radius,
            fillet_radius,
        }
    }

    /// The flank angle that gives `depth` from a given half base (the depth grows with the
    /// angle while the apex radius stays under the half base): bisection.
    fn angle_for(half_base: f64, apex_radius: f64, depth: f64) -> f64 {
        let reach = |a: f64| half_base * a.tan() - apex_radius * (1.0 / a.cos() - 1.0);
        let (mut low, mut high) = (0.01_f64, 89.0_f64.to_radians());
        for _ in 0..60 {
            let mid = 0.5 * (low + high);
            if reach(mid) < depth {
                low = mid;
            } else {
                high = mid;
            }
        }
        0.5 * (low + high)
    }

    /// For the shader, in physical pixels: half base, tan(angle), apex and fillet radii.
    fn uniform(&self, scale: f64) -> [f32; 4] {
        [
            (self.half_base * scale) as f32,
            self.angle.tan() as f32,
            (self.apex_radius * scale) as f32,
            (self.fillet_radius * scale) as f32,
        ]
    }
}

/// A glass region in the output, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalRegion {
    pub rect: Rectangle<i32, Physical>,
    pub radius: f64,
    /// Continuous corners (`add_continuous_region`): 0–1, how far the corner spreads along the
    /// edges; 0 is a circular arc of `radius`.
    pub smoothing: f64,
    pub material: MaterialParams,
    /// Compositing opacity (0–1): it does not change the blurred background, only the drawing.
    pub opacity: f32,
    /// Physical pixels per point.
    pub scale: f64,
    /// A tip instead of a rectangle (`add_tip`, `add_upward_tip`, `add_side_tip`).
    pub tip: Option<Tip>,
    /// Regions merge only within the same group.
    pub group: u32,
    /// The kind of surface: blur, lens, edge, tint and shadow over the material.
    pub profile: GlassProfile,
    /// Rotation about the centre of `rect`, radians clockwise on screen (`set_rotation`):
    /// `rect` is the rectangle before it.
    pub rotation: f64,
}

impl PhysicalRegion {
    /// The bounding box of the region as drawn: `rect`, or the box around it once rotated.
    pub fn bounds(&self) -> Rectangle<i32, Physical> {
        if self.rotation == 0.0 {
            return self.rect;
        }
        let (sin, cos) = self.rotation.sin_cos();
        let (w, h) = (f64::from(self.rect.size.w), f64::from(self.rect.size.h));
        let cx = f64::from(self.rect.loc.x) + w * 0.5;
        let cy = f64::from(self.rect.loc.y) + h * 0.5;
        let half_w = 0.5 * (cos.abs() * w + sin.abs() * h);
        let half_h = 0.5 * (sin.abs() * w + cos.abs() * h);
        let (left, top) = ((cx - half_w).floor(), (cy - half_h).floor());
        let (right, bottom) = ((cx + half_w).ceil(), (cy + half_h).ceil());
        Rectangle::new(
            (left as i32, top as i32).into(),
            ((right - left) as i32, (bottom - top) as i32).into(),
        )
    }

    /// σ of the blur in points: the profile's, or the material's.
    pub fn sigma(&self) -> f64 {
        self.profile.sigma.unwrap_or(self.material.sigma)
    }

    /// What the blurred texture depends on.
    fn blur_key(&self) -> BlurKey {
        BlurKey {
            sigma: self.sigma(),
            sharp: self.material.sharp,
            factor: self.profile.blur_scale.factor(self.scale),
        }
    }

    /// How far the region draws beyond its shape, in pixels: the merging bulge and the shadow.
    fn bleed(&self) -> i32 {
        let bulge = f64::from(self.material.merge_pt) * 0.5;
        let shadow = f64::from(self.profile.shadow.reach());
        ((bulge + shadow) * self.scale).ceil() as i32 + 1
    }

    /// Same shape and same material: the blurred background can be reused even if the
    /// opacity changed (a fade does not redo the blur on every frame).
    fn same_shape(&self, other: &Self) -> bool {
        self.rect == other.rect
            && self.radius == other.radius
            && self.smoothing == other.smoothing
            && self.material == other.material
            && self.scale == other.scale
            && self.tip == other.tip
            && self.group == other.group
            && self.profile == other.profile
            && self.rotation == other.rotation
    }
}

/// The blur of a texture: σ in points, the sharp share and the resolution factor.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BlurKey {
    sigma: f64,
    sharp: f32,
    factor: u32,
}

/// One element behind the glass as the blurred texture saw it: everything the damage tracker
/// compares to decide that an element must be redrawn.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<i32, Physical>,
    src: Rectangle<f64, Buffer>,
    transform: Transform,
    alpha: f32,
}

impl Seen {
    fn of<E: Element>(element: &E, scale: f64) -> Self {
        Self {
            id: element.id().clone(),
            commit: element.current_commit(),
            geometry: element.geometry(Scale::from(scale)),
            src: element.src(),
            transform: element.transform(),
            alpha: element.alpha(),
        }
    }
}

/// The elements of `behind` that reach into `bbox`, from the front, with what they show.
fn seen_in<E: Element>(
    behind: &[E],
    bbox: Rectangle<i32, Physical>,
    scale: f64,
) -> Vec<(Seen, &E)> {
    behind
        .iter()
        .filter(|element| element.geometry(Scale::from(scale)).overlaps(bbox))
        .map(|element| (Seen::of(element, scale), element))
        .collect()
}

/// Whether the pixels behind the glass inside `bbox` are still the ones the texture was made
/// from: the same elements in the same order, at the same place, with the same source,
/// transform and alpha, and any new commit of theirs damaging only outside `bbox`. Anything
/// else (an element that moved, appeared, went away or changed under the glass) means
/// recomputing.
fn unchanged<E: Element>(
    before: &[Seen],
    now: &[(Seen, &E)],
    bbox: Rectangle<i32, Physical>,
    scale: f64,
) -> bool {
    before.len() == now.len()
        && before.iter().zip(now).all(|(old, (new, element))| {
            old.id == new.id
                && old.geometry == new.geometry
                && old.src == new.src
                && old.transform == new.transform
                && old.alpha.to_bits() == new.alpha.to_bits()
                && (old.commit == new.commit
                    || element
                        .damage_since(Scale::from(scale), Some(old.commit))
                        .iter()
                        .all(|rect| {
                            !Rectangle::new(rect.loc + new.geometry.loc, rect.size).overlaps(bbox)
                        }))
        })
}

/// The glass of a surface between frames (in the surface data).
#[derive(Default)]
pub struct GlassCache {
    state: RefCell<Option<Cached>>,
}

struct Cached {
    quality: GlassQuality,
    /// What was behind, element by element, when the textures were made.
    seen: Vec<Seen>,
    /// The output scale and the color under everything the textures were made with.
    scale: f64,
    clear_color: [f32; 4],
    regions: Vec<PhysicalRegion>,
    bbox: Rectangle<i32, Physical>,
    /// One blurred texture per blur present, in the order of the regions.
    textures: Vec<(BlurKey, GlesTexture)>,
    ids: Vec<Id>,
    commit: CommitCounter,
}

/// The glass programs.
#[derive(Clone)]
pub struct GlassPrograms {
    pub blur: GlesTexProgram,
    pub material: GlesTexProgram,
    /// The box prefilter of the reduced-resolution blur.
    pub downsample: GlesTexProgram,
    /// The reduced blur drawn back at full size.
    pub upsample: GlesTexProgram,
}

impl GlassCache {
    /// The glass elements for these regions, given what is behind (`behind`, from the front
    /// to the back) and its fixed part (`behind_static`: the desktop wallpaper, for the
    /// frosted level). Error if the renderer cannot draw offscreen: the caller skips the glass
    /// for this frame.
    #[allow(clippy::too_many_arguments)]
    pub fn elements<E: RenderElement<GlesRenderer>>(
        &self,
        renderer: &mut GlesRenderer,
        programs: &GlassPrograms,
        regions: &[PhysicalRegion],
        behind: &[E],
        behind_static: &[E],
        quality: GlassQuality,
        output: Rectangle<i32, Physical>,
        scale: f64,
        clear_color: [f32; 4],
    ) -> Result<Vec<GlassElement>, GlesError> {
        let behind = match quality {
            GlassQuality::Full => behind,
            GlassQuality::Frosted => behind_static,
            GlassQuality::Opaque => behind.get(..0).unwrap_or_default(),
        };
        let margin = regions
            .iter()
            .map(|region| {
                let reach = f64::from(
                    region.material.lens.outward_reach()
                        + region.material.merge_pt
                        + region.profile.chromatic * region.material.lens.inward_reach()
                        + region.profile.shadow.reach(),
                );
                ((region.sigma() * 3.0 + reach) * scale).ceil() as i32 + 2
            })
            .max()
            .unwrap_or(0);
        let Some(bbox) = regions
            .iter()
            .map(PhysicalRegion::bounds)
            .reduce(|a, b| a.merge(b))
            .map(|rect| {
                Rectangle::new(
                    rect.loc - Point::from((margin, margin)),
                    rect.size + Size::from((margin.saturating_mul(2), margin.saturating_mul(2))),
                )
            })
            .and_then(|rect| rect.intersection(output))
        else {
            return Ok(Vec::new());
        };
        let now = seen_in(behind, bbox, scale);

        // The textures are reused only if nothing they were made from changed: the elements
        // behind (where the glass reads), the glass's own shapes and materials, the quality,
        // the scale and the clear color. When in doubt they are made again.
        let mut state = self.state.borrow_mut();
        let reuse = state.as_ref().is_some_and(|cached| {
            cached.quality == quality
                && cached.scale.to_bits() == scale.to_bits()
                && cached.clear_color.map(f32::to_bits) == clear_color.map(f32::to_bits)
                && unchanged(&cached.seen, &now, bbox, scale)
                && cached.regions.len() == regions.len()
                && cached
                    .regions
                    .iter()
                    .zip(regions)
                    .all(|(a, b)| a.same_shape(b))
                && cached.bbox == bbox
        });
        let seen: Vec<Seen> = now.into_iter().map(|(seen, _)| seen).collect();
        if reuse && let Some(cached) = state.as_mut() {
            // New commits that damaged only outside the glass: the next comparison starts from
            // them.
            cached.seen = seen.clone();
            // Only the opacity changed: same textures, new elements to redraw.
            if cached.regions != regions {
                cached.regions = regions.to_vec();
                cached.commit.increment();
            }
        }
        if !reuse {
            let sharp = render_to_texture(renderer, behind, bbox, scale, clear_color)?;
            let mut textures: Vec<(BlurKey, GlesTexture)> = Vec::new();
            for region in regions {
                let key = region.blur_key();
                if textures.iter().any(|(k, _)| *k == key) {
                    continue;
                }
                let blurred = blur(renderer, programs, &sharp, bbox.size, key, scale)?;
                textures.push((key, blurred));
            }
            let count = groups(regions).len();
            let (ids, mut commit) = match state.take() {
                Some(cached) if cached.ids.len() == count => (cached.ids, cached.commit),
                Some(cached) => ((0..count).map(|_| Id::new()).collect(), cached.commit),
                None => (
                    (0..count).map(|_| Id::new()).collect(),
                    CommitCounter::default(),
                ),
            };
            commit.increment();
            *state = Some(Cached {
                quality,
                seen,
                scale,
                clear_color,
                regions: regions.to_vec(),
                bbox,
                textures,
                ids,
                commit,
            });
        }
        let Some(cached) = state.as_ref() else {
            return Ok(Vec::new());
        };
        Ok(groups(&cached.regions)
            .into_iter()
            .zip(&cached.ids)
            .filter_map(|(shapes, id)| {
                let key = shapes.first()?.blur_key();
                let texture = cached.textures.iter().find(|(k, _)| *k == key)?.1.clone();
                // The smooth union goes out of the regions by at most k / 4 = merge_pt / 2,
                // the shadow by its reach.
                let bulge = shapes.first()?.bleed();
                let area = shapes
                    .iter()
                    .map(PhysicalRegion::bounds)
                    .reduce(|a, b| a.merge(b))?;
                let area = Rectangle::new(
                    area.loc - Point::from((bulge, bulge)),
                    area.size + Size::from((bulge.saturating_mul(2), bulge.saturating_mul(2))),
                )
                .intersection(cached.bbox)?;
                Some(GlassElement {
                    id: id.clone(),
                    commit: cached.commit,
                    texture,
                    bbox: cached.bbox,
                    area,
                    shapes,
                    program: programs.material.clone(),
                    fallback: cached.quality == GlassQuality::Opaque,
                })
            })
            .collect())
    }
}

/// The regions grouped by material, profile and client group (in the order they appear), at most
/// `MAX_SHAPES` per group: the regions of a group merge with each other.
fn groups(regions: &[PhysicalRegion]) -> Vec<Vec<PhysicalRegion>> {
    let mut groups: Vec<Vec<PhysicalRegion>> = Vec::new();
    for region in regions {
        match groups.iter_mut().find(|group| {
            group.len() < MAX_SHAPES
                && group.first().is_some_and(|first| {
                    first.material == region.material
                        && first.profile == region.profile
                        && first.opacity == region.opacity
                        && first.group == region.group
                })
        }) {
            Some(group) => group.push(*region),
            None => groups.push(vec![*region]),
        }
    }
    groups
}

pub(crate) fn buffer_size(size: Size<i32, Physical>) -> Size<i32, Buffer> {
    size.to_logical(1).to_buffer(1, Transform::Normal)
}

/// Draws `elements` (from the front to the back) over `clear_color` into a new texture
/// covering `bbox` of the output: the glass background, the snapshot of a closing
/// window.
pub fn render_to_texture<E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    elements: &[E],
    bbox: Rectangle<i32, Physical>,
    scale: f64,
    clear_color: [f32; 4],
) -> Result<GlesTexture, GlesError> {
    let mut texture = renderer.create_buffer(Fourcc::Abgr8888, buffer_size(bbox.size))?;
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut frame = renderer.render(&mut target, bbox.size, Transform::Normal)?;
        frame.clear(clear_color.into(), &[Rectangle::from_size(bbox.size)])?;
        for element in elements.iter().rev() {
            let geometry = element.geometry(Scale::from(scale));
            let Some(visible) = geometry.intersection(bbox) else {
                continue;
            };
            let dst = Rectangle::new(geometry.loc - bbox.loc, geometry.size);
            let damage = [Rectangle::new(visible.loc - geometry.loc, visible.size)];
            element.draw(&mut frame, element.src(), dst, &damage, &[])?;
        }
        // Same GL context for whoever uses the texture: the order of the commands is enough.
        let _sync = frame.finish()?;
    }
    Ok(texture)
}

/// One pass: the part `src` of `source` drawn into a new 8-bit texture `to` large, over
/// `under` (as large as `to`) if present.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pass(
    renderer: &mut GlesRenderer,
    source: &GlesTexture,
    src: Rectangle<f64, Buffer>,
    to: Size<i32, Physical>,
    program: Option<&GlesTexProgram>,
    uniforms: &[Uniform<'_>],
    alpha: f32,
    under: Option<&GlesTexture>,
) -> Result<GlesTexture, GlesError> {
    pass_in(
        renderer,
        Fourcc::Abgr8888,
        source,
        src,
        to,
        program,
        uniforms,
        alpha,
        under,
    )
}

/// `pass` into a texture of `format`.
#[allow(clippy::too_many_arguments)]
fn pass_in(
    renderer: &mut GlesRenderer,
    format: Fourcc,
    source: &GlesTexture,
    src: Rectangle<f64, Buffer>,
    to: Size<i32, Physical>,
    program: Option<&GlesTexProgram>,
    uniforms: &[Uniform<'_>],
    alpha: f32,
    under: Option<&GlesTexture>,
) -> Result<GlesTexture, GlesError> {
    let mut texture = renderer.create_buffer(format, buffer_size(to))?;
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut frame = renderer.render(&mut target, to, Transform::Normal)?;
        let full = [Rectangle::from_size(to)];
        frame.clear([0.0, 0.0, 0.0, 0.0].into(), &full)?;
        if let Some(under) = under {
            frame.render_texture_from_to(
                under,
                whole(to),
                Rectangle::from_size(to),
                &full,
                &[],
                Transform::Normal,
                1.0,
                None,
                &[],
            )?;
        }
        frame.render_texture_from_to(
            source,
            src,
            Rectangle::from_size(to),
            &full,
            &[],
            Transform::Normal,
            alpha,
            program,
            uniforms,
        )?;
        // Same GL context for whoever uses the texture: the order of the commands is enough.
        let _sync = frame.finish()?;
    }
    Ok(texture)
}

/// The whole of a texture `size` large, as a source rectangle.
pub(crate) fn whole(size: Size<i32, Physical>) -> Rectangle<f64, Buffer> {
    Rectangle::from_size(buffer_size(size).to_f64())
}

/// What lies past the edges of a blurred capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlurEdge {
    /// The edge pixels repeat (the glass: a capture cut by the output's edge).
    Clamp,
    /// The capture is mirrored (the spaces bar, missioncontrol.md › MC-BLUR).
    Mirror,
}

/// A blurred texture, possibly smaller than its source.
pub(crate) struct Blurred {
    pub texture: GlesTexture,
    /// The size of `texture`: smaller than the source if the blur was reduced.
    pub size: Size<i32, Physical>,
    /// The part of `texture` that covers the source, to draw it back at full size.
    pub src: Rectangle<f64, Buffer>,
}

impl Blurred {
    /// Drawn into a new texture `size` large (the source's), with `alpha` over `under`: the
    /// reduced texture through the upsampling program, a full-size one as it is.
    pub(crate) fn back(
        &self,
        renderer: &mut GlesRenderer,
        upsample: &GlesTexProgram,
        size: Size<i32, Physical>,
        alpha: f32,
        under: Option<&GlesTexture>,
    ) -> Result<GlesTexture, GlesError> {
        if self.size == size {
            return pass(
                renderer,
                &self.texture,
                self.src,
                size,
                None,
                &[],
                alpha,
                under,
            );
        }
        let uniforms = [Uniform::new(
            "tex_size",
            UniformValue::_2f(self.size.w as f32, self.size.h as f32),
        )];
        pass(
            renderer,
            &self.texture,
            self.src,
            size,
            Some(upsample),
            &uniforms,
            alpha,
            under,
        )
    }
}

/// The factor actually used for a blur of σ `sigma` pixels at `factor` (1, 2, 4 or 8): halved
/// while the reduced σ would fall under `MIN_REDUCED_SIGMA_PX`.
pub fn effective_factor(sigma: f64, factor: u32) -> u32 {
    let mut factor = match factor {
        0..=1 => 1,
        2..=3 => 2,
        4..=7 => 4,
        _ => 8,
    };
    while factor > 1 && reduced_sigma(sigma, factor) < MIN_REDUCED_SIGMA_PX {
        factor /= 2;
    }
    factor
}

/// σ in pixels of the texture reduced by `factor`. Averaging over f × f pixels adds
/// (f² − 1) / 12 pixels² of variance, the bilinear upsampling f² / 4 − (f² − 1) / 12 on
/// average over the phases: f² / 4 together, taken out of σ² so that the total spread is the
/// full-resolution one.
fn reduced_sigma(sigma: f64, factor: u32) -> f64 {
    if factor <= 1 {
        return sigma;
    }
    let f = f64::from(factor);
    (sigma * sigma - f * f / 4.0).max(0.0).sqrt() / f
}

/// The format of the reduced textures: half floats, so that the extra passes of a reduced
/// blur (the prefilter) add no 8-bit rounding of their own; the full-resolution blur keeps
/// 8 bits, as it always had.
const REDUCED_FORMAT: Fourcc = Fourcc::Abgr16161616f;

/// A separable Gaussian of σ `sigma` pixels over `source` (`size` large) at 1/`factor` of the
/// resolution (`effective_factor`): a box prefilter down to ceil(size / factor), then the two
/// blur passes there with the reduced σ, in half floats. At factor 1, or where the renderer
/// cannot draw into half floats, it is the plain full-resolution blur.
#[allow(clippy::too_many_arguments)]
pub(crate) fn gaussian(
    renderer: &mut GlesRenderer,
    blur: &GlesTexProgram,
    downsample: &GlesTexProgram,
    edge: BlurEdge,
    source: &GlesTexture,
    size: Size<i32, Physical>,
    sigma: f64,
    factor: u32,
) -> Result<Blurred, GlesError> {
    let factor = effective_factor(sigma, factor);
    if factor > 1 {
        match reduced(
            renderer, blur, downsample, edge, source, size, sigma, factor,
        ) {
            Ok(blurred) => return Ok(blurred),
            // When in doubt, full resolution.
            Err(err) => tracing::debug!("reduced blur unavailable, full resolution: {err:?}"),
        }
    }
    let texture = blur_passes(
        renderer,
        blur,
        source.clone(),
        size,
        sigma as f32,
        Fourcc::Abgr8888,
    )?;
    Ok(Blurred {
        texture,
        size,
        src: whole(size),
    })
}

/// The reduced blur of `gaussian`, `factor` > 1.
#[allow(clippy::too_many_arguments)]
fn reduced(
    renderer: &mut GlesRenderer,
    blur: &GlesTexProgram,
    downsample: &GlesTexProgram,
    edge: BlurEdge,
    source: &GlesTexture,
    size: Size<i32, Physical>,
    sigma: f64,
    factor: u32,
) -> Result<Blurred, GlesError> {
    let f = factor as i32;
    let sigma_small = reduced_sigma(sigma, factor);
    // A border as wide as the reduced kernel around the source: there the reduced texture
    // holds the source extended past its edge as the full-resolution blur extends it, rather
    // than a box mean that a single edge column (a line at the screen's edge) would not match.
    let pad = (3.0 * sigma_small).ceil() as i32 + 1;
    let small = Size::from((
        (size.w.max(1) + f - 1) / f + 2 * pad,
        (size.h.max(1) + f - 1) / f + 2 * pad,
    ));
    let uniforms = [
        Uniform::new("src_size", UniformValue::_2f(size.w as f32, size.h as f32)),
        Uniform::new(
            "out_size",
            UniformValue::_2f(small.w as f32, small.h as f32),
        ),
        Uniform::new("factor", UniformValue::_1f(factor as f32)),
        Uniform::new("pad", UniformValue::_1f(pad as f32)),
        Uniform::new(
            "mirror",
            UniformValue::_1f(if edge == BlurEdge::Mirror { 1.0 } else { 0.0 }),
        ),
    ];
    let prefiltered = pass_in(
        renderer,
        REDUCED_FORMAT,
        source,
        whole(size),
        small,
        Some(downsample),
        &uniforms,
        1.0,
        None,
    )?;
    let texture = blur_passes(
        renderer,
        blur,
        prefiltered,
        small,
        sigma_small as f32,
        REDUCED_FORMAT,
    )?;
    // The source covers size / factor of the reduced texture, inside the border.
    let src = Rectangle::new(
        Point::from((f64::from(pad), f64::from(pad))),
        Size::from((
            f64::from(size.w) / f64::from(factor),
            f64::from(size.h) / f64::from(factor),
        )),
    );
    Ok(Blurred {
        texture,
        size: small,
        src,
    })
}

/// The horizontal and vertical passes of a Gaussian of σ `sigma` texture pixels over
/// `texture` (`size` large), into textures of `format`.
fn blur_passes(
    renderer: &mut GlesRenderer,
    blur: &GlesTexProgram,
    texture: GlesTexture,
    size: Size<i32, Physical>,
    sigma: f32,
    format: Fourcc,
) -> Result<GlesTexture, GlesError> {
    let mut current = texture;
    for step in [(1.0 / size.w as f32, 0.0), (0.0, 1.0 / size.h as f32)] {
        let uniforms = [
            Uniform::new("step", UniformValue::_2f(step.0, step.1)),
            Uniform::new("sigma", UniformValue::_1f(sigma)),
        ];
        current = pass_in(
            renderer,
            format,
            &current,
            whole(size),
            size,
            Some(blur),
            &uniforms,
            1.0,
            None,
        )?;
    }
    Ok(current)
}

/// Blurs the capture with the σ and mixes in the sharp share.
fn blur(
    renderer: &mut GlesRenderer,
    programs: &GlassPrograms,
    sharp: &GlesTexture,
    size: Size<i32, Physical>,
    key: BlurKey,
    scale: f64,
) -> Result<GlesTexture, GlesError> {
    let blurred = gaussian(
        renderer,
        &programs.blur,
        &programs.downsample,
        BlurEdge::Clamp,
        sharp,
        size,
        key.sigma * scale,
        key.factor,
    )?;
    // Back to full resolution, with the sharp share below: w · sharp + (1 − w) · blurred.
    blurred.back(
        renderer,
        &programs.upsample,
        size,
        1.0 - key.sharp,
        Some(sharp),
    )
}

/// A color with alpha as a `vec4` uniform.
fn rgba([r, g, b, a]: [f32; 4]) -> UniformValue {
    UniformValue::_4f(r, g, b, a)
}

/// A glass region ready to draw.
#[derive(Debug, Clone)]
pub struct GlassElement {
    id: Id,
    commit: CommitCounter,
    /// The blurred background, as large as `bbox`.
    texture: GlesTexture,
    bbox: Rectangle<i32, Physical>,
    /// Where it is drawn: the regions of the group plus the merging room.
    area: Rectangle<i32, Physical>,
    /// The regions of the group (same material, at most `MAX_SHAPES`).
    shapes: Vec<PhysicalRegion>,
    program: GlesTexProgram,
    /// No blur (the opaque quality level): the tint over the profile's fallback surface.
    fallback: bool,
}

impl Element for GlassElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let local = Rectangle::new(self.area.loc - self.bbox.loc, self.area.size);
        Rectangle::new(
            Point::from((f64::from(local.loc.x), f64::from(local.loc.y))),
            buffer_size(local.size).to_f64(),
        )
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.area
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for GlassElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let Some(first) = self.shapes.first() else {
            return Ok(());
        };
        let m = first.material.matrix;
        let b = first.material.offset;
        let [q2, q3] = first.material.curve;
        let lens = first.material.lens;
        let edge = first.material.edge;
        let profile = first.profile;
        let px = first.scale as f32;
        // The deepest the lens samples, for its width on each shape (`lens_width`).
        let lens_reach_px = lens.inward_reach() * px;
        let mut lens_widths = [profile.falloff; 16];
        // The regions relative to the texture: up to 8 rectangles in two mat4 (one column per
        // rectangle: x, y, width, height) and the radii in a mat4.
        let mut rects = [[0.0_f32; 16]; 2];
        let mut radii = [0.0_f32; 16];
        // The tips' outlines (TIP-01), one column per region: half base, tan(angle), apex and
        // fillet radii in pixels.
        let mut tips = [[0.0_f32; 16]; 2];
        // Each region's rotation about its centre, (cos, sin) two per column: region 2j in
        // xy, 2j + 1 in zw.
        let mut rotations = [0.0_f32; 16];
        for (i, shape) in self.shapes.iter().take(MAX_SHAPES).enumerate() {
            let (sin, cos) = shape.rotation.sin_cos();
            let at = (i / 2) * 4 + (i % 2) * 2;
            if let Some(slot) = rotations.get_mut(at..at + 2) {
                slot.copy_from_slice(&[cos as f32, sin as f32]);
            }
            let local = shape.rect.loc - self.bbox.loc;
            let values = [
                local.x as f32,
                local.y as f32,
                shape.rect.size.w as f32,
                shape.rect.size.h as f32,
            ];
            if let Some(slot) = rects
                .get_mut(i / 4)
                .and_then(|matrix| matrix.get_mut((i % 4) * 4..(i % 4) * 4 + 4))
            {
                slot.copy_from_slice(&values);
            }
            // Negative radius: a tip (the shader recognizes it by the sign): −1 points down,
            // −2 up, −3 left, −4 right.
            if let Some(tip) = shape.tip {
                let (along, away) = match tip {
                    Tip::Down | Tip::Up => (shape.rect.size.w, shape.rect.size.h),
                    Tip::Left | Tip::Right => (shape.rect.size.h, shape.rect.size.w),
                };
                let scale = shape.scale.max(0.01);
                let profile = TipProfile::fit(f64::from(along) / scale, f64::from(away) / scale);
                if let Some(slot) = tips
                    .get_mut(i / 4)
                    .and_then(|matrix| matrix.get_mut((i % 4) * 4..(i % 4) * 4 + 4))
                {
                    slot.copy_from_slice(&profile.uniform(scale));
                }
            }
            // A rounded shape carries its smoothing in the (otherwise unused) tip column.
            if shape.tip.is_none()
                && let Some(slot) = tips
                    .get_mut(i / 4)
                    .and_then(|matrix| matrix.get_mut((i % 4) * 4..(i % 4) * 4 + 1))
            {
                slot.copy_from_slice(&[shape.smoothing.clamp(0.0, 1.0) as f32]);
            }
            let thickness = shape.rect.size.w.min(shape.rect.size.h) as f32;
            if let Some(slot) = lens_widths.get_mut(i) {
                *slot = if profile.depth > 0.0 {
                    refraction_band(thickness / px.max(0.01), profile.depth)
                } else {
                    lens_width(thickness, lens_reach_px, profile.falloff)
                };
            }
            if let Some(slot) = radii.get_mut(i) {
                *slot = match shape.tip {
                    None => shape.radius as f32,
                    Some(Tip::Down) => -1.0,
                    Some(Tip::Up) => -2.0,
                    Some(Tip::Left) => -3.0,
                    Some(Tip::Right) => -4.0,
                };
            }
        }
        let uniforms = [
            Uniform::new(
                "tex_size",
                UniformValue::_2f(self.bbox.size.w as f32, self.bbox.size.h as f32),
            ),
            Uniform::new(
                "shapes",
                UniformValue::Matrix4x4 {
                    matrices: rects.to_vec(),
                    transpose: false,
                },
            ),
            Uniform::new(
                "radii",
                UniformValue::Matrix4x4 {
                    matrices: vec![radii],
                    transpose: false,
                },
            ),
            Uniform::new(
                "rotations",
                UniformValue::Matrix4x4 {
                    matrices: vec![rotations],
                    transpose: false,
                },
            ),
            Uniform::new(
                "tips",
                UniformValue::Matrix4x4 {
                    matrices: tips.to_vec(),
                    transpose: false,
                },
            ),
            Uniform::new(
                "shape_count",
                UniformValue::_1f(self.shapes.len().min(MAX_SHAPES) as f32),
            ),
            Uniform::new(
                "merge",
                UniformValue::_1f(2.0 * first.material.merge_pt * px),
            ),
            Uniform::new("exponent", UniformValue::_1f(2.0)),
            Uniform::new("row_r", UniformValue::_3f(m[0][0], m[0][1], m[0][2])),
            Uniform::new("row_g", UniformValue::_3f(m[1][0], m[1][1], m[1][2])),
            Uniform::new("row_b", UniformValue::_3f(m[2][0], m[2][1], m[2][2])),
            Uniform::new("offset", UniformValue::_3f(b[0], b[1], b[2])),
            Uniform::new("curve2", UniformValue::_3f(q2[0], q2[1], q2[2])),
            Uniform::new("curve3", UniformValue::_3f(q3[0], q3[1], q3[2])),
            Uniform::new("scale_px", UniformValue::_1f(px)),
            Uniform::new("lens", lens.uniform(px)),
            Uniform::new("lens_reach", UniformValue::_1f(lens_extent(&lens) * px)),
            Uniform::new("edges", edge.uniform()),
            Uniform::new(
                "fill_gray",
                UniformValue::_1f(first.material.fill_on_gray_with(&profile)),
            ),
            Uniform::new("edge_power", UniformValue::_1f(EDGE_POWER)),
            Uniform::new(
                "lens_widths",
                UniformValue::Matrix4x4 {
                    matrices: vec![lens_widths],
                    transpose: false,
                },
            ),
            Uniform::new("refraction", UniformValue::_1f(profile.refraction)),
            Uniform::new("chromatic", UniformValue::_1f(profile.chromatic)),
            Uniform::new("brightness", UniformValue::_1f(profile.brightness)),
            Uniform::new("rim", UniformValue::_1f(profile.rim)),
            Uniform::new("line", UniformValue::_1f(profile.line)),
            Uniform::new("saturation", UniformValue::_1f(profile.saturation)),
            Uniform::new("tint_color", rgba(profile.tint)),
            Uniform::new(
                "shadow",
                UniformValue::_3f(
                    profile.shadow.opacity,
                    profile.shadow.sigma * px,
                    profile.shadow.offset_y * px,
                ),
            ),
            Uniform::new(
                "edge_on",
                UniformValue::_1f(if edge == Edge::NONE { 0.0 } else { 1.0 }),
            ),
            Uniform::new("inside_rim", rgba(profile.inside_rim.color)),
            Uniform::new(
                "inside_rim_width",
                UniformValue::_1f(profile.inside_rim.width * px),
            ),
            Uniform::new("rim_light", rgba(profile.rim_light.color)),
            Uniform::new(
                "rim_light_shape",
                UniformValue::_2f(
                    profile.rim_light.offset_y * px,
                    profile.rim_light.sigma * px,
                ),
            ),
            Uniform::new(
                "fallback",
                UniformValue::_4f(
                    profile.fallback[0],
                    profile.fallback[1],
                    profile.fallback[2],
                    if self.fallback { 1.0 } else { 0.0 },
                ),
            ),
        ];
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            first.opacity.clamp(0.0, 1.0),
            Some(&self.program),
            &uniforms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn the_side_edge_fades_around_a_capsule_end_like_the_modules() {
        // Angle from the side (0°) towards the top (90°), outward normal (−cos, −sin) on a
        // left end: the measured dark line ~0.7 at 15°, ~0.5 at 30°, ≤ 0.2 at 45°.
        let side_at = |deg: f32| {
            let a = deg.to_radians();
            edge_weights((-a.cos(), -a.sin()))[1]
        };
        assert!((side_at(0.0) - 1.0).abs() < 1e-6);
        assert!((0.75..0.9).contains(&side_at(15.0)), "{}", side_at(15.0));
        assert!((0.4..0.6).contains(&side_at(30.0)), "{}", side_at(30.0));
        assert!(side_at(45.0) < 0.2, "{}", side_at(45.0));
        assert!(side_at(60.0) < 0.05, "{}", side_at(60.0));
        // The top row has the same falloff towards the side.
        let top_at = |deg: f32| {
            let a = deg.to_radians();
            edge_weights((-a.cos(), -a.sin()))[0]
        };
        assert!((top_at(60.0) - side_at(30.0)).abs() < 1e-6);
        // The weights and the shared part make one, on every direction.
        for deg in (0..360).step_by(5) {
            let a = (deg as f32).to_radians();
            let w = edge_weights((a.cos(), a.sin()));
            let total: f32 = w.iter().sum();
            assert!((total - 1.0).abs() < 1e-5, "{deg}°: {w:?}");
            assert!(w.iter().all(|v| (0.0..=1.0).contains(v)), "{deg}°: {w:?}");
        }
        // A degenerate normal (the middle of a shape) counts as the top.
        assert_eq!(edge_weights((0.0, 0.0))[0], 1.0);
    }

    /// A point at `(x, y)` in the upright frame of `rect` (from its centre), on screen once
    /// the rectangle is rotated by `rotation` about its centre.
    fn on_screen(rect: [f32; 4], rotation: f32, (x, y): (f32, f32)) -> (f32, f32) {
        let (sin, cos) = rotation.sin_cos();
        let (cx, cy) = (rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5);
        (cx + cos * x - sin * y, cy + sin * x + cos * y)
    }

    #[test]
    fn a_rotated_mask_follows_the_turned_rectangle() {
        // A fan label: 120 × 28 capsule-ish rectangle (radius 6), turned 12° clockwise.
        let rect = [40.0, 60.0, 120.0, 28.0];
        let angle = 12.0_f32.to_radians();
        let distance = |p| rotated_rounded_distance(p, rect, 6.0, angle);
        // Just inside each rotated corner (2 px in along both of its own axes, past the
        // rounding): inside the mask.
        for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let near_corner = on_screen(rect, angle, (sx * 57.0, sy * 9.0));
            assert!(distance(near_corner) < 0.0, "{near_corner:?} is outside");
            // 3 px outside the rotated side next to it: outside, at the right distance.
            let out = on_screen(rect, angle, (0.0, sy * 17.0));
            assert!((distance(out) - 3.0).abs() < 1e-3, "{}", distance(out));
        }
        // The upright top-right and bottom-left corners are no longer covered: the turned
        // rectangle swung away from them.
        assert!(distance((rect[0] + rect[2] - 1.0, rect[1] + 1.0)) > 0.0);
        assert!(distance((rect[0] + 1.0, rect[1] + rect[3] - 1.0)) > 0.0);
        // Without a rotation it is the upright rectangle.
        let upright = |p| rotated_rounded_distance(p, rect, 6.0, 0.0);
        assert!(upright((rect[0] + rect[2] - 7.0, rect[1] + 1.0)) < 0.0);
        assert!((upright((100.0, 57.0)) - 3.0).abs() < 1e-4);
        // A turn by a half circle is the same rectangle.
        let half = |p| rotated_rounded_distance(p, rect, 6.0, std::f32::consts::PI);
        for p in [(41.0, 61.0), (100.0, 74.0), (30.0, 74.0)] {
            assert!((half(p) - upright(p)).abs() < 1e-3, "{p:?}");
        }
    }

    #[test]
    fn a_rotated_region_keeps_its_edge_light_on_its_own_top_edge() {
        for degrees in [-30.0_f32, -8.0, 1.6, 12.58, 45.0, 90.0] {
            let angle = degrees.to_radians();
            // The outward normal of the region's own top edge, on screen.
            let (sin, cos) = angle.sin_cos();
            let top = (sin, -cos);
            let w = edge_weights_in_frame(top, angle);
            assert!((w[0] - 1.0).abs() < 1e-5, "{degrees}°: top light {w:?}");
            // Its own side and bottom edges keep their profiles too.
            let side = edge_weights_in_frame((cos, sin), angle);
            assert!((side[1] - 1.0).abs() < 1e-5, "{degrees}°: side {side:?}");
            let bottom = edge_weights_in_frame((-sin, cos), angle);
            assert!(
                (bottom[2] - 1.0).abs() < 1e-5,
                "{degrees}°: bottom {bottom:?}"
            );
        }
        // Read upright, the turned top edge would lose most of its light at 12°…
        let angle = 12.0_f32.to_radians();
        let turned_top = (angle.sin(), -angle.cos());
        assert!(edge_weights(turned_top)[0] < 0.9);
        // …and with no rotation nothing changes.
        assert_eq!(
            edge_weights_in_frame((0.3, -0.8), 0.0),
            edge_weights((0.3, -0.8))
        );
    }

    #[test]
    fn a_rotated_region_is_drawn_over_its_turned_bounds() {
        let region = |rotation: f64| PhysicalRegion {
            rect: Rectangle::new((100, 50).into(), (120, 28).into()),
            radius: 14.0,
            smoothing: 0.0,
            material: MaterialParams::amalfi(16.0, 0.0, MERGE_PT, Lens::NONE),
            opacity: 1.0,
            scale: 1.0,
            tip: None,
            group: 0,
            profile: GlassProfile::NEUTRAL,
            rotation,
        };
        assert_eq!(region(0.0).bounds(), region(0.0).rect);
        let turned = region(20.0_f64.to_radians()).bounds();
        // 120 cos 20° + 28 sin 20° = 122.3 wide, 120 sin 20° + 28 cos 20° = 67.4 tall, about
        // the same centre (160, 64).
        assert!((123..=125).contains(&turned.size.w), "{turned:?}");
        assert!((68..=70).contains(&turned.size.h), "{turned:?}");
        assert!(
            (turned.loc.x + turned.size.w / 2 - 160).abs() <= 1,
            "{turned:?}"
        );
        assert!(
            (turned.loc.y + turned.size.h / 2 - 64).abs() <= 1,
            "{turned:?}"
        );
        assert!(!region(0.0).same_shape(&region(0.1)));
    }

    #[test]
    fn the_presets_give_the_measured_tips() {
        // TIP-01: depth and half width of the three outlines.
        for (profile, depth, half_width) in [
            (TipProfile::dock_menu(), 10.19, 15.29),
            (TipProfile::popover(), 10.80, 16.15),
            (TipProfile::popover_side(), 11.74, 19.45),
        ] {
            assert!(close(profile.depth(), depth, 0.01), "{profile:?}");
            assert!(close(profile.half_width(), half_width, 0.2), "{profile:?}");
        }
    }

    #[test]
    fn a_measured_size_gives_its_preset_back() {
        let dock = TipProfile::dock_menu();
        let fitted = TipProfile::fit(2.0 * dock.half_width(), dock.depth());
        assert!(close(fitted.half_base, dock.half_base, 1e-6));
        assert!(close(fitted.fillet_radius, dock.fillet_radius, 1e-6));
        let side = TipProfile::popover_side();
        let fitted = TipProfile::fit(2.0 * side.half_width(), side.depth());
        assert!(close(fitted.angle, side.angle, 1e-9));
        // A popover-sized tip on a top edge takes the popover's flanks, a Dock menu's the
        // Dock's.
        assert!(close(
            TipProfile::fit(32.0, 11.0).angle,
            TipProfile::popover().angle,
            1e-9
        ));
        assert!(close(TipProfile::fit(31.0, 10.0).angle, dock.angle, 1e-9));
        // Whole-point sizes that round a measured tip draw it exactly.
        assert_eq!(TipProfile::fit(31.0, 10.0), dock);
        assert_eq!(TipProfile::fit(32.0, 11.0), TipProfile::popover());
        assert_eq!(TipProfile::fit(39.0, 12.0), side);
        // Others are fitted: exactly as deep as the rectangle.
        assert!(close(TipProfile::fit(36.0, 14.0).depth(), 14.0, 1e-6));
    }

    #[test]
    fn any_rectangle_gives_a_tip_inside_it() {
        for (w, d) in [
            (24.0, 10.0),
            (31.0, 10.0),
            (8.0, 10.0),
            (40.0, 3.0),
            (39.0, 12.0),
        ] {
            let p = TipProfile::fit(w, d);
            assert!(
                p.fillet_radius >= 0.0 && p.apex_radius <= d,
                "{w}×{d}: {p:?}"
            );
            // Exact, or the measured outline that the whole-point size rounds.
            assert!(close(p.depth(), d, 0.5), "{w}×{d}: depth {}", p.depth());
            assert!(p.half_width() <= w / 2.0 + 0.5 + 1e-6, "{w}×{d}: {p:?}");
        }
    }

    /// The outline of `tools/body.svg` (silica-icons; a copy is in `testdata/`): sampled
    /// points of its lines and cubic curves, on the 1024 canvas.
    fn body_outline() -> Vec<(f32, f32)> {
        let svg = include_str!("../testdata/body.svg");
        let d = svg
            .split(" d=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default();
        let mut numbers = Vec::new();
        let mut ops = Vec::new();
        let mut token = String::new();
        for c in d.chars().chain(std::iter::once(' ')) {
            if c.is_ascii_alphabetic() {
                ops.push((c, numbers.len()));
            }
            if c.is_ascii_digit() || c == '.' || c == '-' {
                token.push(c);
            } else {
                if let Ok(v) = token.parse::<f32>() {
                    numbers.push(v);
                }
                token.clear();
            }
        }
        let mut points = Vec::new();
        let mut at = (0.0_f32, 0.0_f32);
        for (k, &(op, first)) in ops.iter().enumerate() {
            let end = ops.get(k + 1).map_or(numbers.len(), |next| next.1);
            let v = &numbers[first..end];
            match op {
                'M' => at = (v[0], v[1]),
                'L' => {
                    let to = (v[0], v[1]);
                    for t in 0..=20 {
                        let t = t as f32 / 20.0;
                        points.push((at.0 + (to.0 - at.0) * t, at.1 + (to.1 - at.1) * t));
                    }
                    at = to;
                }
                'C' => {
                    for c in v.as_chunks::<6>().0.iter() {
                        let (p1, p2, p3) = ((c[0], c[1]), (c[2], c[3]), (c[4], c[5]));
                        for t in 0..=200 {
                            let t = t as f32 / 200.0;
                            let u = 1.0 - t;
                            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
                            points.push((
                                w[0] * at.0 + w[1] * p1.0 + w[2] * p2.0 + w[3] * p3.0,
                                w[0] * at.1 + w[1] * p1.1 + w[2] * p2.1 + w[3] * p3.1,
                            ));
                        }
                        at = p3;
                    }
                }
                _ => {}
            }
        }
        points
    }

    /// The mask of a continuous region is the body of the icons: its edge is within 0.5 px of
    /// the outline of body.svg (radius 214, smoothing 0.6 on a side of 824) at 60 and 128 pt,
    /// and a circular corner of the same radius is not (it would leave slivers of unblurred
    /// body at the corners).
    #[test]
    fn a_continuous_region_follows_the_body_outline() {
        let outline = body_outline();
        assert!(outline.len() > 1000);
        for side in [60.0_f32, 128.0] {
            let k = side / 824.0;
            let rect = [0.0, 0.0, side, side];
            let at = |(x, y): (f32, f32)| ((x - 100.0) * k, (y - 100.0) * k);
            let mut worst = 0.0_f32;
            let mut circular = 0.0_f32;
            for p in &outline {
                let p = at(*p);
                worst = worst.max(rotated_continuous_distance(p, rect, 214.0 * k, 0.6, 0.0).abs());
                circular = circular.max(rotated_rounded_distance(p, rect, 214.0 * k, 0.0).abs());
            }
            assert!(worst < 0.5, "{side} pt: {worst} px from the body outline");
            assert!(
                circular > worst,
                "{side} pt: circular {circular} vs {worst}"
            );
            // Inside is negative, outside positive; the middle and the corner point.
            let mid = (side / 2.0, side / 2.0);
            assert!(
                rotated_continuous_distance(mid, rect, 214.0 * k, 0.6, 0.0) < -side / 2.0 + 0.5
            );
            assert!(rotated_continuous_distance((0.0, 0.0), rect, 214.0 * k, 0.6, 0.0) > 1.0);
        }
        // Without smoothing it is the circular corner.
        for p in [(1.0, 1.0), (5.0, 3.0), (20.0, 2.0), (-3.0, 40.0)] {
            let a = rotated_continuous_distance(p, [0.0, 0.0, 60.0, 60.0], 12.0, 0.0, 0.0);
            let b = rotated_rounded_distance(p, [0.0, 0.0, 60.0, 60.0], 12.0, 0.0);
            assert!((a - b).abs() < 1e-3, "{p:?}: {a} vs {b}");
        }
    }
}
