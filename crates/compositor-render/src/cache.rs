//! Pixel-shader elements kept between frames: same id and commit counter as long as
//! area and parameters do not change, so the damage tracker redraws them only when
//! needed.

use std::cell::RefCell;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{GlesPixelProgram, Uniform};
use smithay::utils::{Logical, Rectangle};

pub struct ShaderCache<K> {
    state: RefCell<Option<Cached<K>>>,
}

struct Cached<K> {
    element: PixelShaderElement,
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
        program: &GlesPixelProgram,
        area: Rectangle<i32, Logical>,
        key: K,
        uniforms: impl FnOnce() -> Vec<Uniform<'static>>,
    ) -> PixelShaderElement {
        let mut state = self.state.borrow_mut();
        match state.as_mut() {
            Some(cached) if cached.area == area && cached.key == key => cached.element.clone(),
            Some(cached) => {
                cached.element.resize(area, None);
                cached.element.update_uniforms(uniforms());
                cached.area = area;
                cached.key = key;
                cached.element.clone()
            }
            None => {
                let element = PixelShaderElement::new(
                    program.clone(),
                    area,
                    None,
                    1.0,
                    uniforms(),
                    Kind::Unspecified,
                );
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
