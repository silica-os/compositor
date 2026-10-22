//! The display filters of Accessibility: invert, grayscale and increased contrast, applied to
//! the whole frame.

pub const SHADER: &str = include_str!("shaders/filter.frag");
pub const UNIFORMS: [(&str, smithay::backend::renderer::gles::UniformType); 2] = {
    use smithay::backend::renderer::gles::UniformType;
    [("mode", UniformType::_1f), ("contrast", UniformType::_1f)]
};
