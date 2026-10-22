//! The compositor's GLES renderer and glass shaders.
//!
//! The effects are smithay render elements that the compositor puts in the output's list. The
//! parameters come from the macOS 27 measurements in `plan/reference/`, cited next to every
//! constant.

pub mod animation;
pub mod backdrop;
pub mod border;
pub mod cache;
pub mod corners;
pub mod filter;
pub mod genie;
pub mod glass;
pub mod glass_profile;
pub mod shadow;
pub mod sprite;
pub mod title;
pub mod titlebar;

use smithay::backend::renderer::gles::{
    GlesPixelProgram, GlesRenderer, GlesTexProgram, UniformName, UniformType,
};
use tracing::warn;

/// The programs compiled for a renderer. If a shader does not compile, the effect is missing
/// but the compositor keeps drawing (never crash).
#[derive(Clone, Default)]
pub struct Shaders {
    pub corners: Option<GlesTexProgram>,
    pub shadow: Option<GlesPixelProgram>,
    pub titlebar: Option<GlesPixelProgram>,
    /// The 1 pt inner border of windows in dark.
    pub border: Option<GlesPixelProgram>,
    pub glass: Option<glass::GlassPrograms>,
    pub tone: Option<GlesTexProgram>,
    /// The funnel deformation of minimizing into the Dock.
    pub genie: Option<GlesTexProgram>,
    /// The Accessibility display filters (invert, grayscale, contrast).
    pub filter: Option<GlesTexProgram>,
}

/// The renderer programs, compiled on the first request and kept in the EGL context.
pub fn shaders(renderer: &mut GlesRenderer) -> Shaders {
    if let Some(shaders) = renderer.egl_context().user_data().get::<Shaders>() {
        return shaders.clone();
    }
    let corners = compile_texture(renderer, "corners", corners::SHADER, &corners::UNIFORMS);
    let shadow = compile_pixel(renderer, "shadow", shadow::SHADER, &shadow::UNIFORMS);
    let titlebar = compile_pixel(renderer, "title bar", titlebar::SHADER, &titlebar::UNIFORMS);
    let border = compile_pixel(renderer, "window border", border::SHADER, &border::UNIFORMS);
    let blur = compile_texture(renderer, "blur", glass::BLUR_SHADER, &glass::BLUR_UNIFORMS);
    let material = compile_texture(
        renderer,
        "material",
        glass::MATERIAL_SHADER,
        &glass::MATERIAL_UNIFORMS,
    );
    let downsample = compile_texture(
        renderer,
        "blur downsample",
        glass::DOWNSAMPLE_SHADER,
        &glass::DOWNSAMPLE_UNIFORMS,
    );
    let upsample = compile_texture(
        renderer,
        "blur upsample",
        glass::UPSAMPLE_SHADER,
        &glass::UPSAMPLE_UNIFORMS,
    );
    let glass = blur
        .zip(material)
        .zip(downsample.clone().zip(upsample.clone()))
        .map(
            |((blur, material), (downsample, upsample))| glass::GlassPrograms {
                blur,
                material,
                downsample,
                upsample,
            },
        );
    let tone = compile_texture(
        renderer,
        "bar backdrop",
        backdrop::SHADER,
        &backdrop::UNIFORMS,
    );
    let genie = compile_texture(renderer, "genie", genie::SHADER, &genie::UNIFORMS);
    let filter = compile_texture(
        renderer,
        "display filter",
        filter::SHADER,
        &filter::UNIFORMS,
    );
    let shaders = Shaders {
        corners,
        shadow,
        titlebar,
        border,
        glass,
        tone,
        genie,
        filter,
    };
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| shaders.clone());
    shaders
}

fn compile_texture(
    renderer: &mut GlesRenderer,
    name: &str,
    source: &str,
    uniforms: &[(&str, UniformType)],
) -> Option<GlesTexProgram> {
    let names: Vec<UniformName<'_>> = uniforms
        .iter()
        .map(|(name, type_)| UniformName::new(*name, *type_))
        .collect();
    match renderer.compile_custom_texture_shader(source, &names) {
        Ok(program) => Some(program),
        Err(err) => {
            warn!("shader {name} not compiled, effect disabled: {err:?}");
            None
        }
    }
}

fn compile_pixel(
    renderer: &mut GlesRenderer,
    name: &str,
    source: &str,
    uniforms: &[(&str, UniformType)],
) -> Option<GlesPixelProgram> {
    let names: Vec<UniformName<'_>> = uniforms
        .iter()
        .map(|(name, type_)| UniformName::new(*name, *type_))
        .collect();
    match renderer.compile_custom_pixel_shader(source, &names) {
        Ok(program) => Some(program),
        Err(err) => {
            warn!("shader {name} not compiled, effect disabled: {err:?}");
            None
        }
    }
}
