//! `silica-glass-v1`: regions declared by the client, applied on commit, checked as hostile
//! input.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

/// These tests read pixels at fixed positions: the first window is at (32, 32).
fn start_at_32(name: &str, size: (u32, u32), extra: &[&str]) -> Headless {
    let mut args = vec!["--window-origin", "32"];
    args.extend_from_slice(extra);
    Headless::start_with(name, size, &args)
}

use common::Headless;
use common::client::{PopupSpec, TestWindow};
use serde_json::Value;
use silica_control::commands::{GLASS_LIST, WINDOW_LIST};
use silica_protocols::glass::client::silica_glass_v1::{Material, Profile, Shadow, Side};

fn glass_list(compositor: &Headless) -> Value {
    compositor.client().call(GLASS_LIST, Value::Null).unwrap()
}

#[test]
fn regions_apply_on_commit_and_are_listed() {
    let compositor = Headless::start("glass-list");
    let mut window = TestWindow::open(&compositor.wayland, "glass");
    assert_eq!(glass_list(&compositor), serde_json::json!([]));

    window
        .set_glass(&[
            (10, 10, 40, 20, 8.0, Material::Clear),
            (0, 30, 100, 30, 0.0, Material::Thick),
        ])
        .unwrap();
    let list = glass_list(&compositor);
    let regions = &list[0]["regions"];
    assert_eq!(list[0]["surface"], "window 1", "{list}");
    assert_eq!(regions[0]["width"], 40);
    assert_eq!(regions[0]["radius"], 8.0);
    assert_eq!(regions[0]["material"], "clear");
    assert_eq!(regions[1]["material"], "thick");

    // Without regions the surface leaves the list.
    window.set_glass(&[]).unwrap();
    assert_eq!(glass_list(&compositor), serde_json::json!([]));
}

#[test]
fn a_popup_declares_its_own_glass_above_its_window() {
    let compositor = Headless::start("glass-popup");
    let mut window = TestWindow::open(&compositor.wayland, "menu-owner");
    window.open_popup(PopupSpec {
        anchor_rect: (10, 10, 1, 1),
        size: (50, 40),
        adjustment: 0,
        grab: false,
        nested: false,
        transparent: false,
    });
    window
        .set_popup_glass(&[(0, 0, 50, 40, 10.0, Material::Thick)])
        .unwrap();
    let configure = *window.popup_configures().last().unwrap();
    let list = glass_list(&compositor);
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["surface"]
                .as_str()
                .is_some_and(|n| n.starts_with("popup of window"))
        })
        .unwrap_or_else(|| panic!("the popup's glass is not listed: {list}"));
    assert_eq!(entry["regions"][0]["width"], 50, "{entry}");
    assert_eq!(entry["regions"][0]["material"], "thick");
    // The popup's origin on the screen: its window's content plus its place in it.
    let windows: Vec<silica_control::commands::WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    let info = &windows[0];
    assert_eq!(entry["x"], info.x + configure.0, "{entry} {info:?}");
    assert_eq!(entry["y"], info.y + configure.1, "{entry} {info:?}");
}

#[test]
fn a_popups_glass_blurs_its_window_under_it() {
    // The glass of a menu sees the window it hangs from, not only what is behind the window.
    let compositor = start_at_32("glass-popup-pixels", SHOT, &[]);
    let back = (400, 200);
    let mut stripes = TestWindow::open_pixels(&compositor.wayland, "stripes", back, &stripes(back));
    let windows: Vec<silica_control::commands::WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    let info = windows[0].clone();
    stripes.open_popup(PopupSpec {
        anchor_rect: (8, 20, 1, 1),
        size: (100, 60),
        adjustment: 0,
        grab: false,
        nested: false,
        transparent: true,
    });
    stripes
        .set_popup_glass(&[(0, 0, 100, 60, 0.0, Material::Thick)])
        .unwrap();
    let shot = shot(&compositor, "glass-popup.png");
    // Black stripe of the window at its local x 24, inside the popup (x 8–107, y 21–80) and
    // away from the stripe edge at 48 (the blur reaches ~20 px).
    let at = |x: i32, y: i32| rgb(&shot, (info.x + x) as u32, (info.y + y) as u32);
    let under_popup = at(20, 50);
    let beside = at(20, 120);
    assert!(
        beside[0] < 8,
        "the stripe is not black beside the popup: {beside:?}"
    );
    assert!(
        under_popup[0] > 60,
        "the popup's glass does not show over its window: {under_popup:?}"
    );
}

#[test]
fn invalid_regions_disconnect_the_client_not_compositor() {
    let compositor = Headless::start("glass-bad");
    let mut bad = TestWindow::open(&compositor.wayland, "bad");
    let err = bad
        .set_glass(&[(0, 0, -5, 10, 0.0, Material::Clear)])
        .unwrap_err();
    assert!(!err.is_empty());

    let mut too_many = TestWindow::open(&compositor.wayland, "too-many");
    let regions = vec![(0, 0, 1, 1, 0.0, Material::Clear); 65];
    assert!(too_many.set_glass(&regions).is_err());

    // The compositor is alive and still serves the clients.
    let mut good = TestWindow::open(&compositor.wayland, "good");
    good.set_glass(&[(0, 0, 10, 10, 0.0, Material::Clear)])
        .unwrap();
    let windows = compositor.client().call(WINDOW_LIST, Value::Null).unwrap();
    assert!(windows.as_array().is_some_and(|list| !list.is_empty()));
    assert_eq!(glass_list(&compositor).as_array().map(Vec::len), Some(1));
}

/// Transparent pixels: where the client lets the glass show.
fn transparent(size: (u32, u32)) -> Vec<u8> {
    vec![0; (size.0 * size.1 * 4) as usize]
}

/// White and black vertical stripes 48 px wide, like the `stripes-v` background of the
/// recordings (black for x mod 96 < 48).
fn stripes(size: (u32, u32)) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((size.0 * size.1 * 4) as usize);
    for _ in 0..size.1 {
        for x in 0..size.0 {
            let level = if x % 96 < 48 { 0 } else { 255 };
            pixels.extend_from_slice(&[level, level, level, 255]);
        }
    }
    pixels
}

const SHOT: (u32, u32) = (640, 400);

fn shot(compositor: &Headless, name: &str) -> Vec<u8> {
    let shot = common::screenshot(compositor);
    if let Some(dir) = std::env::var_os("SILICA_COMPOSITOR_SHOTS") {
        std::fs::copy(
            compositor.dir.join("shot.png"),
            std::path::Path::new(&dir).join(name),
        )
        .unwrap();
    }
    shot
}

fn rgb(shot: &[u8], x: u32, y: u32) -> [u8; 3] {
    let [r, g, b, _] = common::pixel_in(shot, SHOT.0, x, y);
    [r, g, b]
}

/// What an Amalfi material shows over a flat color (levels), away from its edge: the color
/// saturated by 1.1 around its Rec. 709 luminance, its brightness, then the tint `tint` at
/// `fill.amount` over it (`docs/design/amalfi/materials.md`).
fn amalfi_fill(color: [f64; 3], tint: [f64; 3], fill: Fill) -> [u8; 3] {
    let l = 0.2126 * color[0] + 0.7152 * color[1] + 0.0722 * color[2];
    let mut out = [0u8; 3];
    for c in 0..3 {
        let saturated = ((l + (color[c] - l) * 1.1) * fill.brightness).clamp(0.0, 255.0);
        out[c] = (saturated + (tint[c] - saturated) * fill.amount)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    out
}

/// The reference gray of the glass tests, `#7a7b80`.
const GRAY: [f64; 3] = [122.0, 123.0, 128.0];
/// The tints of `tokens.json`: white in light, `#2A2B30` in dark; 60 % on Glass/Regular, 58 %
/// (`94`) on Glass/Thick.
const TINT_LIGHT: [f64; 3] = [255.0; 3];
const TINT_DARK: [f64; 3] = [42.0, 43.0, 48.0];
/// The tint amount and the backdrop brightness of a material in an appearance.
#[derive(Clone, Copy)]
struct Fill {
    amount: f64,
    brightness: f64,
}
const REGULAR_LIGHT: Fill = Fill {
    amount: 0.6,
    brightness: 1.25,
};
const REGULAR_DARK: Fill = Fill {
    amount: 0.6,
    brightness: 1.10,
};
const THICK_LIGHT: Fill = Fill {
    amount: 148.0 / 255.0,
    brightness: 1.12,
};
const THICK_DARK: Fill = Fill {
    amount: 148.0 / 255.0,
    brightness: 1.05,
};

/// The largest channel difference between two colors.
fn worst(a: [u8; 3], b: [u8; 3]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(b))
        .max()
        .unwrap_or(0)
}

/// Light (or dark, `--appearance dark`) glass on the reference gray: a `clear` region (the
/// Dock: Glass/Regular) at (32, 32) and a `thick` one (a menu: Glass/Thick) at (330, 200).
fn fills_on_gray(name: &str, dark: bool) -> ([u8; 3], [u8; 3]) {
    let mut args = vec!["--background", "7a7b80"];
    if dark {
        args.extend(["--appearance", "dark"]);
    }
    let compositor = start_at_32(name, SHOT, &args);
    let size = (200, 100);
    let mut clear = TestWindow::open_pixels(&compositor.wayland, "clear", size, &transparent(size));
    clear
        .set_glass(&[(0, 0, 200, 100, 0.0, Material::Clear)])
        .unwrap();
    let mut thick = TestWindow::open_pixels(&compositor.wayland, "thick", size, &transparent(size));
    thick
        .set_glass(&[(0, 0, 200, 100, 0.0, Material::Thick)])
        .unwrap();
    // The two windows: the first at (32, 32), the second moved to (330, 200), above.
    compositor
        .client()
        .call(
            silica_control::commands::WINDOW_MOVE,
            serde_json::json!({"x": 330, "y": 200}),
        )
        .unwrap();
    let shot = shot(&compositor, &format!("{name}.png"));
    (rgb(&shot, 132, 82), rgb(&shot, 430, 250))
}

#[test]
fn materials_on_the_reference_gray_are_the_amalfi_fill() {
    let (regular, thick) = fills_on_gray("glass-gray", false);
    let want_regular = amalfi_fill(GRAY, TINT_LIGHT, REGULAR_LIGHT);
    let want_thick = amalfi_fill(GRAY, TINT_LIGHT, THICK_LIGHT);
    eprintln!("regular {regular:?} (Amalfi {want_regular:?}), thick {thick:?} ({want_thick:?})");
    assert!(
        worst(regular, want_regular) <= 2,
        "Glass/Regular {regular:?}"
    );
    assert!(worst(thick, want_thick) <= 2, "Glass/Thick {thick:?}");
    // Regular glass is the brighter one in light (×1.25 against ×1.12, tint 60 % against 58 %
    // of white).
    assert!(thick[1] < regular[1]);
}

/// In dark (`--appearance dark`) the tint is `#2A2B30`.
#[test]
fn dark_materials_on_the_reference_gray() {
    let (regular, thick) = fills_on_gray("glass-gray-dark", true);
    let want_regular = amalfi_fill(GRAY, TINT_DARK, REGULAR_DARK);
    let want_thick = amalfi_fill(GRAY, TINT_DARK, THICK_DARK);
    eprintln!("regular {regular:?} (Amalfi {want_regular:?}), thick {thick:?} ({want_thick:?})");
    assert!(
        worst(regular, want_regular) <= 2,
        "Glass/Regular {regular:?}"
    );
    assert!(worst(thick, want_thick) <= 2, "Glass/Thick {thick:?}");
    // In dark the two are within a level or two of each other.
    assert!(thick[1].abs_diff(regular[1]) <= 3);
}

/// A window that overrides the appearance with dark in a light session gets the dark glass
/// for the regions it declares (silica-window-v1 `set_appearance`).
#[test]
fn glass_of_a_dark_window_in_a_light_session_has_the_dark_tone() {
    let compositor = start_at_32("glass-override-dark", SHOT, &["--background", "7a7b80"]);
    let size = (200, 100);
    let mut thick = TestWindow::open_pixels(&compositor.wayland, "thick", size, &transparent(size));
    thick
        .set_glass(&[(0, 0, 200, 100, 0.0, Material::Thick)])
        .unwrap();
    thick.set_appearance(silica_protocols::window::client::silica_window_v1::Appearance::Dark);
    compositor
        .client()
        .call(
            silica_control::commands::WINDOW_MOVE,
            serde_json::json!({"x": 330, "y": 200}),
        )
        .unwrap();
    let shot = shot(&compositor, "glass-override-dark.png");
    let got = rgb(&shot, 430, 250);
    let expected = amalfi_fill(GRAY, TINT_DARK, THICK_DARK);
    assert!(
        worst(got, expected) <= 2,
        "thick glass of a dark window: {got:?}, expected {expected:?}"
    );
}

/// The standard normal cumulative distribution (Abramowitz and Stegun 7.1.26, |error| <
/// 1.5e-7).
fn normal_cdf(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    if x >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

#[test]
fn regular_glass_blurs_a_stripe_edge_with_sigma_6() {
    let compositor = start_at_32("glass-blur", SHOT, &[]);
    let back = (400, 200);
    let _stripes = TestWindow::open_pixels(&compositor.wayland, "stripes", back, &stripes(back));
    let size = (200, 100);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "glass", size, &transparent(size));
    glass
        .set_glass(&[(0, 0, 200, 100, 0.0, Material::Clear)])
        .unwrap();
    let shot = shot(&compositor, "glass-blur.png");
    // Stripes at x 32 + 96k (black) and 80 + 96k (white); glass on x 64–263, y 64–163. The
    // black → white edge at x 176, far from the glass edges and from the next stripe edges.
    let row: Vec<f64> = (150..202)
        .map(|x| f64::from(rgb(&shot, x, 114)[1]))
        .collect();
    // Glass/Regular over a gray step: the sharp share (0.28 on the Dock) plus a Gaussian of σ
    // `sigma` px, the brightness (×1.25 in light, clamped) and 60 % white (gray does not change
    // with the saturation).
    let model = |sigma: f64, edge: f64, x: f64| {
        let sharp = if x >= edge { 255.0 } else { 0.0 };
        let blurred = 255.0 * normal_cdf((x - edge) / sigma);
        let backdrop = 0.28 * sharp + 0.72 * blurred;
        let lit = (backdrop * REGULAR_LIGHT.brightness).min(255.0);
        lit + (255.0 - lit) * REGULAR_LIGHT.amount
    };
    let fit = |sigma: f64| {
        (0..200)
            .map(|step| 170.0 + f64::from(step) * 0.05)
            .map(|edge| {
                let sum: f64 = row
                    .iter()
                    .enumerate()
                    .map(|(i, got)| (got - model(sigma, edge, 150.0 + i as f64 + 0.5)).powi(2))
                    .sum();
                (sum / row.len() as f64).sqrt()
            })
            .fold(f64::MAX, f64::min)
    };
    let (rms3, rms6, rms12) = (fit(3.0), fit(6.0), fit(12.0));
    eprintln!("row {row:?}");
    eprintln!("rms against σ 3 px {rms3:.2}, σ 6 px {rms6:.2}, σ 12 px {rms12:.2}");
    assert!(rms6 <= 2.0, "the edge is not a σ 6 blur: rms {rms6:.2}");
    assert!(rms6 < rms3 && rms6 < rms12);
}

/// The Amalfi rims of a capsule on the reference gray at 1×: the inside rim (1 pt of black, 12 %
/// in light) on the first row all around, the rim light (white 70 %, an inner shadow moved 1
/// pt down and blurred by 1 pt) on the top inner edge only, nothing outside the shape.
#[test]
fn the_rims_are_a_dark_inside_line_and_a_light_top_edge() {
    let compositor = start_at_32("glass-edge", SHOT, &["--background", "7a7b80"]);
    // The glass 20 px inside a larger window, so that the window's own shadow is not next to it.
    let size = (340, 120);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "edge", size, &transparent(size));
    glass
        .set_glass(&[(20, 20, 300, 80, 22.0, Material::Clear)])
        .unwrap();
    let shot = shot(&compositor, "glass-edge.png");
    // Window at (32, 32), glass from (52, 52) to (351, 131): column halfway, rows top 52 and
    // bottom 131, row halfway 92.
    let (x, top, bottom, mid) = (202, 52, 131, 92);
    let level = |at: (u32, u32)| f64::from(rgb(&shot, at.0, at.1)[1]);
    let fill = level((x, mid));
    let want_fill = f64::from(amalfi_fill(GRAY, TINT_LIGHT, REGULAR_LIGHT)[1]);
    assert!((fill - want_fill).abs() <= 2.0, "fill {fill}");
    // The first row's pixel centre is 0.5 px inside: the inside rim covers it (fill × 0.88),
    // the rim light reaches it on top at 1 / (1 + e^-1.702) of 70 %, at the bottom not at all.
    let rim = fill * (1.0 - 31.0 / 255.0);
    let lit = 0.702 / (1.0 + (-1.702_f64).exp());
    let top_row = rim + (255.0 - rim) * lit;
    let measured = [
        (level((x, top)), top_row, "top row"),
        (level((x, bottom)), rim, "bottom row"),
        // The second row on top still gets the light's tail; at the bottom it is the fill.
        (
            level((x, top + 1)),
            fill + (255.0 - fill) * 0.702 / (1.0 + 1.702_f64.exp()),
            "second row on top",
        ),
        (level((x, bottom - 1)), fill, "second row at the bottom"),
    ];
    for (got, want, what) in measured {
        eprintln!("{what}: {got} (Amalfi {want:.1})");
        assert!((got - want).abs() <= 3.0, "{what}: {got}, Amalfi {want:.1}");
    }
    // The sides: the inside rim and only the light's spill (≤ 11 % of the blur at 0.5 px).
    let left = level((52, mid));
    let right = level((351, mid));
    for side in [left, right] {
        assert!(side < fill - 10.0 && side > rim - 2.0, "side {side}");
    }
    // Nothing outside: no outline, the background right next to every edge.
    for at in [(x, top - 1), (x, bottom + 1), (51, mid), (352, mid)] {
        let got = rgb(&shot, at.0, at.1);
        assert!(
            worst(got, [0x7a, 0x7b, 0x80]) <= 1,
            "outside at {at:?}: {got:?}"
        );
    }
}

#[test]
fn regular_and_thick_glass_refract_at_the_edge_only() {
    // 30 px stripes behind (black for (x - 8) mod 60 < 30), the window at (32, 32): stripe edges
    // at screen x 40 + 30k. The glass is 300 × 100 at (64, 64): the edge at x 70 lies 6 px inside
    // its left edge, the ones at x 130 and 190 in the middle. Glass/Regular and Glass/Thick pull the
    // backdrop in towards the rim over a band of depth × 50 px (17.5 and 14): the edge near the
    // rim is shown further inside (a pixel at distance t shows the backdrop at
    // t - distortion × band × (1 - t / band)², so by about 2.0 px on Regular and 1.1 px on
    // Thick here); the one in the middle stays where it is.
    for (material, low, high) in [(Material::Clear, 1.0, 4.0), (Material::Thick, 0.4, 2.5)] {
        let compositor = start_at_32("glass-refraction", SHOT, &[]);
        let back = (500, 200);
        let mut pixels = Vec::new();
        for _ in 0..back.1 {
            for x in 0..back.0 {
                let level = if (x + 52) % 60 < 30 { 0 } else { 255 };
                pixels.extend_from_slice(&[level, level, level, 255]);
            }
        }
        let _stripes = TestWindow::open_pixels(&compositor.wayland, "stripes", back, &pixels);
        let size = (300, 100);
        let mut glass =
            TestWindow::open_pixels(&compositor.wayland, "glass", size, &transparent(size));
        glass.set_glass(&[(0, 0, 300, 100, 0.0, material)]).unwrap();
        let shot = shot(&compositor, "glass-refraction.png");
        let level = |x: i32| f64::from(rgb(&shot, x as u32, 114)[1]);
        // The edges at x 70 and 190 go the same way. The level at the middle of the one at x
        // 190 (far from the rim) is where the blurred edge is: look for it near x 70.
        let mid = (level(189) + level(190)) / 2.0;
        let rising = level(197) > level(183);
        let crossing = |edge: i32| {
            (edge - 5..edge + 10)
                .find(|x| {
                    let (a, b) = (level(*x), level(*x + 1));
                    if rising {
                        a < mid && b >= mid
                    } else {
                        a > mid && b <= mid
                    }
                })
                .map(|x| {
                    let (a, b) = (level(x), level(x + 1));
                    f64::from(x) + 0.5 + (mid - a) / (b - a)
                })
                .unwrap_or_else(|| panic!("{material:?}: no crossing near {edge}"))
        };
        let near = crossing(70) - 70.0;
        let centre = crossing(130) - 130.0;
        eprintln!("{material:?}: shift at the rim {near:.2} px, in the middle {centre:.2} px");
        assert!(
            (low..high).contains(&near),
            "{material:?}: the rim shifts the edge by {near:.2} px"
        );
        assert!(
            centre.abs() <= 0.5,
            "{material:?} refracts in the middle: {centre:.2} px"
        );
    }
}

#[test]
fn glass_quality_levels_choose_what_the_glass_sees() {
    let compositor = start_at_32("glass-quality", SHOT, &[]);
    let back = (400, 200);
    let _stripes = TestWindow::open_pixels(&compositor.wayland, "stripes", back, &stripes(back));
    let size = (200, 100);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "glass", size, &transparent(size));
    glass
        .set_glass(&[(0, 0, 200, 100, 0.0, Material::Clear)])
        .unwrap();
    let mut client = compositor.client();
    // Swing of the green on the row halfway through the glass (x 74–254, far from the edges),
    // and its middle.
    let mut spread = |quality: &str| {
        let resolved = client
            .call(
                silica_control::commands::UI_GLASS_QUALITY,
                serde_json::json!({ "quality": quality }),
            )
            .unwrap();
        let shot = shot(&compositor, &format!("glass-{quality}.png"));
        let row: Vec<u8> = (74..254).map(|x| rgb(&shot, x, 114)[1]).collect();
        let spread = row.iter().max().unwrap() - row.iter().min().unwrap();
        eprintln!("{quality} ({resolved}): spread {spread}");
        (
            resolved["quality"].as_str().unwrap_or_default().to_owned(),
            spread,
            rgb(&shot, 164, 114),
        )
    };
    // Full: the blurred stripes stay under the tint (60 % white: black → 153, white → 255).
    assert!(spread("full").1 > 80);
    // Frosted: sees only the desktop, not the striped window; opaque: no background at all,
    // the tint over Surface/Window (white in light: white).
    assert!(spread("frosted").1 <= 2);
    let (_, flat, opaque) = spread("opaque");
    assert!(flat <= 2);
    assert!(
        worst(opaque, [255, 255, 255]) <= 1,
        "opaque fill {opaque:?}"
    );
    // On llvmpipe (headless) the automatic level is frosted.
    assert_eq!(spread("auto").0, "frosted");
}

#[test]
fn without_blur_dark_glass_is_its_tint_over_the_window_surface() {
    // Reduced transparency in dark: thick glass is 58 % `#2A2B30` over `#1B1C1F`, whatever is
    // behind (here white).
    let compositor = start_at_32(
        "glass-opaque-dark",
        SHOT,
        &["--background", "ffffff", "--appearance", "dark"],
    );
    let size = (200, 100);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "thick", size, &transparent(size));
    glass
        .set_glass(&[(0, 0, 200, 100, 0.0, Material::Thick)])
        .unwrap();
    compositor
        .client()
        .call(
            silica_control::commands::UI_GLASS_QUALITY,
            serde_json::json!({ "quality": "opaque" }),
        )
        .unwrap();
    let got = rgb(&shot(&compositor, "glass-opaque-dark.png"), 132, 82);
    let mix =
        |surface: f64, tint: f64| (surface + (tint - surface) * THICK_DARK.amount).round() as u8;
    let want = [mix(27.0, 42.0), mix(28.0, 43.0), mix(31.0, 48.0)];
    assert!(
        worst(got, want) <= 1,
        "opaque dark thick glass {got:?}, want {want:?}"
    );
}

#[test]
fn the_regular_protocol_material_is_the_thick_glass_of_the_modules() {
    // `regular` without a profile is a Control Center module: Glass/Thick, with its sharp share
    // (0.04) that a flat gray does not show.
    let compositor = start_at_32("glass-module-fill", SHOT, &["--background", "7a7b80"]);
    let size = (200, 100);
    let mut glass =
        TestWindow::open_pixels(&compositor.wayland, "module", size, &transparent(size));
    glass
        .set_glass(&[(0, 0, 200, 100, 0.0, Material::Regular)])
        .unwrap();
    let fill = rgb(&shot(&compositor, "glass-module-fill.png"), 132, 82);
    let want = amalfi_fill(GRAY, TINT_LIGHT, THICK_LIGHT);
    eprintln!("fill: {fill:?} (Amalfi {want:?})");
    assert!(worst(fill, want) <= 2, "module fill {fill:?}");
}

#[test]
fn nearby_regions_of_the_same_glass_merge_like_drops() {
    let compositor = start_at_32("glass-merge", SHOT, &["--background", "7a7b80"]);
    let size = (400, 100);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "drops", size, &transparent(size));
    let background = [0x7a_u8, 0x7b, 0x80];
    let differs = |shot: &[u8], x: u32, y: u32| {
        rgb(shot, x, y)
            .iter()
            .zip(background)
            .any(|(got, bg)| got.abs_diff(bg) > 6)
    };
    // Two capsules 4 pt from each other: the point in between becomes glass (window at
    // (32, 32): capsules at x 32–131 and 136–235, gap centered at x 134, y 82).
    glass
        .set_glass(&[
            (0, 20, 100, 60, 30.0, Material::Regular),
            (104, 20, 100, 60, 30.0, Material::Regular),
        ])
        .unwrap();
    let joined = shot(&compositor, "glass-merge.png");
    assert!(
        differs(&joined, 134, 82),
        "the nearby capsules do not merge"
    );

    // At 10 pt (the Spotlight buttons from the panel) they stay separate.
    glass
        .set_glass(&[
            (0, 20, 100, 60, 30.0, Material::Regular),
            (110, 20, 100, 60, 30.0, Material::Regular),
        ])
        .unwrap();
    let near = shot(&compositor, "glass-near.png");
    assert!(!differs(&near, 136, 82), "at 10 pt the capsules merge");

    // Far apart (60 pt): the background stays.
    glass
        .set_glass(&[
            (0, 20, 100, 60, 30.0, Material::Regular),
            (160, 20, 100, 60, 30.0, Material::Regular),
        ])
        .unwrap();
    let apart = shot(&compositor, "glass-apart.png");
    assert!(!differs(&apart, 162, 82), "the distant capsules merge");

    // Different materials do not merge, even when close.
    glass
        .set_glass(&[
            (0, 20, 100, 60, 30.0, Material::Regular),
            (104, 20, 100, 60, 30.0, Material::Clear),
        ])
        .unwrap();
    let mixed = shot(&compositor, "glass-mixed.png");
    assert!(!differs(&mixed, 134, 82), "different materials merge");
}

#[test]
fn separate_groups_overlap_without_merging() {
    let compositor = start_at_32("glass-groups", SHOT, &["--background", "7a7b80"]);
    let size = (400, 100);
    let mut glass =
        TestWindow::open_pixels(&compositor.wayland, "groups", size, &transparent(size));
    let background = [0x7a_u8, 0x7b, 0x80];
    let differs = |shot: &[u8], x: u32, y: u32| {
        rgb(shot, x, y)
            .iter()
            .zip(background)
            .any(|(got, bg)| got.abs_diff(bg) > 6)
    };
    // The capsules 4 pt apart that merge (above), but in two groups: the gap stays background,
    // like a submenu next to its menu.
    glass
        .set_glass_with(|g| {
            g.add_region(0, 20, 100, 60, 30.0, Material::Regular);
            g.separate();
            g.add_region(104, 20, 100, 60, 30.0, Material::Regular);
        })
        .unwrap();
    let split = shot(&compositor, "glass-groups.png");
    assert!(!differs(&split, 134, 82), "the separate groups merge");
    let list = glass_list(&compositor);
    let groups: Vec<u64> = list[0]["regions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["group"].as_u64().unwrap())
        .collect();
    assert_eq!(groups, [0, 1]);
}

/// Widths of the reference Dock menu's tip per row of 1 pt below the body edge (TIP-01, light:
/// 24.2 19.4 17.0 15.3 13.7 12.1 10.4 8.8 6.9 2.7), rounded.
const TIP_WIDTHS: [u32; 10] = [24, 19, 17, 15, 14, 12, 10, 9, 7, 3];

/// The glass of a thick body with the Dock menu's tip (31 × 10: TIP-01 measures 30.6 × 10.19
/// from where the outline leaves the body edge) on one of its sides, over the reference gray;
/// per row (or column) of the tip from its base outwards, how many pixels are glass.
fn tip_widths(name: &str, side: &str) -> Vec<u32> {
    // Upward and sideways tips: silica-glass-v1 versions 5 and 6.
    common::client::GLASS_VERSION.with(|v| v.set(6));
    let compositor = start_at_32(name, SHOT, &["--background", "7a7b80"]);
    let size = (300, 300);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "tip", size, &transparent(size));
    // Body from (60, 60) to (240, 240); the tip centered on the chosen side at 150.
    let side_owned = side.to_owned();
    glass
        .set_glass_with(move |g| {
            g.add_region(60, 60, 180, 180, 12.0, Material::Thick);
            match side_owned.as_str() {
                "down" => g.add_tip(135, 240, 31, 10, Material::Thick, 1.0),
                "up" => g.add_upward_tip(135, 50, 31, 10, Material::Thick, 1.0),
                "left" => g.add_side_tip(50, 135, 10, 31, Side::Left, Material::Thick, 1.0),
                _ => g.add_side_tip(240, 135, 10, 31, Side::Right, Material::Thick, 1.0),
            }
        })
        .unwrap();
    let image = shot(&compositor, &format!("{name}.png"));
    let background = [0x7a_u8, 0x7b, 0x80];
    // Glass: halfway or more from the background to the fill (the outline outside is lighter).
    let fill = rgb(&image, 32 + 150, 32 + 150);
    let is_glass = |x: u32, y: u32| {
        let got = rgb(&image, 32 + x, 32 + y);
        let d = |c: [u8; 3]| {
            c.iter()
                .zip(background)
                .map(|(a, b)| f64::from(*a) - f64::from(b))
                .sum::<f64>()
        };
        d(got) / d(fill) >= 0.5
    };
    (0..12)
        .map(|k| {
            (128..174)
                .filter(|&a| match side {
                    "down" => is_glass(a, 240 + k),
                    "up" => is_glass(a, 59 - k),
                    "left" => is_glass(59 - k, a),
                    _ => is_glass(240 + k, a),
                })
                .count() as u32
        })
        .collect()
}

#[test]
fn a_tip_has_the_shape_of_the_dock_menus_tip() {
    let widths = tip_widths("glass-tip", "down");
    eprintln!("widths {widths:?} (reference {TIP_WIDTHS:?})");
    for (k, (got, want)) in widths.iter().zip(TIP_WIDTHS).enumerate() {
        assert!(
            got.abs_diff(want) <= 2,
            "row {k} below the base: {got} px, reference {want}"
        );
    }
    // Nothing past the apex.
    assert!(
        widths[10] <= 1 && widths[11] == 0,
        "the tip goes too far: {widths:?}"
    );
}

#[test]
fn tips_point_every_way_with_the_same_shape() {
    let down = tip_widths("glass-tip-down", "down");
    for side in ["up", "left", "right"] {
        let widths = tip_widths(&format!("glass-tip-{side}"), side);
        eprintln!("{side}: {widths:?} (down {down:?})");
        for (k, (a, b)) in widths.iter().zip(&down).enumerate() {
            // The half-covered pixels along the flanks also carry the rims, which depend on the
            // orientation (the rim light falls on the flanks of a tip pointing up, only the
            // dark inside rim on those of a tip pointing down): they read as glass or not, a
            // pixel more or less per flank.
            let tolerance = 2;
            assert!(
                a.abs_diff(*b) <= tolerance,
                "{side}, step {k}: {a} px against {b} down"
            );
        }
    }
}

#[test]
fn a_tip_joins_its_body_in_one_shape() {
    let compositor = start_at_32("glass-tip-join", SHOT, &["--background", "7a7b80"]);
    let size = (300, 120);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "tip", size, &transparent(size));
    // The Dock menu: thick body and its 31 × 10 tip below, centered at x 100.5.
    glass
        .set_glass_with(|g| {
            g.add_region(40, 10, 200, 60, 12.0, Material::Thick);
            g.add_tip(85, 70, 31, 10, Material::Thick, 1.0);
        })
        .unwrap();
    let image = shot(&compositor, "glass-tip-join.png");
    // No body edge across the tip: on the base, the same color as the inside.
    let base = rgb(&image, 32 + 100, 32 + 70);
    let inside = rgb(&image, 32 + 100, 32 + 60);
    assert!(
        base.iter().zip(inside).all(|(a, b)| a.abs_diff(b) <= 8),
        "the body edge crosses the tip: {base:?} against {inside:?}"
    );
    // Along the body's bottom edge the outline turns into the tip without a notch: right
    // next to the tip's base, the body edge is still glass up to its last row.
    let corner = rgb(&image, 32 + 86, 32 + 69);
    assert!(
        corner.iter().zip(inside).all(|(a, b)| a.abs_diff(b) <= 40),
        "a notch where the tip meets the body: {corner:?}"
    );
    assert_eq!(glass_list(&compositor)[0]["regions"][1]["shape"], "tip");
}

/// The Dock menu with its tip near the left corner (the body 122 × 69, the apex 27 pt from the
/// left edge, as measured): the tip's triangle must not show outside the body above its base. It used to
/// go on into the body, widening, and stuck out of the left side as a wedge.
#[test]
fn a_tip_near_a_corner_does_not_leak_out_of_the_body() {
    let compositor = start_at_32("glass-tip-corner", SHOT, &["--background", "7a7b80"]);
    let size = (240, 120);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "tip", size, &transparent(size));
    glass
        .set_glass_with(|g| {
            g.add_region(50, 10, 122, 69, 12.0, Material::Thick);
            g.add_tip(62, 79, 31, 10, Material::Thick, 1.0);
        })
        .unwrap();
    let image = shot(&compositor, "glass-tip-corner.png");
    let background = [0x7a_u8, 0x7b, 0x80];
    let lift = |c: [u8; 3]| {
        c.iter()
            .zip(background)
            .map(|(a, b)| f64::from(*a) - f64::from(b))
            .sum::<f64>()
    };
    let fill = lift(rgb(&image, 32 + 110, 32 + 40));
    // Left of the body, along its height except the rounded corners: the background (the
    // dark outline just outside the edge is allowed, glass is not).
    for y in 24..66 {
        for x in 38..49 {
            let got = rgb(&image, 32 + x, 32 + y);
            assert!(
                lift(got) / fill < 0.3,
                "glass left of the body at ({x}, {y}): {got:?}"
            );
        }
    }
    // The tip itself is still there.
    let tip = rgb(&image, 32 + 77, 32 + 82);
    assert!(lift(tip) / fill > 0.5, "the tip is gone: {tip:?}");
}

#[test]
fn notification_glass_is_regular_glass_whose_cards_stay_apart() {
    // A banner is Glass/Regular.
    let compositor = start_at_32("glass-notification", SHOT, &["--background", "7a7b80"]);
    let size = (344, 91);
    let mut glass =
        TestWindow::open_pixels(&compositor.wayland, "banner", size, &transparent(size));
    glass
        .set_glass(&[(0, 0, 344, 91, 20.0, Material::Notification)])
        .unwrap();
    let fill = rgb(
        &shot(&compositor, "glass-notification.png"),
        32 + 172,
        32 + 45,
    );
    let want = amalfi_fill(GRAY, TINT_LIGHT, REGULAR_LIGHT);
    eprintln!("fill: {fill:?} (Amalfi {want:?})");
    assert!(worst(fill, want) <= 2, "banner fill {fill:?}");

    // Two cards 6 pt apart (the Center's cards, with the glass 1 pt wider per side) stay
    // separate: in the gap between them there is the background.
    glass
        .set_glass(&[
            (0, 0, 344, 40, 20.0, Material::Notification),
            (0, 46, 344, 40, 20.0, Material::Notification),
        ])
        .unwrap();
    let gap = rgb(
        &shot(&compositor, "glass-notification-gap.png"),
        32 + 172,
        32 + 43,
    );
    assert_eq!(gap, [0x7a, 0x7b, 0x80], "between the cards: {gap:?}");
    drop(glass);

    // It belongs to version 2: a client bound to 1 that asks for it is disconnected.
    common::client::GLASS_VERSION.with(|v| v.set(1));
    let mut old = TestWindow::open_pixels(&compositor.wayland, "old", size, &transparent(size));
    assert!(
        old.set_glass(&[(0, 0, 344, 91, 20.0, Material::Notification)])
            .is_err()
    );
    common::client::GLASS_VERSION.with(|v| v.set(2));
}

#[test]
fn every_region_of_a_group_has_its_rim_on_its_own_edge() {
    // Like the Control Center modules: regions of the same material far from each other,
    // capsules and rounded rectangles. The rim light sits on the first row of each region's
    // top and the inside rim (darker) on its last row, even far from the center of the group.
    let compositor = start_at_32("glass-group-rim", SHOT, &["--background", "7a7b80"]);
    let size = (292, 330);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "stack", size, &transparent(size));
    // The Control Center grid: modules 12 pt from each other, with the glass 1 pt wider
    // per side (10 pt between the glasses horizontally).
    let regions = [
        (0, 0, 142, 64, 32.0, Material::Regular),
        (152, 0, 142, 140, 35.0, Material::Regular),
        (0, 76, 142, 64, 32.0, Material::Regular),
        (0, 152, 142, 64, 32.0, Material::Regular),
        (0, 228, 294, 64, 28.0, Material::Regular),
    ];
    glass.set_glass(&regions).unwrap();
    let shot = shot(&compositor, "glass-group-rim.png");
    let brightest =
        |x: u32, y0: u32, y1: u32| (y0..y1).max_by_key(|&y| rgb(&shot, x, y)[0]).unwrap();
    let darkest = |x: u32, y0: u32, y1: u32| (y0..y1).min_by_key(|&y| rgb(&shot, x, y)[0]).unwrap();
    let background = [0x7a_u8, 0x7b, 0x80];
    for (x, y, w, h, _, _) in regions {
        let column = 32 + x as u32 + w as u32 / 2;
        let top = 32 + y as u32;
        let bottom = top + h as u32 - 1;
        // (The first region sits on the window's own top row, which the window rim dims.)
        if y > 0 {
            assert_eq!(
                brightest(column, top, top + 4),
                top,
                "region at y {y}: top row"
            );
        }
        assert_eq!(
            darkest(column, bottom - 3, bottom + 1),
            bottom,
            "region at y {y}: bottom row"
        );
        // At 12 pt the modules do not touch, and the rims stay inside: the row right below is
        // background.
        let outside = rgb(&shot, column, bottom + 1);
        assert!(
            worst(outside, background) <= 1,
            "region at y {y}: below the edge {outside:?}"
        );
    }
    // Between the first module and the media one: the inside rim on the side edge's last
    // column, then background (no outline outside; the faint start of the merging bridge
    // between the two shapes, 10 pt apart, lifts the first column by a level or two).
    let fill = rgb(&shot, 32 + 71, 32 + 32);
    let side = rgb(&shot, 32 + 141, 32 + 32);
    assert!(side[0] < fill[0] - 10, "no inside rim on the side {side:?}");
    for x in [142, 144] {
        let beside = rgb(&shot, 32 + x, 32 + 32);
        assert!(
            worst(beside, background) <= 3,
            "next to the side edge {beside:?}"
        );
    }
}

#[test]
fn translucent_glass_fades_with_its_panel() {
    // At half opacity the notification glass on the gray is halfway between the background
    // (#7a7b80) and the full fill: a fading panel carries the glass with it.
    let compositor = start_at_32("glass-translucent", SHOT, &["--background", "7a7b80"]);
    let size = (344, 91);
    let mut glass =
        TestWindow::open_pixels(&compositor.wayland, "banner", size, &transparent(size));
    glass
        .set_translucent_glass(&[((0, 0, 344, 91, 20.0, Material::Notification), 0.5)])
        .unwrap();
    let fill = rgb(
        &shot(&compositor, "glass-translucent.png"),
        32 + 172,
        32 + 45,
    );
    let full = amalfi_fill(GRAY, TINT_LIGHT, REGULAR_LIGHT);
    let mut half = [0u8; 3];
    for c in 0..3 {
        half[c] = ((GRAY[c] + f64::from(full[c])) / 2.0).round() as u8;
    }
    assert!(
        worst(fill, half) <= 2,
        "half opacity: {fill:?}, want {half:?}"
    );
    // Fully transparent: the background stays.
    glass
        .set_translucent_glass(&[((0, 0, 344, 91, 20.0, Material::Notification), 0.0)])
        .unwrap();
    let fill = rgb(
        &shot(&compositor, "glass-translucent-0.png"),
        32 + 172,
        32 + 45,
    );
    assert_eq!(fill, [0x7a, 0x7b, 0x80]);
    // Outside 0–1: the client is disconnected, the compositor goes on.
    assert!(
        glass
            .set_translucent_glass(&[((0, 0, 344, 91, 20.0, Material::Notification), 1.5)])
            .is_err()
    );
    let _alive = TestWindow::open(&compositor.wayland, "after");
}

/// A wallpaper window and a glass window both covering the screen (and 40 px beyond, so their
/// corners and shadows stay off screen): screen pixel (x, y) is window pixel (x + 40, y + 40).
fn full_screen_glass(
    compositor: &Headless,
    level: impl Fn(u32, u32) -> [u8; 3],
) -> (TestWindow, TestWindow) {
    const BLEED: u32 = 40;
    let size = (SHOT.0 + 2 * BLEED, SHOT.1 + 2 * BLEED);
    let mut pixels = Vec::with_capacity((size.0 * size.1 * 4) as usize);
    for y in 0..size.1 {
        for x in 0..size.0 {
            let [r, g, b] = level(x.saturating_sub(BLEED), y.saturating_sub(BLEED));
            pixels.extend_from_slice(&[b, g, r, 255]);
        }
    }
    let place = || {
        compositor
            .client()
            .call(
                silica_control::commands::WINDOW_MOVE,
                serde_json::json!({"x": -(BLEED as i32), "y": -(BLEED as i32)}),
            )
            .unwrap();
    };
    let back = TestWindow::open_pixels(&compositor.wayland, "back", size, &pixels);
    place();
    let glass = TestWindow::open_pixels(&compositor.wayland, "glass", size, &transparent(size));
    place();
    (back, glass)
}

#[test]
fn glass_cut_by_the_screen_edge_does_not_blur_in_the_far_side() {
    // A menu 2 px from the left edge of the screen: its capture is cut there. White behind it,
    // black from x 175 on (inside the capture's right margin). The offscreen textures repeat
    // by default: without clamping, the blur at the left edge read the black from the far
    // right of the capture (#168).
    let compositor = start_at_32("glass-edge-clamp", SHOT, &[]);
    let (_back, mut glass) =
        full_screen_glass(&compositor, |x, _| if x < 175 { [255; 3] } else { [0; 3] });
    glass
        .set_glass(&[(40 + 2, 40 + 100, 150, 120, 12.0, Material::Thick)])
        .unwrap();
    let shot = shot(&compositor, "glass-edge-clamp.png");
    let middle = rgb(&shot, 60, 160);
    for x in [4, 6, 10, 20] {
        let edge = rgb(&shot, x, 160);
        // Inside the edge rows: the side profile darkens by a few levels only.
        let worst = edge
            .iter()
            .zip(middle)
            .map(|(e, m)| i32::from(*e) - i32::from(m))
            .min()
            .unwrap_or(0);
        assert!(
            worst >= -8,
            "x {x}: {edge:?} darker than the middle {middle:?}"
        );
    }
}

#[test]
fn the_rims_follow_the_end_of_a_capsule_without_an_outline() {
    // A Control Center module (a capsule, 142 × 64, radius 32) over the reference gray. Around
    // the end the inside rim stays inside the shape and nothing is drawn outside it (the
    // measured glass had a dark line outside the sides that once wrapped the whole end, a
    // black crescent, #168).
    let compositor = start_at_32("glass-capsule-end", SHOT, &[]);
    let (_back, mut glass) = full_screen_glass(&compositor, |_, _| [0x7a, 0x7b, 0x80]);
    glass
        .set_glass(&[(40 + 100, 40 + 100, 142, 64, 32.0, Material::Regular)])
        .unwrap();
    let shot = shot(&compositor, "glass-capsule-end.png");
    let background = rgb(&shot, 40, 40);
    let fill = f64::from(rgb(&shot, 171, 132)[1]);
    let (cx, cy, r) = (100.0 + 32.0, 100.0 + 32.0, 32.0);
    let at = |deg: f64, d: f64| {
        let a = deg.to_radians();
        let x = cx - a.cos() * d;
        let y = cy - a.sin() * d;
        rgb(&shot, x.floor() as u32, y.floor() as u32)
    };
    for deg in [0.0, 30.0, 45.0, 60.0, -30.0, -60.0] {
        // 1.5 px outside the arc: the background.
        let outside = at(deg, r + 1.5);
        assert!(
            worst(outside, background) <= 2,
            "{deg}°: outside the end {outside:?}"
        );
    }
    // Just inside the side (0°) and the lower part of the end, the inside rim darkens the fill.
    for deg in [0.0, -45.0] {
        let rim = f64::from(at(deg, r - 0.75)[1]);
        assert!(
            rim < fill - 8.0,
            "{deg}°: no inside rim {rim} (fill {fill})"
        );
    }
}

#[test]
fn a_profile_applies_to_the_following_regions_until_clear() {
    common::client::GLASS_VERSION.with(|v| v.set(7));
    let compositor = Headless::start("glass-profile-list");
    let mut window = TestWindow::open(&compositor.wayland, "glass");
    window
        .set_glass_with(|g| {
            g.add_region(0, 0, 40, 20, 8.0, Material::Thick);
            g.set_profile(Profile::Tooltip, Shadow::Compositor);
            g.add_region(0, 30, 40, 20, 8.0, Material::Thick);
            g.set_profile(Profile::Default, Shadow::Compositor);
            g.add_region(0, 60, 40, 20, 8.0, Material::Thick);
        })
        .unwrap();
    let list = glass_list(&compositor);
    let regions = &list[0]["regions"];
    assert_eq!(regions[0]["profile"], Value::Null, "{list}");
    assert_eq!(regions[1]["profile"], "tooltip");
    assert_eq!(regions[1]["shadow"], true);
    // Without a profile there is no shadow of its own to draw.
    assert_eq!(regions[2]["shadow"], false);
    // `clear` goes back to the default.
    window
        .set_glass(&[(0, 0, 40, 20, 8.0, Material::Thick)])
        .unwrap();
    assert_eq!(
        glass_list(&compositor)[0]["regions"][0]["profile"],
        Value::Null
    );

    let windows = compositor.client().call(WINDOW_LIST, Value::Null).unwrap();
    assert!(windows.as_array().is_some_and(|list| !list.is_empty()));
}

#[test]
fn the_compositor_draws_the_profile_shadow_only_when_asked() {
    common::client::GLASS_VERSION.with(|v| v.set(7));
    let below = |name: &str, shadow: Shadow| {
        let compositor = start_at_32(name, SHOT, &[]);
        let (_back, mut glass) = full_screen_glass(&compositor, |_, _| [0xc0; 3]);
        glass
            .set_glass_with(|g| {
                g.set_profile(Profile::Menu, shadow);
                g.add_region(40 + 100, 40 + 100, 200, 120, 12.0, Material::Thick);
            })
            .unwrap();
        let image = shot(&compositor, &format!("{name}.png"));
        // 4 pt and 16 pt below the bottom edge, mid-width; and well above the top edge.
        (
            rgb(&image, 200, 224)[1],
            rgb(&image, 200, 236)[1],
            rgb(&image, 200, 80)[1],
        )
    };
    let (near, far, above) = below("glass-shadow-compositor", Shadow::Compositor);
    eprintln!("compositor shadow: {near} at 4 pt, {far} at 16 pt, {above} above");
    assert!(near < 0xc0 - 10, "no shadow under the menu: {near}");
    assert!(far > near, "the shadow does not fade: {near} then {far}");
    assert!(above >= 0xc0 - 4, "the shadow sits above the menu: {above}");
    let (near, _, _) = below("glass-shadow-client", Shadow::Client);
    assert!(
        near >= 0xc0 - 2,
        "a shadow the client did not ask for: {near}"
    );
}

/// A point at `(x, y)` from the centre `c` of a region, in its own upright frame, on screen
/// once the region is turned by `degrees` clockwise about that centre (to its pixel).
fn turned(c: (f64, f64), degrees: f64, (x, y): (f64, f64)) -> (u32, u32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let sx = c.0 + cos * x - sin * y;
    let sy = c.1 + sin * x + cos * y;
    (sx.floor() as u32, sy.floor() as u32)
}

#[test]
fn a_rotated_region_turns_its_whole_shape() {
    common::client::GLASS_VERSION.with(|v| v.set(8));
    let compositor = start_at_32("glass-rotated", SHOT, &["--background", "7a7b80"]);
    let size = (320, 320);
    let mut glass =
        TestWindow::open_pixels(&compositor.wayland, "turned", size, &transparent(size));
    // A 200 × 60 menu glass turned 20° clockwise about its centre (window (160, 160)).
    glass
        .set_glass_with(|g| {
            g.set_rotation(20.0);
            g.add_region(60, 130, 200, 60, 10.0, Material::Thick);
        })
        .unwrap();
    let list = glass_list(&compositor);
    assert_eq!(list[0]["regions"][0]["rotation"], 20.0, "{list}");
    assert_eq!(list[0]["regions"][0]["width"], 200);
    let image = shot(&compositor, "glass-rotated.png");
    let background = [0x7a_u8, 0x7b, 0x80];
    let lift = |(x, y): (u32, u32)| {
        rgb(&image, 32 + x, 32 + y)
            .iter()
            .zip(background)
            .map(|(a, b)| f64::from(*a) - f64::from(b))
            .sum::<f64>()
            / 3.0
    };
    let centre = (160.0, 160.0);
    let fill = lift(turned(centre, 20.0, (0.0, 0.0)));
    assert!(fill > 40.0, "no glass at the centre: {fill:.1}");
    // Glass just inside the four turned corners (past the 10 px rounding).
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let at = turned(centre, 20.0, (sx * 93.0, sy * 23.0));
        assert!(
            lift(at) > 0.6 * fill,
            "no glass inside the turned corner at {at:?}"
        );
        // Past the turned side, the background.
        let out = turned(centre, 20.0, (sx * 60.0, sy * 36.0));
        assert!(
            lift(out).abs() < 0.2 * fill,
            "glass outside the turned side at {out:?}"
        );
    }
    // The upright rectangle's top-right and bottom-left corners are not glass any more.
    for at in [(255, 135), (65, 185)] {
        assert!(
            lift(at).abs() < 0.2 * fill,
            "the upright corner {at:?} is still glass"
        );
    }
    // Without a rotation (an earlier client, or after `clear`) the upright rectangle.
    glass
        .set_glass(&[(60, 130, 200, 60, 10.0, Material::Thick)])
        .unwrap();
    assert_eq!(glass_list(&compositor)[0]["regions"][0]["rotation"], 0.0);
    let image = shot(&compositor, "glass-rotated-cleared.png");
    let corner = rgb(&image, 32 + 255, 32 + 135);
    assert!(
        corner[0] > 0x7a + 40,
        "the upright region lost its corner: {corner:?}"
    );
}

#[test]
fn a_rotated_region_keeps_its_edge_light_on_its_own_top_edge() {
    common::client::GLASS_VERSION.with(|v| v.set(8));
    // Over black, where the rim light stands out most against the fill.
    let compositor = start_at_32("glass-rotated-edge", SHOT, &["--background", "000000"]);
    let size = (320, 320);
    let mut glass =
        TestWindow::open_pixels(&compositor.wayland, "turned", size, &transparent(size));
    // A Control Center module (the rim light on its top edge, none on the sides), turned 60°:
    // read upright, its top edge would be a side (no light) and its side a bottom.
    glass
        .set_glass_with(|g| {
            g.set_rotation(60.0);
            g.add_region(60, 120, 200, 80, 12.0, Material::Regular);
        })
        .unwrap();
    let image = shot(&compositor, "glass-rotated-edge.png");
    let level = |(x, y): (u32, u32)| {
        let [r, g, b] = rgb(&image, 32 + x, 32 + y);
        (f64::from(r) + f64::from(g) + f64::from(b)) / 3.0
    };
    let centre = (160.0, 160.0);
    let fill = level(turned(centre, 60.0, (0.0, 0.0)));
    // The brightest pixel along the middle 40 px of an edge, from 0.5 to 4 px in (on a turned
    // edge the pixel centres sit at every depth: some of them right where the light is).
    let brightest = |normal: (f64, f64), half: f64| {
        let tangent = (-normal.1, normal.0);
        (-20..=20)
            .flat_map(|along| (5..=40).map(move |step| (f64::from(along), step)))
            .map(|(along, step)| {
                let d = half - f64::from(step) / 10.0;
                level(turned(
                    centre,
                    60.0,
                    (
                        normal.0 * d + tangent.0 * along,
                        normal.1 * d + tangent.1 * along,
                    ),
                ))
            })
            .fold(f64::MIN, f64::max)
    };
    let top = brightest((0.0, -1.0), 40.0);
    let side = brightest((1.0, 0.0), 100.0);
    eprintln!("fill {fill:.1}, top edge {top:.1}, side edge {side:.1}");
    // Thick glass over black: the fill 58 % white (148), the top row some levels lighter, the
    // sides no lighter than the fill.
    assert!(
        top > fill + 15.0,
        "no light on the turned top edge: {top:.1} over {fill:.1}"
    );
    assert!(
        side < fill + 2.0,
        "light on the turned side edge: {side:.1} over {fill:.1}"
    );
}

#[test]
fn a_continuous_region_is_listed_and_clips_its_corners() {
    common::client::GLASS_VERSION.with(|v| v.set(9));
    let compositor = start_at_32("glass-continuous", SHOT, &["--background", "7a7b80"]);
    let size = (320, 320);
    let mut glass = TestWindow::open_pixels(&compositor.wayland, "icon", size, &transparent(size));
    // The body of an app icon: 200 pt, radius 214/824, smoothing 0.6.
    glass
        .set_glass_with(|g| {
            g.add_continuous_region(60, 60, 200, 200, 52.0, 0.6, Material::Regular, 1.0);
        })
        .unwrap();
    let list = glass_list(&compositor);
    let region = &list[0]["regions"][0];
    assert_eq!(region["radius"], 52.0, "{list}");
    assert!(
        (region["smoothing"].as_f64().unwrap() - 0.6).abs() < 0.01,
        "{list}"
    );
    let image = shot(&compositor, "glass-continuous.png");
    let lift = |x: u32, y: u32| {
        let [r, g, b] = rgb(&image, 32 + x, 32 + y);
        (f64::from(r) + f64::from(g) + f64::from(b)) / 3.0 - f64::from(0x7b)
    };
    let fill = lift(160, 160);
    assert!(fill.abs() > 5.0, "no glass in the middle: {fill:.1}");
    // The corner point is outside; so is a point on the diagonal 12 px in, where a square
    // corner would be glass; 30 px along the diagonal is inside.
    assert!(
        lift(61, 61).abs() < 0.2 * fill.abs(),
        "glass at the corner point"
    );
    assert!(
        lift(75, 75).abs() > 0.6 * fill.abs(),
        "no glass inside the corner"
    );
}
