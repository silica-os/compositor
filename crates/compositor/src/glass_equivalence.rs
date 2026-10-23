//! The glass blur on the GPU (headless GLES, llvmpipe): the reduced-resolution blur of every
//! glass kind against the full-resolution one, and the backdrop cache against a glass computed
//! from scratch (#204).
//!
//! Equivalence: each kind, in light and dark, is drawn over a set of backdrops (wallpaper-like
//! images, text, sharp edges, light and dark) at full resolution and at its `BlurScale`; the
//! two may differ by at most 1/255 on average over the glass and 3/255 at any pixel and
//! channel. `GLASS_EQUIVALENCE_WALLPAPERS` (PNG paths separated by `:`) adds real wallpapers;
//! The Panorama spaces bar is Glass/Regular: it uses the panels' profile and so their scale.
//! `cargo test -p compositor glass_equivalence -- --ignored --nocapture` prints the table every
//! factor gives, to choose the scales.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use compositor_render::glass::{
    GlassCache, GlassElement, GlassPrograms, GlassQuality, PhysicalRegion, effective_factor,
};
use compositor_render::glass_profile::{BlurScale, GlassKind, GlassProfile};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet};
use smithay::backend::renderer::{Bind, ExportMem, Frame, ImportMem, Offscreen, Renderer};
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

// ---------------------------------------------------------------------------------------------
// Images

/// An RGBA image, 8 bits per channel, opaque.
#[derive(Clone, PartialEq)]
struct Image {
    w: i32,
    h: i32,
    rgba: Vec<u8>,
}

impl Image {
    fn from_fn(w: i32, h: i32, f: impl Fn(f64, f64) -> [f64; 3]) -> Self {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = f(f64::from(x) + 0.5, f64::from(y) + 0.5);
                for c in [r, g, b] {
                    rgba.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
                rgba.push(255);
            }
        }
        Self { w, h, rgba }
    }

    fn size(&self) -> Size<i32, Physical> {
        Size::from((self.w, self.h))
    }
}

/// A small integer hash: the same "random" pattern on every run.
fn hash(x: i64, y: i64, seed: i64) -> f64 {
    let mut h = (x.wrapping_mul(374_761_393)
        ^ y.wrapping_mul(668_265_263)
        ^ seed.wrapping_mul(1_274_126_177)) as u64;
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h % 10_000) as f64 / 10_000.0
}

/// Value noise, smooth between integer lattice points.
fn noise(x: f64, y: f64, seed: i64) -> f64 {
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (x - x0, y - y0);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let (xi, yi) = (x0 as i64, y0 as i64);
    let a = hash(xi, yi, seed);
    let b = hash(xi + 1, yi, seed);
    let c = hash(xi, yi + 1, seed);
    let d = hash(xi + 1, yi + 1, seed);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sy
}

/// The backdrops, `w` × `h` pixels at `scale` (the patterns are in points, the edges and
/// strokes in whole pixels as on screen).
fn backdrops(w: i32, h: i32, scale: f64) -> Vec<(String, Image)> {
    let mut out = Vec::new();
    // A soft multicolour wallpaper (like the default ones).
    out.push((
        "wallpaper-soft".to_owned(),
        Image::from_fn(w, h, |x, y| {
            let (u, v) = (x / scale / 360.0, y / scale / 220.0);
            [
                0.55 + 0.4 * (3.0 * u + 1.0).sin() * (2.0 * v).cos(),
                0.6 + 0.35 * (2.5 * v - 1.5 * u).sin(),
                0.7 + 0.3 * (4.0 * u * v + 0.5).cos(),
            ]
        }),
    ));
    // A detailed, photo-like wallpaper: octaves of colour noise down to the pixel.
    out.push((
        "wallpaper-photo".to_owned(),
        Image::from_fn(w, h, |x, y| {
            let mut c = [0.0; 3];
            for (i, channel) in c.iter_mut().enumerate() {
                let seed = i as i64 * 31 + 7;
                *channel = 0.5 * noise(x / 40.0, y / 40.0, seed)
                    + 0.25 * noise(x / 9.0, y / 9.0, seed + 1)
                    + 0.15 * noise(x / 3.0, y / 3.0, seed + 2)
                    + 0.1 * hash(x as i64, y as i64, seed + 3);
            }
            c
        }),
    ));
    // Text: rows of glyph-like cells of 1 px strokes, dark on light and light on dark.
    let text = |ink: [f64; 3], paper: [f64; 3]| {
        move |x: f64, y: f64| {
            let (px, py) = (x as i64, y as i64);
            let cell_w = (7.0 * scale).round().max(4.0) as i64;
            let line_h = (16.0 * scale).round().max(8.0) as i64;
            let glyph_h = (10.0 * scale).round().max(5.0) as i64;
            let (col, cx) = (px / cell_w, px % cell_w);
            let (row, cy) = (py / line_h, py % line_h);
            let stroke = scale.round().max(1.0) as i64;
            let inside = cx < cell_w - stroke && cy >= line_h - glyph_h - 2 && cy < line_h - 2;
            let seed = col * 131 + row * 977;
            let on = inside
                && ((cx < stroke && hash(col, row, 1) > 0.3)
                    || (cx >= cell_w - 2 * stroke && hash(col, row, 2) > 0.5)
                    || ((cy - (line_h - glyph_h - 2)) < stroke && hash(col, row, 3) > 0.4)
                    || (cy >= line_h - 2 - stroke && hash(col, row, 4) > 0.4)
                    || ((cy - (line_h - glyph_h / 2 - 2)).abs() < stroke
                        && hash(seed, 0, 5) > 0.5));
            if on { ink } else { paper }
        }
    };
    out.push((
        "text-light".to_owned(),
        Image::from_fn(w, h, text([0.0, 0.0, 0.0], [1.0, 1.0, 1.0])),
    ));
    out.push((
        "text-dark".to_owned(),
        Image::from_fn(w, h, text([0.93, 0.93, 0.93], [0.12, 0.12, 0.13])),
    ));
    // Hard black and white edges: a checkerboard, 1 px lines and a diagonal.
    out.push((
        "edges".to_owned(),
        Image::from_fn(w, h, |x, y| {
            let square = (16.0 * scale).round().max(4.0);
            let check = ((x / square).floor() as i64 + (y / square).floor() as i64) % 2 == 0;
            let line = (x as i64) % 23 == 0 || (y as i64) % 29 == 0;
            let diagonal = x - 1.7 * y > 0.0;
            let white = if y < f64::from(h) / 2.0 {
                check ^ line
            } else {
                diagonal ^ line
            };
            if white { [1.0; 3] } else { [0.0; 3] }
        }),
    ));
    // Saturated colour bars with sharp boundaries.
    out.push((
        "color-edges".to_owned(),
        Image::from_fn(w, h, |x, y| {
            let bar = ((x + 0.6 * y) / (13.0 * scale)).floor() as i64 % 5;
            match bar {
                0 => [1.0, 0.0, 0.0],
                1 => [0.0, 0.85, 0.1],
                2 => [0.05, 0.1, 1.0],
                3 => [1.0, 0.95, 0.0],
                _ => [0.0, 0.0, 0.0],
            }
        }),
    ));
    // Real wallpapers, if given: the middle of each, `w` × `h`.
    if let Ok(paths) = std::env::var("GLASS_EQUIVALENCE_WALLPAPERS") {
        for path in paths.split(':').filter(|p| !p.is_empty()) {
            let decoder =
                png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
            let mut reader = decoder.read_info().unwrap();
            let mut buf = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf).unwrap();
            let channels = info.color_type.samples();
            let (iw, ih) = (info.width as i64, info.height as i64);
            let (ox, oy) = ((iw - i64::from(w)) / 2, (ih - i64::from(h)) / 2);
            let image = Image::from_fn(w, h, |x, y| {
                let sx = (x as i64 + ox).clamp(0, iw - 1);
                let sy = (y as i64 + oy).clamp(0, ih - 1);
                let i = ((sy * iw + sx) as usize) * channels;
                [
                    f64::from(buf[i]) / 255.0,
                    f64::from(buf[i + 1]) / 255.0,
                    f64::from(buf[i + 2]) / 255.0,
                ]
            });
            let name = std::path::Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push((format!("file-{name}"), image));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// A test element: a texture whose id, commit, place, alpha and damage the test controls

#[derive(Clone)]
struct Layer {
    id: Id,
    commit: CommitCounter,
    /// The commit before `commit` and what it damaged (relative to the element).
    previous: Option<(CommitCounter, Vec<Rectangle<i32, Physical>>)>,
    texture: GlesTexture,
    image: Image,
    geometry: Rectangle<i32, Physical>,
    alpha: f32,
}

impl Layer {
    fn new(renderer: &mut GlesRenderer, image: Image, at: Point<i32, Physical>) -> Self {
        let texture = import(renderer, &image);
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
            previous: None,
            texture,
            geometry: Rectangle::new(at, image.size()),
            image,
            alpha: 1.0,
        }
    }

    /// New content, damaging `damage` (relative to the element).
    fn update(
        &mut self,
        renderer: &mut GlesRenderer,
        image: Image,
        damage: Vec<Rectangle<i32, Physical>>,
    ) {
        let old = self.commit;
        self.commit.increment();
        self.previous = Some((old, damage));
        self.texture = import(renderer, &image);
        self.geometry.size = image.size();
        self.image = image;
    }
}

fn import(renderer: &mut GlesRenderer, image: &Image) -> GlesTexture {
    renderer
        .import_memory(
            &image.rgba,
            Fourcc::Abgr8888,
            Size::<i32, Buffer>::from((image.w, image.h)),
            false,
        )
        .unwrap()
}

impl Element for Layer {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(Size::from((
            f64::from(self.image.w),
            f64::from(self.image.h),
        )))
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn damage_since(
        &self,
        _scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            return DamageSet::default();
        }
        match &self.previous {
            Some((old, damage)) if commit == Some(*old) => DamageSet::from_slice(damage),
            _ => DamageSet::from_slice(&[Rectangle::from_size(self.geometry.size)]),
        }
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for Layer {
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

// ---------------------------------------------------------------------------------------------
// Drawing and reading back

struct Gpu {
    renderer: GlesRenderer,
    glass: GlassPrograms,
    /// The backdrops made so far, by scale (bits).
    backdrops: Vec<(u64, Vec<(String, Image)>)>,
}

fn gpu() -> Gpu {
    let mut renderer = crate::egl::headless_renderer(false).expect("headless GLES renderer");
    let shaders = compositor_render::shaders(&mut renderer);
    Gpu {
        glass: shaders.glass.expect("glass shaders"),
        renderer,
        backdrops: Vec::new(),
    }
}

fn draw_one<E: RenderElement<GlesRenderer>>(frame: &mut GlesFrame<'_, '_>, element: &E) {
    let geometry = element.geometry(Scale::from(1.0));
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

/// The layers (from the front) and the glass over them, drawn into `size` and read back.
fn compose(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    clear: [f32; 4],
    layers: &[Layer],
    glass: &[GlassElement],
) -> Image {
    let buffer = Size::<i32, Buffer>::from((size.w, size.h));
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, buffer).unwrap();
    let mut target = renderer.bind(&mut texture).unwrap();
    {
        let mut frame = renderer
            .render(&mut target, size, Transform::Normal)
            .unwrap();
        frame
            .clear(clear.into(), &[Rectangle::from_size(size)])
            .unwrap();
        for layer in layers.iter().rev() {
            draw_one(&mut frame, layer);
        }
        // The cache lists the groups from the back; one group here.
        for element in glass {
            draw_one(&mut frame, element);
        }
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

/// The glass of `regions` over `layers` through `cache`, composed.
fn frame(
    gpu: &mut Gpu,
    cache: &GlassCache,
    regions: &[PhysicalRegion],
    layers: &[Layer],
    size: Size<i32, Physical>,
    scale: f64,
    clear: [f32; 4],
) -> (Image, Vec<GlassElement>) {
    let glass = cache
        .elements(
            &mut gpu.renderer,
            &gpu.glass,
            regions,
            layers,
            layers,
            GlassQuality::Full,
            Rectangle::from_size(size),
            scale,
            clear,
        )
        .unwrap();
    (
        compose(&mut gpu.renderer, size, clear, layers, &glass),
        glass,
    )
}

/// Mean and max absolute difference per channel (levels), the mean over `area`.
fn difference(a: &Image, b: &Image, area: Rectangle<i32, Physical>) -> (f64, u8) {
    assert_eq!((a.w, a.h), (b.w, b.h));
    let mut max = 0u8;
    for (x, y) in a.rgba.iter().zip(&b.rgba) {
        max = max.max(x.abs_diff(*y));
    }
    let (mut sum, mut count) = (0u64, 0u64);
    for y in area.loc.y.max(0)..(area.loc.y + area.size.h).min(a.h) {
        for x in area.loc.x.max(0)..(area.loc.x + area.size.w).min(a.w) {
            let i = ((y * a.w + x) * 4) as usize;
            for c in 0..3 {
                sum += u64::from(a.rgba[i + c].abs_diff(b.rgba[i + c]));
                count += 1;
            }
        }
    }
    (sum as f64 / count.max(1) as f64, max)
}

fn save(image: &Image, path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        image.w as u32,
        image.h as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&image.rgba)
        .unwrap();
}

// ---------------------------------------------------------------------------------------------
// Equivalence of the reduced blur

/// The canvas and the glass on it, in points.
const CANVAS_PT: (f64, f64) = (360.0, 220.0);
// Near the left and bottom edges, like the Dock: the capture is cut by the output there.
const GLASS_PT: (f64, f64, f64, f64) = (6.0, 124.0, 220.0, 90.0);
const RADIUS_PT: f64 = 22.0;
/// The scales tested: 1× (where σ in pixels is the smallest), a fractional one, and 2×.
const SCALES: [f64; 3] = [1.0, 1.5, 2.0];
/// The thresholds of #204, in levels.
const MAX_MEAN: f64 = 1.0;
const MAX_ANY: u8 = 3;

/// A glass kind as the compositor draws it: its Amalfi profile and material in an appearance.
struct Case {
    name: String,
    profile: GlassProfile,
    material: compositor_render::glass::MaterialParams,
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for kind in GlassKind::ALL {
        for dark in [false, true] {
            let appearance = if dark { "dark" } else { "light" };
            cases.push(Case {
                name: format!("{}-{appearance}", kind.name()),
                profile: kind.profile(dark),
                material: kind.material(dark),
            });
        }
    }
    cases
}

fn px(value: f64, scale: f64) -> i32 {
    (value * scale).round() as i32
}

fn region(case: &Case, scale: f64, blur_scale: BlurScale) -> PhysicalRegion {
    let (x, y, w, h) = GLASS_PT;
    PhysicalRegion {
        rect: Rectangle::new(
            (px(x, scale), px(y, scale)).into(),
            (px(w, scale), px(h, scale)).into(),
        ),
        radius: RADIUS_PT * scale,
        smoothing: 0.0,
        material: case.material,
        opacity: 1.0,
        scale,
        tip: None,
        group: 0,
        profile: GlassProfile {
            blur_scale,
            ..case.profile
        },
        rotation: 0.0,
    }
}

/// The backdrops at `scale`, made once.
fn backdrops_at(gpu: &mut Gpu, scale: f64) -> Vec<(String, Image)> {
    if let Some((_, images)) = gpu.backdrops.iter().find(|(s, _)| *s == scale.to_bits()) {
        return images.clone();
    }
    let images = backdrops(px(CANVAS_PT.0, scale), px(CANVAS_PT.1, scale), scale);
    gpu.backdrops.push((scale.to_bits(), images.clone()));
    images
}

/// One case at one scale: for each of `factors`, the worst mean and max difference against
/// full resolution over every backdrop, and the backdrop of the worst max. With `out`, the
/// images of the worst one are saved there.
fn compare(
    gpu: &mut Gpu,
    case: &Case,
    scale: f64,
    factors: &[u32],
    out: Option<&std::path::Path>,
) -> Vec<(f64, u8, String)> {
    let size = Size::from((px(CANVAS_PT.0, scale), px(CANVAS_PT.1, scale)));
    let full = region(case, scale, BlurScale::FULL);
    let clear = [0.0, 0.0, 0.0, 1.0];
    let mut worst = vec![(0.0_f64, 0u8, String::new()); factors.len()];
    for (name, image) in backdrops_at(gpu, scale) {
        let layers = [Layer::new(&mut gpu.renderer, image, Point::from((0, 0)))];
        let (a, _) = frame(
            gpu,
            &GlassCache::default(),
            &[full],
            &layers,
            size,
            scale,
            clear,
        );
        for (factor, worst) in factors.iter().zip(&mut worst) {
            let reduced = region(
                case,
                scale,
                BlurScale {
                    below_2x: *factor,
                    from_2x: *factor,
                },
            );
            let (b, _) = frame(
                gpu,
                &GlassCache::default(),
                &[reduced],
                &layers,
                size,
                scale,
                clear,
            );
            let (mean, max) = difference(&a, &b, full.rect);
            if max > worst.1 {
                worst.2 = name.clone();
                if let Some(dir) = out {
                    let stem = format!("{}-{scale}x-f{factor}", case.name);
                    save(&a, &dir.join(format!("{stem}-full.png")));
                    save(&b, &dir.join(format!("{stem}-reduced.png")));
                }
            }
            worst.0 = worst.0.max(mean);
            worst.1 = worst.1.max(max);
        }
    }
    worst
}

/// Every kind's recorded `BlurScale` keeps the glass within the thresholds of the
/// full-resolution one, at every scale tested. Where the factor is 1 the path is the
/// full-resolution one itself.
#[test]
fn every_kind_blurs_at_its_scale_like_at_full_resolution() {
    let mut gpu = gpu();
    let out = std::env::var("GLASS_EQUIVALENCE_OUT")
        .ok()
        .map(std::path::PathBuf::from);
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut failures = Vec::new();
    for case in cases() {
        for scale in SCALES {
            let sigma_px = region(&case, scale, BlurScale::FULL).sigma() * scale;
            let factor = effective_factor(sigma_px, case.profile.blur_scale.factor(scale));
            if factor == 1 {
                eprintln!("{:<22} {scale}×: full resolution", case.name);
                continue;
            }
            let worst = compare(&mut gpu, &case, scale, &[factor], out.as_deref());
            let (mean, max, at) = &worst[0];
            eprintln!(
                "{:<22} {scale}× f{factor}: mean {mean:.3}, max {max} ({at})",
                case.name
            );
            if *mean > MAX_MEAN || *max > MAX_ANY {
                failures.push(format!(
                    "{} {scale}× f{factor}: mean {mean:.3}, max {max} ({at})",
                    case.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "over the thresholds: {failures:#?}");
}

/// The table to choose the scales from: every case at every scale and factor (not run by
/// default).
#[test]
#[ignore = "prints the table of factors; run by hand when the profiles change"]
fn table_of_factors() {
    let mut gpu = gpu();
    let out = std::env::var("GLASS_EQUIVALENCE_OUT")
        .ok()
        .map(std::path::PathBuf::from);
    for case in cases() {
        for scale in SCALES {
            let sigma_px = region(&case, scale, BlurScale::FULL).sigma() * scale;
            let mut row = format!("{:<22} {scale}× σ {sigma_px:>4.1} px", case.name);
            let factors: Vec<u32> = [2, 4, 8]
                .into_iter()
                .filter(|f| effective_factor(sigma_px, *f) == *f)
                .collect();
            let worst = compare(&mut gpu, &case, scale, &factors, out.as_deref());
            for (factor, (mean, max, at)) in factors.iter().zip(worst) {
                row.push_str(&format!(" | f{factor}: {mean:.3} / {max} ({at})"));
            }
            eprintln!("{row}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Refraction at the edge

/// A 400 × 160 canvas of stripes 20 px wide (black and white) whose edges are at
/// `first + 20 k` along x (vertical stripes) or y (horizontal ones).
fn stripes(first: i32, horizontal: bool) -> Image {
    Image::from_fn(400, 160, |x, y| {
        let along = if horizontal { y } else { x } as i32;
        let level = if (along - first).rem_euclid(40) < 20 {
            0.0
        } else {
            1.0
        };
        [level; 3]
    })
}

/// The glass of `kind` (light) over the stripes: a 280 × 120 px region at (60, 20), blurred
/// with σ 1 px and no sharp share so that the stripe edges stay measurable, with the profile's
/// refraction kept or taken away, at `quality`.
fn refracted(
    gpu: &mut Gpu,
    kind: GlassKind,
    backdrop: &Image,
    refraction: bool,
    quality: GlassQuality,
) -> Image {
    let size = backdrop.size();
    let mut profile = kind.profile(false);
    profile.sigma = Some(1.0);
    profile.blur_scale = BlurScale::FULL;
    profile.shadow = compositor_render::glass_profile::GlassShadow::NONE;
    if !refraction {
        profile.refraction = 0.0;
        profile.chromatic = 0.0;
    }
    let mut material = kind.material(false);
    material.sharp = 0.0;
    let region = PhysicalRegion {
        rect: Rectangle::new((60, 20).into(), (280, 120).into()),
        radius: 24.0,
        smoothing: 0.0,
        material,
        opacity: 1.0,
        scale: 1.0,
        tip: None,
        group: 0,
        profile,
        rotation: 0.0,
    };
    let layers = [Layer::new(
        &mut gpu.renderer,
        backdrop.clone(),
        Point::from((0, 0)),
    )];
    let clear = [0.0, 0.0, 0.0, 1.0];
    let glass = GlassCache::default()
        .elements(
            &mut gpu.renderer,
            &gpu.glass,
            &[region],
            &layers,
            &layers,
            quality,
            Rectangle::from_size(size),
            1.0,
            clear,
        )
        .unwrap();
    compose(&mut gpu.renderer, size, clear, &layers, &glass)
}

/// Where channel `c` crosses the middle between its levels along a line near `expected`
/// (sub-pixel, by linear interpolation), the line being row `y` (or column `x` when
/// `horizontal`).
fn crossing(image: &Image, channel: usize, line: i32, expected: i32, horizontal: bool) -> f64 {
    let level = |along: i32| {
        let (x, y) = if horizontal {
            (line, along)
        } else {
            (along, line)
        };
        f64::from(image.rgba[((y * image.w + x) * 4) as usize + channel])
    };
    let span = expected - 3..expected + 12;
    let lo = span.clone().map(level).fold(f64::MAX, f64::min);
    let hi = span.clone().map(level).fold(0.0, f64::max);
    let mid = (lo + hi) / 2.0;
    for along in span {
        let (a, b) = (level(along), level(along + 1));
        if (a - mid) * (b - mid) < 0.0 || (a < mid && b >= mid) || (a > mid && b <= mid) {
            return f64::from(along) + 0.5 + (mid - a) / (b - a);
        }
    }
    panic!("no crossing near {expected} on line {line}");
}

/// The edge band bends the backdrop and nothing else: a stripe edge just inside the glass's
/// left and top edge is moved towards the inside (the glass shows what lies nearer the rim),
/// one in the middle of the glass stays where it is, and the red and blue samples split by a
/// fraction of a pixel in the band and not in the middle.
#[test]
fn the_edge_band_bends_the_stripes_and_the_centre_does_not() {
    let mut gpu = gpu();
    for kind in [GlassKind::Dock, GlassKind::Menu] {
        // The glass is 280 × 120: the band is `depth × 60` px (19.2 for Regular).
        // Stripe edges 6 px inside the left edge (x 66) and the top edge (y 26), and in the
        // middle (x 206, y 86).
        for horizontal in [false, true] {
            let (first, near, centre, line) = if horizontal {
                (26, 26, 86, 200)
            } else {
                (66, 66, 206, 80)
            };
            let backdrop = stripes(first, horizontal);
            let bent = refracted(&mut gpu, kind, &backdrop, true, GlassQuality::Full);
            let flat = refracted(&mut gpu, kind, &backdrop, false, GlassQuality::Full);
            let shift = |at: i32, channel: usize| {
                crossing(&bent, channel, line, at, horizontal)
                    - crossing(&flat, channel, line, at, horizontal)
            };
            let in_band = shift(near, 1);
            let in_centre = shift(centre, 1);
            eprintln!("{kind:?} horizontal {horizontal}: band {in_band:.2}, centre {in_centre:.2}");
            // Towards the inside, by about the distortion × the band at that depth (2.3 px):
            // the Dock and the menus are both Glass/Regular.
            let (low, high) = (1.5, 4.0);
            assert!(
                (low..high).contains(&in_band),
                "{kind:?} horizontal {horizontal}: band shift {in_band:.2}"
            );
            assert!(
                in_centre.abs() < 0.15,
                "{kind:?} horizontal {horizontal}: centre shift {in_centre:.2}"
            );
            // The chromatic split: small in the band, none in the middle or without refraction.
            let split = |image: &Image, at: i32| {
                (crossing(image, 0, line, at, horizontal)
                    - crossing(image, 2, line, at, horizontal))
                .abs()
            };
            let band_split = split(&bent, near);
            eprintln!("{kind:?} horizontal {horizontal}: split {band_split:.2}");
            assert!(
                (0.1..1.5).contains(&band_split),
                "{kind:?} horizontal {horizontal}: split {band_split:.2}"
            );
            assert!(split(&bent, centre) < 0.1, "{kind:?}: split in the centre");
            assert!(
                split(&flat, near) < 0.1,
                "{kind:?}: split without refraction"
            );
        }
    }
}

/// Reduced transparency is unchanged: no backdrop, so nothing to bend and no brightness, the
/// tint over `Surface/Window` all over the glass, from the band to the middle.
#[test]
fn the_opaque_fallback_does_not_refract() {
    let mut gpu = gpu();
    let backdrop = stripes(66, false);
    for kind in [GlassKind::Dock, GlassKind::Menu] {
        let shot = refracted(&mut gpu, kind, &backdrop, true, GlassQuality::Opaque);
        let want =
            compositor_render::glass::fallback_color(&kind.profile(false)).map(|c| c * 255.0);
        for x in [70, 76, 84, 120, 206, 300] {
            let i = ((80 * shot.w + x) * 4) as usize;
            for c in 0..3 {
                assert!(
                    (f32::from(shot.rgba[i + c]) - want[c]).abs() <= 1.0,
                    "{kind:?} x {x}: {:?} against {want:?}",
                    &shot.rgba[i..i + 3]
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The backdrop cache

/// A scene: a wallpaper, a window partly under the glass, the glass.
struct Scene {
    layers: Vec<Layer>,
    regions: Vec<PhysicalRegion>,
    size: Size<i32, Physical>,
    scale: f64,
    clear: [f32; 4],
}

fn window_image(w: i32, h: i32, scale: f64, seed: f64) -> Image {
    let text = backdrops(w, h, scale).swap_remove(2).1;
    Image::from_fn(w, h, |x, y| {
        let i = (((y as i32) * w + x as i32) * 4) as usize;
        let ink = f64::from(text.rgba[i]) / 255.0;
        [ink * (0.8 + 0.2 * seed), ink, ink * (1.0 - 0.3 * seed)]
    })
}

fn scene(gpu: &mut Gpu, scale: f64, dark: bool) -> Scene {
    let size = Size::from((px(CANVAS_PT.0, scale), px(CANVAS_PT.1, scale)));
    let wallpaper = backdrops(size.w, size.h, scale)
        .swap_remove(if dark { 3 } else { 1 })
        .1;
    let wallpaper = Layer::new(&mut gpu.renderer, wallpaper, Point::from((0, 0)));
    let window = window_image(px(150.0, scale), px(100.0, scale), scale, 0.0);
    let window = Layer::new(
        &mut gpu.renderer,
        window,
        Point::from((px(40.0, scale), px(30.0, scale))),
    );
    let case = Case {
        name: "dock".to_owned(),
        profile: GlassKind::Dock.profile(dark),
        material: GlassKind::Dock.material(dark),
    };
    let region = region(&case, scale, case.profile.blur_scale);
    Scene {
        // From the front: the window over the wallpaper.
        layers: vec![window, wallpaper],
        regions: vec![region],
        size,
        scale,
        clear: if dark {
            [0.1, 0.1, 0.1, 1.0]
        } else {
            [0.9, 0.9, 0.9, 1.0]
        },
    }
}

/// The same scene through the long-lived cache and through a new one: the two frames are the
/// same. Returns the glass elements of the cached frame.
fn check(gpu: &mut Gpu, cache: &GlassCache, scene: &Scene, what: &str) -> Vec<GlassElement> {
    let (cached, elements) = frame(
        gpu,
        cache,
        &scene.regions,
        &scene.layers,
        scene.size,
        scene.scale,
        scene.clear,
    );
    let (fresh, _) = frame(
        gpu,
        &GlassCache::default(),
        &scene.regions,
        &scene.layers,
        scene.size,
        scene.scale,
        scene.clear,
    );
    if cached != fresh {
        let (mean, max) = difference(&cached, &fresh, Rectangle::from_size(scene.size));
        panic!("{what}: the cached glass differs from a fresh one (mean {mean:.3}, max {max})");
    }
    elements
}

fn commits(elements: &[GlassElement]) -> Vec<CommitCounter> {
    elements.iter().map(Element::current_commit).collect()
}

#[test]
fn the_cached_backdrop_equals_a_fresh_one_after_every_change() {
    let mut gpu = gpu();
    let cache = GlassCache::default();
    let mut s = scene(&mut gpu, 1.0, false);
    let first = check(&mut gpu, &cache, &s, "first frame");

    // Nothing changed: the texture is reused (same commit) and still right.
    let again = check(&mut gpu, &cache, &s, "unchanged");
    assert_eq!(
        commits(&first),
        commits(&again),
        "an unchanged backdrop was recomputed"
    );

    // A window moves under the glass.
    s.layers[0].geometry.loc += Point::from((7, 5));
    check(&mut gpu, &cache, &s, "window moved");

    // A window resizes under the glass.
    let (w, h) = (px(170.0, 1.0), px(84.0, 1.0));
    let image = window_image(w, h, 1.0, 0.0);
    let full = vec![Rectangle::from_size(image.size())];
    s.layers[0].update(&mut gpu.renderer, image, full);
    check(&mut gpu, &cache, &s, "window resized");

    // New content under the glass (damage where the glass reads).
    let geometry = s.layers[0].geometry;
    let image = window_image(geometry.size.w, geometry.size.h, 1.0, 1.0);
    let full = vec![Rectangle::from_size(image.size())];
    s.layers[0].update(&mut gpu.renderer, image, full);
    let before = check(&mut gpu, &cache, &s, "content under the glass");

    // New content far from the glass (the wallpaper's top left corner, outside the glass's
    // reach): the texture is reused, and still right.
    let mut image = s.layers[1].image.clone();
    for y in 0..6 {
        for x in 0..6 {
            let i = ((y * image.w + x) * 4) as usize;
            image.rgba[i..i + 3].copy_from_slice(&[255, 0, 0]);
        }
    }
    s.layers[1].update(
        &mut gpu.renderer,
        image,
        vec![Rectangle::new((0, 0).into(), (6, 6).into())],
    );
    let after = check(&mut gpu, &cache, &s, "content outside the glass");
    assert_eq!(
        commits(&before),
        commits(&after),
        "damage outside the glass recomputed it"
    );

    // The window fades (alpha only).
    s.layers[0].alpha = 0.6;
    check(&mut gpu, &cache, &s, "window alpha");

    // The order of the layers changes (the wallpaper over the window).
    s.layers.swap(0, 1);
    check(&mut gpu, &cache, &s, "order changed");
    s.layers.swap(0, 1);
    check(&mut gpu, &cache, &s, "order restored");

    // The wallpaper changes.
    let wallpaper = backdrops(s.size.w, s.size.h, 1.0).swap_remove(5).1;
    let full = vec![Rectangle::from_size(wallpaper.size())];
    s.layers[1].update(&mut gpu.renderer, wallpaper, full);
    check(&mut gpu, &cache, &s, "wallpaper changed");

    // A space switch: other elements behind (other ids).
    let other = scene(&mut gpu, 1.0, false);
    s.layers = other.layers;
    check(&mut gpu, &cache, &s, "space switch");

    // The glass's own animation: it moves, grows, fades.
    s.regions[0].rect.loc += Point::from((9, -4));
    check(&mut gpu, &cache, &s, "glass moved");
    s.regions[0].rect.size += Size::from((14, 6));
    check(&mut gpu, &cache, &s, "glass resized");
    s.regions[0].opacity = 0.5;
    let faded = check(&mut gpu, &cache, &s, "glass faded");
    s.regions[0].opacity = 1.0;
    let _ = faded;
    check(&mut gpu, &cache, &s, "glass opaque again");

    // Light to dark: the material, the wallpaper and the clear color change.
    let dark = scene(&mut gpu, 1.0, true);
    s.regions[0].material = dark.regions[0].material;
    s.regions[0].profile = dark.regions[0].profile;
    check(&mut gpu, &cache, &s, "dark material");
    s.layers = dark.layers;
    check(&mut gpu, &cache, &s, "dark wallpaper");
    // The clear color alone (seen where no layer covers: the window is all there is).
    s.layers.truncate(1);
    check(&mut gpu, &cache, &s, "no wallpaper");
    s.clear = [0.3, 0.05, 0.05, 1.0];
    check(&mut gpu, &cache, &s, "clear color");

    // A scale change (the same scene at 2×, then back at 1×).
    let double = scene(&mut gpu, 2.0, false);
    check(&mut gpu, &cache, &double, "scale 2");
    let single = scene(&mut gpu, 1.0, false);
    check(&mut gpu, &cache, &single, "scale 1 again");
    // The scale alone, with the same pixels: the σ in pixels changes.
    let mut same_pixels = scene(&mut gpu, 1.0, false);
    same_pixels.scale = 1.5;
    for region in &mut same_pixels.regions {
        region.scale = 1.5;
    }
    check(&mut gpu, &cache, &same_pixels, "scale only");
}

/// One case, scale and factor (`GLASS_CASE`, `GLASS_SCALE`, `GLASS_FACTOR`), its worst images
/// saved in `GLASS_EQUIVALENCE_OUT` (not run by default).
#[test]
#[ignore = "inspects one case by hand"]
fn inspect_one_case() {
    let mut gpu = gpu();
    let name = std::env::var("GLASS_CASE").unwrap();
    let scale: f64 = std::env::var("GLASS_SCALE").unwrap().parse().unwrap();
    let factor: u32 = std::env::var("GLASS_FACTOR").unwrap().parse().unwrap();
    let out = std::path::PathBuf::from(std::env::var("GLASS_EQUIVALENCE_OUT").unwrap());
    std::fs::create_dir_all(&out).unwrap();
    let case = cases().into_iter().find(|c| c.name == name).unwrap();
    let worst = compare(&mut gpu, &case, scale, &[factor], Some(&out));
    eprintln!("{name} {scale}× f{factor}: {:?}", worst[0]);
}

/// The GPU time of the glass, full resolution against each kind's `BlurScale`: a 600 × 400 pt
/// glass over a 960 × 600 pt photo-like backdrop, made from scratch every time, on the GPU
/// when there is one (`cargo test -p compositor --release glass_equivalence::glass_cost --
/// --ignored --nocapture`).
#[test]
#[ignore = "a benchmark, run by hand"]
fn glass_cost() {
    let mut renderer = crate::egl::headless_renderer(true).expect("headless GLES renderer");
    let shaders = compositor_render::shaders(&mut renderer);
    let mut gpu = Gpu {
        glass: shaders.glass.expect("glass shaders"),
        renderer,
        backdrops: Vec::new(),
    };
    for scale in [1.0, 2.0] {
        let size = Size::from((px(960.0, scale), px(600.0, scale)));
        let image = backdrops(size.w, size.h, scale).swap_remove(1).1;
        let layers = [Layer::new(&mut gpu.renderer, image, Point::from((0, 0)))];
        let clear = [0.0, 0.0, 0.0, 1.0];
        let time = |gpu: &mut Gpu, regions: &[PhysicalRegion]| -> f64 {
            // One warm-up, then the median of 15.
            let _ = frame(
                gpu,
                &GlassCache::default(),
                regions,
                &layers,
                size,
                scale,
                clear,
            );
            let mut times: Vec<f64> = (0..15)
                .map(|_| {
                    let start = std::time::Instant::now();
                    let _ = frame(
                        gpu,
                        &GlassCache::default(),
                        regions,
                        &layers,
                        size,
                        scale,
                        clear,
                    );
                    start.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            times.sort_by(f64::total_cmp);
            times[times.len() / 2]
        };
        let base = time(&mut gpu, &[]);
        for case in cases() {
            let make = |blur_scale: BlurScale| {
                let mut region = region(&case, scale, blur_scale);
                region.rect = Rectangle::new(
                    (px(180.0, scale), px(100.0, scale)).into(),
                    (px(600.0, scale), px(400.0, scale)).into(),
                );
                region
            };
            let full = time(&mut gpu, &[make(BlurScale::FULL)]) - base;
            let chosen = time(&mut gpu, &[make(case.profile.blur_scale)]) - base;
            // The same without the refraction (one sample instead of three, no normal).
            let mut flat = make(case.profile.blur_scale);
            flat.profile.refraction = 0.0;
            flat.profile.chromatic = 0.0;
            let flat = time(&mut gpu, &[flat]) - base;
            eprintln!(
                "{:<22} {scale}×: full {full:6.2} ms, at its scale (f{}) {chosen:6.2} ms, without refraction {flat:6.2} ms",
                case.name,
                case.profile.blur_scale.factor(scale)
            );
        }
    }
}
