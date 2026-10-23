//! The display filters of Accessibility: invert, grayscale and increased contrast, applied to
//! the whole frame.

pub const SHADER: &str = include_str!("shaders/filter.frag");
pub const UNIFORMS: [(&str, smithay::backend::renderer::gles::UniformType); 2] = {
    use smithay::backend::renderer::gles::UniformType;
    [("mode", UniformType::_1f), ("contrast", UniformType::_1f)]
};

use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::Uniform;
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};

use crate::Program;
use crate::renderer::{SilicaError, SilicaFrame, SilicaRenderer, SilicaTexture};

/// The whole frame, drawn through the filter program.
#[derive(Debug)]
pub struct FilterElement {
    inner: TextureRenderElement<SilicaTexture>,
    id: Id,
    program: Program,
    uniforms: Vec<Uniform<'static>>,
}

impl FilterElement {
    pub fn new(
        inner: TextureRenderElement<SilicaTexture>,
        program: Program,
        uniforms: Vec<Uniform<'static>>,
    ) -> Self {
        Self {
            inner,
            id: Id::new(),
            program,
            uniforms,
        }
    }
}

impl Element for FilterElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.inner.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
}

impl RenderElement<SilicaRenderer> for FilterElement {
    fn draw(
        &self,
        frame: &mut SilicaFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), SilicaError> {
        // No CPU program yet: `shaders` reports it missing, so no element is made.
        let SilicaFrame::Gles(gles) = &mut *frame else {
            return Ok(());
        };
        let Some(program) = crate::frame_programs(gles).and_then(|p| p.texture(self.program))
        else {
            return Ok(());
        };
        gles.override_default_tex_program(program.clone(), self.uniforms.clone());
        let result = RenderElement::<SilicaRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
        );
        if let SilicaFrame::Gles(gles) = frame {
            gles.clear_tex_program_override();
        }
        result
    }

    fn underlying_storage(&self, _renderer: &mut SilicaRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
