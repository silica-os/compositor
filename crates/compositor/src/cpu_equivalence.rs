//! The CPU renderer against GLES (headless, llvmpipe): every effect but the glass (that is in
//! `glass_equivalence`) drawn by both renderers over the same backdrops. They may differ by at
//! most 1/255 on average over the effect and 3/255 at any pixel and channel.
//! `CPU_EQUIVALENCE_OUT` (a directory) keeps the images of the cases over the thresholds.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use compositor_raster::CpuRenderer;
use compositor_render::Program;
use compositor_render::animation::genie_minimize;
use compositor_render::backdrop::Backdrop;
use compositor_render::border::{self, WindowBorder};
use compositor_render::corners::{Rounding, draw_rounded, radius_for};
use compositor_render::filter::FilterElement;
use compositor_render::genie::{GenieElement, GenieParts};
use compositor_render::renderer::{SilicaFrame, SilicaRenderer};
use compositor_render::shadow::{self, WindowShadow};
use compositor_render::titlebar::{Disabled, Titlebar, TitlebarState};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::Uniform;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Bind, ExportMem, Frame, Offscreen, Renderer};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::glass_equivalence::{
    Image, Layer, MAX_ANY, MAX_MEAN, backdrops, difference, gpu, import, save,
};

/// The canvas, in points.
const CANVAS_PT: (i32, i32) = (360, 220);

/// The window on it, in points.
const WINDOW_PT: (i32, i32, i32, i32) = (50, 40, 240, 140);

fn canvas(scale: f64) -> Size<i32, Physical> {
    Size::from((
        (f64::from(CANVAS_PT.0) * scale).round() as i32,
        (f64::from(CANVAS_PT.1) * scale).round() as i32,
    ))
}

fn window_pt() -> Rectangle<i32, Logical> {
    let (x, y, w, h) = WINDOW_PT;
    Rectangle::new((x, y).into(), (w, h).into())
}

/// Both renderers: GLES first.
fn renderers() -> [SilicaRenderer; 2] {
    let mut renderers = [gpu().renderer, SilicaRenderer::Cpu(CpuRenderer::new())];
    for renderer in &mut renderers {
        // Compiles the GLSL programs (none on the CPU).
        let shaders = compositor_render::shaders(renderer);
        assert!(shaders.corners.is_some() && shaders.titlebar.is_some());
    }
    renderers
}

/// What `draw` draws over black into a `size` canvas, read back.
fn render(
    renderer: &mut SilicaRenderer,
    size: Size<i32, Physical>,
    draw: impl FnOnce(&mut SilicaFrame<'_, '_>),
) -> Image {
    let buffer = Size::<i32, Buffer>::from((size.w, size.h));
    let mut texture = renderer.create_buffer(Fourcc::Abgr8888, buffer).unwrap();
    let mut target = renderer.bind(&mut texture).unwrap();
    {
        let mut frame = renderer
            .render(&mut target, size, Transform::Normal)
            .unwrap();
        frame
            .clear([0.0, 0.0, 0.0, 1.0].into(), &[Rectangle::from_size(size)])
            .unwrap();
        draw(&mut frame);
        let _ = frame.finish().unwrap();
    }
    let mapping = renderer
        .copy_framebuffer(&target, Rectangle::from_size(buffer), Fourcc::Abgr8888)
        .unwrap();
    let rgba = renderer.map_texture(&mapping).unwrap().to_vec();
    Image {
        w: size.w,
        h: size.h,
        rgba,
    }
}

/// Draws all of `element`, at its geometry at `scale`.
fn draw<E: RenderElement<SilicaRenderer>>(
    frame: &mut SilicaFrame<'_, '_>,
    element: &E,
    scale: f64,
) {
    let geometry = element.geometry(Scale::from(scale));
    element
        .draw(
            frame,
            element.src(),
            geometry,
            &[Rectangle::from_size(geometry.size)],
            &[],
        )
        .unwrap();
}

/// One case: the mean over `area` and the max anywhere within the thresholds, or a failure.
fn check(
    name: &str,
    [gles, cpu]: &[Image; 2],
    area: Rectangle<i32, Physical>,
    failures: &mut Vec<String>,
) {
    let (mean, max) = difference(gles, cpu, area);
    eprintln!("{name:<44} mean {mean:.3}, max {max}");
    if mean <= MAX_MEAN && max <= MAX_ANY {
        return;
    }
    if let Ok(dir) = std::env::var("CPU_EQUIVALENCE_OUT") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        save(gles, &dir.join(format!("{name}-gles.png")));
        save(cpu, &dir.join(format!("{name}-cpu.png")));
    }
    failures.push(format!("{name}: mean {mean:.3}, max {max}"));
}

/// A backdrop of the set, by name.
fn backdrop(size: Size<i32, Physical>, scale: f64, name: &str) -> Image {
    backdrops(size.w, size.h, scale)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, image)| image)
        .unwrap()
}

/// The shadow, border, veil and title bar of a window in every state.
#[test]
fn the_cpu_draws_window_decorations_like_gles() {
    let mut renderers = renderers();
    let mut failures = Vec::new();
    let window = window_pt();
    for scale in [1.0, 1.5, 2.0] {
        let size = canvas(scale);
        let image = backdrop(size, scale, "wallpaper-soft");
        let layers = renderers
            .each_mut()
            .map(|r| Layer::new(r, image.clone(), Point::from((0, 0))));
        let states = [
            ("plain", 0.0, None, false, Disabled::None),
            ("hover", 1.0, None, false, Disabled::None),
            ("pressed", 1.0, Some(0), false, Disabled::None),
            ("alt", 1.0, None, true, Disabled::None),
            ("dialog", 1.0, None, false, Disabled::MinimizeAndZoom),
        ];
        for dark in [false, true] {
            for active in [false, true] {
                for (what, symbols, pressed, alt, disabled) in states {
                    let state = TitlebarState {
                        active,
                        symbols,
                        pressed,
                        alt,
                        disabled,
                        scale,
                        bar: true,
                        bar_height: 32.0,
                        dark,
                    };
                    let shadow = WindowShadow::default().element(
                        Program::Shadow,
                        window,
                        shadow::style(active, dark),
                    );
                    let bar = Titlebar::default().element(Program::Titlebar, window, state);
                    let border = border::style(active, dark).map(|style| {
                        WindowBorder::default().element(Program::Border, window, style, scale)
                    });
                    let veil = WindowBorder::default().veil_element(
                        Program::Border,
                        window,
                        border::veil(dark),
                        scale,
                    );
                    let images = [0, 1].map(|i| {
                        render(&mut renderers[i], size, |frame| {
                            draw(frame, &layers[i], 1.0);
                            draw(frame, &shadow, scale);
                            draw(frame, &veil, scale);
                            draw(frame, &bar, scale);
                            if let Some(border) = &border {
                                draw(frame, border, scale);
                            }
                        })
                    });
                    let appearance = if dark { "dark" } else { "light" };
                    let focus = if active { "active" } else { "inactive" };
                    check(
                        &format!("decorations-{appearance}-{focus}-{what}-{scale}x"),
                        &images,
                        shadow.geometry(Scale::from(scale)),
                        &mut failures,
                    );
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the CPU differs from GLES: {failures:#?}"
    );
}

/// The window corners, at whole and fractional scales.
#[test]
fn the_cpu_cuts_window_corners_like_gles() {
    let mut renderers = renderers();
    let mut failures = Vec::new();
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let size = canvas(scale);
        let under = backdrop(size, scale, "wallpaper-soft");
        let window = window_pt().to_f64().to_physical(scale);
        let at = Point::from((window.loc.x.round() as i32, window.loc.y.round() as i32));
        let content = backdrop(
            Size::from((window.size.w.round() as i32, window.size.h.round() as i32)),
            scale,
            "text-dark",
        );
        let window = Rectangle::new(at.to_f64(), content.size().to_f64());
        let rounding = Rounding {
            window,
            radius: radius_for(window.size, scale),
            output: size,
            shift: Point::default(),
        };
        let images = [0, 1].map(|i| {
            let renderer = &mut renderers[i];
            let under = Layer::new(renderer, under.clone(), Point::from((0, 0)));
            let content = Layer::new(renderer, content.clone(), at);
            render(renderer, size, |frame| {
                draw(frame, &under, 1.0);
                draw_rounded(frame, Some(Program::Corners), rounding, |frame| {
                    let geometry = content.geometry(Scale::from(1.0));
                    content.draw(
                        frame,
                        content.src(),
                        geometry,
                        &[Rectangle::from_size(geometry.size)],
                        &[],
                    )
                })
                .unwrap();
            })
        });
        let area = Rectangle::new(at, content.size());
        check(&format!("corners-{scale}x"), &images, area, &mut failures);
    }
    assert!(
        failures.is_empty(),
        "the CPU differs from GLES: {failures:#?}"
    );
}

/// The display filters over a photo and text.
#[test]
fn the_cpu_filters_colors_like_gles() {
    let mut renderers = renderers();
    let mut failures = Vec::new();
    let size = canvas(1.0);
    for name in ["wallpaper-photo", "text-light", "color-edges"] {
        let image = backdrop(size, 1.0, name);
        for mode in [0.0_f32, 1.0, 2.0, 3.0] {
            for contrast in [1.0_f32, 1.35] {
                let images = [0, 1].map(|i| {
                    let renderer = &mut renderers[i];
                    let texture = import(renderer, &image);
                    let inner = TextureRenderElement::from_static_texture(
                        Id::new(),
                        renderer.context_id(),
                        (0.0, 0.0),
                        texture,
                        1,
                        Transform::Normal,
                        None,
                        None,
                        None,
                        None,
                        Kind::Unspecified,
                    );
                    let element = FilterElement::new(
                        inner,
                        Program::Filter,
                        vec![
                            Uniform::new("mode", mode),
                            Uniform::new("contrast", contrast),
                        ],
                    );
                    render(renderer, size, |frame| draw(frame, &element, 1.0))
                });
                check(
                    &format!("filter-{name}-mode{mode}-contrast{contrast}"),
                    &images,
                    Rectangle::from_size(size),
                    &mut failures,
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the CPU differs from GLES: {failures:#?}"
    );
}

/// The genie at several instants of minimizing into a Dock icon.
#[test]
fn the_cpu_draws_the_genie_like_gles() {
    let mut renderers = renderers();
    let mut failures = Vec::new();
    for scale in [1.0, 2.0] {
        let size = canvas(scale);
        let under = backdrop(size, scale, "wallpaper-soft");
        let window = window_pt().to_physical_precise_round(scale);
        let snapshot = backdrop(window.size, scale, "text-dark");
        let target = Rectangle::new(
            Point::from((250.0 * scale, 196.0 * scale)),
            Size::from((22.0 * scale, 22.0 * scale)),
        );
        for ms in [0.0, 40.0, 120.0, 200.0, 300.0, 420.0] {
            let images = [0, 1].map(|i| {
                let renderer = &mut renderers[i];
                let under = Layer::new(renderer, under.clone(), Point::from((0, 0)));
                let genie = GenieElement::new(GenieParts {
                    id: Id::new(),
                    commit: CommitCounter::default(),
                    context_id: renderer.context_id(),
                    texture: import(renderer, &snapshot),
                    snapshot: window,
                    window: window.to_f64(),
                    target,
                    shape: genie_minimize(ms),
                    output: size,
                    program: Some(Program::Genie),
                })
                .unwrap();
                render(renderer, size, |frame| {
                    draw(frame, &under, 1.0);
                    draw(frame, &genie, 1.0);
                })
            });
            check(
                &format!("genie-{ms}ms-{scale}x"),
                &images,
                Rectangle::from_size(size),
                &mut failures,
            );
        }
    }
    assert!(
        failures.is_empty(),
        "the CPU differs from GLES: {failures:#?}"
    );
}

/// The menu bar's tone over light and dark wallpapers.
#[test]
fn the_cpu_tones_the_bar_like_gles() {
    let mut renderers = renderers();
    let mut failures = Vec::new();
    for scale in [1.0, 2.0] {
        let size = canvas(scale);
        for name in [
            "wallpaper-soft",
            "wallpaper-photo",
            "text-light",
            "text-dark",
        ] {
            let Some((_, image)) = backdrops(size.w, size.h, scale)
                .into_iter()
                .find(|(n, _)| n == name)
            else {
                continue;
            };
            let images = [0, 1].map(|i| {
                let renderer = &mut renderers[i];
                let layers = [Layer::new(renderer, image.clone(), Point::from((0, 0)))];
                let tone = Backdrop::default()
                    .element(
                        renderer,
                        Program::Tone,
                        &layers,
                        size,
                        scale,
                        [0.0, 0.0, 0.0, 1.0],
                    )
                    .unwrap();
                render(renderer, size, |frame| {
                    draw(frame, &layers[0], 1.0);
                    draw(frame, &tone, scale);
                })
            });
            check(
                &format!("tone-{name}-{scale}x"),
                &images,
                Rectangle::from_size(size),
                &mut failures,
            );
        }
    }
    assert!(
        failures.is_empty(),
        "the CPU differs from GLES: {failures:#?}"
    );
}
