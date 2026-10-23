//! Window corners: superellipse measured on macOS 27 (`plan/reference/windows.md`,
//! "Windows · shape and corners"), applied with a shader to the window surfaces.

use smithay::backend::renderer::Frame;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{Uniform, UniformValue};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

use std::sync::Arc;

use crate::Program;
use crate::cpu::shade::Corners;
use crate::renderer::{SilicaError, SilicaFrame, SilicaRenderer};

/// Superellipse |x/r|ⁿ + |y/r|ⁿ = 1 that stands for the Amalfi window corner: radius 18 with
/// smoothing 0.6 (the kit's smooth corner, `tokens.json` `radii.window`). Fitted to the
/// smoothed-corner curve (a 28.8 pt long cubic-arc-cubic path), RMS error 0.05 pt. In logical
/// points.
pub const CORNER_RADIUS: f64 = 21.5;
pub const CORNER_EXPONENT: f32 = 2.5;

/// The radius in physical pixels for a window of this size: not more than half the
/// shorter side, so that the corner boxes do not overlap.
pub fn radius_for(geometry: Size<f64, Physical>, scale: f64) -> f64 {
    (CORNER_RADIUS * scale)
        .min(geometry.w / 2.0)
        .min(geometry.h / 2.0)
        .max(0.0)
}

pub const SHADER: &str = include_str!("shaders/corners.frag");
pub const UNIFORMS: [(&str, smithay::backend::renderer::gles::UniformType); 5] = {
    use smithay::backend::renderer::gles::UniformType;
    [
        ("frag_to_local_x", UniformType::_3f),
        ("frag_to_local_y", UniformType::_3f),
        ("geo_size", UniformType::_2f),
        ("radius", UniformType::_1f),
        ("exponent", UniformType::_1f),
    ]
};

/// The two rows of the affine transform from gl_FragCoord to the position in the window.
/// The frame draws the output with `transform` (a flipped GL framebuffer: Flipped180, capture: Normal), so
/// the framebuffer coordinates are brought back to the output with the inverse.
pub(crate) fn frag_to_local(
    output: Size<i32, Physical>,
    window: Rectangle<f64, Physical>,
    transform: Transform,
) -> ([f32; 3], [f32; 3]) {
    let framebuffer = transform.transform_size(output.to_f64());
    let inverse = transform.invert();
    let map = |x: f64, y: f64| {
        inverse.transform_point_in(Point::<f64, Physical>::from((x, y)), &framebuffer)
    };
    let origin = map(0.0, 0.0);
    let along_x = map(1.0, 0.0) - origin;
    let along_y = map(0.0, 1.0) - origin;
    let offset = origin - window.loc;
    (
        [along_x.x as f32, along_y.x as f32, offset.x as f32],
        [along_x.y as f32, along_y.y as f32, offset.y as f32],
    )
}

/// A window surface (main or subsurface), scaled around a point during the
/// animations (scale 1 at rest).
pub type WindowSurface = RescaleRenderElement<WaylandSurfaceRenderElement<SilicaRenderer>>;

/// A window surface with the corners cut. Without a program (shader not
/// compiled) it is drawn as it is.
pub struct RoundedSurface {
    inner: WindowSurface,
    /// The window geometry in the output, in physical pixels.
    window: Rectangle<f64, Physical>,
    radius: f64,
    scale: f64,
    /// Output size in physical pixels, before the frame transform.
    output: Size<i32, Physical>,
    program: Option<Program>,
    /// The cell this surface is clipped to ([`RoundedSurface::cropped`]), in the same
    /// space as the element geometry.
    crop: Option<(Rectangle<i32, Physical>, f64)>,
}

impl RoundedSurface {
    pub fn new(
        inner: WindowSurface,
        window: Rectangle<f64, Physical>,
        scale: f64,
        output: Size<i32, Physical>,
        program: Option<Program>,
    ) -> Self {
        let radius = radius_for(window.size, scale);
        Self {
            inner,
            window,
            radius,
            scale,
            output,
            program,
            crop: None,
        }
    }

    /// The surface clipped to `cell` (a space thumbnail). The crop-aware variant: the frame
    /// then draws the cropped geometry, so the shift of the corner shader is measured from
    /// where the cropped element starts, not from the whole surface. `None` when nothing is
    /// left inside the cell.
    pub fn cropped(
        mut self,
        scale: f64,
        cell: Rectangle<i32, Physical>,
    ) -> Option<CropRenderElement<RoundedSurface>> {
        self.crop = Some((cell, scale));
        CropRenderElement::from_element(self, scale, cell)
    }

    /// Where the frame draws the element when nothing moves it: the geometry, cropped.
    fn drawn_origin(&self) -> Point<i32, Physical> {
        let Some((cell, scale)) = self.crop else {
            return self.inner.geometry(Scale::from(self.scale)).loc;
        };
        let geometry = self.inner.geometry(Scale::from(scale));
        match geometry.intersection(cell) {
            Some(cropped) => cropped.loc,
            None => geometry.loc,
        }
    }

    /// The four corner boxes, relative to the element's origin.
    fn corner_squares(&self, scale: Scale<f64>) -> [Rectangle<i32, Physical>; 4] {
        let origin = self.inner.geometry(scale).loc.to_f64();
        let local = self.window.loc - origin;
        let r = self.radius.ceil();
        let (w, h) = (self.window.size.w, self.window.size.h);
        let square = |x: f64, y: f64| {
            Rectangle::new(
                Point::from((x.floor() as i32, y.floor() as i32)),
                Size::from((r as i32 + 1, r as i32 + 1)),
            )
        };
        [
            square(local.x, local.y),
            square(local.x + w - r, local.y),
            square(local.x, local.y + h - r),
            square(local.x + w - r, local.y + h - r),
        ]
    }
}

impl Element for RoundedSurface {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    /// The cut corners are no longer opaque: they are removed from the opaque regions, or the
    /// damage tracker would skip what is below.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let opaque = self.inner.opaque_regions(scale);
        if self.program.is_none() {
            return opaque;
        }
        let corners = self.corner_squares(scale);
        Rectangle::subtract_rects_many(opaque.iter().copied(), corners)
            .into_iter()
            .collect()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

/// The corners a frame cuts from what it draws.
#[derive(Debug, Clone, Copy)]
pub struct Rounding {
    /// The window, in physical pixels of the output.
    pub window: Rectangle<f64, Physical>,
    pub radius: f64,
    /// The output's size before the frame transform.
    pub output: Size<i32, Physical>,
    /// How much the frame moves what it draws relative to the output (glass capture into a
    /// smaller texture, always with the Normal transform).
    pub shift: Point<i32, Physical>,
}

impl Rounding {
    fn uniforms(&self, transform: Transform) -> Vec<Uniform<'static>> {
        let (mut x, mut y) = frag_to_local(self.output, self.window, transform);
        if transform == Transform::Normal {
            x[2] += self.shift.x as f32;
            y[2] += self.shift.y as f32;
        }
        vec![
            Uniform::new("frag_to_local_x", UniformValue::_3f(x[0], x[1], x[2])),
            Uniform::new("frag_to_local_y", UniformValue::_3f(y[0], y[1], y[2])),
            Uniform::new(
                "geo_size",
                UniformValue::_2f(self.window.size.w as f32, self.window.size.h as f32),
            ),
            Uniform::new("radius", UniformValue::_1f(self.radius as f32)),
            Uniform::new("exponent", UniformValue::_1f(CORNER_EXPONENT)),
        ]
    }
}

/// Runs `draw` with the window's corners cut from the textures it draws (the frame's texture
/// program, for the time of `draw`). Without `program`, or on the CPU on a rotated output,
/// the corners stay square.
pub fn draw_rounded(
    frame: &mut SilicaFrame<'_, '_>,
    program: Option<Program>,
    corners: Rounding,
    draw: impl FnOnce(&mut SilicaFrame<'_, '_>) -> Result<(), SilicaError>,
) -> Result<(), SilicaError> {
    let mut overridden = false;
    match (program, &mut *frame) {
        (None, _) => {}
        (Some(program), SilicaFrame::Gles(gles)) => {
            if let Some(texture) = crate::frame_programs(gles).and_then(|p| p.texture(program)) {
                let texture = texture.clone();
                gles.override_default_tex_program(texture, corners.uniforms(gles.transformation()));
                overridden = true;
            }
        }
        (Some(_), SilicaFrame::Cpu(cpu)) => {
            // In the target's pixels: the window's origin there is where the frame moves it.
            let origin = corners.window.loc - corners.shift.to_f64();
            let shader = Corners::new(
                [origin.x, origin.y],
                [corners.window.size.w as f32, corners.window.size.h as f32],
                corners.radius as f32,
                CORNER_EXPONENT,
            );
            overridden = cpu.override_shader(Arc::new(shader)).is_ok();
        }
    }
    let result = draw(frame);
    if overridden {
        match frame {
            SilicaFrame::Gles(gles) => gles.clear_tex_program_override(),
            SilicaFrame::Cpu(cpu) => cpu.clear_shader(),
        }
    }
    result
}

impl RenderElement<SilicaRenderer> for RoundedSurface {
    fn draw(
        &self,
        frame: &mut SilicaFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), SilicaError> {
        let shift = self.drawn_origin() - dst.loc;
        let corners = Rounding {
            window: self.window,
            radius: self.radius,
            output: self.output,
            shift,
        };
        draw_rounded(frame, self.program, corners, |frame| {
            self.inner.draw(frame, src, dst, damage, opaque_regions)
        })
    }

    fn underlying_storage(&self, renderer: &mut SilicaRenderer) -> Option<UnderlyingStorage<'_>> {
        // With the corners cut the buffer cannot be shown as it is (DRM planes).
        if self.program.is_some() {
            return None;
        }
        self.inner.underlying_storage(renderer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies the shader transform to a framebuffer point.
    fn local(rows: ([f32; 3], [f32; 3]), x: f32, y: f32) -> (f32, f32) {
        let (rx, ry) = rows;
        (rx[0] * x + rx[1] * y + rx[2], ry[0] * x + ry[1] * y + ry[2])
    }

    #[test]
    fn fragment_maps_to_window_for_offscreen_and_flipped_frames() {
        let window = Rectangle::new(Point::from((10.0, 5.0)), Size::from((40.0, 20.0)));
        let map = |transform| frag_to_local(Size::from((100, 50)), window, transform);
        // Offscreen capture (Normal): the framebuffer coincides with the output.
        assert_eq!(local(map(Transform::Normal), 10.5, 5.5), (0.5, 0.5));
        // Flipped framebuffer (Flipped180): row 0 of the output is at the top of the GL framebuffer.
        assert_eq!(local(map(Transform::Flipped180), 10.5, 44.5), (0.5, 0.5));
    }

    #[test]
    fn radius_is_clamped_to_half_the_short_side() {
        assert_eq!(radius_for(Size::from((900.0, 600.0)), 1.0), 21.5);
        assert_eq!(radius_for(Size::from((900.0, 600.0)), 2.0), 43.0);
        assert_eq!(radius_for(Size::from((100.0, 40.0)), 1.0), 20.0);
        assert_eq!(radius_for(Size::from((0.0, 0.0)), 1.0), 0.0);
    }
}
