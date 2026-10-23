//! Panorama (M5.2a) on the headless compositor: windows arranged without overlapping, spaces bar at
//! the top, click on a window, Esc (`plan/reference/missioncontrol.md`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::client::TestWindow;
use common::{Headless, pixel, pixel_in, screenshot};
use serde_json::{Value, json};
use silica_control::commands::{
    INPUT_CLICK, INPUT_KEY, INPUT_POINTER_BUTTON, INPUT_POINTER_MOVE, PANORAMA_SPACES_BAR,
    PANORAMA_STATE, PANORAMA_TOGGLE, PanoramaState, SPACE_ADD, SPACE_LIST, SpaceInfo, WINDOW_LIST,
    WindowInfo,
};

fn solid(size: (u32, u32), rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat((size.0 * size.1) as usize)
}

fn state(compositor: &Headless) -> PanoramaState {
    serde_json::from_value(
        compositor
            .client()
            .call(PANORAMA_STATE, Value::Null)
            .unwrap(),
    )
    .unwrap()
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).take(3).all(|(x, y)| x.abs_diff(y) < 12)
}

// In the test buffer the channels are BGRA.
const RED: [u8; 4] = [0, 0, 255, 255];
const BLUE: [u8; 4] = [255, 0, 0, 255];

#[test]
fn windows_spread_out_and_a_click_picks_one() {
    // On the reference gray: the bar is Glass/Thick over it.
    let compositor = Headless::start_with(
        "panorama",
        (common::WIDTH, common::HEIGHT),
        &["--background", "7a7b80"],
    );
    let _red = TestWindow::open_pixels(
        &compositor.wayland,
        "red",
        (100, 60),
        &solid((100, 60), RED),
    );
    let _blue = TestWindow::open_pixels(
        &compositor.wayland,
        "blue",
        (100, 60),
        &solid((100, 60), BLUE),
    );
    let mut c = compositor.client();
    assert!(!state(&compositor).open);

    assert_eq!(
        c.call(PANORAMA_TOGGLE, Value::Null).unwrap(),
        json!({"open": true})
    );
    let s = state(&compositor);
    assert!(s.open);
    assert_eq!(s.progress, 1.0);
    assert_eq!(s.windows.len(), 2);
    // Both fit at full size, 16 pt apart, below the bar.
    let (a, b) = (&s.windows[0], &s.windows[1]);
    let (left, right) = if a.x < b.x { (a, b) } else { (b, a) };
    assert_eq!(right.x - (left.x + left.width), 16.0);
    assert!(left.y >= 40.0 && right.y >= 40.0);

    let shot = screenshot(&compositor);
    // The spaces bar: Glass/Thick (light tint over the gray) at the top left, no window.
    assert!(
        close(pixel(&shot, 4, 4), [0xcd, 0xce, 0xd0, 255]),
        "{:?}",
        pixel(&shot, 4, 4)
    );
    let list = windows(&compositor);
    let red_id = list
        .iter()
        .find(|w| w.title.as_deref() == Some("red"))
        .unwrap()
        .id;
    let blue_id = list
        .iter()
        .find(|w| w.title.as_deref() == Some("blue"))
        .unwrap()
        .id;
    for (id, rgb) in [(red_id, [255, 0, 0, 255]), (blue_id, [0, 0, 255, 255])] {
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        let (x, y) = (w.x + w.width / 2.0, w.y + w.height / 2.0);
        assert!(
            close(pixel(&shot, x as u32, y as u32), rgb),
            "window {id} at {x},{y}: {:?}",
            pixel(&shot, x as u32, y as u32)
        );
    }

    // A click on the red one brings it to the front and closes Panorama.
    let red = s.windows.iter().find(|w| w.id == red_id).unwrap();
    c.call(
        INPUT_CLICK,
        json!({"x": red.x + red.width / 2.0, "y": red.y + red.height / 2.0}),
    )
    .unwrap();
    assert!(!state(&compositor).open);
    let list = windows(&compositor);
    assert!(list.iter().find(|w| w.id == red_id).unwrap().focused);

    // Esc closes; the "+" adds a space. Going there expands the bar and the "+" comes down
    // with it (center at y 98).
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    c.call(INPUT_POINTER_MOVE, json!({"x": 320 - 26, "y": 20}))
        .unwrap();
    c.call(INPUT_CLICK, json!({"x": 320 - 26, "y": 101}))
        .unwrap();
    let spaces: Vec<SpaceInfo> =
        serde_json::from_value(c.call(SPACE_LIST, Value::Null).unwrap()).unwrap();
    assert_eq!(spaces.len(), 2);
    assert!(state(&compositor).open);
    c.call(INPUT_KEY, json!({"key": "Escape"})).unwrap();
    assert!(!state(&compositor).open);
    // Once closed, the menu bar and the windows go back to their place.
    let shot = screenshot(&compositor);
    assert!(!close(pixel(&shot, 4, 4), [0xcd, 0xce, 0xd0, 255]));
}

/// A window that maps while Panorama is open is arranged with the others, below the bar: left
/// at its real place it would be drawn under the spaces bar (silica-os/silica-desktop#246).
#[test]
fn a_window_that_maps_while_panorama_is_open_is_arranged_too() {
    let compositor = Headless::start_with("panorama-late", (1280, 800), &[]);
    let _first = TestWindow::open_pixels(
        &compositor.wayland,
        "first",
        (300, 200),
        &solid((300, 200), RED),
    );
    compositor
        .client()
        .call(PANORAMA_TOGGLE, Value::Null)
        .unwrap();
    assert_eq!(state(&compositor).windows.len(), 1);
    // A window right under the bar, opened now.
    let _late = TestWindow::open_pixels(
        &compositor.wayland,
        "late",
        (400, 300),
        &solid((400, 300), BLUE),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let s = loop {
        let s = state(&compositor);
        if s.windows.len() == 2 && s.windows.iter().all(|w| w.width > 0.0) {
            break s;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "not arranged: {:?}",
            s.windows
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    // Both in the usable area, below the expanded bar's top (y 80), not overlapping.
    for w in &s.windows {
        assert!(w.y >= 80.0, "{w:?}");
    }
    let (a, b) = (&s.windows[0], &s.windows[1]);
    let apart = a.x + a.width <= b.x
        || b.x + b.width <= a.x
        || a.y + a.height <= b.y
        || b.y + b.height <= a.y;
    assert!(apart, "{a:?} {b:?}");
}

fn spaces(compositor: &Headless) -> Vec<SpaceInfo> {
    serde_json::from_value(compositor.client().call(SPACE_LIST, Value::Null).unwrap()).unwrap()
}

#[test]
fn a_window_dragged_onto_a_thumbnail_changes_space() {
    // A screen large enough to have the windows below the expanded bar (170 pt).
    let compositor = Headless::start_with("panorama-drag", (1280, 800), &[]);
    let size = (300, 300);
    let _red = TestWindow::open_pixels(&compositor.wayland, "red", size, &solid(size, RED));
    let _blue = TestWindow::open_pixels(&compositor.wayland, "blue", size, &solid(size, BLUE));
    let mut c = compositor.client();
    c.call(SPACE_ADD, Value::Null).unwrap();
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    // The pointer at the top expands the bar: the thumbnails appear.
    c.call(INPUT_POINTER_MOVE, json!({"x": 100, "y": 5}))
        .unwrap();
    let s = state(&compositor);
    assert_eq!(s.expansion, 1.0);
    assert_eq!(s.thumbnails.len(), 2);
    // Once expanded it stays so, also with the pointer down (missioncontrol.md › PA-14).
    c.call(INPUT_POINTER_MOVE, json!({"x": 100, "y": 799}))
        .unwrap();
    assert_eq!(state(&compositor).expansion, 1.0);
    let s = state(&compositor);
    assert_eq!(s.expansion, 1.0);
    let shot = screenshot(&compositor);
    // Blue ring around the thumbnail of the current space (1 pt of bar, then 3 pt).
    let t = s.thumbnails[0];
    let ring = pixel_in(
        &shot,
        1280,
        (t.x + t.width / 2.0) as u32,
        (t.y - 2.5) as u32,
    );
    assert!(close(ring, [0x3b, 0x99, 0xfd, 255]), "{ring:?}");

    let blue_id = windows(&compositor)
        .iter()
        .find(|w| w.title.as_deref() == Some("blue"))
        .unwrap()
        .id;
    let blue = *s.windows.iter().find(|w| w.id == blue_id).unwrap();
    let target = s.thumbnails[1];
    c.call(
        INPUT_POINTER_MOVE,
        json!({"x": blue.x + blue.width / 2.0, "y": blue.y + blue.height / 2.0}),
    )
    .unwrap();
    c.call(INPUT_POINTER_BUTTON, json!({"pressed": true}))
        .unwrap();
    // Upwards: once it reaches the compact bar the bar expands.
    c.call(INPUT_POINTER_MOVE, json!({"x": target.x, "y": 10}))
        .unwrap();
    c.call(
        INPUT_POINTER_MOVE,
        json!({"x": target.x + target.width / 2.0, "y": target.y + target.height / 2.0}),
    )
    .unwrap();
    c.call(INPUT_POINTER_BUTTON, json!({"pressed": false}))
        .unwrap();
    // The blue one moved to space 2; Panorama stays open with only the red one.
    assert_eq!(spaces(&compositor)[1].windows, [blue_id]);
    let s = state(&compositor);
    assert!(s.open);
    assert_eq!(s.windows.len(), 1);

    // A click on the thumbnail of space 2 goes there and closes Panorama.
    c.call(
        INPUT_CLICK,
        json!({"x": target.x + target.width / 2.0, "y": target.y + target.height / 2.0}),
    )
    .unwrap();
    assert!(!state(&compositor).open);
    assert!(spaces(&compositor)[1].active);
}

#[test]
fn app_windows_and_the_desktop() {
    let compositor = Headless::start_with("panorama-app", (1280, 800), &[]);
    let size = (300, 200);
    let _red = TestWindow::open_pixels(&compositor.wayland, "red", size, &solid(size, RED));
    let _blue = TestWindow::open_pixels(&compositor.wayland, "blue", size, &solid(size, BLUE));
    let mut c = compositor.client();
    let list = windows(&compositor);
    let focused = list.iter().find(|w| w.focused).unwrap().clone();
    let other = list.iter().find(|w| !w.focused).unwrap().clone();

    // App Exposé (⌃↓): only the window of the frontmost app (the test windows have no
    // app_id), centered, scaled at most 0.947; the other one is not visible.
    c.call(INPUT_KEY, json!({"key": "Control_L", "pressed": true}))
        .unwrap();
    c.call(INPUT_KEY, json!({"key": "Down"})).unwrap();
    c.call(INPUT_KEY, json!({"key": "Control_L", "pressed": false}))
        .unwrap();
    let s = state(&compositor);
    assert_eq!(s.mode.as_deref(), Some("app"));
    assert_eq!(s.windows.len(), 1);
    let w = s.windows[0];
    assert_eq!(w.id, focused.id);
    assert!((w.x + w.width / 2.0 - 640.0).abs() < 1.0, "{w:?}");
    let shot = screenshot(&compositor);
    let at = |w: &WindowInfo| ((w.x + w.width / 2) as u32, (w.y + w.height / 2) as u32);
    let hidden = pixel_in(&shot, 1280, at(&other).0, at(&other).1);
    assert!(!close(hidden, [255, 0, 0, 255]) && !close(hidden, [0, 0, 255, 255]));
    // Esc closes.
    c.call(INPUT_KEY, json!({"key": "Escape"})).unwrap();
    assert!(state(&compositor).mode.is_none());

    // Show Desktop: the windows move almost entirely out; the center of the screen is free.
    assert_eq!(
        c.call(silica_control::commands::DESKTOP_SHOW, Value::Null)
            .unwrap(),
        json!({"open": true})
    );
    let s = state(&compositor);
    assert_eq!(s.mode.as_deref(), Some("desktop"));
    for w in &s.windows {
        let inside_x = (w.x + w.width).min(1280.0) - w.x.max(0.0);
        let inside_y = (w.y + w.height).min(800.0) - w.y.max(0.0);
        assert!(inside_x <= 24.0 + 1e-6 || inside_y <= 24.0 + 1e-6, "{w:?}");
    }
    // A click puts them back.
    c.call(INPUT_CLICK, json!({"x": 640, "y": 400})).unwrap();
    assert!(state(&compositor).mode.is_none());
}

#[test]
fn trackpad_gestures_run_their_commands() {
    let compositor = Headless::start("panorama-gestures");
    let size = (100, 60);
    let _red = TestWindow::open_pixels(&compositor.wayland, "red", size, &solid(size, RED));
    let mut c = compositor.client();
    c.call(SPACE_ADD, Value::Null).unwrap();
    let gesture = |c: &mut silica_control::Client, params: Value| {
        c.call(silica_control::commands::INPUT_GESTURE, params)
            .unwrap()["command"]
            .clone()
    };
    // Three fingers to the left, in pieces: the space on the right.
    let begin = json!({"kind": "swipe", "fingers": 3, "phase": "begin"});
    assert_eq!(gesture(&mut c, begin), Value::Null);
    for _ in 0..4 {
        gesture(
            &mut c,
            json!({"kind": "swipe", "fingers": 3, "phase": "update", "dx": -40}),
        );
    }
    let end = json!({"kind": "swipe", "fingers": 3, "phase": "end"});
    assert_eq!(gesture(&mut c, end), json!("space.next"));
    assert!(spaces(&compositor)[1].active);
    // Up Panorama, down it closes; too short does nothing.
    let up = json!({"kind": "swipe", "fingers": 3, "dy": -150});
    assert_eq!(gesture(&mut c, up), json!("panorama.toggle"));
    assert_eq!(state(&compositor).mode.as_deref(), Some("panorama"));
    let down = json!({"kind": "swipe", "fingers": 4, "dy": 150});
    assert_eq!(gesture(&mut c, down), json!("panorama.toggle"));
    assert!(state(&compositor).mode.is_none());
    let short = json!({"kind": "swipe", "fingers": 3, "dx": 20});
    assert_eq!(gesture(&mut c, short), Value::Null);
    // Thumb and three fingers spreading: the desktop; a cancelled gesture does not count.
    let cancelled = json!({"kind": "pinch", "fingers": 4, "scale": 2.0, "cancelled": true});
    assert_eq!(gesture(&mut c, cancelled), Value::Null);
    let spread = json!({"kind": "pinch", "fingers": 4, "scale": 2.0});
    assert_eq!(gesture(&mut c, spread), json!("desktop.show"));
    assert_eq!(state(&compositor).mode.as_deref(), Some("desktop"));
}

use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;

const GREEN: [u8; 4] = [0, 255, 0, 255];

/// A desktop wallpaper (background layer) painted by `paint`.
fn wallpaper(compositor: &Headless, paint: impl Fn(u32, u32, u32, u32) -> [u8; 4]) -> TestWindow {
    TestWindow::open_layer(
        &compositor.wayland,
        Layer::Background,
        "wallpaper",
        None,
        paint,
    )
}

fn rgb_at(shot: &[u8], x: f64, y: f64) -> [u8; 3] {
    let [r, g, b, _] = pixel_in(shot, 1280, x as u32, y as u32);
    [r, g, b]
}

#[test]
fn the_wallpaper_of_a_space_fits_its_thumbnail() {
    let compositor = Headless::start_with("panorama-thumb-wallpaper", (1280, 800), &[]);
    // Left half red, right half blue.
    let _wallpaper = wallpaper(&compositor, |x, _, w, _| if x < w / 2 { RED } else { BLUE });
    let mut c = compositor.client();
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    c.call(PANORAMA_SPACES_BAR, json!({"enabled": true}))
        .unwrap();
    let s = state(&compositor);
    assert_eq!(s.expansion, 1.0);
    let t = s.thumbnails[0];
    let shot = screenshot(&compositor);
    // The whole wallpaper, shrunk: red on the left of the thumbnail, blue on the right.
    let middle = t.y + t.height / 2.0;
    let left = rgb_at(&shot, t.x + t.width * 0.25, middle);
    let right = rgb_at(&shot, t.x + t.width * 0.75, middle);
    assert!(
        left[0] > 200 && left[2] < 60,
        "left of the thumbnail: {left:?}"
    );
    assert!(
        right[2] > 200 && right[0] < 60,
        "right of the thumbnail: {right:?}"
    );
    // Around it the bar (a tinted blur), not the wallpaper at full size.
    for (x, y) in [
        (t.x + t.width + 40.0, middle),
        (t.x - 40.0, middle),
        (t.x + t.width / 2.0, 10.0),
    ] {
        let [r, g, b] = rgb_at(&shot, x, y);
        let pure_red = r > 200 && g < 60 && b < 60;
        let pure_blue = b > 200 && r < 60 && g < 60;
        assert!(
            !pure_red && !pure_blue,
            "bar at ({x}, {y}): {:?}",
            [r, g, b]
        );
    }
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
}

#[test]
fn the_wallpaper_stays_as_at_rest_and_the_desktop_hides() {
    let compositor = Headless::start_with("panorama-backdrop", (1280, 800), &[]);
    // Black and white stripes 48 px wide.
    let _wallpaper = wallpaper(&compositor, |x, _, _, _| {
        if x % 96 < 48 {
            [0, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        }
    });
    // A desktop icon: a green square in the middle of the desktop layer.
    let _icons = TestWindow::open_layer(
        &compositor.wayland,
        Layer::Bottom,
        "desktop-icons:test",
        None,
        |x, y, w, h| {
            if x.abs_diff(w / 2) < 50 && y.abs_diff(h / 2) < 50 {
                GREEN
            } else {
                [0; 4]
            }
        },
    );
    let mut c = compositor.client();
    c.call(INPUT_POINTER_MOVE, json!({"x": 640, "y": 799}))
        .unwrap();
    let row = |shot: &[u8]| -> Vec<u8> {
        (100..300)
            .map(|x| rgb_at(shot, f64::from(x), 600.0)[0])
            .collect()
    };
    let icon = |shot: &[u8]| rgb_at(shot, 640.0, 400.0);
    let shot = screenshot(&compositor);
    assert!(row(&shot).iter().any(|&v| v < 10) && row(&shot).iter().any(|&v| v > 245));
    assert_eq!(icon(&shot), [0, 255, 0]);

    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    assert_eq!(state(&compositor).progress, 1.0);
    let shot = screenshot(&compositor);
    // The wallpaper behind the windows is untouched: no blur, dimming or tint (MC-BLUR).
    let open = row(&shot);
    assert!(
        open.contains(&0) && open.contains(&255),
        "the wallpaper changed: {open:?}"
    );
    // The desktop icons are gone: the wallpaper shows in their place.
    let [r, g, b] = icon(&shot);
    assert!(
        g.abs_diff(r) < 12 && g.abs_diff(b) < 12,
        "desktop icon still visible: {:?}",
        [r, g, b]
    );

    // Closed: sharp wallpaper and icons again.
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    assert!(!state(&compositor).open);
    let shot = screenshot(&compositor);
    assert!(row(&shot).iter().any(|&v| v < 10) && row(&shot).iter().any(|&v| v > 245));
    assert_eq!(icon(&shot), [0, 255, 0]);
}

/// A solid `#7a7b80` wallpaper, the gray of the reference sessions.
fn reference_gray(compositor: &Headless) -> TestWindow {
    // BGRA.
    wallpaper(compositor, |_, _, _, _| [0x80, 0x7b, 0x7a, 255])
}

fn near(got: [u8; 3], want: [u8; 3], tolerance: u8) -> bool {
    got.iter()
        .zip(want)
        .all(|(a, b)| a.abs_diff(b) <= tolerance)
}

/// What Glass/Regular shows over a flat `gray` (0-255, encoded): the tint over the saturated
/// gray, the way the glass pass composes it.
fn regular_over(dark: bool, gray: [f32; 3]) -> [u8; 3] {
    let profile = compositor_render::glass_profile::GlassKind::Panel.profile(dark);
    let [r, g, b] = compositor_render::glass::profile_color(&profile, gray.map(|c| c / 255.0));
    [r, g, b].map(|c| (c * 255.0).round() as u8)
}

#[test]
fn the_spaces_bar_is_glass_regular_over_the_wallpaper() {
    let gray = compositor_render::glass::REFERENCE_GRAY.map(|c| c * 255.0);
    for dark in [false, true] {
        let want = regular_over(dark, gray);
        // Light: ~#c8c8cc over the reference gray; dark: much darker.
        assert!(if dark { want[0] < 80 } else { want[0] > 190 }, "{want:?}");
        let name = if dark {
            "panorama-bar-dark"
        } else {
            "panorama-bar-light"
        };
        let extra: &[&str] = if dark { &["--appearance", "dark"] } else { &[] };
        let compositor = Headless::start_with(name, (1280, 800), extra);
        let _wallpaper = reference_gray(&compositor);
        let mut c = compositor.client();
        c.call(INPUT_POINTER_MOVE, json!({"x": 640, "y": 700}))
            .unwrap();
        c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
        let shot = screenshot(&compositor);
        let got = rgb_at(&shot, 100.0, 20.0);
        assert!(
            near(got, want, 3),
            "dark {dark}, compact bar: {got:?} vs {want:?}"
        );
        // The wallpaper below is not dimmed (only the bar's shadow darkens its top edge).
        assert_eq!(rgb_at(&shot, 100.0, 300.0), [0x7a, 0x7b, 0x80]);
        c.call(PANORAMA_SPACES_BAR, json!({"enabled": true}))
            .unwrap();
        assert_eq!(state(&compositor).expansion, 1.0);
        let shot = screenshot(&compositor);
        let low = rgb_at(&shot, 100.0, 100.0);
        assert!(
            near(low, want, 3),
            "dark {dark}, expanded: {low:?} vs {want:?}"
        );
        c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    }
}

#[test]
fn the_spaces_bar_blurs_the_wallpaper_under_it_with_sigma_10() {
    let compositor =
        Headless::start_with("panorama-bar-blur", (1280, 800), &["--appearance", "dark"]);
    // Left half: black and white stripes 8 px wide; right half: black above y 20, white below.
    let _wallpaper = wallpaper(&compositor, |x, y, w, _| {
        let white = if x < w / 2 { x % 16 >= 8 } else { y >= 20 };
        if white {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 255]
        }
    });
    let mut c = compositor.client();
    c.call(INPUT_POINTER_MOVE, json!({"x": 640, "y": 700}))
        .unwrap();
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    let shot = screenshot(&compositor);
    // The stripes (period 16 px) are wiped out by σ 10: the mean gray under the tint.
    let mean = regular_over(true, [127.5; 3]);
    for x in (100..500).step_by(7) {
        let v = rgb_at(&shot, f64::from(x), 20.0);
        assert!(near(v, mean, 4), "stripes at {x}: {v:?} vs {mean:?}");
    }
    // The step at y 20: the blur of a wide step is a ramp from mostly black at the top of the
    // bar to mostly white at its bottom (the glass reads past the bar's edges).
    let top = rgb_at(&shot, 960.0, 3.0);
    let bottom = rgb_at(&shot, 960.0, 36.0);
    assert!(
        top[0] < mean[0] && bottom[0] > mean[0],
        "{top:?} {bottom:?}"
    );
    assert!(bottom[0] - top[0] > 25, "{top:?} {bottom:?}");
    // Below the bar the wallpaper is as at rest (after the shadow).
    assert_eq!(rgb_at(&shot, 960.0, 300.0), [255, 255, 255]);
}

#[test]
fn the_row_recenters_on_expansion_with_its_labels_inside_the_bar_and_the_border_below() {
    let compositor = Headless::start_with("panorama-row", (1280, 800), &[]);
    let _wallpaper = reference_gray(&compositor);
    let mut c = compositor.client();
    c.call(INPUT_POINTER_MOVE, json!({"x": 640, "y": 700}))
        .unwrap();
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    // Compact bar: the shadow starts right below it (no border row).
    let shot = screenshot(&compositor);
    assert!(
        near(rgb_at(&shot, 100.0, 40.0), [0x62, 0x63, 0x67], 3),
        "{:?}",
        rgb_at(&shot, 100.0, 40.0)
    );
    assert!(near(rgb_at(&shot, 100.0, 110.0), [0x7a, 0x7b, 0x80], 2));

    // One space: its thumbnail in the middle, 90 pt tall centred on y 98 (16:10 → 144 wide).
    c.call(PANORAMA_SPACES_BAR, json!({"enabled": true}))
        .unwrap();
    let t = state(&compositor).thumbnails;
    assert_eq!(t.len(), 1);
    assert_eq!(
        (t[0].x + t[0].width / 2.0, t[0].y, t[0].height),
        (640.0, 53.0, 90.0)
    );
    let shot = screenshot(&compositor);
    // Expanded (168 pt, kit): shadow from #6a6b6f at y 168.
    assert!(
        near(rgb_at(&shot, 100.0, 168.0), [0x6a, 0x6b, 0x6f], 3),
        "{:?}",
        rgb_at(&shot, 100.0, 168.0)
    );
    // The label: dark ink under the thumbnail inside the bar (caps ~150–159), none below it.
    let ink = |shot: &[u8], rows: std::ops::Range<u32>| {
        rows.flat_map(|y| (560..720).map(move |x| (x, y)))
            .filter(|&(x, y)| rgb_at(shot, f64::from(x), f64::from(y))[0] < 60)
            .count()
    };
    assert!(ink(&shot, 146..166) > 20, "no label under the thumbnail");
    assert_eq!(ink(&shot, 166..190), 0, "the label spills out of the bar");

    // A space added: the row recenters at once (user decision, issue #246)...
    c.call(SPACE_ADD, Value::Null).unwrap();
    let t = state(&compositor).thumbnails;
    assert_eq!(t.len(), 2);
    assert_eq!((t[0].x, t[1].x), (480.0, 656.0));
    // ...and it stays centered when the bar expands again.
    c.call(PANORAMA_SPACES_BAR, json!({"enabled": false}))
        .unwrap();
    c.call(PANORAMA_SPACES_BAR, json!({"enabled": true}))
        .unwrap();
    let t = state(&compositor).thumbnails;
    assert_eq!((t[0].x, t[1].x), (480.0, 656.0));
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
}

fn wait_for(compositor: &Headless, what: impl Fn(&PanoramaState) -> bool) -> PanoramaState {
    for _ in 0..1000 {
        let s = state(compositor);
        if what(&s) {
            return s;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("Panorama did not get there: {:?}", state(compositor));
}

#[test]
fn a_space_closes_from_the_button_on_its_thumbnail() {
    // Real durations: the button waits ~1 s.
    let compositor = Headless::start_with(
        "panorama-close-space",
        (1280, 800),
        &["--animation-scale", "1"],
    );
    let _wallpaper = reference_gray(&compositor);
    let size = (200, 150);
    let _red = TestWindow::open_pixels(&compositor.wayland, "red", size, &solid(size, RED));
    let mut c = compositor.client();
    c.call(SPACE_ADD, Value::Null).unwrap();
    c.call(SPACE_ADD, Value::Null).unwrap();
    let red_id = windows(&compositor)[0].id;
    c.call(
        silica_control::commands::WINDOW_MOVE_TO_SPACE,
        json!({"id": red_id, "index": 1}),
    )
    .unwrap();
    c.call(INPUT_POINTER_MOVE, json!({"x": 640, "y": 700}))
        .unwrap();
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    wait_for(&compositor, |s| s.progress >= 1.0);
    c.call(PANORAMA_SPACES_BAR, json!({"enabled": true}))
        .unwrap();
    let s = wait_for(&compositor, |s| s.expansion >= 1.0);
    let t = s.thumbnails[1];
    let white = |shot: &[u8]| near(rgb_at(shot, t.x + 6.0, t.y - 6.0), [255, 255, 255], 3);
    // On the thumbnail: no button yet...
    c.call(
        INPUT_POINTER_MOVE,
        json!({"x": t.x + t.width / 2.0, "y": t.y + t.height / 2.0}),
    )
    .unwrap();
    assert!(!white(&screenshot(&compositor)), "the button shows at once");
    // ...after ~1 s, a white disc on its top-left corner with a gray cross in the middle.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let shot = screenshot(&compositor);
    assert!(
        white(&shot),
        "no close button: {:?}",
        rgb_at(&shot, t.x + 6.0, t.y - 6.0)
    );
    let cross = rgb_at(&shot, t.x, t.y);
    assert!(cross[0] < 200 && cross[0] > 60, "cross: {cross:?}");
    // Press and release on it: the space goes, its window moves to the previous one.
    c.call(INPUT_POINTER_MOVE, json!({"x": t.x + 3.0, "y": t.y + 3.0}))
        .unwrap();
    c.call(INPUT_POINTER_BUTTON, json!({"pressed": true}))
        .unwrap();
    assert_eq!(spaces(&compositor).len(), 3, "removed on press");
    c.call(INPUT_POINTER_BUTTON, json!({"pressed": false}))
        .unwrap();
    let after = spaces(&compositor);
    assert_eq!(after.len(), 2);
    assert!(after[0].windows.contains(&red_id), "{after:?}");
    assert!(state(&compositor).open);
}

/// The bounding box of the red pixels.
fn red_box(shot: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..800 {
        for x in 0..1280 {
            let [r, g, b] = rgb_at(shot, f64::from(x), f64::from(y));
            if r > 200 && g < 60 && b < 60 {
                found = Some(match found {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    found
}

#[test]
fn a_click_on_a_thumbnail_magnifies_that_space() {
    // Slowed down x5 to catch the magnification.
    let compositor =
        Headless::start_with("panorama-zoom", (1280, 800), &["--animation-scale", "5"]);
    let _wallpaper = reference_gray(&compositor);
    let size = (400, 300);
    let _red = TestWindow::open_pixels(&compositor.wayland, "red", size, &solid(size, RED));
    let mut c = compositor.client();
    c.call(SPACE_ADD, Value::Null).unwrap();
    let red_id = windows(&compositor)[0].id;
    c.call(
        silica_control::commands::WINDOW_MOVE_TO_SPACE,
        json!({"id": red_id, "index": 1}),
    )
    .unwrap();
    c.call(INPUT_POINTER_MOVE, json!({"x": 640, "y": 700}))
        .unwrap();
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    wait_for(&compositor, |s| s.progress >= 1.0);
    c.call(PANORAMA_SPACES_BAR, json!({"enabled": true}))
        .unwrap();
    let s = wait_for(&compositor, |s| s.expansion >= 0.99);
    let t = s.thumbnails[1];
    c.call(
        INPUT_CLICK,
        json!({"x": t.x + t.width / 2.0, "y": t.y + t.height / 2.0}),
    )
    .unwrap();
    // The space is the current one at once (no slide)...
    assert!(spaces(&compositor)[1].active);
    let shot = screenshot(&compositor);
    // ...and its window starts small, near its thumbnail, growing to its place.
    let (x0, y0, x1, _) = red_box(&shot).expect("the window of the space is not drawn");
    assert!(x1 - x0 < 300, "not magnifying: {:?}", (x0, y0, x1));
    assert!(y0 < 400, "not coming from the bar: {:?}", (x0, y0, x1));
    wait_for(&compositor, |s| s.mode.is_none());
    let shot = screenshot(&compositor);
    let (x0, _, x1, _) = red_box(&shot).unwrap();
    assert!(x1 - x0 >= 398, "not back to full size: {:?}", (x0, x1));
}

/// The window under the pointer gets the 4 pt `Ring` (inside its edge) and, in Panorama, the
/// title capsule at its centre; the other window gets neither (kit `Hover ring`, `Hover title`).
#[test]
fn the_window_under_the_pointer_gets_the_ring_and_the_title_capsule() {
    let compositor = Headless::start_with(
        "panorama-hover-window",
        (common::WIDTH, common::HEIGHT),
        &["--background", "7a7b80"],
    );
    let _red = TestWindow::open_pixels(
        &compositor.wayland,
        "red",
        (160, 100),
        &solid((160, 100), RED),
    );
    let _blue = TestWindow::open_pixels(
        &compositor.wayland,
        "blue",
        (160, 100),
        &solid((160, 100), BLUE),
    );
    let mut c = compositor.client();
    c.call(INPUT_POINTER_MOVE, json!({"x": 20, "y": 300}))
        .unwrap();
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    let s = state(&compositor);
    let (a, b) = (s.windows[0], s.windows[1]);
    let (left, right) = if a.x < b.x { (a, b) } else { (b, a) };
    // `Ring` is #3B99FD (green 0x99, between the red (0) and blue (0) windows); RGBA, and the
    // edge may be half a pixel off.
    let ring_at = |shot: &[u8], w: &silica_control::commands::PanoramaWindow| {
        let p = pixel(shot, (w.x + 2.0) as u32, (w.y + w.height / 2.0) as u32);
        p[1] > 110 && p[1] < 190 && p[2] > 200 && p[0] < 140
    };
    // The pointer on the left window.
    c.call(
        INPUT_POINTER_MOVE,
        json!({"x": left.x + 30.0, "y": left.y + 20.0}),
    )
    .unwrap();
    let shot = screenshot(&compositor);
    if let Some(dir) = std::env::var_os("SILICA_COMPOSITOR_SHOTS") {
        std::fs::copy(
            compositor.dir.join("shot.png"),
            std::path::Path::new(&dir).join("panorama-hover-window.png"),
        )
        .unwrap();
    }
    assert!(ring_at(&shot, &left), "no ring on the hovered window");
    assert!(!ring_at(&shot, &right), "ring on the other window");
    // The capsule (translucent white, 32 tall) sits at the centre: lighter than the window.
    let centre = pixel(
        &shot,
        (left.x + left.width / 2.0) as u32 - 40,
        (left.y + left.height / 2.0) as u32,
    );
    assert!(
        centre[1] > 100 && centre[2] > 100,
        "no title capsule: {centre:?}"
    );
    // On the other window the ring follows.
    c.call(
        INPUT_POINTER_MOVE,
        json!({"x": right.x + 30.0, "y": right.y + 20.0}),
    )
    .unwrap();
    let shot = screenshot(&compositor);
    assert!(ring_at(&shot, &right) && !ring_at(&shot, &left));
}

#[test]
fn windows_in_a_thumbnail_are_clipped_to_its_cell() {
    let compositor = Headless::start_with("panorama-thumb-clip", (1280, 800), &[]);
    // Taller than the output: its thumbnail would spill below the cell.
    let size = (1100u32, 1500u32);
    let _tall = TestWindow::open_pixels(&compositor.wayland, "tall", size, &solid(size, GREEN));
    let mut c = compositor.client();
    c.call(PANORAMA_TOGGLE, Value::Null).unwrap();
    c.call(PANORAMA_SPACES_BAR, json!({"enabled": true}))
        .unwrap();
    let t = state(&compositor).thumbnails[0];
    let shot = screenshot(&compositor);
    let inside = rgb_at(&shot, t.x + t.width / 2.0, t.y + t.height / 2.0);
    let below = rgb_at(&shot, t.x + t.width / 2.0, t.y + t.height + 6.0);
    eprintln!("inside {inside:?} below {below:?}");
    assert!(inside[1] > 150 && inside[0] < 80, "inside: {inside:?}");
    assert!(
        !(below[1] > 150 && below[0] < 80),
        "below the cell: {below:?}"
    );
}
