//! Dark appearance of windows (`--appearance dark`): title bar (Amalfi `Surface/Window`),
//! buttons (measured on macOS 27, `docs/spec/windows.md`), the 1 pt rim (`Line/Border`) and the
//! shadow (`shadows.3`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::client::TestWindow;
use common::{Headless, pixel_in};
use serde_json::{Value, json};
use silica_control::commands::{INPUT_POINTER_MOVE, WINDOW_LIST, WINDOW_MOVE, WindowInfo};
use silica_protocols::window::client::silica_window_v1::Appearance;

const SIZE: (u32, u32) = (640, 400);
const BAR: i32 = 32;
const GRAY: &str = "7a7b80";
/// `Dark/Surface/Window`.
const DARK_BAR: [u8; 3] = [0x1b, 0x1c, 0x1f];

/// `Dark/Line/Border` (`#F4F5F729`) over `under`.
fn dark_rim_over(under: [u8; 3]) -> [u8; 3] {
    let alpha = 0x29_u8 as f32 / 255.0;
    let rim: [u8; 3] = [0xf4, 0xf5, 0xf7];
    std::array::from_fn(|i| {
        (f32::from(under[i]) * (1.0 - alpha) + f32::from(rim[i]) * alpha).round() as u8
    })
}

fn start(name: &str, appearance: &str) -> Headless {
    Headless::start_with(
        name,
        SIZE,
        &["--background", GRAY, "--appearance", appearance],
    )
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

fn shot(compositor: &Headless, name: &str) -> Vec<u8> {
    let shot = common::screenshot(compositor);
    if let Some(dir) = std::env::var_os("SILICA_COMPOSITOR_SHOTS") {
        std::fs::copy(
            compositor.dir.join("shot.png"),
            std::path::Path::new(&dir).join(format!("{name}.png")),
        )
        .unwrap();
    }
    shot
}

fn rgb(shot: &[u8], (x, y): (i32, i32)) -> [u8; 3] {
    let p = pixel_in(shot, SIZE.0, x as u32, y as u32);
    [p[0], p[1], p[2]]
}

/// The pixel is within `tolerance` of the expected color, channel by channel.
fn assert_near(shot: &[u8], at: (i32, i32), expected: [u8; 3], tolerance: u8, what: &str) {
    let got = rgb(shot, at);
    let close = got
        .iter()
        .zip(expected)
        .all(|(g, e)| g.abs_diff(e) <= tolerance);
    assert!(close, "{what} at {at:?}: {got:?}, expected {expected:?}");
}

/// Center of a bar button (0 close, 1 minimize, 2 full screen).
fn button(window: &WindowInfo, index: i32) -> (i32, i32) {
    (window.x + 16 + 23 * index, window.y - BAR + 16)
}

#[test]
fn dark_windows_use_the_measured_bar_buttons_and_inner_border() {
    let compositor = start("dark-active", "dark");
    let _window = TestWindow::open_decorated(&compositor.wayland, "dark", (240, 140));
    let info = windows(&compositor).remove(0);
    let shot = shot(&compositor, "appearance-dark");

    // Active bar: `Dark/Surface/Window`.
    assert_near(
        &shot,
        (info.x + 150, info.y - 16),
        DARK_BAR,
        1,
        "background",
    );
    // Dark active buttons at the center: `#f26056`, `#fdc002`, `#39c101`. The center row
    // lies between the darker band (30–45 %) and the glow (75–80 %), hence the tolerance.
    assert_near(&shot, button(&info, 0), [0xf2, 0x60, 0x56], 32, "close");
    assert_near(&shot, button(&info, 1), [0xfd, 0xc0, 0x02], 32, "minimize");
    assert_near(
        &shot,
        button(&info, 2),
        [0x39, 0xc1, 0x01],
        32,
        "full screen",
    );
    // 1 pt rim `Line/Border`: above the bar, on the left and at the bottom on the content.
    assert_near(
        &shot,
        (info.x + 150, info.y - BAR),
        dark_rim_over(DARK_BAR),
        2,
        "top border",
    );
    assert_near(
        &shot,
        (info.x, info.y + 70),
        dark_rim_over([255, 0, 0]),
        2,
        "left border",
    );
    assert_near(
        &shot,
        (info.x + 120, info.y + 139),
        dark_rim_over([255, 0, 0]),
        2,
        "bottom border",
    );
    // Right inside the content the client's color stays.
    assert_near(&shot, (info.x + 1, info.y + 70), [255, 0, 0], 0, "content");
}

#[test]
fn inactive_dark_windows_have_their_own_bar_and_gray_buttons() {
    let compositor = start("dark-inactive", "dark");
    let _first = TestWindow::open_decorated(&compositor.wayland, "first", (240, 140));
    let first = windows(&compositor).remove(0);
    let _second = TestWindow::open_decorated(&compositor.wayland, "second", (120, 80));
    let second = windows(&compositor).remove(0);
    let mut client = compositor.client();
    client
        .call(WINDOW_MOVE, json!({"id": second.id, "x": 450, "y": 250}))
        .unwrap();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": 5, "y": 395}))
        .unwrap();
    let shot = shot(&compositor, "appearance-dark-inactive");

    // Inactive bar: the same `Surface/Window`, and no separator row above the content.
    assert_near(
        &shot,
        (first.x + 150, first.y - 16),
        DARK_BAR,
        1,
        "background",
    );
    assert_near(
        &shot,
        (first.x + 150, first.y - 2),
        DARK_BAR,
        1,
        "no separator",
    );
    // Inactive buttons: center `#4d4d4d`.
    for index in 0..3 {
        assert_near(
            &shot,
            button(&first, index),
            [0x4d; 3],
            4,
            "inactive button",
        );
    }
    // The rim is the same when inactive.
    assert_near(
        &shot,
        (first.x + 150, first.y - BAR),
        dark_rim_over(DARK_BAR),
        2,
        "top border",
    );
    assert_near(
        &shot,
        (first.x, first.y + 70),
        dark_rim_over([255, 0, 0]),
        2,
        "left border",
    );
}

#[test]
fn the_dark_shadow_is_stronger_than_the_light_one() {
    let mut below = Vec::new();
    for appearance in ["light", "dark"] {
        let compositor = start(&format!("shadow-{appearance}"), appearance);
        // A first window moves the second one off the screen edge (cascade), so the
        // shadow beside it is on screen.
        let _first = TestWindow::open_decorated(&compositor.wayland, "first", (40, 40));
        let _window = TestWindow::open_decorated(&compositor.wayland, "shadow", (240, 140));
        let info = windows(&compositor)
            .into_iter()
            .max_by_key(|w| w.x)
            .expect("the second window");
        let shot = shot(&compositor, &format!("appearance-shadow-{appearance}"));
        // Below the window (shadow shifted down) and on the side, over gray.
        let under = rgb(&shot, (info.x + 120, info.y + 140 + 8))[1];
        let side = rgb(&shot, (info.x - 6, info.y + 70))[1];
        // Next to the rim: shadows.3 has no contact ring, only the soft Gaussian.
        let edge = rgb(&shot, (info.x - 2, info.y + 70))[1];
        below.push((under, side, edge));
    }
    let (light, dark) = (below[0], below[1]);
    assert!(
        dark.0 + 10 < light.0,
        "below: light {light:?}, dark {dark:?}"
    );
    assert!(
        dark.1 + 5 < light.1,
        "beside: light {light:?}, dark {dark:?}"
    );
    assert!(dark.2 + 8 < light.2, "edge: light {light:?}, dark {dark:?}");
}

#[test]
fn appearance_flag_is_validated() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_silica-compositor"))
        .args(["--appearance", "sepia"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// A window that overrides the appearance (`silica-window-v1` `set_appearance`) in a light
/// session draws the dark bar, buttons and inner border, as in a dark session.
#[test]
fn a_dark_window_in_a_light_session_draws_the_dark_traffic_lights_and_bar() {
    let compositor = start("override-dark", "light");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "dark", (240, 140));
    window.set_appearance(Appearance::Dark);
    window.commit_frames(1, std::time::Duration::ZERO);
    let info = windows(&compositor).remove(0);
    let shot = shot(&compositor, "appearance-override-dark");

    assert_near(&shot, (info.x + 150, info.y - 16), DARK_BAR, 1, "dark bar");
    assert_near(
        &shot,
        button(&info, 0),
        [0xf2, 0x60, 0x56],
        32,
        "dark close",
    );
    assert_near(
        &shot,
        button(&info, 2),
        [0x39, 0xc1, 0x01],
        32,
        "dark full screen",
    );
    assert_near(
        &shot,
        (info.x + 150, info.y - BAR),
        dark_rim_over(DARK_BAR),
        2,
        "dark top border",
    );
}

/// The override works both ways, and `system` gives the session's appearance back.
#[test]
fn a_light_window_in_a_dark_session_stays_light_and_system_follows_the_session() {
    let compositor = start("override-light", "dark");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "light", (240, 140));
    window.set_appearance(Appearance::Light);
    window.commit_frames(1, std::time::Duration::ZERO);
    let info = windows(&compositor).remove(0);
    let image = shot(&compositor, "appearance-override-light");
    assert_near(
        &image,
        (info.x + 150, info.y - 16),
        [255, 255, 255],
        1,
        "light bar",
    );
    assert_near(
        &image,
        button(&info, 0),
        [0xf8, 0x7d, 0x77],
        12,
        "light close",
    );

    window.set_appearance(Appearance::System);
    window.commit_frames(1, std::time::Duration::ZERO);
    let image = shot(&compositor, "appearance-override-system");
    assert_near(
        &image,
        (info.x + 150, info.y - 16),
        DARK_BAR,
        1,
        "the dark session's bar again",
    );
}

/// Two windows in one session: only the one that overrides is dark.
#[test]
fn the_override_is_per_window() {
    let compositor = start("override-per-window", "light");
    let mut dark = TestWindow::open_decorated(&compositor.wayland, "dark", (240, 140));
    dark.set_appearance(Appearance::Dark);
    let dark_info = windows(&compositor).remove(0);
    let _plain = TestWindow::open_decorated(&compositor.wayland, "plain", (120, 80));
    let plain = windows(&compositor).remove(0);
    let mut client = compositor.client();
    client
        .call(WINDOW_MOVE, json!({"id": plain.id, "x": 450, "y": 250}))
        .unwrap();
    let image = shot(&compositor, "appearance-override-two");
    assert_near(
        &image,
        (dark_info.x + 150, dark_info.y - 16),
        // Inactive here (the other window was opened after it): the same dark bar.
        DARK_BAR,
        1,
        "the overriding window",
    );
    assert_near(&image, (455, 250 - 16), [255, 255, 255], 8, "the other one");
}
