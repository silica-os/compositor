//! Full-damage frames of a desktop-like scene, timed: the wallpaper, a window, a menu bar,
//! a Dock, a scaled thumbnail and the cursor at 1280 × 800.
//!
//! ```text
//! cargo test --release -p compositor-raster --test bench -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use compositor_raster::{CpuRenderer, CpuTexture};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::{Bind, Color32F, Frame, ImportMem, Offscreen, Renderer};
use smithay::utils::{Physical, Rectangle, Transform};

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
    Rectangle::new((x, y).into(), (w, h).into())
}

fn image(renderer: &mut CpuRenderer, w: i32, h: i32, format: Fourcc, alpha: u32) -> CpuTexture {
    let bytes: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let i = i as u32;
            let c = |s: u32| ((i * s) % 256) * alpha / 255;
            ((alpha << 24) | (c(3) << 16) | (c(5) << 8) | c(7)).to_le_bytes()
        })
        .collect();
    renderer
        .import_memory(&bytes, format, (w, h).into(), false)
        .unwrap()
}

fn frame_times(threads: usize, frames: usize) -> Vec<Duration> {
    let mut renderer = CpuRenderer::with_threads(threads);
    let wallpaper = image(&mut renderer, 1280, 800, Fourcc::Xrgb8888, 255);
    let window = image(&mut renderer, 640, 400, Fourcc::Argb8888, 255);
    let bar = image(&mut renderer, 1280, 24, Fourcc::Argb8888, 200);
    let dock = image(&mut renderer, 640, 64, Fourcc::Argb8888, 180);
    let cursor = image(&mut renderer, 24, 24, Fourcc::Argb8888, 128);
    let mut target = renderer
        .create_buffer(Fourcc::Argb8888, (1280, 800).into())
        .unwrap();
    let full = [rect(0, 0, 1280, 800)];
    (0..frames)
        .map(|i| {
            let start = Instant::now();
            let mut framebuffer = renderer.bind(&mut target).unwrap();
            let mut frame = renderer
                .render(&mut framebuffer, (1280, 800).into(), Transform::Normal)
                .unwrap();
            frame
                .clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &full)
                .unwrap();
            let at = |frame: &mut _, t: &CpuTexture, x, y, alpha| {
                Frame::render_texture_at(
                    frame,
                    t,
                    (x, y).into(),
                    1,
                    1.0,
                    Transform::Normal,
                    &full,
                    &[],
                    alpha,
                )
                .unwrap();
            };
            at(&mut frame, &wallpaper, 0, 0, 1.0);
            let x = 300 + (i % 20) as i32;
            // The window: opaque inside, rounded corners outside.
            Frame::render_texture_at(
                &mut frame,
                &window,
                (x, 160).into(),
                1,
                1.0,
                Transform::Normal,
                &full,
                &[rect(0, 12, 640, 376)],
                1.0,
            )
            .unwrap();
            frame
                .render_texture_from_to(
                    &window,
                    Rectangle::from_size((640.0, 400.0).into()),
                    rect(40, 500, 320, 200),
                    &full,
                    &[],
                    Transform::Normal,
                    0.9,
                )
                .unwrap();
            at(&mut frame, &bar, 0, 0, 1.0);
            at(&mut frame, &dock, 320, 728, 1.0);
            at(&mut frame, &cursor, 640, 400, 1.0);
            frame.finish().unwrap().wait().unwrap();
            drop(framebuffer);
            start.elapsed()
        })
        .collect()
}

#[test]
#[ignore = "a bench: run it in release, see the module docs"]
fn full_damage_frames() {
    for threads in [1, 2, 4] {
        let mut times = frame_times(threads, 200);
        times.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!(
            "{threads} threads: p50 {:.3} ms, p99 {:.3} ms",
            ms(times[times.len() / 2]),
            ms(times[times.len() * 99 / 100])
        );
    }
}
