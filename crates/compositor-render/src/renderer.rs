//! The renderer the compositor draws with: GLES (a GPU, or llvmpipe in the tests) or the CPU
//! renderer of `compositor-raster`, behind one type so that the scene and its effects are built
//! once for both. Every call goes to the renderer inside; a texture or a target of the other
//! renderer is an error, never a crash.

use std::fmt;

use compositor_raster::{CpuError, CpuFrame, CpuMapping, CpuRenderer, CpuTarget, CpuTexture};
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesMapping, GlesRenderer, GlesTarget, GlesTexture,
};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{
    Bind, Color32F, ContextId, DebugFlags, ExportMem, Frame, ImportDma, ImportDmaWl, ImportMem,
    ImportMemWl, Offscreen, Renderer, RendererSuper, Texture, TextureFilter, TextureMapping,
};
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_shm};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
use smithay::wayland::compositor::SurfaceData;

/// GLES or the CPU.
#[derive(Debug)]
pub enum SilicaRenderer {
    Gles(Box<GlesRenderer>),
    Cpu(CpuRenderer),
}

impl From<GlesRenderer> for SilicaRenderer {
    fn from(renderer: GlesRenderer) -> Self {
        Self::Gles(Box::new(renderer))
    }
}

impl From<CpuRenderer> for SilicaRenderer {
    fn from(renderer: CpuRenderer) -> Self {
        Self::Cpu(renderer)
    }
}

impl SilicaRenderer {
    /// The GLES renderer, if this is one.
    pub fn gles(&mut self) -> Option<&mut GlesRenderer> {
        match self {
            Self::Gles(renderer) => Some(renderer),
            Self::Cpu(_) => None,
        }
    }

    pub fn as_gles(&self) -> Option<&GlesRenderer> {
        match self {
            Self::Gles(renderer) => Some(renderer),
            Self::Cpu(_) => None,
        }
    }

    /// The CPU renderer, if this is one.
    pub fn cpu(&mut self) -> Option<&mut CpuRenderer> {
        match self {
            Self::Gles(_) => None,
            Self::Cpu(renderer) => Some(renderer),
        }
    }
}

/// A texture of either renderer.
#[derive(Debug, Clone)]
pub enum SilicaTexture {
    Gles(GlesTexture),
    Cpu(CpuTexture),
}

impl SilicaTexture {
    /// Whether the rows run from the bottom (a GL texture drawn into); the CPU renderer keeps
    /// every image from the top.
    pub fn is_y_inverted(&self) -> bool {
        match self {
            Self::Gles(texture) => texture.is_y_inverted(),
            Self::Cpu(_) => false,
        }
    }
}

impl Texture for SilicaTexture {
    fn size(&self) -> Size<i32, Buffer> {
        match self {
            Self::Gles(texture) => texture.size(),
            Self::Cpu(texture) => texture.size(),
        }
    }

    fn width(&self) -> u32 {
        match self {
            Self::Gles(texture) => texture.width(),
            Self::Cpu(texture) => texture.width(),
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Gles(texture) => texture.height(),
            Self::Cpu(texture) => texture.height(),
        }
    }

    fn format(&self) -> Option<Fourcc> {
        match self {
            Self::Gles(texture) => texture.format(),
            Self::Cpu(texture) => texture.format(),
        }
    }
}

/// What a frame draws into.
#[derive(Debug)]
pub enum SilicaTarget<'a> {
    Gles(GlesTarget<'a>),
    Cpu(CpuTarget<'a>),
}

impl Texture for SilicaTarget<'_> {
    fn size(&self) -> Size<i32, Buffer> {
        match self {
            Self::Gles(target) => target.size(),
            Self::Cpu(target) => target.size(),
        }
    }

    fn width(&self) -> u32 {
        match self {
            Self::Gles(target) => target.width(),
            Self::Cpu(target) => target.width(),
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Gles(target) => target.height(),
            Self::Cpu(target) => target.height(),
        }
    }

    fn format(&self) -> Option<Fourcc> {
        match self {
            Self::Gles(target) => target.format(),
            Self::Cpu(target) => target.format(),
        }
    }
}

/// Pixels read back from either renderer.
#[derive(Debug)]
pub enum SilicaMapping {
    Gles(GlesMapping),
    Cpu(CpuMapping),
}

impl Texture for SilicaMapping {
    fn size(&self) -> Size<i32, Buffer> {
        match self {
            Self::Gles(mapping) => mapping.size(),
            Self::Cpu(mapping) => mapping.size(),
        }
    }

    fn width(&self) -> u32 {
        match self {
            Self::Gles(mapping) => mapping.width(),
            Self::Cpu(mapping) => mapping.width(),
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Gles(mapping) => mapping.height(),
            Self::Cpu(mapping) => mapping.height(),
        }
    }

    fn format(&self) -> Option<Fourcc> {
        match self {
            Self::Gles(mapping) => Texture::format(mapping),
            Self::Cpu(mapping) => Texture::format(mapping),
        }
    }
}

impl TextureMapping for SilicaMapping {
    fn flipped(&self) -> bool {
        match self {
            Self::Gles(mapping) => mapping.flipped(),
            Self::Cpu(mapping) => mapping.flipped(),
        }
    }

    fn format(&self) -> Fourcc {
        match self {
            Self::Gles(mapping) => TextureMapping::format(mapping),
            Self::Cpu(mapping) => TextureMapping::format(mapping),
        }
    }
}

#[derive(Debug)]
pub enum SilicaError {
    Gles(GlesError),
    Cpu(CpuError),
    /// A texture or a target of the other renderer.
    Mismatch,
    /// What this renderer cannot draw.
    Unsupported(&'static str),
}

impl fmt::Display for SilicaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gles(err) => write!(f, "GLES: {err}"),
            Self::Cpu(err) => write!(f, "CPU renderer: {err}"),
            Self::Mismatch => f.write_str("a texture or target of another renderer"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
        }
    }
}

impl std::error::Error for SilicaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gles(err) => Some(err),
            Self::Cpu(err) => Some(err),
            Self::Mismatch | Self::Unsupported(_) => None,
        }
    }
}

impl From<GlesError> for SilicaError {
    fn from(err: GlesError) -> Self {
        Self::Gles(err)
    }
}

impl From<CpuError> for SilicaError {
    fn from(err: CpuError) -> Self {
        Self::Cpu(err)
    }
}

/// A frame of either renderer.
pub enum SilicaFrame<'frame, 'buffer> {
    Gles(GlesFrame<'frame, 'buffer>),
    Cpu(CpuFrame<'frame, 'buffer>),
}

impl fmt::Debug for SilicaFrame<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gles(_) => f.write_str("SilicaFrame::Gles"),
            Self::Cpu(_) => f.write_str("SilicaFrame::Cpu"),
        }
    }
}

impl Frame for SilicaFrame<'_, '_> {
    type Error = SilicaError;
    type TextureId = SilicaTexture;

    fn context_id(&self) -> ContextId<SilicaTexture> {
        match self {
            Self::Gles(frame) => frame.context_id().map(),
            Self::Cpu(frame) => frame.context_id().map(),
        }
    }

    fn clear(
        &mut self,
        color: Color32F,
        at: &[Rectangle<i32, Physical>],
    ) -> Result<(), SilicaError> {
        match self {
            Self::Gles(frame) => Ok(frame.clear(color, at)?),
            Self::Cpu(frame) => Ok(frame.clear(color, at)?),
        }
    }

    fn draw_solid(
        &mut self,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        color: Color32F,
    ) -> Result<(), SilicaError> {
        match self {
            Self::Gles(frame) => Ok(frame.draw_solid(dst, damage, color)?),
            Self::Cpu(frame) => Ok(frame.draw_solid(dst, damage, color)?),
        }
    }

    fn render_texture_from_to(
        &mut self,
        texture: &SilicaTexture,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        src_transform: Transform,
        alpha: f32,
    ) -> Result<(), SilicaError> {
        match (self, texture) {
            (Self::Gles(frame), SilicaTexture::Gles(texture)) => Ok(Frame::render_texture_from_to(
                frame,
                texture,
                src,
                dst,
                damage,
                opaque_regions,
                src_transform,
                alpha,
            )?),
            (Self::Cpu(frame), SilicaTexture::Cpu(texture)) => Ok(frame.render_texture_from_to(
                texture,
                src,
                dst,
                damage,
                opaque_regions,
                src_transform,
                alpha,
            )?),
            _ => Err(SilicaError::Mismatch),
        }
    }

    fn transformation(&self) -> Transform {
        match self {
            Self::Gles(frame) => frame.transformation(),
            Self::Cpu(frame) => frame.transformation(),
        }
    }

    fn wait(&mut self, sync: &SyncPoint) -> Result<(), SilicaError> {
        match self {
            Self::Gles(frame) => Ok(frame.wait(sync)?),
            Self::Cpu(frame) => Ok(frame.wait(sync)?),
        }
    }

    fn finish(self) -> Result<SyncPoint, SilicaError> {
        match self {
            Self::Gles(frame) => Ok(frame.finish()?),
            Self::Cpu(frame) => Ok(frame.finish()?),
        }
    }
}

impl RendererSuper for SilicaRenderer {
    type Error = SilicaError;
    type TextureId = SilicaTexture;
    type Framebuffer<'buffer> = SilicaTarget<'buffer>;
    type Frame<'frame, 'buffer>
        = SilicaFrame<'frame, 'buffer>
    where
        'buffer: 'frame,
        Self: 'frame;
}

impl Renderer for SilicaRenderer {
    fn context_id(&self) -> ContextId<SilicaTexture> {
        match self {
            Self::Gles(renderer) => renderer.context_id().map(),
            Self::Cpu(renderer) => renderer.context_id().map(),
        }
    }

    fn downscale_filter(&mut self, filter: TextureFilter) -> Result<(), SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(renderer.downscale_filter(filter)?),
            Self::Cpu(renderer) => Ok(renderer.downscale_filter(filter)?),
        }
    }

    fn upscale_filter(&mut self, filter: TextureFilter) -> Result<(), SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(renderer.upscale_filter(filter)?),
            Self::Cpu(renderer) => Ok(renderer.upscale_filter(filter)?),
        }
    }

    fn set_debug_flags(&mut self, flags: DebugFlags) {
        match self {
            Self::Gles(renderer) => renderer.set_debug_flags(flags),
            Self::Cpu(renderer) => renderer.set_debug_flags(flags),
        }
    }

    fn debug_flags(&self) -> DebugFlags {
        match self {
            Self::Gles(renderer) => renderer.debug_flags(),
            Self::Cpu(renderer) => renderer.debug_flags(),
        }
    }

    fn render<'frame, 'buffer>(
        &'frame mut self,
        framebuffer: &'frame mut SilicaTarget<'buffer>,
        output_size: Size<i32, Physical>,
        dst_transform: Transform,
    ) -> Result<SilicaFrame<'frame, 'buffer>, SilicaError>
    where
        'buffer: 'frame,
    {
        match (self, framebuffer) {
            (Self::Gles(renderer), SilicaTarget::Gles(target)) => Ok(SilicaFrame::Gles(
                renderer.render(target, output_size, dst_transform)?,
            )),
            (Self::Cpu(renderer), SilicaTarget::Cpu(target)) => Ok(SilicaFrame::Cpu(
                renderer.render(target, output_size, dst_transform)?,
            )),
            _ => Err(SilicaError::Mismatch),
        }
    }

    fn wait(&mut self, sync: &SyncPoint) -> Result<(), SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(Renderer::wait(renderer.as_mut(), sync)?),
            Self::Cpu(renderer) => Ok(Renderer::wait(renderer, sync)?),
        }
    }

    fn cleanup_texture_cache(&mut self) -> Result<(), SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(renderer.cleanup_texture_cache()?),
            Self::Cpu(renderer) => Ok(renderer.cleanup_texture_cache()?),
        }
    }
}

impl ImportMem for SilicaRenderer {
    fn import_memory(
        &mut self,
        data: &[u8],
        format: Fourcc,
        size: Size<i32, Buffer>,
        flipped: bool,
    ) -> Result<SilicaTexture, SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(SilicaTexture::Gles(
                renderer.import_memory(data, format, size, flipped)?,
            )),
            Self::Cpu(renderer) => Ok(SilicaTexture::Cpu(
                renderer.import_memory(data, format, size, flipped)?,
            )),
        }
    }

    fn update_memory(
        &mut self,
        texture: &SilicaTexture,
        data: &[u8],
        region: Rectangle<i32, Buffer>,
    ) -> Result<(), SilicaError> {
        match (self, texture) {
            (Self::Gles(renderer), SilicaTexture::Gles(texture)) => {
                Ok(renderer.update_memory(texture, data, region)?)
            }
            (Self::Cpu(renderer), SilicaTexture::Cpu(texture)) => {
                Ok(renderer.update_memory(texture, data, region)?)
            }
            _ => Err(SilicaError::Mismatch),
        }
    }

    fn mem_formats(&self) -> Box<dyn Iterator<Item = Fourcc>> {
        match self {
            Self::Gles(renderer) => renderer.mem_formats(),
            Self::Cpu(renderer) => renderer.mem_formats(),
        }
    }
}

impl ImportMemWl for SilicaRenderer {
    fn import_shm_buffer(
        &mut self,
        buffer: &wl_buffer::WlBuffer,
        surface: Option<&SurfaceData>,
        damage: &[Rectangle<i32, Buffer>],
    ) -> Result<SilicaTexture, SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(SilicaTexture::Gles(
                renderer.import_shm_buffer(buffer, surface, damage)?,
            )),
            Self::Cpu(renderer) => Ok(SilicaTexture::Cpu(
                renderer.import_shm_buffer(buffer, surface, damage)?,
            )),
        }
    }

    fn shm_formats(&self) -> Box<dyn Iterator<Item = wl_shm::Format>> {
        match self {
            Self::Gles(renderer) => renderer.shm_formats(),
            Self::Cpu(renderer) => renderer.shm_formats(),
        }
    }
}

impl ImportDma for SilicaRenderer {
    fn dmabuf_formats(&self) -> FormatSet {
        match self {
            Self::Gles(renderer) => renderer.dmabuf_formats(),
            Self::Cpu(renderer) => renderer.dmabuf_formats(),
        }
    }

    fn import_dmabuf(
        &mut self,
        dmabuf: &Dmabuf,
        damage: Option<&[Rectangle<i32, Buffer>]>,
    ) -> Result<SilicaTexture, SilicaError> {
        match self {
            Self::Gles(renderer) => {
                Ok(SilicaTexture::Gles(renderer.import_dmabuf(dmabuf, damage)?))
            }
            Self::Cpu(renderer) => Ok(SilicaTexture::Cpu(renderer.import_dmabuf(dmabuf, damage)?)),
        }
    }
}

impl ImportDmaWl for SilicaRenderer {
    fn import_dma_buffer(
        &mut self,
        buffer: &wl_buffer::WlBuffer,
        surface: Option<&SurfaceData>,
        damage: &[Rectangle<i32, Buffer>],
    ) -> Result<SilicaTexture, SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(SilicaTexture::Gles(
                renderer.import_dma_buffer(buffer, surface, damage)?,
            )),
            Self::Cpu(renderer) => Ok(SilicaTexture::Cpu(
                renderer.import_dma_buffer(buffer, surface, damage)?,
            )),
        }
    }
}

impl Bind<Dmabuf> for SilicaRenderer {
    fn bind<'a>(&mut self, target: &'a mut Dmabuf) -> Result<SilicaTarget<'a>, SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(SilicaTarget::Gles(renderer.bind(target)?)),
            Self::Cpu(renderer) => Ok(SilicaTarget::Cpu(renderer.bind(target)?)),
        }
    }

    fn supported_formats(&self) -> Option<FormatSet> {
        match self {
            Self::Gles(renderer) => Bind::<Dmabuf>::supported_formats(renderer.as_ref()),
            Self::Cpu(renderer) => Bind::<Dmabuf>::supported_formats(renderer),
        }
    }
}

impl Bind<SilicaTexture> for SilicaRenderer {
    fn bind<'a>(&mut self, target: &'a mut SilicaTexture) -> Result<SilicaTarget<'a>, SilicaError> {
        match (self, target) {
            (Self::Gles(renderer), SilicaTexture::Gles(texture)) => {
                Ok(SilicaTarget::Gles(renderer.bind(texture)?))
            }
            (Self::Cpu(renderer), SilicaTexture::Cpu(texture)) => {
                Ok(SilicaTarget::Cpu(renderer.bind(texture)?))
            }
            _ => Err(SilicaError::Mismatch),
        }
    }

    fn supported_formats(&self) -> Option<FormatSet> {
        match self {
            Self::Gles(renderer) => Bind::<GlesTexture>::supported_formats(renderer.as_ref()),
            Self::Cpu(renderer) => Bind::<CpuTexture>::supported_formats(renderer),
        }
    }
}

impl Offscreen<SilicaTexture> for SilicaRenderer {
    fn create_buffer(
        &mut self,
        format: Fourcc,
        size: Size<i32, Buffer>,
    ) -> Result<SilicaTexture, SilicaError> {
        match self {
            Self::Gles(renderer) => Ok(SilicaTexture::Gles(
                Offscreen::<GlesTexture>::create_buffer(renderer.as_mut(), format, size)?,
            )),
            Self::Cpu(renderer) => Ok(SilicaTexture::Cpu(renderer.create_buffer(format, size)?)),
        }
    }
}

impl ExportMem for SilicaRenderer {
    type TextureMapping = SilicaMapping;

    fn copy_framebuffer(
        &mut self,
        target: &SilicaTarget<'_>,
        region: Rectangle<i32, Buffer>,
        format: Fourcc,
    ) -> Result<SilicaMapping, SilicaError> {
        match (self, target) {
            (Self::Gles(renderer), SilicaTarget::Gles(target)) => Ok(SilicaMapping::Gles(
                renderer.copy_framebuffer(target, region, format)?,
            )),
            (Self::Cpu(renderer), SilicaTarget::Cpu(target)) => Ok(SilicaMapping::Cpu(
                renderer.copy_framebuffer(target, region, format)?,
            )),
            _ => Err(SilicaError::Mismatch),
        }
    }

    fn copy_texture(
        &mut self,
        texture: &SilicaTexture,
        region: Rectangle<i32, Buffer>,
        format: Fourcc,
    ) -> Result<SilicaMapping, SilicaError> {
        match (self, texture) {
            (Self::Gles(renderer), SilicaTexture::Gles(texture)) => Ok(SilicaMapping::Gles(
                renderer.copy_texture(texture, region, format)?,
            )),
            (Self::Cpu(renderer), SilicaTexture::Cpu(texture)) => Ok(SilicaMapping::Cpu(
                renderer.copy_texture(texture, region, format)?,
            )),
            _ => Err(SilicaError::Mismatch),
        }
    }

    fn can_read_texture(&mut self, texture: &SilicaTexture) -> Result<bool, SilicaError> {
        match (self, texture) {
            (Self::Gles(renderer), SilicaTexture::Gles(texture)) => {
                Ok(renderer.can_read_texture(texture)?)
            }
            (Self::Cpu(renderer), SilicaTexture::Cpu(texture)) => {
                Ok(renderer.can_read_texture(texture)?)
            }
            _ => Ok(false),
        }
    }

    fn map_texture<'a>(&mut self, mapping: &'a SilicaMapping) -> Result<&'a [u8], SilicaError> {
        match (self, mapping) {
            (Self::Gles(renderer), SilicaMapping::Gles(mapping)) => {
                Ok(renderer.map_texture(mapping)?)
            }
            (Self::Cpu(renderer), SilicaMapping::Cpu(mapping)) => {
                Ok(renderer.map_texture(mapping)?)
            }
            _ => Err(SilicaError::Mismatch),
        }
    }
}
