//! Pixel-shader elements kept between frames: same id and commit counter as long as
//! area and parameters do not change, so the damage tracker redraws them only when
//! needed.

use std::cell::RefCell;

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::Uniform;
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

use crate::Program;
use crate::renderer::{SilicaError, SilicaFrame, SilicaRenderer};

/// An area drawn by a pixel program from its parameters (the uniforms of the GLSL shader,
/// which the CPU renderer reads the same).
#[derive(Debug, Clone)]
pub struct ShaderElement {
    program: Program,
    id: Id,
    commit: CommitCounter,
    area: Rectangle<i32, Logical>,
    alpha: f32,
    uniforms: Vec<Uniform<'static>>,
    kind: Kind,
}

impl ShaderElement {
    pub fn new(
        program: Program,
        area: Rectangle<i32, Logical>,
        alpha: f32,
        uniforms: Vec<Uniform<'static>>,
        kind: Kind,
    ) -> Self {
        Self {
            program,
            id: Id::new(),
            commit: CommitCounter::default(),
            area,
            alpha,
            uniforms,
            kind,
        }
    }

    pub fn resize(&mut self, area: Rectangle<i32, Logical>) {
        if self.area != area {
            self.area = area;
            self.commit.increment();
        }
    }

    pub fn update_uniforms(&mut self, uniforms: Vec<Uniform<'static>>) {
        self.uniforms = uniforms;
        self.commit.increment();
    }
}

impl Element for ShaderElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.area.size.to_f64().to_buffer(1.0, Transform::Normal))
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.area.to_physical_precise_round(scale)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        self.kind
    }
}

impl RenderElement<SilicaRenderer> for ShaderElement {
    fn draw(
        &self,
        frame: &mut SilicaFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), SilicaError> {
        match frame {
            SilicaFrame::Gles(frame) => {
                let Some(program) =
                    crate::frame_programs(frame).and_then(|p| p.pixel(self.program).cloned())
                else {
                    return Ok(());
                };
                Ok(frame.render_pixel_shader_to(
                    &program,
                    src,
                    dst,
                    self.area.size.to_buffer(1, Transform::Normal),
                    Some(damage),
                    self.alpha,
                    &self.uniforms,
                )?)
            }
            // No CPU program yet: `shaders` reports it missing, so no element is made.
            SilicaFrame::Cpu(_) => Ok(()),
        }
    }

    fn underlying_storage(&self, _renderer: &mut SilicaRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

pub struct ShaderCache<K> {
    state: RefCell<Option<Cached<K>>>,
}

struct Cached<K> {
    element: ShaderElement,
    area: Rectangle<i32, Logical>,
    key: K,
}

impl<K> Default for ShaderCache<K> {
    fn default() -> Self {
        Self {
            state: RefCell::new(None),
        }
    }
}

impl<K: PartialEq + Copy> ShaderCache<K> {
    /// The element for this area and these parameters; `uniforms` is computed only if they change.
    pub fn element(
        &self,
        program: Program,
        area: Rectangle<i32, Logical>,
        key: K,
        uniforms: impl FnOnce() -> Vec<Uniform<'static>>,
    ) -> ShaderElement {
        let mut state = self.state.borrow_mut();
        match state.as_mut() {
            Some(cached) if cached.area == area && cached.key == key => cached.element.clone(),
            Some(cached) => {
                cached.element.resize(area);
                cached.element.update_uniforms(uniforms());
                cached.area = area;
                cached.key = key;
                cached.element.clone()
            }
            None => {
                let element = ShaderElement::new(program, area, 1.0, uniforms(), Kind::Unspecified);
                *state = Some(Cached {
                    element: element.clone(),
                    area,
                    key,
                });
                element
            }
        }
    }
}
