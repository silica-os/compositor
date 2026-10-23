//! The rim of windows: 1 pt inside the outline, above the content, in `Line/Border` of the
//! Amalfi tokens (`#1D1E21` at 14 % in light, `#F4F5F7` at 16 % in dark), the same for an active
//! and an inactive window (the kit's "Window rim" layer).

use smithay::backend::renderer::gles::{Uniform, UniformType, UniformValue};
use smithay::utils::{Logical, Size};

use crate::Program;
use crate::cache::{ShaderCache, ShaderElement};
use crate::corners::{CORNER_EXPONENT, CORNER_RADIUS};

/// The rim colour: sRGB 0-1 with straight alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderStyle {
    pub rgba: [f32; 4],
}

const fn rgba(r: u8, g: u8, b: u8, a: u8) -> [f32; 4] {
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

/// `Light/Line/Border` `#1D1E2124`.
pub const LIGHT: BorderStyle = BorderStyle {
    rgba: rgba(0x1d, 0x1e, 0x21, 0x24),
};

/// `Dark/Line/Border` `#F4F5F729`.
pub const DARK: BorderStyle = BorderStyle {
    rgba: rgba(0xf4, 0xf5, 0xf7, 0x29),
};

/// The rim of a window according to the appearance (the state does not change it).
pub fn style(_active: bool, dark: bool) -> Option<BorderStyle> {
    Some(if dark { DARK } else { LIGHT })
}

pub const SHADER: &str = include_str!("shaders/border.frag");
pub const UNIFORMS: [(&str, UniformType); 5] = [
    ("radius", UniformType::_1f),
    ("exponent", UniformType::_1f),
    ("pixel", UniformType::_1f),
    ("rim", UniformType::_4f),
    ("fill", UniformType::_1f),
];

/// The veil over a window while it has a sheet (docs/spec/dialogs.md "Veil": white
/// becomes `#cccccc` in light, the dark surface `#1a1a1a` in dark).
pub fn veil(dark: bool) -> BorderStyle {
    if dark {
        BorderStyle {
            rgba: [0.0, 0.0, 0.0, 0.33],
        }
    } else {
        BorderStyle {
            rgba: [0.0, 0.0, 0.0, 0.2],
        }
    }
}

/// The border of a window, kept in its data.
#[derive(Default)]
pub struct WindowBorder {
    cache: ShaderCache<(Size<i32, Logical>, BorderStyle, u32)>,
}

impl WindowBorder {
    /// The border element for a frame (logical, in the output).
    pub fn element(
        &self,
        program: Program,
        frame: smithay::utils::Rectangle<i32, Logical>,
        style: BorderStyle,
        scale: f64,
    ) -> ShaderElement {
        self.shape(program, frame, style, scale, false)
    }

    /// The veil element: the window's shape filled with `style`'s colour.
    pub fn veil_element(
        &self,
        program: Program,
        frame: smithay::utils::Rectangle<i32, Logical>,
        style: BorderStyle,
        scale: f64,
    ) -> ShaderElement {
        self.shape(program, frame, style, scale, true)
    }

    fn shape(
        &self,
        program: Program,
        frame: smithay::utils::Rectangle<i32, Logical>,
        style: BorderStyle,
        scale: f64,
        fill: bool,
    ) -> ShaderElement {
        let pixel = (1.0 / scale.max(1.0)) as f32;
        self.cache
            .element(program, frame, (frame.size, style, pixel.to_bits()), || {
                let radius = (CORNER_RADIUS as f32)
                    .min(frame.size.w as f32 / 2.0)
                    .min(frame.size.h as f32 / 2.0)
                    .max(0.0);
                vec![
                    Uniform::new("radius", UniformValue::_1f(radius)),
                    Uniform::new("exponent", UniformValue::_1f(CORNER_EXPONENT)),
                    Uniform::new("pixel", UniformValue::_1f(pixel)),
                    Uniform::new(
                        "rim",
                        UniformValue::_4f(
                            style.rgba[0],
                            style.rgba[1],
                            style.rgba[2],
                            style.rgba[3],
                        ),
                    ),
                    Uniform::new("fill", UniformValue::_1f(if fill { 1.0 } else { 0.0 })),
                ]
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rim_is_line_border_in_both_appearances() {
        assert_eq!(style(true, false), Some(LIGHT));
        assert_eq!(style(false, false), Some(LIGHT));
        assert_eq!(style(true, true), Some(DARK));
        assert_eq!(style(false, true), Some(DARK));
        assert_eq!(LIGHT.rgba[3], 36.0 / 255.0);
        assert_eq!(DARK.rgba[3], 41.0 / 255.0);
    }
}
