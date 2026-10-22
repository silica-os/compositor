//! Glass parity harness: renders our glass over the same wallpapers and at the same places as
//! the reference captures, and saves the screenshots for `xtask/tools/glass_parity.py`.
//!
//! Ignored by default (it needs the local reference). Run it through the tool, or directly:
//! `GLASS_PARITY_SPEC=spec.json cargo test -p compositor --test glass_parity -- --ignored`.
//! The spec is a JSON list of cases:
//! `{"name", "dark", "wallpaper": png path, "out": png path, "regions": [{"x", "y", "w", "h",
//! "r", "material": "clear|regular|thick|notification", "tip": "none|down|up|left|right"}]}` in screen
//! points of a 2560 × 1440 screen at 1×.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::Headless;
use common::client::TestWindow;
use serde::Deserialize;
use silica_control::commands::WINDOW_MOVE;
use silica_protocols::glass::client::silica_glass_v1::{Material, Side};

const SCREEN: (u32, u32) = (2560, 1440);
/// The windows overflow the screen by this much on every side: their rounded corners and
/// shadows stay off screen.
const BLEED: u32 = 40;

#[derive(Deserialize)]
struct Case {
    name: String,
    #[serde(default)]
    dark: bool,
    wallpaper: String,
    out: String,
    regions: Vec<Region>,
}

#[derive(Deserialize)]
struct Region {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    #[serde(default)]
    r: f64,
    material: String,
    #[serde(default)]
    tip: Option<String>,
}

fn material(name: &str) -> Material {
    match name {
        "clear" => Material::Clear,
        "regular" => Material::Regular,
        "thick" => Material::Thick,
        "notification" => Material::Notification,
        other => panic!("unknown material {other}"),
    }
}

/// The wallpaper as a BGRA buffer of the window: screen pixel (x, y) at window pixel
/// (x + BLEED, y + BLEED), edges repeated outside the image.
fn wallpaper_pixels(path: &str, size: (u32, u32)) -> Vec<u8> {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let channels = info.color_type.samples();
    let (w, h) = (info.width as i64, info.height as i64);
    let mut out = Vec::with_capacity((size.0 * size.1 * 4) as usize);
    for wy in 0..i64::from(size.1) {
        for wx in 0..i64::from(size.0) {
            let x = (wx - i64::from(BLEED)).clamp(0, w - 1);
            let y = (wy - i64::from(BLEED)).clamp(0, h - 1);
            let i = ((y * w + x) as usize) * channels;
            let (r, g, b) = (buf[i], buf[i + 1], buf[i + 2]);
            out.extend_from_slice(&[b, g, r, 255]);
        }
    }
    out
}

fn render(case: &Case) {
    let mut args = vec!["--background", "7a7b80", "--glass", "full"];
    if case.dark {
        args.extend(["--appearance", "dark"]);
    }
    let compositor = Headless::start_with(&format!("parity-{}", case.name), SCREEN, &args);
    // Upward and sideways tips need silica-glass-v1 versions 5 and 6.
    common::client::GLASS_VERSION.with(|v| v.set(6));
    let size = (SCREEN.0 + 2 * BLEED, SCREEN.1 + 2 * BLEED);
    let mut client = compositor.client();
    let place = |client: &mut silica_control::Client| {
        client
            .call(
                WINDOW_MOVE,
                serde_json::json!({"x": -(BLEED as i32), "y": -(BLEED as i32)}),
            )
            .unwrap();
    };
    let _wallpaper = TestWindow::open_pixels(
        &compositor.wayland,
        "wallpaper",
        size,
        &wallpaper_pixels(&case.wallpaper, size),
    );
    place(&mut client);
    let mut glass = TestWindow::open_pixels(
        &compositor.wayland,
        "glass",
        size,
        &vec![0; (size.0 * size.1 * 4) as usize],
    );
    place(&mut client);
    let b = BLEED as i32;
    glass
        .set_glass_with(|glass| {
            for region in &case.regions {
                let m = material(&region.material);
                match region.tip.as_deref() {
                    Some("down") => {
                        glass.add_tip(region.x + b, region.y + b, region.w, region.h, m, 1.0)
                    }
                    Some("up") => {
                        glass.add_upward_tip(region.x + b, region.y + b, region.w, region.h, m, 1.0)
                    }
                    Some(side @ ("left" | "right")) => glass.add_side_tip(
                        region.x + b,
                        region.y + b,
                        region.w,
                        region.h,
                        if side == "left" {
                            Side::Left
                        } else {
                            Side::Right
                        },
                        m,
                        1.0,
                    ),
                    _ => glass.add_region(
                        region.x + b,
                        region.y + b,
                        region.w,
                        region.h,
                        region.r,
                        m,
                    ),
                }
            }
        })
        .unwrap();
    common::screenshot(&compositor);
    std::fs::copy(compositor.dir.join("shot.png"), &case.out).unwrap();
}

#[test]
#[ignore = "needs the local reference: run through xtask/tools/glass_parity.py"]
fn render_parity_cases() {
    let Some(spec) = std::env::var_os("GLASS_PARITY_SPEC") else {
        eprintln!("GLASS_PARITY_SPEC not set: nothing to render");
        return;
    };
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(spec).unwrap()).unwrap();
    for case in &cases {
        render(case);
        eprintln!("rendered {}", case.name);
    }
}
