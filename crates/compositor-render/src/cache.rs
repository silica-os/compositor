//! Pixel-shader elements kept between frames: same id and commit counter as long as
//! area and parameters do not change, so the damage tracker redraws them only when
//! needed.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, PoisonError};

use compositor_raster::{CpuRenderer, CpuTexture};
use smithay::backend::renderer::Frame;

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::Uniform;
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

use crate::Program;
use crate::cpu::pixel::PixelProgram;
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
    /// On the CPU, the program painted once, shared by the clones of the element.
    painted: Arc<Mutex<Option<Painted>>>,
}

/// A pixel program painted into a texture, and what it was painted for.
#[derive(Debug)]
struct Painted {
    commit: CommitCounter,
    src: Rectangle<f64, Buffer>,
    size: smithay::utils::Size<i32, Physical>,
    texture: CpuTexture,
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
            painted: Arc::default(),
        }
    }

    /// The program painted over `src` at `size` pixels: kept while the element and the size do
    /// not change.
    fn painted(
        &self,
        renderer: &CpuRenderer,
        src: Rectangle<f64, Buffer>,
        size: smithay::utils::Size<i32, Physical>,
    ) -> Option<CpuTexture> {
        let mut painted = self.painted.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = painted.as_ref()
            && p.commit == self.commit
            && p.src == src
            && p.size == size
        {
            return Some(p.texture.clone());
        }
        let program = PixelProgram::new(self.program, &self.uniforms)?;
        let texture = CpuTexture::new(
            u32::try_from(size.w).ok()?,
            u32::try_from(size.h).ok()?,
            smithay::backend::allocator::Fourcc::Argb8888,
        )
        .ok()?;
        program.paint(
            renderer,
            [self.area.size.w as f32, self.area.size.h as f32],
            [
                src.loc.x as f32,
                src.loc.y as f32,
                src.size.w as f32,
                src.size.h as f32,
            ],
            &mut texture.write(),
        );
        *painted = Some(Painted {
            commit: self.commit,
            src,
            size,
            texture: texture.clone(),
        });
        Some(texture)
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
            // Painted once into a texture, then composited.
            SilicaFrame::Cpu(frame) => {
                let Some(texture) = self.painted(frame.renderer(), src, dst.size) else {
                    return Ok(());
                };
                let whole = Rectangle::from_size(
                    smithay::utils::Size::<i32, Buffer>::from((dst.size.w, dst.size.h)).to_f64(),
                );
                Ok(frame.render_texture_from_to(
                    &texture,
                    whole,
                    dst,
                    damage,
                    &[],
                    Transform::Normal,
                    self.alpha,
                )?)
            }
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
