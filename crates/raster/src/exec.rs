//! A frame's display list and its execution in bands of rows: each band runs every command
//! on its rows while they sit in the cache, and the bands run on the renderer's workers.

use std::cell::RefCell;
use std::fmt;
use std::ops::Range;
use std::sync::{Arc, RwLockReadGuard, TryLockError};

use rayon::ThreadPool;
use rayon::prelude::*;
use smithay::backend::renderer::TextureFilter;

use crate::CpuTexture;
use crate::image::{Image, Rect, dim, index};
use crate::kernels::{Kernels, Walk, sample_bilinear, sample_nearest};

/// Rows per band: 16 rows of a 1280-wide target are 80 KiB, which stay in L2 while every
/// command of the frame runs on them.
const BAND_ROWS: usize = 16;

/// Below this many pixels of work a frame runs on the calling thread: waking the workers
/// costs more than it saves.
const PARALLEL_PIXELS: u64 = 64 * 1024;

/// An effect drawn by its own code (glass, shadows, the genie), band by band.
pub trait Effect: Send + Sync + fmt::Debug {
    /// Draws the pixels of `rect` (target pixels, within the band's rows) into the band.
    fn draw(&self, band: &mut Band<'_>, rect: Rect);
}

/// The rows `y0..y0 + rows / width` of the target, for one band's commands.
pub struct Band<'a> {
    y0: i32,
    width: usize,
    pixels: &'a mut [u32],
    kernels: Kernels,
}

impl Band<'_> {
    /// The target rows this band holds.
    pub fn rows(&self) -> Range<i32> {
        let rows = self.pixels.len().checked_div(self.width).unwrap_or(0);
        self.y0..self.y0 + dim(rows)
    }

    /// Pixels `x0..x1` of target row `y`, if the band holds them.
    pub fn span_mut(&mut self, y: i32, x0: i32, x1: i32) -> Option<&mut [u32]> {
        let row = index(y - self.y0)?;
        let (x0, x1) = (index(x0)?, index(x1)?);
        if x1 > self.width {
            return None;
        }
        let start = row.checked_mul(self.width)?;
        self.pixels.get_mut(start + x0..start + x1)
    }

    pub fn kernels(&self) -> Kernels {
        self.kernels
    }
}

/// Where a texture command reads its source pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Map {
    /// One texel per pixel: target pixel `(x, y)` reads source pixel `(x + dx, y + dy)`.
    Offset { dx: i32, dy: i32 },
    /// Any affine map: `u = xx·x + xy·y + x0`, `v = yx·x + yy·y + y0` from target pixel
    /// centers to texel coordinates (texel centers at .5).
    Affine {
        u: [f64; 3],
        v: [f64; 3],
        filter: TextureFilter,
    },
}

#[derive(Debug)]
pub(crate) enum Op {
    /// Replace with a color.
    Fill(u32),
    /// A color over.
    FillOver(u32),
    /// A texture × alpha (0..=255), over or replacing (`copy`).
    Texture {
        source: usize,
        map: Map,
        alpha: u32,
        copy: bool,
    },
    Effect(Arc<dyn Effect>),
}

#[derive(Debug)]
struct Command {
    op: Op,
    rects: Range<usize>,
}

/// The commands of a frame, in order, with the target rectangles each one covers (already
/// transformed, clipped to the target and, for an offset texture, to its source).
#[derive(Debug, Default)]
pub(crate) struct DisplayList {
    commands: Vec<Command>,
    rects: Vec<Rect>,
    sources: Vec<CpuTexture>,
}

impl DisplayList {
    pub fn clear(&mut self) {
        self.commands.clear();
        self.rects.clear();
        self.sources.clear();
    }

    /// Every rectangle the commands draw into.
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// The index of `texture` among the sources, added if new.
    pub fn source(&mut self, texture: &CpuTexture) -> usize {
        match self.sources.iter().position(|s| s.same(texture)) {
            Some(i) => i,
            None => {
                self.sources.push(texture.clone());
                self.sources.len() - 1
            }
        }
    }

    /// Adds `op` over `rects` (dropping the empty ones); nothing if none is left.
    pub fn push(&mut self, op: Op, rects: impl IntoIterator<Item = Rect>) {
        let start = self.rects.len();
        self.rects
            .extend(rects.into_iter().filter(|r| !r.is_empty()));
        if self.rects.len() > start {
            self.commands.push(Command {
                op,
                rects: start..self.rects.len(),
            });
        }
    }

    /// Runs the commands on `target`.
    pub fn execute(&self, target: &mut Image, kernels: Kernels, pool: Option<&ThreadPool>) {
        let Some(rows) = self.rows(target.height()) else {
            return;
        };
        // The sources stay read-locked for the whole run; one that cannot be read (the
        // target itself, sampled while drawn into) skips its commands.
        let guards: Vec<Option<RwLockReadGuard<'_, Image>>> = self
            .sources
            .iter()
            .map(|s| match s.0.image.try_read() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    tracing::debug!("a texture drawn into its own frame: skipped");
                    None
                }
            })
            .collect();
        let sources: Vec<Option<&Image>> = guards.iter().map(|g| g.as_deref()).collect();
        let width = target.width();
        if width == 0 {
            return;
        }
        let Some(pixels) = target
            .pixels_mut()
            .get_mut(rows.start * width..rows.end * width)
        else {
            return;
        };
        let band = |(i, pixels): (usize, &mut [u32])| {
            let mut band = Band {
                y0: dim(rows.start + i * BAND_ROWS),
                width,
                pixels,
                kernels,
            };
            self.run_band(&mut band, &sources);
        };
        let work: u64 = self.rects.iter().map(|r| r.area()).sum();
        match pool {
            Some(pool) if work >= PARALLEL_PIXELS => pool.install(|| {
                pixels
                    .par_chunks_mut(BAND_ROWS * width)
                    .enumerate()
                    .for_each(band);
            }),
            _ => pixels
                .chunks_mut(BAND_ROWS * width)
                .enumerate()
                .for_each(band),
        }
    }

    /// The rows the commands touch, within `0..height`.
    fn rows(&self, height: usize) -> Option<Range<usize>> {
        let y0 = self.rects.iter().map(|r| r.y0).min()?;
        let y1 = self.rects.iter().map(|r| r.y1).max()?;
        let (y0, y1) = (index(y0.max(0))?, index(y1.max(0))?.min(height));
        (y0 < y1).then_some(y0..y1)
    }

    fn run_band(&self, band: &mut Band<'_>, sources: &[Option<&Image>]) {
        let rows = band.rows();
        let band_rect = Rect::new(0, rows.start, dim(band.width), rows.end);
        let kernels = band.kernels;
        for command in &self.commands {
            let rects = self.rects.get(command.rects.clone()).unwrap_or_default();
            for rect in rects.iter().filter_map(|r| r.intersect(band_rect)) {
                match &command.op {
                    Op::Fill(color) => each_row(band, rect, |dst, _| kernels.fill(dst, *color)),
                    Op::FillOver(color) => {
                        each_row(band, rect, |dst, _| kernels.fill_over(dst, *color))
                    }
                    Op::Texture {
                        source,
                        map,
                        alpha,
                        copy,
                    } => {
                        let Some(Some(src)) = sources.get(*source) else {
                            continue;
                        };
                        let blend = |dst: &mut [u32], s: &[u32]| {
                            if *copy {
                                kernels.copy(dst, s, *alpha);
                            } else {
                                kernels.over(dst, s, *alpha);
                            }
                        };
                        match *map {
                            Map::Offset { dx, dy } => each_row(band, rect, |dst, y| {
                                if let Some(s) = src.span(y + dy, rect.x0 + dx, rect.x1 + dx) {
                                    blend(dst, s);
                                }
                            }),
                            Map::Affine { u, v, filter } => SCRATCH.with_borrow_mut(|scratch| {
                                scratch.resize(index(rect.width()).unwrap_or(0), 0);
                                each_row(band, rect, |dst, y| {
                                    let (x, y) = (f64::from(rect.x0) + 0.5, f64::from(y) + 0.5);
                                    let walk = Walk {
                                        u: fixed(u[0] * x + u[1] * y + u[2]),
                                        v: fixed(v[0] * x + v[1] * y + v[2]),
                                        du: fixed(u[0]),
                                        dv: fixed(v[0]),
                                    };
                                    match filter {
                                        TextureFilter::Linear => {
                                            sample_bilinear(src, walk, scratch)
                                        }
                                        TextureFilter::Nearest => {
                                            sample_nearest(src, walk, scratch)
                                        }
                                    }
                                    blend(dst, scratch);
                                });
                            }),
                        }
                    }
                    Op::Effect(effect) => effect.draw(band, rect),
                }
            }
        }
    }
}

thread_local! {
    /// A row of sampled texels, per thread.
    static SCRATCH: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` on every row of `rect` in the band, with the row's target `y`.
fn each_row(band: &mut Band<'_>, rect: Rect, mut f: impl FnMut(&mut [u32], i32)) {
    for y in rect.y0..rect.y1 {
        if let Some(dst) = band.span_mut(y, rect.x0, rect.x1) {
            f(dst, y);
        }
    }
}

/// 16.16 fixed point, saturating far outside any texture.
fn fixed(v: f64) -> i64 {
    (v * 65536.0)
        .round()
        .clamp(-(1i64 << 46) as f64, (1i64 << 46) as f64) as i64
}
