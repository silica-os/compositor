//! Pixels in memory: premultiplied BGRA8, one `u32` per pixel (`0xAARRGGBB`, which is DRM's
//! `Argb8888` in little-endian memory), rows packed with no padding.

use smithay::backend::allocator::Fourcc;
use smithay::utils::Rectangle;

/// A rectangle of pixels, `x0..x1` × `y0..y1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn of<K>(rect: Rectangle<i32, K>) -> Self {
        Self {
            x0: rect.loc.x,
            y0: rect.loc.y,
            x1: rect.loc.x.saturating_add(rect.size.w),
            y1: rect.loc.y.saturating_add(rect.size.h),
        }
    }

    pub fn to_rectangle<K>(self) -> Rectangle<i32, K> {
        Rectangle::new(
            (self.x0, self.y0).into(),
            (self.width(), self.height()).into(),
        )
    }

    pub fn width(self) -> i32 {
        (self.x1 - self.x0).max(0)
    }

    pub fn height(self) -> i32 {
        (self.y1 - self.y0).max(0)
    }

    pub fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn area(self) -> u64 {
        u64::from(self.width().unsigned_abs()) * u64::from(self.height().unsigned_abs())
    }

    pub fn intersect(self, other: Rect) -> Option<Rect> {
        let rect = Rect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (!rect.is_empty()).then_some(rect)
    }

    pub fn translate(self, dx: i32, dy: i32) -> Rect {
        Rect {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
        }
    }
}

/// How a memory format lays out a pixel, relative to ours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// `Argb8888`: ours.
    Argb,
    /// `Xrgb8888`: ours, alpha ignored (opaque).
    Xrgb,
    /// `Abgr8888`: red and blue swapped.
    Abgr,
    /// `Xbgr8888`: red and blue swapped, opaque.
    Xbgr,
}

/// The formats the renderer reads and writes.
pub const FORMATS: [Fourcc; 4] = [
    Fourcc::Argb8888,
    Fourcc::Xrgb8888,
    Fourcc::Abgr8888,
    Fourcc::Xbgr8888,
];

impl Layout {
    pub fn of(format: Fourcc) -> Option<Layout> {
        match format {
            Fourcc::Argb8888 => Some(Layout::Argb),
            Fourcc::Xrgb8888 => Some(Layout::Xrgb),
            Fourcc::Abgr8888 => Some(Layout::Abgr),
            Fourcc::Xbgr8888 => Some(Layout::Xbgr),
            _ => None,
        }
    }

    pub fn opaque(self) -> bool {
        matches!(self, Layout::Xrgb | Layout::Xbgr)
    }

    /// A pixel read from memory in this layout, as ours.
    #[inline(always)]
    fn load(self, p: u32) -> u32 {
        match self {
            Layout::Argb => p,
            Layout::Xrgb => p | 0xff00_0000,
            Layout::Abgr => swap_red_blue(p),
            Layout::Xbgr => swap_red_blue(p) | 0xff00_0000,
        }
    }

    /// Our pixel as this layout stores it.
    #[inline(always)]
    fn store(self, p: u32) -> u32 {
        match self {
            Layout::Argb | Layout::Xrgb => p,
            Layout::Abgr | Layout::Xbgr => swap_red_blue(p),
        }
    }

    /// Converts a row of bytes in this layout into our pixels (as many as both hold).
    pub fn load_row(self, src: &[u8], dst: &mut [u32]) {
        for (d, s) in dst.iter_mut().zip(src.as_chunks::<4>().0) {
            *d = self.load(u32::from_le_bytes(*s));
        }
    }

    /// Converts our pixels into a row of bytes in this layout.
    pub fn store_row(self, src: &[u32], dst: &mut [u8]) {
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src) {
            *d = self.store(*s).to_le_bytes();
        }
    }
}

#[inline(always)]
fn swap_red_blue(p: u32) -> u32 {
    (p & 0xff00_ff00) | ((p >> 16) & 0xff) | ((p & 0xff) << 16)
}

/// An image of premultiplied BGRA8 pixels.
#[derive(Debug, Clone, Default)]
pub struct Image {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

impl Image {
    /// A transparent image.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width.saturating_mul(height)],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, dim(self.width), dim(self.height))
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    pub fn row(&self, y: usize) -> Option<&[u32]> {
        let start = y.checked_mul(self.width)?;
        self.pixels.get(start..start.checked_add(self.width)?)
    }

    pub fn row_mut(&mut self, y: usize) -> Option<&mut [u32]> {
        let start = y.checked_mul(self.width)?;
        self.pixels.get_mut(start..start.checked_add(self.width)?)
    }

    /// Pixels `x0..x1` of row `y`, if all inside.
    pub fn span(&self, y: i32, x0: i32, x1: i32) -> Option<&[u32]> {
        let (y, x0, x1) = (index(y)?, index(x0)?, index(x1)?);
        self.row(y)?.get(x0..x1)
    }

    pub fn span_mut(&mut self, y: i32, x0: i32, x1: i32) -> Option<&mut [u32]> {
        let (y, x0, x1) = (index(y)?, index(x0)?, index(x1)?);
        self.row_mut(y)?.get_mut(x0..x1)
    }

    /// The pixel at `(x, y)`, transparent outside.
    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        self.span(y, x, x + 1)
            .and_then(|p| p.first().copied())
            .unwrap_or(0)
    }

    /// Copies `region` of a memory image (`stride` bytes per row, `layout`, rows from the
    /// top unless `flipped`) into the same place of this one. Whatever falls outside either
    /// is skipped.
    pub fn load(
        &mut self,
        bytes: &[u8],
        stride: usize,
        layout: Layout,
        region: Rect,
        flipped: bool,
    ) {
        let Some(region) = region.intersect(self.bounds()) else {
            return;
        };
        let height = self.height;
        for y in region.y0..region.y1 {
            let Some(src_y) = index(y).map(|y| if flipped { height - 1 - y } else { y }) else {
                continue;
            };
            let start = src_y
                .checked_mul(stride)
                .and_then(|s| s.checked_add(index(region.x0)? * 4));
            let end = start.and_then(|s| s.checked_add(index(region.width())? * 4));
            let (Some(src), Some(dst)) = (
                start.zip(end).and_then(|(s, e)| bytes.get(s..e)),
                self.span_mut(y, region.x0, region.x1),
            ) else {
                continue;
            };
            layout.load_row(src, dst);
        }
    }

    /// Writes `region` of this image into a memory image (`stride` bytes per row, `layout`)
    /// whose first byte is pixel `origin` of this one.
    pub fn store(
        &self,
        bytes: &mut [u8],
        stride: usize,
        layout: Layout,
        region: Rect,
        origin: (i32, i32),
    ) {
        let Some(region) = region.intersect(self.bounds()) else {
            return;
        };
        for y in region.y0..region.y1 {
            let start = index(y - origin.1)
                .and_then(|y| y.checked_mul(stride))
                .and_then(|s| s.checked_add(index(region.x0 - origin.0)? * 4));
            let end = start.and_then(|s| s.checked_add(index(region.width())? * 4));
            let (Some(dst), Some(src)) = (
                start.zip(end).and_then(|(s, e)| bytes.get_mut(s..e)),
                self.span(y, region.x0, region.x1),
            ) else {
                continue;
            };
            layout.store_row(src, dst);
        }
    }
}

pub(crate) fn index(v: i32) -> Option<usize> {
    usize::try_from(v).ok()
}

pub(crate) fn dim(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_round_trip_through_our_pixels() {
        let bytes = [10u8, 20, 30, 40, 50, 60, 70, 80];
        for layout in [Layout::Argb, Layout::Abgr] {
            let mut pixels = [0u32; 2];
            layout.load_row(&bytes, &mut pixels);
            let mut back = [0u8; 8];
            layout.store_row(&pixels, &mut back);
            assert_eq!(back, bytes);
        }
        let mut pixels = [0u32; 2];
        Layout::Abgr.load_row(&bytes, &mut pixels);
        // R, G, B, A in memory: our 0xAARRGGBB.
        assert_eq!(pixels[0], 0x280a_141e);
        Layout::Xrgb.load_row(&bytes, &mut pixels);
        assert_eq!(pixels[0] >> 24, 0xff);
    }

    #[test]
    fn load_skips_what_falls_outside() {
        let mut image = Image::new(3, 2);
        let bytes = vec![0xffu8; 4 * 4 * 2];
        image.load(&bytes, 16, Layout::Argb, Rect::new(-1, -1, 9, 9), false);
        assert!(image.pixels().iter().all(|p| *p == u32::MAX));
        // A buffer shorter than it claims: the missing rows are skipped.
        let mut image = Image::new(3, 2);
        image.load(&bytes[..12], 16, Layout::Argb, image.bounds(), false);
        assert_eq!(image.pixel(0, 0), u32::MAX);
        assert_eq!(image.pixel(0, 1), 0);
    }

    #[test]
    fn flipped_rows_come_from_the_bottom() {
        let mut image = Image::new(1, 2);
        let bytes = [1u8, 0, 0, 0, 2, 0, 0, 0];
        image.load(&bytes, 4, Layout::Argb, image.bounds(), true);
        assert_eq!(image.pixel(0, 0), 2);
        assert_eq!(image.pixel(0, 1), 1);
    }
}
