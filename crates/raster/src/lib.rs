//! Silica's CPU renderer: Smithay's renderer traits on premultiplied BGRA8 images in RAM.
//!
//! It is the compositor's renderer when there is no GPU, compiled ahead of time: the
//! compositor has no JIT entitlement, so no llvmpipe. A frame records a display list and runs
//! it at `finish()` in bands of rows, on a few workers, with SIMD kernels picked at run time.
//! Client buffers are copied into owned images (only their damage), so no frame ever reads
//! memory a client can shrink under it.

mod exec;
mod image;
mod kernels;
mod memory;

use std::cell::Cell;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use rayon::{ThreadPool, ThreadPoolBuilder};
use smithay::backend::allocator::dmabuf::{
    Dmabuf, DmabufMapping, DmabufMappingFailed, DmabufMappingMode, DmabufSyncFailed,
    DmabufSyncFlags, WeakDmabuf,
};
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::{Buffer, Format, Fourcc, Modifier};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{
    Bind, Color32F, ContextId, DebugFlags, ExportMem, Frame, ImportDma, ImportDmaWl, ImportMem,
    ImportMemWl, Offscreen, Renderer, RendererSuper, Texture, TextureFilter, TextureMapping,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::utils::{Buffer as BufferCoord, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::compositor::SurfaceData;
use smithay::wayland::shm::{BufferAccessError, shm_format_to_fourcc};

pub use exec::{Band, Effect, Shader};
/// The SIMD crate of the kernels, for effects that `dispatch!` their own loops.
pub use fearless_simd;
pub use image::{FORMATS, Image, Layout, Rect};
pub use kernels::{Kernels, Walk};

use exec::{DisplayList, Map, Op};

/// Threads drawing a frame at most, the caller's included: past four, a 1280 × 800 frame waits
/// on memory, not on the cores.
const MAX_THREADS: usize = 4;

/// The longest the workers stay awake for one `warm`, should its guard never be dropped.
const MAX_WARM: Duration = Duration::from_millis(50);

/// Where the frames' time went on this thread, since the last [`take_times`].
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct DrawTimes {
    /// Running the display lists: the pixels.
    pub raster: Duration,
    /// Writing the damage of dmabuf targets back from their shadows.
    pub write_back: Duration,
}

thread_local! {
    static TIMES: Cell<DrawTimes> = const {
        Cell::new(DrawTimes {
            raster: Duration::ZERO,
            write_back: Duration::ZERO,
        })
    };
}

/// The frames' times on this thread since the last call, which start again from zero.
pub fn take_times() -> DrawTimes {
    TIMES.with(Cell::take)
}

/// Keeps the renderer's workers awake while it lives (see [`CpuRenderer::warm`]).
#[must_use = "the workers sleep again when the guard is dropped"]
pub struct Warm(Option<Arc<AtomicBool>>);

impl Drop for Warm {
    fn drop(&mut self) {
        if let Some(awake) = &self.0 {
            awake.store(false, Ordering::Release);
        }
    }
}

/// An image the renderer reads from and draws into, shared between its users.
#[derive(Clone)]
pub struct CpuTexture(Arc<TextureInner>);

struct TextureInner {
    image: RwLock<Image>,
    width: u32,
    height: u32,
    format: Fourcc,
    opaque: bool,
    /// Rows stored bottom-up in the memory it was imported from.
    flipped: bool,
}

impl fmt::Debug for CpuTexture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuTexture")
            .field("size", &(self.0.width, self.0.height))
            .field("format", &self.0.format)
            .finish()
    }
}

impl CpuTexture {
    /// A transparent texture of a format the renderer knows.
    pub fn new(width: u32, height: u32, format: Fourcc) -> Result<Self, CpuError> {
        Self::with(width, height, format, false)
    }

    /// A texture holding `image` (premultiplied ARGB, as every texture keeps it).
    pub fn from_image(image: Image) -> Result<Self, CpuError> {
        let (width, height) = (image.width(), image.height());
        if width == 0 || height == 0 || width > i32::MAX as usize || height > i32::MAX as usize {
            return Err(CpuError::InvalidSize);
        }
        Ok(Self(Arc::new(TextureInner {
            image: RwLock::new(image),
            width: width as u32,
            height: height as u32,
            format: Fourcc::Argb8888,
            opaque: false,
            flipped: false,
        })))
    }

    fn with(width: u32, height: u32, format: Fourcc, flipped: bool) -> Result<Self, CpuError> {
        let layout = Layout::of(format).ok_or(CpuError::UnsupportedFormat(format))?;
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(CpuError::InvalidSize);
        }
        Ok(Self(Arc::new(TextureInner {
            image: RwLock::new(Image::new(width as usize, height as usize)),
            width,
            height,
            format,
            opaque: layout.opaque(),
            flipped,
        })))
    }

    /// The pixels, for reading.
    pub fn read(&self) -> RwLockReadGuard<'_, Image> {
        self.0.image.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The pixels, for reading, unless someone is writing them (a texture drawn into its own
    /// frame).
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, Image>> {
        match self.0.image.try_read() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// The pixels, for writing.
    pub fn write(&self) -> RwLockWriteGuard<'_, Image> {
        self.0.image.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether every pixel is opaque, whatever it holds (an X format).
    pub fn is_opaque(&self) -> bool {
        self.0.opaque
    }

    fn layout(&self) -> Layout {
        Layout::of(self.0.format).unwrap_or(Layout::Argb)
    }

    pub(crate) fn same(&self, other: &CpuTexture) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Texture for CpuTexture {
    fn width(&self) -> u32 {
        self.0.width
    }

    fn height(&self) -> u32 {
        self.0.height
    }

    fn format(&self) -> Option<Fourcc> {
        Some(self.0.format)
    }
}

/// What a frame draws into: a texture, or the RAM copy of a dmabuf that the frame's damage
/// is written back to at the end.
#[derive(Debug)]
pub struct CpuTarget<'a>(Target<'a>);

#[derive(Debug)]
enum Target<'a> {
    Texture(CpuTexture),
    Dmabuf {
        dmabuf: &'a Dmabuf,
        shadow: CpuTexture,
    },
}

impl CpuTarget<'_> {
    fn texture(&self) -> &CpuTexture {
        match &self.0 {
            Target::Texture(texture) => texture,
            Target::Dmabuf { shadow, .. } => shadow,
        }
    }
}

impl Texture for CpuTarget<'_> {
    fn width(&self) -> u32 {
        self.texture().width()
    }

    fn height(&self) -> u32 {
        self.texture().height()
    }

    fn format(&self) -> Option<Fourcc> {
        self.texture().format()
    }
}

/// Pixels read back from a texture or a target.
#[derive(Debug)]
pub struct CpuMapping {
    data: Vec<u8>,
    width: u32,
    height: u32,
    format: Fourcc,
}

impl Texture for CpuMapping {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn format(&self) -> Option<Fourcc> {
        Some(self.format)
    }
}

impl TextureMapping for CpuMapping {
    fn flipped(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub enum CpuError {
    UnsupportedFormat(Fourcc),
    UnsupportedModifier(Modifier),
    UnsupportedPlanes(usize),
    InvalidSize,
    IncompleteBuffer {
        expected: usize,
        actual: usize,
    },
    Shm(BufferAccessError),
    Map(DmabufMappingFailed),
    Sync(DmabufSyncFailed),
    SyncInterrupted,
    /// An effect on a rotated or flipped output.
    UnsupportedTransform(Transform),
}

impl fmt::Display for CpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat(format) => write!(f, "format {format} not supported"),
            Self::UnsupportedModifier(modifier) => {
                write!(f, "modifier {modifier:?} not supported, only linear")
            }
            Self::UnsupportedPlanes(planes) => write!(f, "{planes} planes, only one supported"),
            Self::InvalidSize => f.write_str("empty or oversized buffer"),
            Self::IncompleteBuffer { expected, actual } => {
                write!(f, "buffer of {actual} bytes, {expected} expected")
            }
            Self::Shm(err) => write!(f, "shm buffer: {err}"),
            Self::Map(err) => write!(f, "mapping a dmabuf: {err}"),
            Self::Sync(err) => write!(f, "syncing a dmabuf: {err}"),
            Self::SyncInterrupted => f.write_str("waiting on a sync point was interrupted"),
            Self::UnsupportedTransform(transform) => {
                write!(f, "effects on a {transform:?} output are not supported")
            }
        }
    }
}

impl std::error::Error for CpuError {}

/// A client dmabuf imported as a texture: its mapping stays, so updates read only damage.
struct DmabufImport {
    buffer: WeakDmabuf,
    texture: CpuTexture,
    mapping: Option<DmabufMapping>,
}

/// A dmabuf drawn into: the frames draw into `shadow` (RAM, fast to read back for blending)
/// and copy their damage into the mapping. One per buffer, so the buffer's age stays true.
struct DmabufTarget {
    buffer: WeakDmabuf,
    shadow: CpuTexture,
    mapping: Option<DmabufMapping>,
}

/// The per-surface cache of shm imports, one texture per renderer.
#[derive(Default)]
struct ShmCache(Mutex<Vec<(ContextId<CpuTexture>, CpuTexture)>>);

pub struct CpuRenderer {
    context: ContextId<CpuTexture>,
    kernels: Kernels,
    pool: Option<ThreadPool>,
    upscale: TextureFilter,
    downscale: TextureFilter,
    debug: DebugFlags,
    /// The display list's storage, kept between frames.
    list: DisplayList,
    imports: Vec<DmabufImport>,
    targets: Vec<DmabufTarget>,
}

impl fmt::Debug for CpuRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuRenderer")
            .field("kernels", &self.kernels)
            .field("threads", &self.threads())
            .finish_non_exhaustive()
    }
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuRenderer {
    /// A renderer on `SILICA_RENDERER_THREADS` threads, or on all cores but one (at most 4),
    /// the caller's included. The core left is the clients': with every core held by a frame,
    /// a client that wakes takes one from a thread in the middle of its band, and the frame
    /// waits for its time slice.
    pub fn new() -> Self {
        let threads = std::env::var("SILICA_RENDERER_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map_or(1, |n| n.get().saturating_sub(1))
                    .clamp(1, MAX_THREADS)
            });
        Self::with_threads(threads)
    }

    /// A renderer drawing large frames on `threads` threads: the caller's and `threads - 1`
    /// workers. Never more workers than that: held warm over a frame, they would take the cores
    /// from the caller, which draws its share and does the frame's serial work.
    pub fn with_threads(threads: usize) -> Self {
        let pool = (threads > 1)
            .then(|| {
                ThreadPoolBuilder::new()
                    .num_threads(threads - 1)
                    .thread_name(|i| format!("raster-{i}"))
                    .build()
                    .inspect_err(|err| tracing::warn!("CPU renderer on one thread: {err}"))
                    .ok()
            })
            .flatten();
        Self {
            context: ContextId::default(),
            kernels: Kernels::new(),
            pool,
            upscale: TextureFilter::Linear,
            downscale: TextureFilter::Linear,
            debug: DebugFlags::empty(),
            list: DisplayList::default(),
            imports: Vec::new(),
            targets: Vec::new(),
        }
    }

    /// Runs `f` on every row of `data` (`width` items per row) with the row's index, on the
    /// workers when there is enough of it: the effects' own passes.
    pub fn rows<T: Send>(&self, data: &mut [T], width: usize, f: impl Fn(usize, &mut [T]) + Sync) {
        exec::rows(self.pool.as_ref(), data, width, f);
    }

    /// Wakes the workers and keeps them waiting for work, spinning, until the guard is dropped:
    /// held over a frame, its passes start at once instead of waiting each time for sleeping
    /// cores to wake up, which costs more than a small pass itself.
    pub fn warm(&self) -> Warm {
        let Some(pool) = &self.pool else {
            return Warm(None);
        };
        let awake = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&awake);
        let until = Instant::now() + MAX_WARM;
        pool.spawn_broadcast(move |_| {
            while flag.load(Ordering::Acquire) && Instant::now() < until {
                if rayon::yield_now() != Some(rayon::Yield::Executed) {
                    std::hint::spin_loop();
                }
            }
        });
        Warm(Some(awake))
    }

    /// The kernels, for effects that draw with them.
    pub fn kernels(&self) -> Kernels {
        self.kernels
    }

    /// The SIMD level of the kernels, for effects that `dispatch!` their own loops.
    pub fn level(&self) -> fearless_simd::Level {
        self.kernels.level()
    }

    /// `rows` for work of tens of nanoseconds per item (a shader run per pixel): on the
    /// workers in bands of two rows, as soon as there is more than one.
    pub fn rows_costly<T: Send>(
        &self,
        data: &mut [T],
        width: usize,
        f: impl Fn(usize, &mut [T]) + Sync,
    ) {
        const ROWS: usize = 2;
        let band = (ROWS * width) as u64;
        exec::rows_from(self.pool.as_ref(), data, width, (band + 1, ROWS), f);
    }

    /// The threads a large frame runs on, the caller's included.
    pub fn threads(&self) -> usize {
        self.pool
            .as_ref()
            .map_or(1, |pool| pool.current_num_threads() + 1)
    }

    /// The dmabuf formats the renderer reads and draws into: ours, linear.
    pub fn dmabuf_format_set() -> FormatSet {
        FORMATS
            .into_iter()
            .map(|code| Format {
                code,
                modifier: Modifier::Linear,
            })
            .collect()
    }

    fn forget_gone_buffers(&mut self) {
        self.imports.retain(|i| !i.buffer.is_gone());
        self.targets.retain(|t| !t.buffer.is_gone());
    }

    /// Writes the `rects` of the shadow of `dmabuf` into it.
    fn write_back(
        &mut self,
        dmabuf: &Dmabuf,
        shadow: &CpuTexture,
        rects: &[Rect],
    ) -> Result<(), CpuError> {
        let weak = dmabuf.weak();
        let Some(target) = self.targets.iter_mut().find(|t| t.buffer == weak) else {
            return Ok(());
        };
        let stride = dmabuf_layout(dmabuf, true)?.1;
        let mapping = match &mut target.mapping {
            Some(mapping) => mapping,
            mapping => mapping.insert(
                dmabuf
                    .map_plane(0, DmabufMappingMode::READ | DmabufMappingMode::WRITE)
                    .map_err(CpuError::Map)?,
            ),
        };
        // Each pixel once, however many commands drew it.
        let mut disjoint: Vec<Rectangle<i32, Physical>> = Vec::new();
        for rect in rects {
            let pieces =
                Rectangle::subtract_rects_many([rect.to_rectangle()], disjoint.iter().copied());
            disjoint.extend(pieces);
        }
        dmabuf
            .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::WRITE)
            .map_err(CpuError::Sync)?;
        let (guard, layout) = (shadow.read(), shadow.layout());
        let image = &*guard;
        let rects: Vec<Rect> = disjoint.iter().map(|rect| Rect::of(*rect)).collect();
        // The rows the damage covers, on the workers as a frame's bands are.
        let top = rects.iter().map(|r| r.y0).min().unwrap_or(0).max(0);
        let bottom = rects.iter().map(|r| r.y1).max().unwrap_or(0).max(top);
        let (top, bottom) = (top as usize, bottom as usize);
        let bytes = memory::mapped_mut(mapping);
        let end = bottom.saturating_mul(stride).min(bytes.len());
        if let Some(rows) = bytes.get_mut(top.saturating_mul(stride)..end) {
            exec::rows(self.pool.as_ref(), rows, stride, |i, row| {
                let y = i32::try_from(top + i).unwrap_or(i32::MAX);
                for rect in rects.iter().filter(|r| (r.y0..r.y1).contains(&y)) {
                    let line = Rect {
                        y0: y,
                        y1: y + 1,
                        ..*rect
                    };
                    image.store(row, stride, layout, line, (0, y));
                }
            });
        }
        dmabuf
            .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::WRITE)
            .map_err(CpuError::Sync)
    }
}

/// The layout and stride of a dmabuf the renderer can use. `implicit` takes an implicit
/// modifier as linear: only for targets, which the compositor allocates linear
/// (`GBM_BO_USE_LINEAR`) where the display has no modifiers; a client's buffer with an
/// implicit modifier may be tiled.
fn dmabuf_layout(dmabuf: &Dmabuf, implicit: bool) -> Result<(Layout, usize), CpuError> {
    if dmabuf.num_planes() != 1 {
        return Err(CpuError::UnsupportedPlanes(dmabuf.num_planes()));
    }
    let format = dmabuf.format();
    let linear =
        format.modifier == Modifier::Linear || (implicit && format.modifier == Modifier::Invalid);
    if !linear {
        return Err(CpuError::UnsupportedModifier(format.modifier));
    }
    let layout = Layout::of(format.code).ok_or(CpuError::UnsupportedFormat(format.code))?;
    let stride = dmabuf
        .strides()
        .next()
        .ok_or(CpuError::UnsupportedPlanes(0))?;
    Ok((layout, stride as usize))
}

fn texture_size(size: Size<i32, BufferCoord>) -> Result<(u32, u32), CpuError> {
    match (u32::try_from(size.w), u32::try_from(size.h)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(CpuError::InvalidSize),
    }
}

impl RendererSuper for CpuRenderer {
    type Error = CpuError;
    type TextureId = CpuTexture;
    type Framebuffer<'buffer> = CpuTarget<'buffer>;
    type Frame<'frame, 'buffer>
        = CpuFrame<'frame, 'buffer>
    where
        'buffer: 'frame,
        Self: 'frame;
}

impl Renderer for CpuRenderer {
    fn context_id(&self) -> ContextId<CpuTexture> {
        self.context.clone()
    }

    fn downscale_filter(&mut self, filter: TextureFilter) -> Result<(), CpuError> {
        self.downscale = filter;
        Ok(())
    }

    fn upscale_filter(&mut self, filter: TextureFilter) -> Result<(), CpuError> {
        self.upscale = filter;
        Ok(())
    }

    fn set_debug_flags(&mut self, flags: DebugFlags) {
        self.debug = flags;
    }

    fn debug_flags(&self) -> DebugFlags {
        self.debug
    }

    fn render<'frame, 'buffer>(
        &'frame mut self,
        framebuffer: &'frame mut CpuTarget<'buffer>,
        output_size: Size<i32, Physical>,
        dst_transform: Transform,
    ) -> Result<CpuFrame<'frame, 'buffer>, CpuError>
    where
        'buffer: 'frame,
    {
        self.forget_gone_buffers();
        let mut list = std::mem::take(&mut self.list);
        list.clear();
        let bounds = {
            let texture = framebuffer.texture();
            Rect::new(0, 0, texture.0.width as i32, texture.0.height as i32)
        };
        Ok(CpuFrame {
            renderer: self,
            target: framebuffer,
            transform: dst_transform,
            space: dst_transform.transform_size(output_size),
            bounds,
            list,
            shader: None,
            finished: false,
        })
    }

    fn wait(&mut self, sync: &SyncPoint) -> Result<(), CpuError> {
        sync.wait().map_err(|_| CpuError::SyncInterrupted)
    }

    fn cleanup_texture_cache(&mut self) -> Result<(), CpuError> {
        self.forget_gone_buffers();
        Ok(())
    }
}

/// A frame being recorded; it is drawn at `finish()`, or when dropped.
pub struct CpuFrame<'frame, 'buffer> {
    renderer: &'frame mut CpuRenderer,
    target: &'frame mut CpuTarget<'buffer>,
    transform: Transform,
    /// The output's size before the transform: where the frame's rectangles live.
    space: Size<i32, Physical>,
    /// The target's pixels.
    bounds: Rect,
    list: DisplayList,
    /// The texture program of the textures drawn now, if any.
    shader: Option<Arc<dyn Shader>>,
    finished: bool,
}

/// A color as a premultiplied pixel (`Color32F` is premultiplied).
fn pixel(color: Color32F) -> u32 {
    let [r, g, b, a] = color
        .components()
        .map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u32);
    (a << 24) | (r << 16) | (g << 8) | b
}

/// `damage` (relative to `dst`) clamped to `dst`, in output coordinates, as GLES does.
fn damage_in(
    dst: Rectangle<i32, Physical>,
    damage: &[Rectangle<i32, Physical>],
) -> impl Iterator<Item = Rectangle<i32, Physical>> + Clone + '_ {
    damage.iter().filter_map(move |d| {
        d.intersection(Rectangle::from_size(dst.size)).map(|mut d| {
            d.loc += dst.loc;
            d
        })
    })
}

/// An affine map of the plane: `(x, y) → (a·x + b·y + c, d·x + e·y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine([f64; 6]);

impl Affine {
    fn apply(self, (x, y): (f64, f64)) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + b * y + c, d * x + e * y + f)
    }

    /// The affine map `f` is, from where it takes three points.
    fn fit(f: impl Fn((f64, f64)) -> (f64, f64)) -> Self {
        let o = f((0.0, 0.0));
        let x = f((1.0, 0.0));
        let y = f((0.0, 1.0));
        Affine([x.0 - o.0, y.0 - o.0, o.0, x.1 - o.1, y.1 - o.1, o.1])
    }

    fn invert(self) -> Option<Self> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * e - b * d;
        if det.abs() < 1e-12 {
            return None;
        }
        let (ia, ib, id, ie) = (e / det, -b / det, -d / det, a / det);
        Some(Affine([
            ia,
            ib,
            -(ia * c + ib * f),
            id,
            ie,
            -(id * c + ie * f),
        ]))
    }

    fn determinant(self) -> f64 {
        let [a, b, _, d, e, _] = self.0;
        a * e - b * d
    }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

impl CpuFrame<'_, '_> {
    /// An output rectangle in target pixels, clipped to the target.
    fn to_target(&self, rect: Rectangle<i32, Physical>) -> Option<Rect> {
        Rect::of(self.transform.transform_rect_in(rect, &self.space)).intersect(self.bounds)
    }

    /// Where the target pixels of `dst` read `src` of a texture shown with `src_transform`.
    fn map(
        &self,
        src: Rectangle<f64, BufferCoord>,
        dst: Rectangle<i32, Physical>,
        src_transform: Transform,
    ) -> Option<Map> {
        let space = self.space.to_f64();
        let transform = self.transform;
        let from_target = Affine::fit(|(x, y)| {
            let p = transform.transform_point_in(Point::<f64, Physical>::from((x, y)), &space);
            (p.x, p.y)
        })
        .invert()?;
        let size = src_transform.transform_size(src.size);
        let (w, h) = (size.w, size.h);
        let (kx, ky) = (w / f64::from(dst.size.w), h / f64::from(dst.size.h));
        let (dx, dy) = (f64::from(dst.loc.x), f64::from(dst.loc.y));
        // From the target to the output, to `dst`'s pixels scaled to `src`, through the
        // buffer transform as GLES's texture matrix applies it, to texels.
        let to_texel = Affine::fit(|t| {
            let (px, py) = from_target.apply(t);
            let (x, y) = ((px - dx) * kx, (py - dy) * ky);
            let (bx, by) = match src_transform {
                Transform::Normal => (x, y),
                Transform::_90 => (y, w - x),
                Transform::_180 => (w - x, h - y),
                Transform::_270 => (h - y, x),
                Transform::Flipped => (w - x, y),
                Transform::Flipped90 => (y, x),
                Transform::Flipped180 => (x, h - y),
                Transform::Flipped270 => (h - y, w - x),
            };
            (src.loc.x + bx, src.loc.y + by)
        });
        let [a, b, c, d, e, f] = to_texel.0;
        if near(a, 1.0)
            && near(b, 0.0)
            && near(d, 0.0)
            && near(e, 1.0)
            && near(c, c.round())
            && near(f, f.round())
        {
            return Some(Map::Offset {
                dx: c.round() as i32,
                dy: f.round() as i32,
            });
        }
        let filter = if to_texel.determinant().abs() > 1.0 + 1e-9 {
            self.renderer.downscale
        } else {
            self.renderer.upscale
        };
        Some(Map::Affine {
            u: [a, b, c],
            v: [d, e, f],
            filter,
        })
    }

    /// The renderer drawing the frame, for an effect painted while drawing.
    pub fn renderer(&self) -> &CpuRenderer {
        self.renderer
    }

    /// Shades the textures drawn from now on with `shader`, until `clear_shader` (GLES's
    /// texture-program override). Shaders work in target pixels, so only on outputs with the
    /// normal transform.
    pub fn override_shader(&mut self, shader: Arc<dyn Shader>) -> Result<(), CpuError> {
        if self.transform != Transform::Normal {
            return Err(CpuError::UnsupportedTransform(self.transform));
        }
        self.shader = Some(shader);
        Ok(())
    }

    pub fn clear_shader(&mut self) {
        self.shader = None;
    }

    /// Draws `effect` over the `damage` of `dst` (as in `draw_solid`). Effects draw in target
    /// pixels, so only on outputs with the normal transform.
    pub fn draw_effect(
        &mut self,
        effect: Arc<dyn Effect>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), CpuError> {
        if self.transform != Transform::Normal {
            return Err(CpuError::UnsupportedTransform(self.transform));
        }
        let rects: Vec<Rect> = damage_in(dst, damage)
            .filter_map(|r| self.to_target(r))
            .collect();
        self.list.push(Op::Effect(effect), rects);
        Ok(())
    }

    /// Runs the display list, and writes a dmabuf target's damage back.
    fn flush(&mut self) -> Result<(), CpuError> {
        if std::mem::replace(&mut self.finished, true) {
            return Ok(());
        }
        let texture = self.target.texture().clone();
        let start = Instant::now();
        self.list.execute(
            &mut texture.write(),
            self.renderer.kernels,
            self.renderer.pool.as_ref(),
        );
        let executed = Instant::now();
        let result = match &self.target.0 {
            Target::Dmabuf { dmabuf, shadow } => {
                self.renderer.write_back(dmabuf, shadow, self.list.rects())
            }
            Target::Texture(_) => Ok(()),
        };
        TIMES.with(|times| {
            let mut now = times.get();
            now.raster += executed - start;
            now.write_back += executed.elapsed();
            times.set(now);
        });
        self.renderer.list = std::mem::take(&mut self.list);
        result
    }
}

impl Drop for CpuFrame<'_, '_> {
    fn drop(&mut self) {
        if let Err(err) = self.flush() {
            tracing::warn!("CPU frame dropped unfinished: {err}");
        }
    }
}

impl Frame for CpuFrame<'_, '_> {
    type Error = CpuError;
    type TextureId = CpuTexture;

    fn context_id(&self) -> ContextId<CpuTexture> {
        self.renderer.context.clone()
    }

    fn clear(&mut self, color: Color32F, at: &[Rectangle<i32, Physical>]) -> Result<(), CpuError> {
        let rects: Vec<Rect> = at.iter().filter_map(|r| self.to_target(*r)).collect();
        self.list.push(Op::Fill(pixel(color)), rects);
        Ok(())
    }

    fn draw_solid(
        &mut self,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        color: Color32F,
    ) -> Result<(), CpuError> {
        let color = pixel(color);
        let op = match color >> 24 {
            255 => Op::Fill(color),
            _ => Op::FillOver(color),
        };
        let rects: Vec<Rect> = damage_in(dst, damage)
            .filter_map(|r| self.to_target(r))
            .collect();
        self.list.push(op, rects);
        Ok(())
    }

    fn render_texture_from_to(
        &mut self,
        texture: &CpuTexture,
        src: Rectangle<f64, BufferCoord>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        src_transform: Transform,
        alpha: f32,
    ) -> Result<(), CpuError> {
        let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
        if alpha == 0 || dst.is_empty() || src.size.w <= 0.0 || src.size.h <= 0.0 {
            return Ok(());
        }
        let Some(map) = self.map(src, dst, src_transform) else {
            return Ok(());
        };
        // An offset texture draws only where its source has pixels.
        let clip = match map {
            Map::Offset { dx, dy } => {
                Rect::new(0, 0, texture.0.width as i32, texture.0.height as i32).translate(-dx, -dy)
            }
            Map::Affine { .. } => self.bounds,
        };
        let target = |frame: &Self, rects: &mut dyn Iterator<Item = Rectangle<i32, Physical>>| {
            rects
                .filter_map(|r| frame.to_target(r)?.intersect(clip))
                .collect::<Vec<Rect>>()
        };
        let source = self.list.source(texture);
        let op = |copy, shader| Op::Texture {
            source,
            map,
            alpha,
            copy,
            shader,
        };
        let mut damage: Vec<Rectangle<i32, Physical>> = damage_in(dst, damage).collect();
        if let Some(shader) = self.shader.clone() {
            // The shader runs only where it changes something; the rest is drawn as usual.
            let shaded = match shader.region() {
                None => std::mem::take(&mut damage),
                Some(region) => {
                    let region: Vec<Rectangle<i32, Physical>> =
                        region.into_iter().map(Rect::to_rectangle).collect();
                    let shaded = damage
                        .iter()
                        .flat_map(|d| region.iter().filter_map(|r| d.intersection(*r)))
                        .collect();
                    damage = Rectangle::subtract_rects_many(damage, region);
                    shaded
                }
            };
            let copy = alpha == 255 && texture.is_opaque() && shader.keeps_opaque();
            let rects = target(self, &mut shaded.into_iter());
            self.list.push(op(copy, Some(shader)), rects);
        }
        let damage = damage.into_iter();
        if alpha == 255 && texture.is_opaque() {
            let rects = target(self, &mut damage.clone());
            self.list.push(op(true, None), rects);
        } else if alpha == 255 && !opaque_regions.is_empty() {
            // As GLES: blend outside the opaque regions, copy inside.
            let opaque = opaque_regions.iter().map(|r| {
                let mut r = *r;
                r.loc += dst.loc;
                r
            });
            let blended = Rectangle::subtract_rects_many(damage.clone(), opaque);
            let copied = Rectangle::subtract_rects_many(damage, blended.iter().copied());
            let rects = target(self, &mut blended.into_iter());
            self.list.push(op(false, None), rects);
            let rects = target(self, &mut copied.into_iter());
            self.list.push(op(true, None), rects);
        } else {
            let rects = target(self, &mut damage.clone());
            self.list.push(op(false, None), rects);
        }
        Ok(())
    }

    fn transformation(&self) -> Transform {
        self.transform
    }

    fn wait(&mut self, sync: &SyncPoint) -> Result<(), CpuError> {
        sync.wait().map_err(|_| CpuError::SyncInterrupted)
    }

    fn finish(mut self) -> Result<SyncPoint, CpuError> {
        self.flush()?;
        Ok(SyncPoint::signaled())
    }
}

impl ImportMem for CpuRenderer {
    fn import_memory(
        &mut self,
        data: &[u8],
        format: Fourcc,
        size: Size<i32, BufferCoord>,
        flipped: bool,
    ) -> Result<CpuTexture, CpuError> {
        let (w, h) = texture_size(size)?;
        let texture = CpuTexture::with(w, h, format, flipped)?;
        let stride = w as usize * 4;
        let expected = stride * h as usize;
        if data.len() < expected {
            return Err(CpuError::IncompleteBuffer {
                expected,
                actual: data.len(),
            });
        }
        let mut image = texture.write();
        let bounds = image.bounds();
        image.load(data, stride, texture.layout(), bounds, flipped);
        drop(image);
        Ok(texture)
    }

    fn update_memory(
        &mut self,
        texture: &CpuTexture,
        data: &[u8],
        region: Rectangle<i32, BufferCoord>,
    ) -> Result<(), CpuError> {
        let stride = texture.0.width as usize * 4;
        let expected = stride * texture.0.height as usize;
        if data.len() < expected {
            return Err(CpuError::IncompleteBuffer {
                expected,
                actual: data.len(),
            });
        }
        let mut region = Rect::of(region);
        if texture.0.flipped {
            // The region is in the memory's rows; ours run the other way.
            let h = texture.0.height as i32;
            region = Rect::new(region.x0, h - region.y1, region.x1, h - region.y0);
        }
        texture
            .write()
            .load(data, stride, texture.layout(), region, texture.0.flipped);
        Ok(())
    }

    fn mem_formats(&self) -> Box<dyn Iterator<Item = Fourcc>> {
        Box::new(FORMATS.into_iter())
    }
}

impl ImportMemWl for CpuRenderer {
    fn import_shm_buffer(
        &mut self,
        buffer: &WlBuffer,
        surface: Option<&SurfaceData>,
        damage: &[Rectangle<i32, BufferCoord>],
    ) -> Result<CpuTexture, CpuError> {
        let context = self.context.clone();
        let cache = surface.and_then(|surface| {
            surface
                .data_map
                .insert_if_missing_threadsafe(ShmCache::default);
            surface.data_map.get::<ShmCache>()
        });
        memory::with_shm(buffer, |bytes, data| {
            let format = shm_format_to_fourcc(data.format)
                .ok_or(CpuError::UnsupportedFormat(Fourcc::Argb8888))?;
            let layout = Layout::of(format).ok_or(CpuError::UnsupportedFormat(format))?;
            let (w, h) = texture_size((data.width, data.height).into())?;
            let (Ok(offset), Ok(stride)) =
                (usize::try_from(data.offset), usize::try_from(data.stride))
            else {
                return Err(CpuError::InvalidSize);
            };
            let pool = bytes.get(offset..).unwrap_or_default();
            let mut entries = cache.map(|c| c.0.lock().unwrap_or_else(PoisonError::into_inner));
            let cached = entries.as_ref().and_then(|entries| {
                entries
                    .iter()
                    .find(|(id, t)| {
                        *id == context && t.0.width == w && t.0.height == h && t.0.format == format
                    })
                    .map(|(_, t)| t.clone())
            });
            let texture = match cached {
                Some(texture) => {
                    let mut image = texture.write();
                    for rect in damage {
                        image.load(pool, stride, layout, Rect::of(*rect), false);
                    }
                    drop(image);
                    texture
                }
                None => {
                    let texture = CpuTexture::new(w, h, format)?;
                    let mut image = texture.write();
                    let bounds = image.bounds();
                    image.load(pool, stride, layout, bounds, false);
                    drop(image);
                    if let Some(entries) = &mut entries {
                        entries.retain(|(id, _)| *id != context);
                        entries.push((context.clone(), texture.clone()));
                    }
                    texture
                }
            };
            Ok(texture)
        })
        .map_err(CpuError::Shm)?
    }
}

impl ImportDma for CpuRenderer {
    fn dmabuf_formats(&self) -> FormatSet {
        Self::dmabuf_format_set()
    }

    fn import_dmabuf(
        &mut self,
        dmabuf: &Dmabuf,
        damage: Option<&[Rectangle<i32, BufferCoord>]>,
    ) -> Result<CpuTexture, CpuError> {
        let (layout, stride) = dmabuf_layout(dmabuf, false)?;
        let (w, h) = texture_size(dmabuf.size())?;
        let weak = dmabuf.weak();
        let index = match self.imports.iter().position(|i| i.buffer == weak) {
            Some(index) => index,
            None => {
                self.imports.push(DmabufImport {
                    buffer: weak,
                    texture: CpuTexture::new(w, h, dmabuf.format().code)?,
                    mapping: None,
                });
                self.imports.len() - 1
            }
        };
        let Some(import) = self.imports.get_mut(index) else {
            return Err(CpuError::InvalidSize);
        };
        let fresh = import.mapping.is_none();
        let mapping = match &mut import.mapping {
            Some(mapping) => mapping,
            mapping => mapping.insert(
                dmabuf
                    .map_plane(0, DmabufMappingMode::READ)
                    .map_err(CpuError::Map)?,
            ),
        };
        dmabuf
            .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::READ)
            .map_err(CpuError::Sync)?;
        let bytes = memory::mapped(mapping);
        let mut image = import.texture.write();
        match damage {
            Some(damage) if !fresh => {
                for rect in damage {
                    image.load(bytes, stride, layout, Rect::of(*rect), false);
                }
            }
            _ => {
                let bounds = image.bounds();
                image.load(bytes, stride, layout, bounds, false);
            }
        }
        drop(image);
        dmabuf
            .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::READ)
            .map_err(CpuError::Sync)?;
        Ok(import.texture.clone())
    }
}

impl ImportDmaWl for CpuRenderer {}

impl Bind<Dmabuf> for CpuRenderer {
    fn bind<'a>(&mut self, target: &'a mut Dmabuf) -> Result<CpuTarget<'a>, CpuError> {
        let (layout, stride) = dmabuf_layout(target, true)?;
        let weak = target.weak();
        let shadow = match self.targets.iter().find(|t| t.buffer == weak) {
            Some(entry) => entry.shadow.clone(),
            None => {
                let (w, h) = texture_size(target.size())?;
                let shadow = CpuTexture::new(w, h, target.format().code)?;
                // What the buffer holds already, for a frame that draws only damage on it.
                let mapping = target
                    .map_plane(0, DmabufMappingMode::READ | DmabufMappingMode::WRITE)
                    .map_err(CpuError::Map)?;
                if target
                    .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::READ)
                    .is_ok()
                {
                    let mut image = shadow.write();
                    let bounds = image.bounds();
                    image.load(memory::mapped(&mapping), stride, layout, bounds, false);
                    drop(image);
                    let _ = target.sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::READ);
                }
                self.targets.push(DmabufTarget {
                    buffer: weak,
                    shadow: shadow.clone(),
                    mapping: Some(mapping),
                });
                shadow
            }
        };
        Ok(CpuTarget(Target::Dmabuf {
            dmabuf: target,
            shadow,
        }))
    }

    fn supported_formats(&self) -> Option<FormatSet> {
        Some(Self::dmabuf_format_set())
    }
}

impl Bind<CpuTexture> for CpuRenderer {
    fn bind<'a>(&mut self, target: &'a mut CpuTexture) -> Result<CpuTarget<'a>, CpuError> {
        Ok(CpuTarget(Target::Texture(target.clone())))
    }
}

impl Offscreen<CpuTexture> for CpuRenderer {
    fn create_buffer(
        &mut self,
        format: Fourcc,
        size: Size<i32, BufferCoord>,
    ) -> Result<CpuTexture, CpuError> {
        let (w, h) = texture_size(size)?;
        CpuTexture::new(w, h, format)
    }
}

/// `region` of `texture` in `format`.
fn read_back(
    texture: &CpuTexture,
    region: Rectangle<i32, BufferCoord>,
    format: Fourcc,
) -> Result<CpuMapping, CpuError> {
    let layout = Layout::of(format).ok_or(CpuError::UnsupportedFormat(format))?;
    let (w, h) = texture_size(region.size)?;
    let stride = w as usize * 4;
    let mut data = vec![0; stride * h as usize];
    let region = Rect::of(region);
    texture
        .read()
        .store(&mut data, stride, layout, region, (region.x0, region.y0));
    Ok(CpuMapping {
        data,
        width: w,
        height: h,
        format,
    })
}

impl ExportMem for CpuRenderer {
    type TextureMapping = CpuMapping;

    fn copy_framebuffer(
        &mut self,
        target: &CpuTarget<'_>,
        region: Rectangle<i32, BufferCoord>,
        format: Fourcc,
    ) -> Result<CpuMapping, CpuError> {
        read_back(target.texture(), region, format)
    }

    fn copy_texture(
        &mut self,
        texture: &CpuTexture,
        region: Rectangle<i32, BufferCoord>,
        format: Fourcc,
    ) -> Result<CpuMapping, CpuError> {
        read_back(texture, region, format)
    }

    fn can_read_texture(&mut self, _texture: &CpuTexture) -> Result<bool, CpuError> {
        Ok(true)
    }

    fn map_texture<'a>(&mut self, texture_mapping: &'a CpuMapping) -> Result<&'a [u8], CpuError> {
        Ok(&texture_mapping.data)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    const RED: Color32F = Color32F::new(1.0, 0.0, 0.0, 1.0);
    const BLACK: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn texture(renderer: &mut CpuRenderer, w: i32, h: i32, pixels: &[u32]) -> CpuTexture {
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
        renderer
            .import_memory(&bytes, Fourcc::Argb8888, (w, h).into(), false)
            .unwrap()
    }

    /// Draws `draw` into a new `w` × `h` texture seen as an output with `transform`.
    fn draw(
        renderer: &mut CpuRenderer,
        (w, h): (i32, i32),
        transform: Transform,
        draw: impl FnOnce(&mut CpuFrame<'_, '_>),
    ) -> Vec<u32> {
        let mut target = renderer
            .create_buffer(Fourcc::Argb8888, (w, h).into())
            .unwrap();
        let mut framebuffer = renderer.bind(&mut target).unwrap();
        let mut frame = renderer
            .render(&mut framebuffer, (w, h).into(), transform)
            .unwrap();
        draw(&mut frame);
        frame.finish().unwrap().wait().unwrap();
        drop(framebuffer);
        target.read().pixels().to_vec()
    }

    #[test]
    fn implicit_modifiers_are_linear_only_on_targets() {
        let dmabuf = |modifier| {
            let fd = std::os::fd::OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
            let mut builder = Dmabuf::builder(
                (4, 2),
                Fourcc::Argb8888,
                modifier,
                smithay::backend::allocator::dmabuf::DmabufFlags::empty(),
            );
            builder.add_plane(fd, 0, 0, 16);
            builder.build().unwrap()
        };
        let implicit = dmabuf(Modifier::Invalid);
        assert!(dmabuf_layout(&implicit, true).is_ok_and(|(_, stride)| stride == 16));
        assert!(dmabuf_layout(&implicit, false).is_err());
        assert!(dmabuf_layout(&dmabuf(Modifier::Linear), false).is_ok());
    }

    #[test]
    fn clears_fills_and_copies() {
        let mut renderer = CpuRenderer::with_threads(1);
        let tex = texture(
            &mut renderer,
            2,
            2,
            &[0xff00_00ff, 0x8000_0080, 0, 0xffff_ffff],
        );
        let out = draw(&mut renderer, (8, 4), Transform::Normal, |frame| {
            frame.clear(BLACK, &[rect(0, 0, 8, 4)]).unwrap();
            frame
                .draw_solid(rect(0, 0, 2, 2), &[rect(0, 0, 9, 9)], RED)
                .unwrap();
            frame
                .render_texture_at(
                    &tex,
                    (4, 0).into(),
                    1,
                    1.0,
                    Transform::Normal,
                    &[rect(0, 0, 2, 2)],
                    &[],
                    1.0,
                )
                .unwrap();
        });
        // The damage is clamped to the destination.
        assert_eq!(&out[..3], &[0xffff_0000, 0xffff_0000, 0xff00_0000]);
        assert_eq!(out[4], 0xff00_00ff);
        // Half-transparent blue over black.
        assert_eq!(out[5], 0xff00_0080);
        assert_eq!(out[8 + 4], 0xff00_0000);
        assert_eq!(out[8 + 5], 0xffff_ffff);
    }

    #[test]
    fn scales_with_exact_corners_and_honours_alpha() {
        let mut renderer = CpuRenderer::with_threads(1);
        let tex = texture(
            &mut renderer,
            2,
            2,
            &[0xffff_0000, 0xff00_ff00, 0xff00_00ff, 0xffff_ffff],
        );
        let out = draw(&mut renderer, (4, 4), Transform::Normal, |frame| {
            frame
                .render_texture_from_to(
                    &tex,
                    Rectangle::from_size((2.0, 2.0).into()),
                    rect(0, 0, 4, 4),
                    &[rect(0, 0, 4, 4)],
                    &[],
                    Transform::Normal,
                    1.0,
                )
                .unwrap();
        });
        assert_eq!(
            [out[0], out[3], out[12], out[15]],
            [0xffff_0000, 0xff00_ff00, 0xff00_00ff, 0xffff_ffff]
        );
        let out = draw(&mut renderer, (2, 2), Transform::Normal, |frame| {
            frame
                .render_texture_at(
                    &tex,
                    (0, 0).into(),
                    1,
                    1.0,
                    Transform::Normal,
                    &[rect(0, 0, 2, 2)],
                    &[],
                    0.5,
                )
                .unwrap();
        });
        assert_eq!(out[0], 0x8080_0000);
    }

    #[test]
    fn opaque_regions_copy_and_the_rest_blends() {
        let mut renderer = CpuRenderer::with_threads(1);
        // Transparent pixels marked opaque are copied, as GLES does with blending off.
        let tex = texture(&mut renderer, 2, 1, &[0, 0]);
        let out = draw(&mut renderer, (2, 1), Transform::Normal, |frame| {
            frame.clear(RED, &[rect(0, 0, 2, 1)]).unwrap();
            frame
                .render_texture_at(
                    &tex,
                    (0, 0).into(),
                    1,
                    1.0,
                    Transform::Normal,
                    &[rect(0, 0, 2, 1)],
                    &[rect(1, 0, 1, 1)],
                    1.0,
                )
                .unwrap();
        });
        assert_eq!(out, [0xffff_0000, 0]);
    }

    #[test]
    fn an_analyzed_texture_draws_as_before() {
        let mut renderer = CpuRenderer::with_threads(1);
        // Runs of 16 transparent, opaque and translucent pixels, the last one cut short.
        let pixels: Vec<u32> = (0..70 * 3)
            .map(|i: u32| match (i % 70 / 16 + i / 70) % 3 {
                0 => 0,
                1 => 0xff00_0000 | i.wrapping_mul(2_654_435_761) >> 8,
                _ => 0x8040_2010 | (i & 0x0f),
            })
            .collect();
        let plain = texture(&mut renderer, 70, 3, &pixels);
        let analyzed = texture(&mut renderer, 70, 3, &pixels);
        analyzed.write().analyze();
        for alpha in [1.0, 0.5] {
            let draws = [&plain, &analyzed].map(|tex| {
                draw(&mut renderer, (80, 3), Transform::Normal, |frame| {
                    frame.clear(RED, &[rect(0, 0, 80, 3)]).unwrap();
                    frame
                        .render_texture_at(
                            tex,
                            (3, 0).into(),
                            1,
                            1.0,
                            Transform::Normal,
                            &[rect(5, 0, 60, 3)],
                            &[],
                            alpha,
                        )
                        .unwrap();
                })
            });
            assert_eq!(draws[0], draws[1], "alpha {alpha}");
        }
        let covers = |tex: &CpuTexture| {
            let mut covers = Vec::new();
            tex.read()
                .covers(0, 0, 70, |run, cover| covers.push((run, cover)));
            covers
        };
        use crate::image::Cover::{Clear, Mixed, Opaque};
        assert_eq!(
            covers(&analyzed),
            [
                (0..16, Clear),
                (16..32, Opaque),
                (32..48, Mixed),
                (48..64, Clear),
                (64..70, Opaque)
            ]
        );
        // Drawing into it forgets what it knew.
        analyzed.write().pixels_mut().fill(0);
        assert_eq!(covers(&analyzed), [(0..70, Mixed)]);
    }

    #[test]
    fn rotated_outputs_map_like_smithay() {
        let mut renderer = CpuRenderer::with_threads(1);
        let tex = texture(&mut renderer, 2, 2, &[1 << 24, 2 << 24, 3 << 24, 4 << 24]);
        // A 4 × 8 buffer is an 8 × 4 output turned by 90°.
        let out = draw(&mut renderer, (4, 8), Transform::_90, |frame| {
            frame
                .draw_solid(rect(7, 3, 1, 1), &[rect(0, 0, 1, 1)], RED)
                .unwrap();
            frame
                .render_texture_at(
                    &tex,
                    (0, 0).into(),
                    1,
                    1.0,
                    Transform::Normal,
                    &[rect(0, 0, 2, 2)],
                    &[],
                    1.0,
                )
                .unwrap();
        });
        let at = |x: usize, y: usize| out[y * 4 + x];
        let corner = Transform::_90.transform_rect_in(rect(7, 3, 1, 1), &(8, 4).into());
        assert_eq!(
            at(corner.loc.x as usize, corner.loc.y as usize),
            0xffff_0000
        );
        for (p, texel) in [((0, 0), 1), ((1, 0), 2), ((0, 1), 3), ((1, 1), 4)] {
            let t = Transform::_90.transform_rect_in(rect(p.0, p.1, 1, 1), &(8, 4).into());
            assert_eq!(
                at(t.loc.x as usize, t.loc.y as usize) >> 24,
                texel,
                "output pixel {p:?}"
            );
        }
    }

    #[test]
    fn bands_on_workers_draw_what_one_thread_does() {
        let scene = |renderer: &mut CpuRenderer| {
            let pixels: Vec<u32> = (0..64 * 48u32)
                .map(|i| {
                    let a = (i * 7) % 256;
                    (a << 24) | ((i % a.max(1)) << 16) | ((i * 3) % (a + 1)) << 8 | (a / 2)
                })
                .collect();
            let tex = texture(renderer, 64, 48, &pixels);
            draw(renderer, (517, 389), Transform::Normal, |frame| {
                frame
                    .clear(Color32F::new(0.1, 0.2, 0.3, 1.0), &[rect(0, 0, 517, 389)])
                    .unwrap();
                frame
                    .draw_solid(
                        rect(13, 17, 400, 300),
                        &[rect(0, 0, 400, 300)],
                        Color32F::new(0.2, 0.0, 0.2, 0.5),
                    )
                    .unwrap();
                for (i, alpha) in [1.0, 0.6, 0.3].into_iter().enumerate() {
                    let i = i as i32;
                    frame
                        .render_texture_from_to(
                            &tex,
                            Rectangle::new((3.0, 5.0).into(), (50.0, 40.0).into()),
                            rect(20 + 90 * i, 31 * i, 450 - 90 * i, 350 - 40 * i),
                            &[rect(0, 0, 999, 999)],
                            &[],
                            Transform::Normal,
                            alpha,
                        )
                        .unwrap();
                }
                frame
                    .render_texture_at(
                        &tex,
                        (-7, 300).into(),
                        1,
                        1.0,
                        Transform::_270,
                        &[rect(0, 0, 64, 64)],
                        &[],
                        1.0,
                    )
                    .unwrap();
            })
        };
        let one = scene(&mut CpuRenderer::with_threads(1));
        let many = scene(&mut CpuRenderer::with_threads(4));
        assert!(one == many);
    }

    #[test]
    fn odd_requests_draw_nothing_and_never_panic() {
        let mut renderer = CpuRenderer::with_threads(2);
        let tex = texture(&mut renderer, 3, 3, &[0xffff_ffff; 9]);
        let out = draw(&mut renderer, (4, 4), Transform::Flipped270, |frame| {
            for (src, dst) in [
                (
                    Rectangle::new((-5.0, -5.0).into(), (100.0, 1.0).into()),
                    rect(-50, -50, 60, 60),
                ),
                (Rectangle::from_size((0.0, 3.0).into()), rect(0, 0, 4, 4)),
                (
                    Rectangle::from_size((3.0, 3.0).into()),
                    rect(1_000_000, -1_000_000, 4, 4),
                ),
                (Rectangle::from_size((3.0, 3.0).into()), rect(0, 0, 0, 4)),
            ] {
                for transform in [Transform::Normal, Transform::_90, Transform::Flipped180] {
                    frame
                        .render_texture_from_to(
                            &tex,
                            src,
                            dst,
                            &[rect(-9, -9, 99, 99)],
                            &[rect(1, 1, 1, 1)],
                            transform,
                            f32::NAN,
                        )
                        .unwrap();
                }
            }
            frame
                .clear(RED, &[rect(-1_000_000, -1_000_000, 2_000_000, 2_000_000)])
                .unwrap();
        });
        assert!(out.iter().all(|p| *p == 0xffff_0000));
    }

    #[test]
    fn a_texture_drawn_into_itself_is_skipped() {
        let mut renderer = CpuRenderer::with_threads(1);
        let mut target = renderer
            .create_buffer(Fourcc::Argb8888, (2, 2).into())
            .unwrap();
        let source = target.clone();
        let mut framebuffer = renderer.bind(&mut target).unwrap();
        let mut frame = renderer
            .render(&mut framebuffer, (2, 2).into(), Transform::Normal)
            .unwrap();
        frame
            .render_texture_at(
                &source,
                (0, 0).into(),
                1,
                1.0,
                Transform::Normal,
                &[rect(0, 0, 2, 2)],
                &[],
                1.0,
            )
            .unwrap();
        frame
            .draw_solid(rect(0, 0, 1, 1), &[rect(0, 0, 1, 1)], RED)
            .unwrap();
        // Dropped, not finished: still drawn.
        drop(frame);
        drop(framebuffer);
        assert_eq!(target.read().pixel(0, 0), 0xffff_0000);
    }

    #[test]
    fn reads_back_a_region_in_any_known_format() {
        let mut renderer = CpuRenderer::with_threads(1);
        let tex = texture(&mut renderer, 2, 2, &[0, 0, 0, 0x8011_2233]);
        let mapping = renderer
            .copy_texture(
                &tex,
                Rectangle::new((1, 1).into(), (1, 1).into()),
                Fourcc::Abgr8888,
            )
            .unwrap();
        assert_eq!(
            renderer.map_texture(&mapping).unwrap(),
            &[0x11, 0x22, 0x33, 0x80]
        );
        assert!(
            renderer
                .copy_texture(&tex, Rectangle::from_size((2, 2).into()), Fourcc::Nv12)
                .is_err()
        );
    }

    #[test]
    fn updates_only_the_region() {
        let mut renderer = CpuRenderer::with_threads(1);
        let tex = texture(&mut renderer, 2, 1, &[1, 2]);
        let bytes: Vec<u8> = [7u32, 8].iter().flat_map(|p| p.to_le_bytes()).collect();
        renderer
            .update_memory(&tex, &bytes, Rectangle::new((1, 0).into(), (1, 1).into()))
            .unwrap();
        assert_eq!(tex.read().pixels(), &[1, 8]);
        assert!(
            renderer
                .update_memory(&tex, &bytes[..4], Rectangle::from_size((1, 1).into()))
                .is_err()
        );
    }
}
