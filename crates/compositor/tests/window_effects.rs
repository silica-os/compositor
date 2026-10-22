//! Window effects of the Amalfi design system on the headless compositor at scale 1: the smooth corner
//! (radius 18, smoothing 0.6) and `shadows.3`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

/// These tests read pixels at fixed positions: the first window is at (32, 32).
const AT_32: u32 = 32;

fn start_at_32(name: &str, size: (u32, u32), extra: &[&str]) -> Headless {
    let mut args = vec!["--window-origin", "32"];
    args.extend_from_slice(extra);
    Headless::start_with(name, size, &args)
}

use common::client::TestWindow;
use common::{Headless, pixel_in, screenshot};
use serde_json::json;
use silica_control::commands::{WINDOW_LIST, WINDOW_MOVE};

/// Missing pixels (whole pixels not covered) for rows 0…29 of the top-left corner of a large
/// window with the Amalfi corner: radius 18, smoothing 0.6, the Figma/Sketch smooth-corner path
/// (28.8 pt long cubic, arc, cubic), integrated per row.
const REFERENCE_MISSING: [f64; 30] = [
    15.38, 10.81, 8.79, 7.28, 6.05, 5.01, 4.12, 3.35, 2.69, 2.12, 1.63, 1.25, 0.95, 0.71, 0.53,
    0.39, 0.29, 0.20, 0.14, 0.10, 0.07, 0.04, 0.03, 0.01, 0.01, 0.0, 0.0, 0.0, 0.0, 0.0,
];

#[test]
fn window_corners_follow_the_reference() {
    // A green background: the window is red, so its green channel (0, 4 under the light rim)
    // tells covered from uncovered pixels whatever the rim does to the red one.
    let compositor = start_at_32("corners", (640, 400), &["--background", "00ff00"]);
    let _window = TestWindow::open_sized(&compositor.wayland, "corners", (200, 120));
    let shot = screenshot(&compositor);
    // With SILICA_COMPOSITOR_SHOTS=<folder> the screenshot is kept, to look at it.
    if let Some(dir) = std::env::var_os("SILICA_COMPOSITOR_SHOTS") {
        std::fs::copy(
            compositor.dir.join("shot.png"),
            std::path::Path::new(&dir).join("corners.png"),
        )
        .unwrap();
    }
    let at = AT_32;
    // The red channel goes from the background (read in the output's corner) to 255 in the window.
    let background = f64::from(pixel_in(&shot, 640, 0, 0)[1]);
    let coverage = |x: u32, y: u32| 1.0 - f64::from(pixel_in(&shot, 640, x, y)[1]) / background;

    let mut errors = Vec::new();
    for (row, reference) in REFERENCE_MISSING.iter().enumerate() {
        let y = at + row as u32;
        let missing: f64 = (0..40).map(|dx| 1.0 - coverage(at + dx, y)).sum();
        errors.push(missing - reference);
        eprintln!("row {row}: {missing:.2} (reference {reference:.2})");
    }
    let worst = errors.iter().fold(0.0_f64, |m, e| m.max(e.abs()));
    let rms = (errors.iter().map(|e| e * e).sum::<f64>() / errors.len() as f64).sqrt();
    eprintln!("maximum error {worst:.2} px, rms {rms:.2} px");
    assert!(
        worst <= 1.0,
        "corner far from the reference: {worst:.2} px on a row"
    );
    assert!(rms <= 0.5, "corner far from the reference: rms {rms:.2} px");

    // The four corners are the same: the apex is background (under the light shadow, at most
    // 18 %), the center is window.
    let (w, h) = (200, 120);
    for (x, y) in [
        (at, at),
        (at + w - 1, at),
        (at, at + h - 1),
        (at + w - 1, at + h - 1),
    ] {
        assert!(coverage(x, y) < 0.25, "corner not cut at ({x}, {y})");
    }
    assert_eq!(
        pixel_in(&shot, 640, at + w / 2, at + h / 2),
        [255, 0, 0, 255]
    );
}

/// Background of the reference recordings (`setup.md`).
const REFERENCE_GRAY: &str = "7a7b80";

/// Distances from the window edge (pt) where the shadow profile is compared.
const PROFILE_D: [u32; 15] = [2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 24, 28, 32, 40, 48];
/// `shadows.3` light: black at `#2E` (46), blur 32 = sigma 16, y 12: beside a window, at half
/// height, alpha(d) = 46 * ½ erfc(d / (sigma sqrt 2)) (a pixel centre is d + 0.5 from the edge).
fn reference_alpha(d: u32) -> f64 {
    46.0 * 0.5 * erfc((f64::from(d) + 0.5) / (16.0 * std::f64::consts::SQRT_2))
}

/// erfc, Abramowitz and Stegun 7.1.26 (error < 1.5e-7).
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let e = poly * (-z * z).exp();
    if x >= 0.0 { e } else { 2.0 - e }
}

/// Alpha (0–255) of a black shadow over the reference gray, from the green channel.
fn shadow_alpha(shot: &[u8], x: u32, y: u32) -> f64 {
    let gray = f64::from(0x7b_u8);
    255.0 * (1.0 - f64::from(pixel_in(shot, 640, x, y)[1]) / gray)
}

/// Compares the profile to the right of a window (right edge at `right`, row `y`).
fn compare_profile(shot: &[u8], right: u32, y: u32, name: &str) {
    let mut worst = 0.0_f64;
    for d in PROFILE_D {
        let expected = reference_alpha(d);
        let measured = shadow_alpha(shot, right + d, y);
        eprintln!("{name} d={d}: {measured:.1} (reference {expected})");
        worst = worst.max((measured - expected).abs());
    }
    eprintln!("{name}: maximum error {worst:.1} levels");
    // The 8-bit green on 123 resolves the alpha to ~2 levels.
    assert!(
        worst <= 4.0,
        "shadow {name} far from the reference: {worst:.1} levels"
    );
}

#[test]
fn window_shadows_follow_the_reference() {
    let compositor = start_at_32("shadow", (640, 400), &["--background", REFERENCE_GRAY]);
    let _first = TestWindow::open_sized(&compositor.wayland, "first", (200, 120));
    let _second = TestWindow::open_sized(&compositor.wayland, "second", (200, 120));
    // The second (active) goes to the right; the first stays at (32, 32), inactive.
    let mut client = compositor.client();
    client
        .call(WINDOW_MOVE, json!({"x": 330, "y": 120}))
        .unwrap();
    let list = client.call(WINDOW_LIST, json!({})).unwrap();
    assert_eq!(list.as_array().map(Vec::len), Some(2), "{list}");
    let shot = screenshot(&compositor);
    if let Some(dir) = std::env::var_os("SILICA_COMPOSITOR_SHOTS") {
        std::fs::copy(
            compositor.dir.join("shot.png"),
            std::path::Path::new(&dir).join("shadow.png"),
        )
        .unwrap();
    }

    // Active: right edge at x 530, half height y 180. Inactive: right edge at x 232, half
    // height y 92 (the second is further right). The same shadow.
    compare_profile(&shot, 530, 180, "active");
    compare_profile(&shot, 232, 92, "inactive");

    // Vertical shift: more shadow below than above (active, at half width).
    let above = shadow_alpha(&shot, 430, 120 - 3);
    let below = shadow_alpha(&shot, 430, 240 + 2);
    eprintln!("active: above {above:.1}, below {below:.1}");
    assert!(
        below > above + 8.0,
        "shadow not shifted down: above {above:.1}, below {below:.1}"
    );
}
