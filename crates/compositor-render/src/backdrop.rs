//! The menu bar backdrop: in macOS 27 the bar has no material, but above the desktop
//! wallpaper there is a gradient that compresses the highlights in the first ~120 pt
//! (`plan/reference/glass.md`, "Menu bar · gradient"; identical in light and dark).
//! It applies to the desktop wallpaper, below the windows.

use std::cell::RefCell;

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformType,
    UniformValue,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::glass::render_to_texture;

/// Height of the effect: it disappears at y ≈ 115–120 pt.
pub const HEIGHT_PT: f64 = 120.0;

/// Levels after the gradient: rows y 0, 30, 45, 60, 80, 100, 120 pt; columns 96, 112,
/// …, 240, 255. From the measured table; where the capture was covered by widgets it uses
/// 96 → 96 (below ~108 the wallpaper does not change) and white (255) is extrapolated from the
/// two previous columns, except row 0 (white → 176, measured).
pub const TABLE: [[f32; 11]; 7] = [
    [
        96., 110., 112., 116., 121., 127., 135., 144., 154., 165., 176.,
    ],
    [
        96., 110., 113., 118., 124., 131., 140., 149., 160., 171., 181.,
    ],
    [
        96., 111., 116., 123., 130., 139., 148., 159., 171., 183., 194.,
    ],
    [
        96., 111., 119., 128., 138., 148., 160., 172., 185., 198., 210.,
    ],
    [
        96., 111., 124., 136., 149., 163., 177., 191., 205., 220., 234.,
    ],
    [
        96., 112., 127., 142., 157., 172., 188., 203., 219., 235., 250.,
    ],
    [
        96., 112., 128., 144., 160., 176., 192., 208., 224., 240., 255.,
    ],
];

pub const SHADER: &str = include_str!("shaders/tone.frag");
pub const UNIFORMS: [(&str, UniformType); 3] = [
    ("tone", UniformType::Matrix4x4),
    ("tex_height", UniformType::_1f),
    ("scale_px", UniformType::_1f),
];

fn table_uniform() -> UniformValue {
    let mut flat = [0.0_f32; 80];
    for (slot, value) in flat.iter_mut().zip(TABLE.iter().flatten()) {
        *slot = *value;
    }
    let matrices = flat.as_chunks::<16>().0.to_vec();
    UniformValue::Matrix4x4 {
        matrices,
        transpose: false,
    }
}

type Signature = Vec<(Id, CommitCounter, Rectangle<i32, Physical>)>;

/// The backdrop between frames: it is redrawn only if the wallpaper changes.
#[derive(Default)]
pub struct Backdrop {
    state: RefCell<Option<Cached>>,
}

/// The wallpaper the texture was made for, the texture, the element's id and commit.
type Cached = (
    Signature,
    Rectangle<i32, Physical>,
    GlesTexture,
    Id,
    CommitCounter,
);

impl Backdrop {
    /// The backdrop element over `behind` (the desktop wallpaper, from the front),
    /// as wide as the output.
    pub fn element<E: RenderElement<GlesRenderer>>(
        &self,
        renderer: &mut GlesRenderer,
        program: &GlesTexProgram,
        behind: &[E],
        output: Size<i32, Physical>,
        scale: f64,
        clear_color: [f32; 4],
    ) -> Result<ToneElement, GlesError> {
        let height = ((HEIGHT_PT * scale).ceil() as i32).min(output.h);
        let area = Rectangle::new(Point::from((0, 0)), Size::from((output.w, height)));
        let signature: Signature = behind
            .iter()
            .filter(|e| e.geometry(Scale::from(scale)).overlaps(area))
            .map(|e| {
                (
                    e.id().clone(),
                    e.current_commit(),
                    e.geometry(Scale::from(scale)),
                )
            })
            .collect();
        let mut state = self.state.borrow_mut();
        let fresh = !matches!(state.as_ref(), Some((s, a, ..)) if *s == signature && *a == area);
        if fresh {
            let texture = render_to_texture(renderer, behind, area, scale, clear_color)?;
            let (id, mut commit) = match state.take() {
                Some((.., id, commit)) => (id, commit),
                None => (Id::new(), CommitCounter::default()),
            };
            commit.increment();
            *state = Some((signature, area, texture, id, commit));
        }
        let Some((_, area, texture, id, commit)) = state.as_ref() else {
            return Err(GlesError::FramebufferBindingError);
        };
        Ok(ToneElement {
            id: id.clone(),
            commit: *commit,
            texture: texture.clone(),
            area: *area,
            scale,
            program: program.clone(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ToneElement {
    id: Id,
    commit: CommitCounter,
    texture: GlesTexture,
    area: Rectangle<i32, Physical>,
    scale: f64,
    program: GlesTexProgram,
}

impl Element for ToneElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(
            self.area
                .size
                .to_logical(1)
                .to_buffer(1, Transform::Normal)
                .to_f64(),
        )
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.area
    }

    fn opaque_regions(
        &self,
        _scale: Scale<f64>,
    ) -> smithay::backend::renderer::utils::OpaqueRegions<i32, Physical> {
        // It covers the wallpaper entirely: what is below does not need redrawing.
        smithay::backend::renderer::utils::OpaqueRegions::from_slice(&[Rectangle::from_size(
            self.area.size,
        )])
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for ToneElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let uniforms = [
            Uniform::new("tone", table_uniform()),
            Uniform::new("tex_height", UniformValue::_1f(self.area.size.h as f32)),
            Uniform::new("scale_px", UniformValue::_1f(self.scale as f32)),
        ];
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            1.0,
            Some(&self.program),
            &uniforms,
        )
    }
}

/// A blurred capture (the spaces bar material), drawn at `dst`.
#[derive(Debug, Clone)]
pub struct BlurredElement {
    id: Id,
    texture: GlesTexture,
    src: Rectangle<f64, Buffer>,
    dst: Rectangle<i32, Physical>,
    alpha: f32,
}

impl BlurredElement {
    pub fn new(
        id: Id,
        texture: GlesTexture,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        alpha: f32,
    ) -> Self {
        Self {
            id,
            texture,
            src,
            dst,
            alpha,
        }
    }
}

impl Element for BlurredElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        CommitCounter::default()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.dst
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for BlurredElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
            None,
            &[],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_gray_becomes_the_measured_bar_background() {
        // #7a7b80 (luminance ~122.9) at the top becomes ~111 (#6f7075): between columns 112 and 128.
        let l = 0.2126 * 122.0 + 0.7152 * 123.0 + 0.0722 * 128.0;
        let f = (l - 112.0) / 16.0;
        let top = TABLE[0][1] + (TABLE[0][2] - TABLE[0][1]) * f;
        assert!((top - 111.0).abs() < 1.0, "{top}");
    }
}
