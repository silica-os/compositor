//! Glass profiles: the Amalfi materials and the settings of the glass per kind of surface
//! (silica-glass-v1 version 7).
//!
//! Silica 1.0 "Amalfi" has two glass materials, like Apple's (ui-kit `tokens.json` →
//! `materials`, `shadows.glass`, `colors.*.Glass/*`): Glass/Regular, the one glass of every
//! surface the protocol names ([`GlassKind::amalfi`]), and Glass/Clear, the nearly transparent
//! glass for overlays on media. They are drawn in the render order of the spec:
//! backdrop → blur → saturation → tint fill → inside rim → rim light (the top inner edge) →
//! drop shadow outside the shape.
//!
//! On top of the spec the compositor keeps its sharp share (`MaterialParams::sharp`): the blurred
//! backdrop is mixed with a little of the sharp one, so the glass "blurs a little and keeps part
//! of the backdrop sharp". Each kind keeps the share it had before Amalfi until the share is
//! tuned against the kit's exported images (`docs/design/amalfi/materials.md`, "Implementation
//! in the compositor").

use crate::glass::{Lens, MERGE_PT, MaterialParams};

/// The kinds of surface (`silica_glass_v1.profile`, without `default`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlassKind {
    /// Menus and context menus.
    Menu,
    /// Popovers (a bubble with a tip).
    Popover,
    /// Large panels: Notification Center, search, the launcher, an expanded module.
    Panel,
    /// Control Center modules and HUDs.
    Module,
    /// Small controls on glass: toolbar buttons, pills, round buttons.
    Control,
    /// The Dock and the app switcher.
    Dock,
    /// Tooltips and labels.
    Tooltip,
    /// Sheets and alerts.
    Sheet,
    /// Notification banners.
    Banner,
}

/// The two Amalfi glass materials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Amalfi {
    /// The glass of controls, bars, menus, popovers, panels, banners and the Dock.
    Regular,
    /// Static, nearly transparent glass for overlays on media.
    Clear,
}

/// The shadow under a glass, outside its shape (drawn by the compositor only when the client asks).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassShadow {
    /// Black alpha right under the edge, before the falloff.
    pub opacity: f32,
    /// σ of the falloff, in points.
    pub sigma: f32,
    /// How far the shadow sits below the shape, in points.
    pub offset_y: f32,
}

impl GlassShadow {
    pub const NONE: Self = Self {
        opacity: 0.0,
        sigma: 0.0,
        offset_y: 0.0,
    };

    /// How far it reaches beyond the shape, in points (3σ plus the offset).
    pub fn reach(&self) -> f32 {
        if self.opacity <= 0.0 {
            return 0.0;
        }
        3.0 * self.sigma.max(0.0) + self.offset_y.abs()
    }
}

/// The inside rim: a band just inside the shape's edge filled with a color over the tint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InsideRim {
    /// sRGB 0–1 and alpha.
    pub color: [f32; 4],
    /// Width of the band, in points.
    pub width: f32,
}

impl InsideRim {
    pub const NONE: Self = Self {
        color: [0.0; 4],
        width: 0.0,
    };
}

/// The rim light: an inner shadow of a light color, the outside of the shape moved by
/// `offset_y` and blurred, drawn inside it. With a downward offset it lights the top inner edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RimLight {
    /// sRGB 0–1 and alpha.
    pub color: [f32; 4],
    /// How far the outside is moved down, in points.
    pub offset_y: f32,
    /// σ of its blur, in points.
    pub sigma: f32,
}

impl RimLight {
    pub const NONE: Self = Self {
        color: [0.0; 4],
        offset_y: 0.0,
        sigma: 0.0,
    };
}

/// The refraction of a glass material (`tokens.json` → `materials.*.refraction`): the shape bends
/// the backdrop at its edge. The compositor draws it with the shader's edge lens (see [`bend_curve`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Refraction {
    /// How far the backdrop is pulled in at the rim, as a fraction of the band's width.
    pub distortion: f32,
    /// Width of the band: this fraction of half the shorter side of the shape (at most
    /// [`REFRACTION_BAND_PT`]).
    pub depth: f32,
    /// The split of the red and blue samples at the rim, as a fraction of the band's width.
    pub chromatic: f32,
}

impl Refraction {
    pub const NONE: Self = Self {
        distortion: 0.0,
        depth: 0.0,
        chromatic: 0.0,
    };
}

/// The widest band of the refraction, in points: also the reference band the lens curves are
/// written for (a shape whose band is narrower scales them down). Bounds how far the glass
/// reads the backdrop outside its edge (`distortion` × this).
pub const REFRACTION_BAND_PT: f32 = 24.0;

/// The lens curve of a refraction with this `distortion`, for the reference band
/// [`REFRACTION_BAND_PT`], as the `(t, s)` points of [`Lens`]: a pixel `t` points inside the edge
/// samples the backdrop at `s = t - distortion × band × (1 - t / band)²`, so the backdrop is
/// pulled in towards the rim, the most at the edge and not at all at the band's inner side.
pub const fn bend_curve(distortion: f32) -> [(f32, f32); 7] {
    // Point k of 0..=6, at t = band × k / 6.
    const fn point(distortion: f32, k: u8) -> (f32, f32) {
        let u = 1.0 - k as f32 / 6.0;
        let t = REFRACTION_BAND_PT * (1.0 - u);
        (t, t - distortion * REFRACTION_BAND_PT * u * u)
    }
    [
        point(distortion, 0),
        point(distortion, 1),
        point(distortion, 2),
        point(distortion, 3),
        point(distortion, 4),
        point(distortion, 5),
        point(distortion, 6),
    ]
}

static BEND_REGULAR: [(f32, f32); 7] = bend_curve(0.30);
static BEND_CLEAR: [(f32, f32); 7] = bend_curve(0.30);

/// The band of the refraction on a shape whose smaller side is `thickness` points, relative to
/// the reference band: `depth × thickness / 2`, at most [`REFRACTION_BAND_PT`].
pub fn refraction_band(thickness: f32, depth: f32) -> f32 {
    (depth * thickness.max(0.0) * 0.5 / REFRACTION_BAND_PT).clamp(0.05, 1.0)
}

/// One Amalfi material in one appearance: everything but the sharp share (a kind's).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmalfiMaterial {
    /// σ of the background blur, in points: the kit's background blur radius, read as the σ
    /// of a CSS `blur()`.
    pub sigma: f64,
    /// Saturation of the blurred backdrop around the Rec. 709 luminance.
    pub saturation: f32,
    /// Brightness of the backdrop (a multiplier on the saturated color, before the tint).
    pub brightness: f32,
    /// Refraction at the edge.
    pub refraction: Refraction,
    /// The tint fill over the saturated backdrop (sRGB 0–1 and alpha).
    pub tint: [f32; 4],
    pub inside_rim: InsideRim,
    pub rim_light: RimLight,
    /// The drop shadow outside the shape (`shadows.glass`).
    pub shadow: GlassShadow,
    /// `Surface/Window`: what the tint covers when the glass cannot blur (reduced transparency).
    pub fallback: [f32; 3],
}

/// A `#RRGGBBAA` token as sRGB 0–1 and alpha.
const fn token(hex: u32) -> [f32; 4] {
    [
        ((hex >> 24) & 0xff) as f32 / 255.0,
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ]
}

/// A `#RRGGBB` token as sRGB 0–1.
const fn opaque(hex: u32) -> [f32; 3] {
    let [r, g, b, _] = token((hex << 8) | 0xff);
    [r, g, b]
}

/// A black drop shadow of the kit (`shadows.N`): color `#000000AA`, y offset and blur radius in
/// points, the blur read as a CSS `box-shadow` blur (σ = blur / 2).
const fn drop_shadow(color: u32, y: f32, blur: f32) -> GlassShadow {
    GlassShadow {
        opacity: token(color)[3],
        sigma: blur / 2.0,
        offset_y: y,
    }
}

/// `Surface/Window`, light and dark.
const SURFACE_WINDOW_LIGHT: [f32; 3] = opaque(0xFFFFFF);
const SURFACE_WINDOW_DARK: [f32; 3] = opaque(0x1B1C1F);

/// `shadows.glass` (Glass/Regular), light and dark (~2× denser).
const SHADOW_GLASS_LIGHT: GlassShadow = drop_shadow(0x00000029, 8.0, 20.0);
const SHADOW_GLASS_DARK: GlassShadow = drop_shadow(0x00000058, 8.0, 20.0);

/// The glass level that leaves the measured materials as they are.
pub const GLASS_LEVEL_DEFAULT: f32 = 0.5;

/// The inside rim of every material: 1 pt of `Glass/Rim Dark`.
const INSIDE_RIM_LIGHT: InsideRim = InsideRim {
    color: token(0x0000001F),
    width: 1.0,
};
const INSIDE_RIM_DARK: InsideRim = InsideRim {
    color: token(0x00000066),
    width: 1.0,
};

/// The rim light of every material: an inner shadow (x 0, y 1, blur 1) of `Glass/Rim Light`.
const RIM_LIGHT_LIGHT: RimLight = RimLight {
    color: token(0xFFFFFFB3),
    offset_y: 1.0,
    sigma: 0.5,
};
const RIM_LIGHT_DARK: RimLight = RimLight {
    color: token(0xFFFFFF38),
    ..RIM_LIGHT_LIGHT
};

impl Amalfi {
    pub const ALL: [Self; 2] = [Self::Regular, Self::Clear];

    /// The name of the material in the kit and in `tokens.json`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Regular => "Glass/Regular",
            Self::Clear => "Glass/Clear",
        }
    }

    /// The lens curve of the material's refraction (for the reference band, see [`bend_curve`]).
    pub fn lens(self) -> Lens {
        let curve: &'static [(f32, f32)] = match self {
            Self::Regular => &BEND_REGULAR,
            Self::Clear => &BEND_CLEAR,
        };
        Lens {
            side: curve,
            top: curve,
            bottom: curve,
        }
    }

    /// The material in the light or dark appearance.
    pub fn material(self, dark: bool) -> AmalfiMaterial {
        let (inside_rim, rim_light, fallback) = if dark {
            (INSIDE_RIM_DARK, RIM_LIGHT_DARK, SURFACE_WINDOW_DARK)
        } else {
            (INSIDE_RIM_LIGHT, RIM_LIGHT_LIGHT, SURFACE_WINDOW_LIGHT)
        };
        match self {
            Self::Regular => AmalfiMaterial {
                sigma: 8.0,
                saturation: 1.1,
                brightness: if dark { 1.07 } else { 1.18 },
                refraction: Refraction {
                    distortion: 0.30,
                    depth: 0.35,
                    chromatic: 0.03,
                },
                tint: token(if dark { 0x2A2B3096 } else { 0xFFFFFF80 }),
                inside_rim,
                rim_light,
                shadow: if dark {
                    SHADOW_GLASS_DARK
                } else {
                    SHADOW_GLASS_LIGHT
                },
                fallback,
            },
            // The magnification and specular highlight are the client's: here only what the
            // glass pass would draw.
            Self::Clear => AmalfiMaterial {
                sigma: 2.0,
                saturation: 1.0,
                brightness: if dark { 0.98 } else { 1.04 },
                refraction: Refraction {
                    distortion: 0.30,
                    depth: 0.50,
                    chromatic: 0.08,
                },
                tint: [0.0; 4],
                inside_rim,
                rim_light,
                shadow: GlassShadow::NONE,
                fallback,
            },
        }
    }
}

/// The settings of a kind of surface: its Amalfi material plus what the glass pass needs to
/// draw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassProfile {
    /// σ of the blur in points; `None`: the material's.
    pub sigma: Option<f64>,
    /// Strength of the edge lens (`MaterialParams::lens`): 1 the material's displacement, 0
    /// none. Glass/Regular refracts at its edge (1).
    pub refraction: f32,
    /// Width of the refraction band: this fraction of half the shorter side of the shape
    /// ([`refraction_band`]); 0: the profile's `falloff` and the lens's own reach ([`lens_width`]).
    pub depth: f32,
    /// The split of the red and blue samples at the rim, as a fraction of the band's width.
    pub chromatic: f32,
    /// Brightness of the backdrop (multiplier on the saturated color, before the tint).
    pub brightness: f32,
    /// Width of the lens band: 1 the material's band, 0.5 half as far from the edge (the curve
    /// keeps its shape). On a thin shape the band also narrows so that it never reaches past
    /// the far edge ([`lens_width`]).
    pub falloff: f32,
    /// Strength of the material's edge light (`MaterialParams::edge`, the light offsets): 1 as
    /// in the material.
    pub rim: f32,
    /// Strength of the material's edge dark (the dark offsets and the outline): 1 as in the
    /// material.
    pub line: f32,
    /// Saturation over the material's color (around the Rec. 709 luminance): 1 unchanged.
    pub saturation: f32,
    /// The tint fill over the saturated color (sRGB 0–1, alpha in the fourth slot): 0 none.
    pub tint: [f32; 4],
    /// The band just inside the edge.
    pub inside_rim: InsideRim,
    /// The light on the top inner edge.
    pub rim_light: RimLight,
    /// The shadow under the glass.
    pub shadow: GlassShadow,
    /// What the tint covers instead of the blurred backdrop at the opaque quality level (sRGB).
    pub fallback: [f32; 3],
    /// At what fraction of the resolution its blur runs (#204).
    pub blur_scale: BlurScale,
}

/// At what fraction of the resolution a kind's blur runs: the source is averaged over
/// `factor` × `factor` pixels, blurred there with the σ that gives the same total spread and
/// drawn back at full size. The factor is chosen per kind by the equivalence test (`compositor`,
/// `glass_equivalence`): over wallpapers, text and sharp edges in light and dark, the glass
/// drawn this way may differ from the full-resolution one by at most 1/255 on average and
/// 3/255 at any pixel and channel. A kind that fails at a factor keeps a smaller one, down to
/// full resolution (1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlurScale {
    /// The factor on outputs below 2× (σ in pixels is the smallest there).
    pub below_2x: u32,
    /// The factor from 2× up.
    pub from_2x: u32,
}

impl BlurScale {
    /// Full resolution at every scale.
    pub const FULL: Self = Self {
        below_2x: 1,
        from_2x: 1,
    };

    /// The factor on an output of this scale (1, 2, 4 or 8).
    pub fn factor(self, scale: f64) -> u32 {
        let factor = if scale >= 2.0 {
            self.from_2x
        } else {
            self.below_2x
        };
        match factor {
            0..=1 => 1,
            2..=3 => 2,
            4..=7 => 4,
            _ => 8,
        }
    }
}

impl GlassProfile {
    /// The material alone: no saturation, tint, rims or shadow.
    pub const NEUTRAL: Self = Self {
        sigma: None,
        refraction: 1.0,
        depth: 0.0,
        chromatic: 0.0,
        brightness: 1.0,
        falloff: 1.0,
        rim: 1.0,
        line: 1.0,
        saturation: 1.0,
        tint: [0.0; 4],
        inside_rim: InsideRim::NONE,
        rim_light: RimLight::NONE,
        shadow: GlassShadow::NONE,
        fallback: SURFACE_WINDOW_LIGHT,
        blur_scale: BlurScale::FULL,
    };

    /// An Amalfi material as a profile, at full resolution.
    pub fn amalfi(material: AmalfiMaterial) -> Self {
        Self {
            sigma: Some(material.sigma),
            refraction: if material.refraction.depth > 0.0 {
                1.0
            } else {
                0.0
            },
            depth: material.refraction.depth,
            chromatic: material.refraction.chromatic,
            brightness: material.brightness,
            saturation: material.saturation,
            tint: material.tint,
            inside_rim: material.inside_rim,
            rim_light: material.rim_light,
            shadow: material.shadow,
            fallback: material.fallback,
            ..Self::NEUTRAL
        }
    }

    /// The profile for the user's glass level (Settings › Appearance › Glass, 0 clearest to 1
    /// most tinted; [`GLASS_LEVEL_DEFAULT`] leaves the measured material untouched): the tint
    /// fill's strength goes from nothing to fully covering the blurred backdrop. A profile
    /// without a tint (clear glass) stays as it is.
    pub fn with_glass_level(self, level: f32) -> Self {
        let level = if level.is_finite() {
            level.clamp(0.0, 1.0)
        } else {
            GLASS_LEVEL_DEFAULT
        };
        let base = self.tint[3];
        if base <= 0.0 || level == GLASS_LEVEL_DEFAULT {
            return self;
        }
        let amount = if level < GLASS_LEVEL_DEFAULT {
            base * (level / GLASS_LEVEL_DEFAULT)
        } else {
            base + (1.0 - base) * ((level - GLASS_LEVEL_DEFAULT) / (1.0 - GLASS_LEVEL_DEFAULT))
        };
        let mut tint = self.tint;
        tint[3] = amount.clamp(0.0, 1.0);
        Self { tint, ..self }
    }

    /// Without the shadow (the client draws its own).
    pub fn without_shadow(self) -> Self {
        Self {
            shadow: GlassShadow::NONE,
            ..self
        }
    }
}

impl GlassKind {
    pub const ALL: [Self; 9] = [
        Self::Menu,
        Self::Popover,
        Self::Panel,
        Self::Module,
        Self::Control,
        Self::Dock,
        Self::Tooltip,
        Self::Sheet,
        Self::Banner,
    ];

    /// The name used by the protocol, the `silica/glass` channel and `glass.list`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Menu => "menu",
            Self::Popover => "popover",
            Self::Panel => "panel",
            Self::Module => "module",
            Self::Control => "control",
            Self::Dock => "dock",
            Self::Tooltip => "tooltip",
            Self::Sheet => "sheet",
            Self::Banner => "banner",
        }
    }

    /// The kind with this name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The Amalfi material of this kind: Glass/Regular, the one glass of every kind (Clear
    /// only sits over media, inside clients).
    pub fn amalfi(self) -> Amalfi {
        Amalfi::Regular
    }

    /// The share of the sharp backdrop mixed into the blur: the one each kind had before Amalfi
    /// (the Dock's 0.28, the modules' and controls' 0.04, the banners' 0.03 / 0.02, none on
    /// menus, popovers, panels, tooltips and sheets),
    /// until it is tuned against the kit's images.
    pub fn sharp(self, dark: bool) -> f32 {
        match self {
            Self::Dock => 0.28,
            Self::Module | Self::Control => 0.04,
            Self::Banner if dark => 0.02,
            Self::Banner => 0.03,
            Self::Menu | Self::Popover | Self::Panel | Self::Tooltip | Self::Sheet => 0.0,
        }
    }

    /// The material the glass pass draws this kind with, in the light or dark appearance: its
    /// Amalfi blur and its sharp share, a neutral color transfer (the profile adds saturation,
    /// tint and rims), no lens.
    pub fn material(self, dark: bool) -> MaterialParams {
        // Banners and Notification Center cards separate even a few points apart.
        let merge_pt = if self == Self::Banner { 0.0 } else { MERGE_PT };
        MaterialParams::amalfi(
            self.amalfi().material(dark).sigma,
            self.sharp(dark),
            merge_pt,
            self.amalfi().lens(),
        )
    }

    /// The settings of this kind in the light or dark appearance.
    pub fn profile(self, dark: bool) -> GlassProfile {
        GlassProfile {
            blur_scale: self.blur_scale(),
            ..GlassProfile::amalfi(self.amalfi().material(dark))
        }
    }

    /// At what fraction of the resolution this kind blurs (#204): the factors the equivalence
    /// test passes with (`compositor::glass_equivalence::table_of_factors`, mean / max levels
    /// against full resolution, the worst backdrop of each, light and dark). The tint covers
    /// 58–60 % of the backdrop, so a reduced blur shows even less than it did before Amalfi.
    fn blur_scale(self) -> BlurScale {
        match self.amalfi() {
            // σ 8 pt. Half below 2× (σ 8 px: f4 would take it near the 1.5 px floor of a
            // reduced blur), a quarter from 2×: the factors of σ 6 pt, which the larger σ only
            // makes safer.
            Amalfi::Regular => BlurScale {
                below_2x: 2,
                from_2x: 4,
            },
            // σ 2 pt: too small to reduce.
            Amalfi::Clear => BlurScale::FULL,
        }
    }
}

/// How much the compositor dims the window a sheet is attached to (GLASS-PROFILES: ~40 %).
/// The compositor does not draw it yet: nothing in the render path darkens a parent window behind a
/// surface.
pub const SHEET_PARENT_DIM: f32 = 0.4;

/// The width of the lens band on a shape whose smaller side is `thickness` (pixels), for a
/// lens that samples at most `reach` pixels inside the edge and a profile `falloff`: the
/// profile's width, narrowed so the lens never samples past the shape's far edge. A lens that
/// samples the background up to 50 pt inside fits a 64 pt shape as it is; on a 24 pt pill it
/// would read the background beyond the pill (the "far too strong" refraction of #168), so it
/// gets 0.48 of the band there.
pub fn lens_width(thickness: f32, reach: f32, falloff: f32) -> f32 {
    let falloff = falloff.max(0.0);
    if reach <= 0.0 || thickness <= 0.0 {
        return falloff;
    }
    falloff.min(thickness / reach).max(0.05)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_glass_level_moves_the_tint_and_the_default_changes_nothing() {
        for dark in [false, true] {
            let regular = GlassKind::Panel.profile(dark);
            assert!(regular.tint[3] > 0.0);
            assert_eq!(regular.with_glass_level(GLASS_LEVEL_DEFAULT), regular);
            assert_eq!(regular.with_glass_level(f32::NAN), regular);
            assert_eq!(regular.with_glass_level(0.0).tint[3], 0.0);
            assert_eq!(regular.with_glass_level(1.0).tint[3], 1.0);
            let mid = regular.with_glass_level(0.75).tint[3];
            assert!(mid > regular.tint[3] && mid < 1.0);
            // Only the tint's strength changes.
            assert_eq!(regular.with_glass_level(1.0).tint[..3], regular.tint[..3]);
        }
    }

    use super::*;
    use crate::glass::{REFERENCE_GRAY, fallback_color, profile_color};

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn close_rgba(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(a, b)| close(*a, b))
    }

    #[test]
    fn every_kind_has_sane_settings_in_both_appearances() {
        for kind in GlassKind::ALL {
            assert_eq!(GlassKind::from_name(kind.name()), Some(kind));
            for dark in [false, true] {
                let p = kind.profile(dark);
                let label = format!("{kind:?} dark {dark}");
                let sigma = p.sigma.unwrap_or(kind.material(dark).sigma);
                assert!((1.0..=20.0).contains(&sigma), "{label}: σ {sigma}");
                assert_eq!(kind.material(dark).sigma, sigma, "{label}");
                assert!((0.0..=1.0).contains(&p.refraction), "{label}");
                assert!((0.1..=2.0).contains(&p.falloff), "{label}");
                assert!((0.5..=1.5).contains(&p.saturation), "{label}");
                assert!((0.9..=1.3).contains(&p.brightness), "{label}");
                assert!((0.0..=1.0).contains(&p.tint[3]), "{label}");
                assert!((0.0..=0.5).contains(&p.shadow.opacity), "{label}");
                assert!(p.shadow.reach() <= 120.0, "{label}");
                assert!((0.0..=0.3).contains(&kind.sharp(dark)), "{label}");
            }
        }
        assert_eq!(GlassKind::from_name("default"), None);
    }

    #[test]
    fn each_kind_uses_its_amalfi_material() {
        use GlassKind::*;
        // Glass/Regular is the one glass of every kind.
        let table = [
            (Menu, Amalfi::Regular),
            (Popover, Amalfi::Regular),
            (Panel, Amalfi::Regular),
            (Module, Amalfi::Regular),
            (Tooltip, Amalfi::Regular),
            (Sheet, Amalfi::Regular),
            (Dock, Amalfi::Regular),
            (Control, Amalfi::Regular),
            (Banner, Amalfi::Regular),
        ];
        assert_eq!(table.len(), GlassKind::ALL.len());
        for (kind, material) in table {
            assert_eq!(kind.amalfi(), material, "{kind:?}");
            for dark in [false, true] {
                let spec = material.material(dark);
                let p = kind.profile(dark);
                assert_eq!(p.sigma, Some(spec.sigma), "{kind:?}");
                assert_eq!(p.saturation, spec.saturation, "{kind:?}");
                assert_eq!(p.tint, spec.tint, "{kind:?}");
                assert_eq!(p.inside_rim, spec.inside_rim, "{kind:?}");
                assert_eq!(p.rim_light, spec.rim_light, "{kind:?}");
                assert_eq!(p.shadow, spec.shadow, "{kind:?}");
                assert_eq!(p.fallback, spec.fallback, "{kind:?}");
                // Glass/Regular refracts at its edge (a lens with the
                // material's distortion); the material's color transfer is neutral.
                assert_eq!(p.refraction, 1.0, "{kind:?}");
                assert_eq!(p.depth, spec.refraction.depth, "{kind:?}");
                assert_eq!(p.chromatic, spec.refraction.chromatic, "{kind:?}");
                assert_eq!(p.brightness, spec.brightness, "{kind:?}");
                let m = kind.material(dark);
                // The backdrop is pulled in towards the rim by distortion × band at the edge
                // and not at all at the band's inner side.
                let want = spec.refraction.distortion * REFRACTION_BAND_PT;
                for curve in [m.lens.side, m.lens.top, m.lens.bottom] {
                    let (t, s) = curve[0];
                    assert!((t, s) == (0.0, -want), "{kind:?}: {curve:?}");
                    let (t, s) = curve[curve.len() - 1];
                    assert!(close(t, REFRACTION_BAND_PT) && close(s, t), "{kind:?}");
                    assert!(curve.windows(2).all(|w| w[1].1 > w[0].1), "{kind:?}");
                }
                assert_eq!(m.lens.outward_reach(), want, "{kind:?}");
                assert_eq!(m.transfer([0.2, 0.5, 0.9]), [0.2, 0.5, 0.9], "{kind:?}");
            }
        }
    }

    #[test]
    fn the_refraction_band_follows_the_shape() {
        // depth × half the shorter side, at most the reference band, at least a sliver.
        assert!(close(refraction_band(40.0, 0.35), 7.0 / REFRACTION_BAND_PT));
        assert!(close(
            refraction_band(120.0, 0.35),
            21.0 / REFRACTION_BAND_PT
        ));
        assert_eq!(refraction_band(600.0, 0.35), 1.0);
        assert_eq!(refraction_band(0.0, 0.35), 0.05);
    }

    #[test]
    fn glass_regular_is_the_token() {
        for dark in [false, true] {
            let m = Amalfi::Regular.material(dark);
            assert_eq!(m.sigma, 8.0);
            assert_eq!(m.saturation, 1.1);
            assert_eq!(m.brightness, if dark { 1.07 } else { 1.18 });
            assert_eq!(
                m.refraction,
                Refraction {
                    distortion: 0.30,
                    depth: 0.35,
                    chromatic: 0.03
                }
            );
            let tint = if dark {
                [42.0 / 255.0, 43.0 / 255.0, 48.0 / 255.0, 150.0 / 255.0]
            } else {
                [1.0, 1.0, 1.0, 128.0 / 255.0]
            };
            assert!(close_rgba(m.tint, tint), "{dark}: {:?}", m.tint);
            // shadows.glass: y 8, blur 20 (σ 10), black 16 % in light, 35 % in dark.
            assert!(close(m.shadow.opacity, if dark { 0.3451 } else { 0.1608 }));
            assert_eq!((m.shadow.offset_y, m.shadow.sigma), (8.0, 10.0));
        }
    }

    #[test]
    fn glass_clear_is_the_token() {
        for dark in [false, true] {
            let m = Amalfi::Clear.material(dark);
            assert_eq!(m.sigma, 2.0);
            assert_eq!(m.saturation, 1.0);
            assert_eq!(m.brightness, if dark { 0.98 } else { 1.04 });
            assert_eq!(m.refraction.depth, 0.5);
            assert_eq!(m.tint[3], 0.0);
            assert_eq!(m.shadow, GlassShadow::NONE);
            assert_eq!(m.shadow.reach(), 0.0);
        }
        assert_eq!(Amalfi::Clear.name(), "Glass/Clear");
    }

    #[test]
    fn the_rims_are_the_same_on_every_material_and_stronger_in_dark() {
        for material in Amalfi::ALL {
            let light = material.material(false);
            let dark = material.material(true);
            // Inside rim: 1 pt of black, 12 % in light, 40 % in dark.
            for (m, alpha) in [(light, 0.1216), (dark, 0.4)] {
                assert_eq!(m.inside_rim.width, 1.0, "{material:?}");
                assert_eq!(m.inside_rim.color[..3], [0.0; 3], "{material:?}");
                assert!(close(m.inside_rim.color[3], alpha), "{material:?}");
            }
            // Rim light: an inner shadow (0, 1, blur 1 → σ 0.5) of white, 70 % in light, 22 %
            // in dark.
            for (m, alpha) in [(light, 0.702), (dark, 0.2196)] {
                assert_eq!(m.rim_light.color[..3], [1.0; 3], "{material:?}");
                assert!(close(m.rim_light.color[3], alpha), "{material:?}");
                assert_eq!((m.rim_light.offset_y, m.rim_light.sigma), (1.0, 0.5));
            }
            assert!(dark.inside_rim.color[3] > light.inside_rim.color[3]);
            assert!(dark.rim_light.color[3] < light.rim_light.color[3]);
        }
    }

    #[test]
    fn dark_shadows_are_about_twice_as_dense() {
        let m = Amalfi::Regular;
        let ratio = m.material(true).shadow.opacity / m.material(false).shadow.opacity;
        assert!((1.9..=2.7).contains(&ratio), "{ratio}");
        let regular = m.material(false).shadow;
        assert!((regular.reach() - (3.0 * 10.0 + 8.0)).abs() < 1e-4);
    }

    #[test]
    fn saturation_and_tint_follow_the_render_order() {
        // Over the reference gray (nearly neutral) the saturation barely moves the color; the
        // tint then covers it: 50 % white in light, 59 % of #2A2B30 in dark, on every kind.
        let level = |kind: GlassKind, dark: bool| {
            let m = kind.material(dark);
            let [r, g, b] = profile_color(&kind.profile(dark), m.transfer(REFERENCE_GRAY));
            [r * 255.0, g * 255.0, b * 255.0]
        };
        let gray = REFERENCE_GRAY.map(|c| c * 255.0);
        let l = 0.2126 * gray[0] + 0.7152 * gray[1] + 0.0722 * gray[2];
        let saturated =
            |brightness: f32| gray.map(|c| ((l + (c - l) * 1.1) * brightness).min(255.0));
        for (kind, dark, tint, amount, brightness) in [
            (GlassKind::Dock, false, [255.0; 3], 128.0 / 255.0, 1.18),
            (GlassKind::Menu, false, [255.0; 3], 128.0 / 255.0, 1.18),
            (
                GlassKind::Dock,
                true,
                [42.0, 43.0, 48.0],
                150.0 / 255.0,
                1.07,
            ),
            (
                GlassKind::Menu,
                true,
                [42.0, 43.0, 48.0],
                150.0 / 255.0,
                1.07,
            ),
        ] {
            let got = level(kind, dark);
            let saturated = saturated(brightness);
            for c in 0..3 {
                let want = saturated[c] + (tint[c] - saturated[c]) * amount;
                assert!((got[c] - want).abs() < 0.05, "{kind:?} {dark}: {got:?}");
            }
        }
        // A saturated color gets more vivid before the tint: red stays redder than the gray.
        let dock = GlassKind::Dock.profile(false);
        let red = profile_color(&dock, [0.8, 0.3, 0.3]);
        let flat = GlassProfile {
            saturation: 1.0,
            brightness: 1.0,
            ..dock
        };
        let plain = profile_color(&flat, [0.8, 0.3, 0.3]);
        assert!(red[0] - red[1] != plain[0] - plain[1], "{red:?}");
    }

    #[test]
    fn without_blur_the_tint_covers_the_window_surface() {
        // Reduced transparency: the tint over Surface/Window (#FFFFFF / #1B1C1F), rims and
        // shadow kept.
        let regular_light = fallback_color(&GlassKind::Dock.profile(false));
        assert!(
            regular_light.iter().all(|c| close(*c, 1.0)),
            "{regular_light:?}"
        );
        let menu_dark = fallback_color(&GlassKind::Menu.profile(true));
        let a = 150.0 / 255.0;
        let want = [
            (27.0 * (1.0 - a) + 42.0 * a) / 255.0,
            (28.0 * (1.0 - a) + 43.0 * a) / 255.0,
            (31.0 * (1.0 - a) + 48.0 * a) / 255.0,
        ];
        assert!(
            menu_dark.iter().zip(want).all(|(g, w)| close(*g, w)),
            "{menu_dark:?}"
        );
        let regular_dark = fallback_color(&GlassKind::Banner.profile(true));
        assert!((regular_dark[0] * 255.0 - (27.0 * (1.0 - a) + 42.0 * a)).abs() < 0.01);
        for kind in GlassKind::ALL {
            for dark in [false, true] {
                let p = kind.profile(dark);
                assert!(p.inside_rim.color[3] > 0.0 && p.rim_light.color[3] > 0.0);
            }
        }
    }

    #[test]
    fn each_kind_keeps_its_sharp_share() {
        use GlassKind::*;
        assert_eq!(Dock.material(false).sharp, 0.28);
        assert_eq!(Dock.material(true).sharp, 0.28);
        assert_eq!(Control.material(false).sharp, 0.04);
        assert_eq!(Module.material(true).sharp, 0.04);
        assert_eq!(Banner.material(false).sharp, 0.03);
        assert_eq!(Banner.material(true).sharp, 0.02);
        for kind in [Menu, Popover, Panel, Tooltip, Sheet] {
            assert_eq!(kind.material(false).sharp, 0.0, "{kind:?}");
        }
        // Only banners never merge.
        assert_eq!(Banner.material(false).merge_pt, 0.0);
        assert_eq!(Menu.material(false).merge_pt, MERGE_PT);
    }

    #[test]
    fn each_material_blurs_at_its_validated_scale() {
        // The factors of the equivalence test (#204): half below 2× and a quarter from 2×.
        for kind in GlassKind::ALL {
            for dark in [false, true] {
                let scale = kind.profile(dark).blur_scale;
                assert_eq!((scale.factor(1.0), scale.factor(2.0)), (2, 4), "{kind:?}");
            }
        }
    }

    #[test]
    fn the_lens_band_fits_the_shape() {
        // A 64 pt shape (a lens sampling up to 50 pt inside) keeps the whole band.
        assert_eq!(lens_width(64.0, 50.0, 1.0), 1.0);
        // A 24 pt pill: the lens samples at most 24 pt inside, its far edge.
        let pill = lens_width(24.0, 50.0, 1.0);
        assert!((pill - 0.48).abs() < 1e-6, "{pill}");
        assert!(pill * 50.0 <= 24.0 + 1e-4);
        // The profile's own width when it fits.
        assert_eq!(lens_width(200.0, 50.0, 0.6), 0.6);
        // A wide band on a large panel stays wide.
        assert_eq!(lens_width(600.0, 50.0, 1.5), 1.5);
        // Degenerate inputs never give zero or a negative width.
        assert!(lens_width(0.0, 50.0, 1.0) > 0.0);
        assert!(lens_width(1.0, 50.0, 1.0) >= 0.05);
        assert_eq!(lens_width(30.0, 0.0, 0.8), 0.8);
        assert!((SHEET_PARENT_DIM - 0.4).abs() < 1e-6);
    }
}
