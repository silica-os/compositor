//! Window shadow, measured on macOS 27 (`plan/reference/windows.md`,
//! "Windows · shadow"): black, Gaussian-blur profile, shifted down, wider for the active
//! window, with a 1 pt contact ring; in dark about twice as strong.

use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{GlesPixelProgram, Uniform, UniformType, UniformValue};
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::cache::ShaderCache;
use crate::corners::{CORNER_EXPONENT, CORNER_RADIUS};

/// Parameters of a shadow, in logical points; alpha from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowStyle {
    /// Amplitude A/255 of the fit alpha(d) = A·½·erfc((d − s)/(√2 σ)).
    pub amplitude: f32,
    pub shift: f32,
    pub sigma: f32,
    /// Vertical shift of the shadow (downwards).
    pub offset_y: f32,
    /// Alpha of the 1 pt contact ring: sides and top.
    pub ring_side: f32,
    pub ring_top: f32,
    /// Extent of the shadow beyond the window: sides, top, bottom (where it reaches zero).
    pub extent: (i32, i32, i32),
}

/// `shadows.3` of the Amalfi tokens, on every window (active or not; `materials.md`: only the
/// window has a shadow): y 12, blur 32 (a CSS blur, so sigma 16), black at 18 % in light
/// (`#0000002E`) and 36 % in dark (`#0000005C`), no spread, no contact ring. Margins where the
/// Gaussian reaches zero (3 sigma): sides 48, top 48 - 12, bottom 48 + 12.
const fn shadow_3(alpha: u8) -> ShadowStyle {
    ShadowStyle {
        amplitude: alpha as f32 / 255.0,
        // The shader measures from the first pixel after the edge (0.5 pt in): undo it.
        shift: -0.5,
        sigma: 16.0,
        offset_y: 12.0,
        ring_side: 0.0,
        ring_top: 0.0,
        extent: (50, 38, 62),
    }
}

pub const ACTIVE_LIGHT: ShadowStyle = shadow_3(0x2e);
pub const INACTIVE_LIGHT: ShadowStyle = shadow_3(0x2e);
pub const ACTIVE_DARK: ShadowStyle = shadow_3(0x5c);
pub const INACTIVE_DARK: ShadowStyle = shadow_3(0x5c);

pub fn style(active: bool, dark: bool) -> ShadowStyle {
    match (active, dark) {
        (true, false) => ACTIVE_LIGHT,
        (false, false) => INACTIVE_LIGHT,
        (true, true) => ACTIVE_DARK,
        (false, true) => INACTIVE_DARK,
    }
}

pub const SHADER: &str = include_str!("shaders/shadow.frag");
pub const UNIFORMS: [(&str, UniformType); 9] = [
    ("window_rect", UniformType::_4f),
    ("radius", UniformType::_1f),
    ("exponent", UniformType::_1f),
    ("offset_y", UniformType::_1f),
    ("amplitude", UniformType::_1f),
    ("shift", UniformType::_1f),
    ("sigma", UniformType::_1f),
    ("ring_side", UniformType::_1f),
    ("ring_top", UniformType::_1f),
];

/// The shadow of a window, kept in the window's data.
#[derive(Default)]
pub struct WindowShadow {
    cache: ShaderCache<(Size<i32, Logical>, ShadowStyle)>,
}

impl WindowShadow {
    /// The shadow element for a window with this frame (logical, in the output).
    pub fn element(
        &self,
        program: &GlesPixelProgram,
        window: Rectangle<i32, Logical>,
        style: ShadowStyle,
    ) -> PixelShaderElement {
        let (side, top, bottom) = style.extent;
        let area = Rectangle::new(
            window.loc - Point::from((side, top)),
            window.size + Size::from((side.saturating_mul(2), top.saturating_add(bottom))),
        );
        self.cache.element(program, area, (window.size, style), || {
            uniforms(window.size, (side, top), style)
        })
    }
}

fn uniforms(
    window: Size<i32, Logical>,
    (side, top): (i32, i32),
    style: ShadowStyle,
) -> Vec<Uniform<'static>> {
    let radius = (CORNER_RADIUS as f32)
        .min(window.w as f32 / 2.0)
        .min(window.h as f32 / 2.0)
        .max(0.0);
    vec![
        Uniform::new(
            "window_rect",
            UniformValue::_4f(side as f32, top as f32, window.w as f32, window.h as f32),
        ),
        Uniform::new("radius", UniformValue::_1f(radius)),
        Uniform::new("exponent", UniformValue::_1f(CORNER_EXPONENT)),
        Uniform::new("offset_y", UniformValue::_1f(style.offset_y)),
        Uniform::new("amplitude", UniformValue::_1f(style.amplitude)),
        Uniform::new("shift", UniformValue::_1f(style.shift)),
        Uniform::new("sigma", UniformValue::_1f(style.sigma)),
        Uniform::new("ring_side", UniformValue::_1f(style.ring_side)),
        Uniform::new("ring_top", UniformValue::_1f(style.ring_top)),
    ]
}
