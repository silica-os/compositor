//! The compositor's renderers (GLES, or the CPU where there is no GPU), its effects and its
//! glass.
//!
//! The effects are smithay render elements that the compositor puts in the output's list. The
//! parameters come from the macOS 27 measurements in `plan/reference/`, cited next to every
//! constant.

pub mod animation;
pub mod backdrop;
pub mod border;
pub mod cache;
pub mod corners;
mod cpu;
pub mod filter;
pub mod genie;
pub mod glass;
pub mod glass_profile;
pub mod renderer;
pub mod shadow;
pub mod title;
pub mod titlebar;

use smithay::backend::renderer::gles::{
    GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, UniformName, UniformType,
};
use tracing::warn;

use crate::renderer::SilicaRenderer;

/// A program of the effects: a GLSL shader on GLES, its port on the CPU, both reading the same
/// parameters (the shader's uniforms).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Program {
    Corners,
    Shadow,
    Titlebar,
    /// The 1 pt inner border of windows in dark.
    Border,
    /// The glass: its material, blur and resampling passes.
    Glass,
    Tone,
    /// The funnel deformation of minimizing into the Dock.
    Genie,
    /// The Accessibility display filters (invert, grayscale, contrast).
    Filter,
}

/// The effects a renderer draws. If a program is missing (a shader that does not compile, an
/// effect the CPU renderer lacks), the effect is off but the compositor keeps drawing (never
/// crash).
#[derive(Clone, Copy, Debug, Default)]
pub struct Shaders {
    pub corners: Option<Program>,
    pub shadow: Option<Program>,
    pub titlebar: Option<Program>,
    pub border: Option<Program>,
    pub glass: Option<Program>,
    pub tone: Option<Program>,
    pub genie: Option<Program>,
    pub filter: Option<Program>,
}

/// The effects of `renderer`.
pub fn shaders(renderer: &mut SilicaRenderer) -> Shaders {
    match renderer {
        SilicaRenderer::Gles(renderer) => gles_programs(renderer).available(),
        // Every effect is ported (`cpu`).
        SilicaRenderer::Cpu(_) => Shaders {
            corners: Some(Program::Corners),
            shadow: Some(Program::Shadow),
            titlebar: Some(Program::Titlebar),
            border: Some(Program::Border),
            glass: Some(Program::Glass),
            tone: Some(Program::Tone),
            genie: Some(Program::Genie),
            filter: Some(Program::Filter),
        },
    }
}

/// The GLSL programs compiled for a GLES renderer.
#[derive(Clone, Default)]
pub struct GlesPrograms {
    pub corners: Option<GlesTexProgram>,
    pub shadow: Option<GlesPixelProgram>,
    pub titlebar: Option<GlesPixelProgram>,
    pub border: Option<GlesPixelProgram>,
    pub glass: Option<glass::GlassPrograms>,
    pub tone: Option<GlesTexProgram>,
    pub genie: Option<GlesTexProgram>,
    pub filter: Option<GlesTexProgram>,
}

impl GlesPrograms {
    fn available(&self) -> Shaders {
        let has = |present: bool, program| present.then_some(program);
        Shaders {
            corners: has(self.corners.is_some(), Program::Corners),
            shadow: has(self.shadow.is_some(), Program::Shadow),
            titlebar: has(self.titlebar.is_some(), Program::Titlebar),
            border: has(self.border.is_some(), Program::Border),
            glass: has(self.glass.is_some(), Program::Glass),
            tone: has(self.tone.is_some(), Program::Tone),
            genie: has(self.genie.is_some(), Program::Genie),
            filter: has(self.filter.is_some(), Program::Filter),
        }
    }

    /// The pixel program of `program`, if it is one and compiled.
    pub fn pixel(&self, program: Program) -> Option<&GlesPixelProgram> {
        match program {
            Program::Shadow => self.shadow.as_ref(),
            Program::Titlebar => self.titlebar.as_ref(),
            Program::Border => self.border.as_ref(),
            _ => None,
        }
    }

    /// The texture program of `program`, if it is one and compiled (the glass has several:
    /// `glass`).
    pub fn texture(&self, program: Program) -> Option<&GlesTexProgram> {
        match program {
            Program::Corners => self.corners.as_ref(),
            Program::Tone => self.tone.as_ref(),
            Program::Genie => self.genie.as_ref(),
            Program::Filter => self.filter.as_ref(),
            _ => None,
        }
    }
}

/// The programs of the renderer drawing `frame`, once compiled.
pub(crate) fn frame_programs<'a>(frame: &'a GlesFrame<'_, '_>) -> Option<&'a GlesPrograms> {
    frame.egl_context().user_data().get::<GlesPrograms>()
}

/// The renderer programs, compiled on the first request and kept in the EGL context.
pub fn gles_programs(renderer: &mut GlesRenderer) -> GlesPrograms {
    if let Some(shaders) = renderer.egl_context().user_data().get::<GlesPrograms>() {
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
    let shaders = GlesPrograms {
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
