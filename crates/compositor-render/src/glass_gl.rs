//! The GLES glass blur, drawn inside the frame.
//!
//! The elements are drawn from the back, so when a glass element draws, what is behind it is
//! already in the frame's framebuffer: it is copied from there into the texture the material
//! reads and blurred in place, with plain GL passes into textures kept from frame to frame. No
//! offscreen frame draws the backdrop again, and no pass makes a framebuffer object, a fence or
//! a flush of its own: the blur goes to the GPU with the frame. The passes are those of
//! `glass::gaussian` and `Blurred::back`, with the same shaders, so the pixels are the same.

#![allow(unsafe_code)]

use std::cell::Cell;
use std::ffi::CString;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexture, ffi, link_program,
};
use smithay::backend::renderer::{Offscreen, Texture};
use smithay::utils::{Physical, Point, Rectangle, Size};

use crate::glass::{
    BLUR_SHADER, DOWNSAMPLE_SHADER, REDUCED_FORMAT, UPSAMPLE_SHADER, buffer_size, effective_factor,
    reduced_sigma,
};

/// The vertex shader of the passes: the unit square covers the target, and `src` is the part
/// of the source it reads, in texture coordinates.
const VERTEX: &str = "#version 100
attribute vec2 vert;
uniform vec4 src;
varying vec2 v_coords;
void main() {
    v_coords = src.xy + vert * src.zw;
    gl_Position = vec4(vert * 2.0 - 1.0, 0.0, 1.0);
}
";

/// A pass's program and where its inputs go.
#[derive(Debug, Clone)]
struct Pass {
    program: u32,
    vert: u32,
    tex: i32,
    src: i32,
    alpha: i32,
    /// The shader's own uniforms, in the order the pass gives their values.
    uniforms: Vec<i32>,
}

/// A value of a pass's own uniform.
#[derive(Debug, Clone, Copy)]
enum Value {
    F1(f32),
    F2(f32, f32),
}

/// The programs of the blur passes in one GL context, and what the context can do.
#[derive(Debug, Clone)]
pub struct BlurPrograms {
    downsample: Pass,
    blur: Pass,
    upsample: Pass,
    /// The unit square, as a triangle strip.
    quad: u32,
    /// Half floats can be drawn into: the reduced blur keeps them (`glass::REDUCED_FORMAT`).
    half_float: bool,
}

/// Compiles the passes, if the context is GLES 3 (the copy out of the frame is a blit): `None`
/// leaves the glass to the offscreen blur.
pub(crate) fn compile(renderer: &mut GlesRenderer) -> Option<BlurPrograms> {
    // A half-float texture to find out whether the reduced blur can draw into one.
    let probe = Offscreen::<GlesTexture>::create_buffer(
        renderer,
        REDUCED_FORMAT,
        buffer_size(Size::from((4, 4))),
    )
    .ok();
    renderer
        .with_context(|gl| {
            // SAFETY: inside `with_context` the context is current; GL_VERSION is a valid name
            // and the string it returns lives with the context.
            let version = unsafe { gl.GetString(ffi::VERSION) };
            if version.is_null()
                // SAFETY: not null, so a NUL-terminated string.
                || !unsafe { std::ffi::CStr::from_ptr(version.cast()) }
                    .to_bytes()
                    .starts_with(b"OpenGL ES 3")
                || !gl.BlitFramebuffer.is_loaded()
                || !gl.VertexAttribDivisor.is_loaded()
            {
                return None;
            }
            let pass = |name: &str, frag: &str, uniforms: &[&str]| {
                // SAFETY: the context is current.
                let program = unsafe { link_program(gl, VERTEX, frag) }
                    .inspect_err(|err| tracing::warn!("glass {name} pass: {err}"))
                    .ok()?;
                let location = |name: &str| {
                    let name = CString::new(name).ok()?;
                    // SAFETY: the context is current, `program` is linked and `name` is
                    // NUL-terminated.
                    Some(unsafe { gl.GetUniformLocation(program, name.as_ptr()) })
                };
                // SAFETY: as above.
                let vert = unsafe { gl.GetAttribLocation(program, c"vert".as_ptr()) };
                Some(Pass {
                    program,
                    vert: u32::try_from(vert).ok()?,
                    tex: location("tex")?,
                    src: location("src")?,
                    alpha: location("alpha")?,
                    uniforms: uniforms
                        .iter()
                        .map(|u| location(u))
                        .collect::<Option<_>>()?,
                })
            };
            let downsample = pass(
                "downsample",
                DOWNSAMPLE_SHADER,
                &["src_size", "out_size", "factor", "pad", "mirror"],
            )?;
            let blur = pass("blur", BLUR_SHADER, &["step", "sigma"])?;
            let upsample = pass("upsample", UPSAMPLE_SHADER, &["tex_size"])?;
            let square: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
            let mut quad = 0;
            // SAFETY: the context is current; one buffer name is written into `quad`, and the
            // data pointer covers the size given.
            unsafe {
                gl.GenBuffers(1, &mut quad);
                gl.BindBuffer(ffi::ARRAY_BUFFER, quad);
                gl.BufferData(
                    ffi::ARRAY_BUFFER,
                    std::mem::size_of_val(&square) as isize,
                    square.as_ptr().cast(),
                    ffi::STATIC_DRAW,
                );
                gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            }
            let half_float = probe.as_ref().is_some_and(|probe| {
                let mut fbo = 0;
                // SAFETY: the context is current; the framebuffer object made here is
                // deleted before returning and the frame's binding is restored.
                unsafe {
                    let mut bound = 0;
                    gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut bound);
                    gl.GenFramebuffers(1, &mut fbo);
                    gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
                    gl.FramebufferTexture2D(
                        ffi::FRAMEBUFFER,
                        ffi::COLOR_ATTACHMENT0,
                        ffi::TEXTURE_2D,
                        probe.tex_id(),
                        0,
                    );
                    let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
                    gl.BindFramebuffer(ffi::FRAMEBUFFER, u32::try_from(bound).unwrap_or(0));
                    gl.DeleteFramebuffers(1, &fbo);
                    status == ffi::FRAMEBUFFER_COMPLETE
                }
            });
            Some(BlurPrograms {
                downsample,
                blur,
                upsample,
                quad,
                half_float,
            })
        })
        .ok()
        .flatten()
}

/// The blur of one glass texture: its size, σ and sharp share, as `glass::blur` takes them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Plan {
    /// The captured area, the glass's bounding box.
    pub size: Size<i32, Physical>,
    /// σ in pixels of the output.
    pub sigma: f64,
    /// The reduction asked for (`effective_factor` decides).
    pub factor: u32,
    /// The share of the sharp backdrop left under the blur.
    pub sharp: f32,
}

/// What a reduced blur adds to its plan: as in `glass::reduced`.
#[derive(Debug, Clone, Copy)]
struct Reduced {
    factor: u32,
    sigma: f64,
    pad: i32,
    size: Size<i32, Physical>,
}

impl Plan {
    fn reduced(&self, half_float: bool) -> Option<Reduced> {
        let factor = effective_factor(self.sigma, self.factor);
        if factor <= 1 || !half_float {
            return None;
        }
        let f = factor as i32;
        let sigma = reduced_sigma(self.sigma, factor);
        let pad = (3.0 * sigma).ceil() as i32 + 1;
        let size = Size::from((
            (self.size.w.max(1) + f - 1) / f + 2 * pad,
            (self.size.h.max(1) + f - 1) / f + 2 * pad,
        ));
        Some(Reduced {
            factor,
            sigma,
            pad,
            size,
        })
    }
}

/// The textures of one blur, kept while its plan stays.
#[derive(Debug)]
pub(crate) struct Blur {
    pub plan: Plan,
    /// The backdrop copied out of the frame and blurred in place: what the material reads.
    pub texture: GlesTexture,
    /// The passes' own textures: reduced, or as large as `texture`.
    scratch: [GlesTexture; 2],
    reduced: Option<Reduced>,
}

impl Blur {
    pub(crate) fn new(
        renderer: &mut GlesRenderer,
        programs: &BlurPrograms,
        plan: Plan,
    ) -> Result<Blur, GlesError> {
        let reduced = plan.reduced(programs.half_float);
        let (format, size) = match reduced {
            Some(reduced) => (REDUCED_FORMAT, reduced.size),
            None => (Fourcc::Abgr8888, plan.size),
        };
        let mut make = |format, size| {
            Offscreen::<GlesTexture>::create_buffer(renderer, format, buffer_size(size))
        };
        Ok(Blur {
            plan,
            texture: make(Fourcc::Abgr8888, plan.size)?,
            scratch: [make(format, size)?, make(format, size)?],
            reduced,
        })
    }
}

/// Copies what the frame drew at `dst` (the glass's bounding box in the frame, as large as the
/// plan) into `blur.texture` and blurs it there. The GL state is left as the frame had it.
/// False if `dst` is not wholly inside the frame's framebuffer: nothing is drawn.
pub(crate) fn capture(
    gl: &ffi::Gles2,
    programs: &BlurPrograms,
    blur: &Blur,
    dst: Rectangle<i32, Physical>,
) -> bool {
    // SAFETY: called inside `GlesFrame::with_context`, so the frame's context is current with
    // its framebuffer bound. Every name used is this context's: the programs and the quad
    // from `compile`, the textures from its renderer, and a framebuffer object made and
    // deleted here. The state changed is saved first and restored at the end.
    unsafe {
        let mut viewport = [0; 4];
        gl.GetIntegerv(ffi::VIEWPORT, viewport.as_mut_ptr());
        let [x, y, w, h] = viewport;
        let inside = Rectangle::new(Point::from((x, y)), Size::from((w, h)));
        if dst.size != blur.plan.size || inside.intersection(dst) != Some(dst) {
            return false;
        }
        let mut frame = 0;
        gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut frame);
        let mut array = 0;
        gl.GetIntegerv(ffi::ARRAY_BUFFER_BINDING, &mut array);
        let mut program = 0;
        gl.GetIntegerv(ffi::CURRENT_PROGRAM, &mut program);
        let mut active = 0;
        gl.GetIntegerv(ffi::ACTIVE_TEXTURE, &mut active);
        let scissor = gl.IsEnabled(ffi::SCISSOR_TEST) == ffi::TRUE;
        let blend = gl.IsEnabled(ffi::BLEND) == ffi::TRUE;

        gl.Disable(ffi::SCISSOR_TEST);
        gl.Disable(ffi::BLEND);
        let mut fbo = 0;
        gl.GenFramebuffers(1, &mut fbo);
        // The copy: the frame stays the read framebuffer.
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
        gl.FramebufferTexture2D(
            ffi::DRAW_FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            blur.texture.tex_id(),
            0,
        );
        let (x0, y0) = (dst.loc.x, dst.loc.y);
        let (x1, y1) = (x0 + dst.size.w, y0 + dst.size.h);
        gl.BlitFramebuffer(
            x0,
            y0,
            x1,
            y1,
            0,
            0,
            dst.size.w,
            dst.size.h,
            ffi::COLOR_BUFFER_BIT,
            ffi::NEAREST,
        );
        gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
        gl.BindBuffer(ffi::ARRAY_BUFFER, programs.quad);
        gl.ActiveTexture(ffi::TEXTURE0);
        passes(gl, programs, blur);

        gl.DeleteFramebuffers(1, &fbo);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, u32::try_from(frame).unwrap_or(0));
        gl.Viewport(x, y, w, h);
        if scissor {
            gl.Enable(ffi::SCISSOR_TEST);
        }
        if blend {
            gl.Enable(ffi::BLEND);
        } else {
            gl.Disable(ffi::BLEND);
        }
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.ActiveTexture(u32::try_from(active).unwrap_or(ffi::TEXTURE0));
        gl.BindBuffer(ffi::ARRAY_BUFFER, u32::try_from(array).unwrap_or(0));
        gl.UseProgram(u32::try_from(program).unwrap_or(0));
    }
    true
}

/// The passes of `glass::blur` over `blur.texture`, which holds the backdrop: the framebuffer
/// object, the quad and the texture unit are bound, blending is off.
///
/// # Safety
///
/// As in `capture`.
unsafe fn passes(gl: &ffi::Gles2, programs: &BlurPrograms, blur: &Blur) {
    let size = blur.plan.size;
    let [first, second] = &blur.scratch;
    // SAFETY: as in `capture`.
    unsafe {
        let (blurred, src, tex_size) = match blur.reduced {
            Some(reduced) => {
                let small = reduced.size;
                draw(
                    gl,
                    &programs.downsample,
                    &blur.texture,
                    WHOLE,
                    first,
                    1.0,
                    &[
                        Value::F2(size.w as f32, size.h as f32),
                        Value::F2(small.w as f32, small.h as f32),
                        Value::F1(reduced.factor as f32),
                        Value::F1(reduced.pad as f32),
                        // The edge repeats (`BlurEdge::Clamp`).
                        Value::F1(0.0),
                    ],
                );
                gaussian(gl, programs, first, second, first, small, reduced.sigma);
                // The source covers size / factor of the reduced texture, inside the border.
                let f = f64::from(reduced.factor);
                let src = [
                    reduced.pad as f32 / small.w as f32,
                    reduced.pad as f32 / small.h as f32,
                    (f64::from(size.w) / f / f64::from(small.w)) as f32,
                    (f64::from(size.h) / f / f64::from(small.h)) as f32,
                ];
                (first, src, small)
            }
            None => {
                gaussian(
                    gl,
                    programs,
                    &blur.texture,
                    first,
                    second,
                    size,
                    blur.plan.sigma,
                );
                // At full size the upsampling reads texel centres: a plain copy.
                (second, WHOLE, size)
            }
        };
        // Back at full size over the sharp backdrop: w · sharp + (1 − w) · blurred.
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        draw(
            gl,
            &programs.upsample,
            blurred,
            src,
            &blur.texture,
            1.0 - blur.plan.sharp,
            &[Value::F2(tex_size.w as f32, tex_size.h as f32)],
        );
        gl.Disable(ffi::BLEND);
    }
}

/// The whole of a texture, as the part a pass reads.
const WHOLE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// The horizontal and vertical passes of `glass::blur_passes`, σ `sigma` texels over `source`
/// (`size` large): across into `across`, then down into `down` (which may be `source`).
///
/// # Safety
///
/// As in `capture`.
unsafe fn gaussian(
    gl: &ffi::Gles2,
    programs: &BlurPrograms,
    source: &GlesTexture,
    across: &GlesTexture,
    down: &GlesTexture,
    size: Size<i32, Physical>,
    sigma: f64,
) {
    let sigma = Value::F1(sigma as f32);
    // SAFETY: as in `capture`.
    unsafe {
        draw(
            gl,
            &programs.blur,
            source,
            WHOLE,
            across,
            1.0,
            &[Value::F2(1.0 / size.w as f32, 0.0), sigma],
        );
        draw(
            gl,
            &programs.blur,
            across,
            WHOLE,
            down,
            1.0,
            &[Value::F2(0.0, 1.0 / size.h as f32), sigma],
        );
    }
}

/// Draws the part `src` of `source` through `pass` over the whole of `target`.
///
/// # Safety
///
/// As in `capture`, with the framebuffer object bound.
unsafe fn draw(
    gl: &ffi::Gles2,
    pass: &Pass,
    source: &GlesTexture,
    src: [f32; 4],
    target: &GlesTexture,
    alpha: f32,
    values: &[Value],
) {
    // SAFETY: as in `capture`.
    unsafe {
        gl.FramebufferTexture2D(
            ffi::FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            target.tex_id(),
            0,
        );
        let size = target.size();
        gl.Viewport(0, 0, size.w, size.h);
        gl.BindTexture(ffi::TEXTURE_2D, source.tex_id());
        // The filters smithay sets before each draw (it leaves the wrapping alone).
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
        gl.UseProgram(pass.program);
        gl.Uniform1i(pass.tex, 0);
        gl.Uniform4f(pass.src, src[0], src[1], src[2], src[3]);
        gl.Uniform1f(pass.alpha, alpha);
        for (location, value) in pass.uniforms.iter().zip(values) {
            match *value {
                Value::F1(a) => gl.Uniform1f(*location, a),
                Value::F2(a, b) => gl.Uniform2f(*location, a, b),
            }
        }
        gl.EnableVertexAttribArray(pass.vert);
        // Smithay draws instanced: one vertex per corner here, whatever divisor it left.
        gl.VertexAttribDivisor(pass.vert, 0);
        gl.VertexAttribPointer(pass.vert, 2, ffi::FLOAT, ffi::FALSE, 0, std::ptr::null());
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        gl.DisableVertexAttribArray(pass.vert);
    }
}

/// The blurs of one glass cache made inside the frame: once per backdrop (`invalidate`), by
/// the first of the cache's elements the frame draws, for all of them.
#[derive(Debug)]
pub(crate) struct Capture {
    programs: BlurPrograms,
    /// One per blur key, in the order of the cache's textures.
    blurs: Vec<Blur>,
    /// The backdrop the textures must hold, and the one they hold.
    wanted: Cell<u64>,
    done: Cell<u64>,
    /// A draw found the textures out of date and could not make them.
    failed: Cell<bool>,
}

impl Capture {
    pub(crate) fn new(
        renderer: &mut GlesRenderer,
        programs: BlurPrograms,
        plans: &[Plan],
    ) -> Result<Capture, GlesError> {
        let blurs = plans
            .iter()
            .map(|plan| Blur::new(renderer, &programs, *plan))
            .collect::<Result<_, _>>()?;
        Ok(Capture {
            programs,
            blurs,
            wanted: Cell::new(1),
            done: Cell::new(0),
            failed: Cell::new(false),
        })
    }

    /// Whether these are the blurs of `plans`.
    pub(crate) fn plans(&self, plans: &[Plan]) -> bool {
        self.blurs.len() == plans.len() && self.blurs.iter().zip(plans).all(|(b, p)| b.plan == *p)
    }

    /// The backdrop changed: the next draw makes the blurs again.
    pub(crate) fn invalidate(&self) {
        self.wanted.set(self.wanted.get().wrapping_add(1));
        self.failed.set(false);
    }

    /// The blurred textures, in the order of the plans.
    pub(crate) fn textures(&self) -> impl Iterator<Item = &GlesTexture> {
        self.blurs.iter().map(|blur| &blur.texture)
    }

    /// A draw left the textures out of date: the cache must make them another way.
    pub(crate) fn failed(&self) -> bool {
        self.failed.get() && self.done.get() != self.wanted.get()
    }

    /// Makes the blurs from the frame, if they are out of date: `dst` is the glass's bounding
    /// box in the frame and `damage` (relative to it) what the frame drew again there.
    pub(crate) fn run(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
    ) {
        let wanted = self.wanted.get();
        if self.done.get() == wanted {
            return;
        }
        // Only where the frame drew the whole box again: elsewhere its framebuffer may hold an
        // older frame, the glass included.
        let whole = Rectangle::from_size(dst.size);
        let redrawn = Rectangle::subtract_rects_many([whole], damage.iter().copied()).is_empty();
        let made = redrawn
            && frame
                .with_context(|gl| {
                    self.blurs
                        .iter()
                        .all(|blur| capture(gl, &self.programs, blur, dst))
                })
                .unwrap_or(false);
        if !made && std::env::var_os("PROBE").is_some() {
            eprintln!("PROBE fail dst={dst:?} redrawn={redrawn} damage={damage:?}");
        }
        if made {
            self.done.set(wanted);
        } else {
            self.failed.set(true);
        }
    }
}
