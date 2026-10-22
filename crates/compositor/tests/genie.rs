//! Minimizing into the Dock as a genie, restore and full screen (`plan/reference/windows.md`,
//! "Animations"), slowed down with `ui.animation_scale` to read them in the screenshots.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::time::Duration;

use common::client::TestWindow;
use common::{Headless, pixel_in};
use serde_json::{Value, json};
use silica_control::commands::{
    UI_ANIMATION_SCALE, WINDOW_FOCUS, WINDOW_FULLSCREEN, WINDOW_LIST, WINDOW_MINIMIZE,
    WINDOW_TILE_FILL, WindowInfo,
};

const SIZE: (u32, u32) = (640, 400);
/// Without a Dock the thumbnail goes to the bottom center: center (320, 360), 57 wide.
const TARGET: (i32, i32) = (320, 360);

fn start(name: &str) -> Headless {
    Headless::start_with(name, SIZE, &["--background", "7a7b80"])
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

fn set_speed(compositor: &Headless, scale: f64) {
    compositor
        .client()
        .call(UI_ANIMATION_SCALE, json!({ "scale": scale }))
        .unwrap();
}

fn shot(compositor: &Headless, name: &str) -> Vec<u8> {
    let shot = common::screenshot(compositor);
    if let Some(dir) = std::env::var_os("SILICA_COMPOSITOR_SHOTS") {
        std::fs::copy(
            compositor.dir.join("shot.png"),
            std::path::Path::new(&dir).join(format!("genie-{name}.png")),
        )
        .unwrap();
    }
    shot
}

fn is_red(shot: &[u8], x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= SIZE.0 as i32 || y >= SIZE.1 as i32 {
        return false;
    }
    let [r, g, b, _] = pixel_in(shot, SIZE.0, x as u32, y as u32);
    r > 180 && g < 90 && b < 90
}

/// The red pixels (the content of the test window) on a row: first and last.
fn red_span(shot: &[u8], y: i32) -> Option<(i32, i32)> {
    let xs: Vec<i32> = (0..SIZE.0 as i32).filter(|&x| is_red(shot, x, y)).collect();
    Some((*xs.first()?, *xs.last()?))
}

fn red_count(shot: &[u8]) -> usize {
    (0..SIZE.1 as i32)
        .flat_map(|y| (0..SIZE.0 as i32).map(move |x| (x, y)))
        .filter(|&(x, y)| is_red(shot, x, y))
        .count()
}

#[test]
fn minimize_bends_the_window_into_the_dock_and_restore_brings_it_back() {
    let compositor = start("genie");
    let _window = TestWindow::open_decorated(&compositor.wayland, "genie", (240, 140));
    let info = windows(&compositor).remove(0);
    let (left, right) = (info.x, info.x + 239);
    let mid_row = info.y + 120;
    assert_eq!(
        red_span(&shot(&compositor, "before"), mid_row),
        Some((left, right))
    );

    // ×20: minimizing lasts 9.6 s.
    set_speed(&compositor, 20.0);
    compositor
        .client()
        .call(WINDOW_MINIMIZE, json!({ "id": info.id }))
        .unwrap();
    // At the start the snapshot is the window as it was: white bar on top, red below (the
    // texture orientation is right).
    let at_start = shot(&compositor, "start");
    // (Changing the speed also recomputes the opening: the window may still be at 98 %.)
    let (a, b) = red_span(&at_start, mid_row).expect("window at the start");
    assert!((a - left).abs() <= 4 && (b - right).abs() <= 4, "{a}–{b}");
    let [r, g, b, _] = pixel_in(&at_start, SIZE.0, (left + 210) as u32, (info.y - 16) as u32);
    assert!(r > 230 && g > 230 && b > 230, "bar on top: {r} {g} {b}");

    // Halfway (~215 ms of 480): the bottom of the window is bent towards the thumbnail, which
    // is to the right and below; the top has left its previous place.
    std::thread::sleep(Duration::from_millis(4300));
    let middle = shot(&compositor, "middle");
    let bottom = info.y + 135;
    let (bent_left, bent_right) = red_span(&middle, bottom).expect("the bottom is still visible");
    assert!(
        bent_left > left + 30,
        "the left edge at the bottom goes towards the thumbnail: {bent_left} (before {left})"
    );
    assert!(bent_right - bent_left < 239 - 40, "the funnel narrows");
    assert!(
        !is_red(&middle, left + 5, info.y + 2),
        "the top of the window came down"
    );
    // The genie faces the thumbnail: there is red lower than the window was.
    let lowest = (info.y + 140..SIZE.1 as i32)
        .rev()
        .find(|&y| red_span(&middle, y).is_some());
    assert!(lowest.is_some(), "no funnel below the window");

    // At the end the window is gone.
    set_speed(&compositor, 0.0);
    assert_eq!(red_count(&shot(&compositor, "end")), 0);

    // Slow restore: the window is mapped but only the genie shows, starting from the
    // thumbnail (the window's place is still empty).
    set_speed(&compositor, 20.0);
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({ "id": info.id }))
        .unwrap();
    let restoring = shot(&compositor, "restoring");
    assert!(
        !is_red(&restoring, left + 120, info.y + 40),
        "the restored window stays hidden until the genie ends"
    );
    let near_target = (TARGET.1 - 30..SIZE.1 as i32).any(|y| {
        red_span(&restoring, y).is_some_and(|(a, b)| a <= TARGET.0 + 40 && b >= TARGET.0 - 40)
    });
    assert!(near_target, "the restore starts from the thumbnail");

    // Once the genie is over, the window is in its place.
    set_speed(&compositor, 0.0);
    assert_eq!(
        red_span(&shot(&compositor, "restored"), mid_row),
        Some((left, right))
    );
}

#[test]
fn speed_zero_minimizes_and_restores_at_once() {
    let compositor = start("genie-zero");
    let _window = TestWindow::open_decorated(&compositor.wayland, "immediate", (240, 140));
    let info = windows(&compositor).remove(0);
    set_speed(&compositor, 0.0);
    compositor
        .client()
        .call(WINDOW_MINIMIZE, json!({ "id": info.id }))
        .unwrap();
    assert_eq!(red_count(&shot(&compositor, "zero")), 0);
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({ "id": info.id }))
        .unwrap();
    assert_eq!(
        red_span(&shot(&compositor, "zero-restore"), info.y + 120),
        Some((info.x, info.x + 239))
    );
}

#[test]
fn the_genie_lasts_as_measured() {
    let compositor = start("genie-duration");
    let _window = TestWindow::open_decorated(&compositor.wayland, "duration", (240, 140));
    let info = windows(&compositor).remove(0);
    // At normal speed: 480 ms. At 300 ms there is still something, at 600 ms nothing.
    set_speed(&compositor, 1.0);
    compositor
        .client()
        .call(WINDOW_MINIMIZE, json!({ "id": info.id }))
        .unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let during = red_count(&shot(&compositor, "duration-250"));
    assert!(during > 0, "at 250 ms the genie is still in progress");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(red_count(&shot(&compositor, "duration-650")), 0);
}

#[test]
fn closing_a_window_while_it_comes_out_of_the_dock_does_not_crash() {
    let compositor = start("genie-closed");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "closed", (240, 140));
    let info = windows(&compositor).remove(0);
    set_speed(&compositor, 0.0);
    compositor
        .client()
        .call(WINDOW_MINIMIZE, json!({ "id": info.id }))
        .unwrap();
    set_speed(&compositor, 20.0);
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({ "id": info.id }))
        .unwrap();
    shot(&compositor, "closed-before");
    window.destroy_toplevel();
    drop(window);
    std::thread::sleep(Duration::from_millis(100));
    // The compositor still answers and the genie of the window that is gone disappears.
    let after = shot(&compositor, "closed-after");
    assert!(windows(&compositor).is_empty());
    set_speed(&compositor, 0.0);
    let _ = after;
    assert_eq!(red_count(&shot(&compositor, "closed-end")), 0);
}

#[test]
fn fullscreen_grows_the_window_to_the_screen() {
    let compositor = start("genie-full-screen");
    let _window = TestWindow::open_decorated(&compositor.wayland, "full", (240, 140));
    let info = windows(&compositor).remove(0);
    let width = 240;
    // ×20: entering lasts 10.4 s; at ~2.6 s (130 ms) the rectangle is at ~0.55.
    set_speed(&compositor, 20.0);
    compositor
        .client()
        .call(WINDOW_FULLSCREEN, json!({ "id": info.id }))
        .unwrap();
    let start = shot(&compositor, "full-start");
    let row = info.y + 70;
    // (Changing the speed also recomputes the opening: the window may still be at 98 %.)
    let (a, b) = red_span(&start, row).expect("window at the start");
    assert!(
        (b - a + 1 - width).abs() <= 6,
        "at the start it is as it was: {}",
        b - a + 1
    );
    std::thread::sleep(Duration::from_millis(2600));
    let middle = shot(&compositor, "full-middle");
    let widest = (0..SIZE.1 as i32)
        .filter_map(|y| red_span(&middle, y))
        .map(|(a, b)| b - a + 1)
        .max()
        .expect("window halfway");
    assert!(
        widest > width + 60 && widest < SIZE.0 as i32,
        "halfway the window grows towards the screen: {widest}"
    );

    // Once entering is over, the slow exit (×20: 9 s): at ~2.4 s (120 ms, fraction ~0.56) the
    // frame is between the screen and the previous frame.
    set_speed(&compositor, 0.0);
    shot(&compositor, "full-done");
    set_speed(&compositor, 20.0);
    compositor
        .client()
        .call(WINDOW_FULLSCREEN, json!({ "id": info.id }))
        .unwrap();
    std::thread::sleep(Duration::from_millis(2400));
    let leaving = shot(&compositor, "full-exiting");
    let widest = (0..SIZE.1 as i32)
        .filter_map(|y| red_span(&leaving, y))
        .map(|(a, b)| b - a + 1)
        .max()
        .expect("window exiting");
    assert!(
        widest > width + 20 && widest < SIZE.0 as i32 - 20,
        "while exiting the window goes back to its frame: {widest}"
    );
    // At the end it is as it was again.
    set_speed(&compositor, 0.0);
    let (a, b) = red_span(&shot(&compositor, "full-back"), row).expect("window back");
    assert_eq!(b - a + 1, width);
}

#[test]
fn tiling_slides_the_frame_instead_of_jumping() {
    let compositor = start("tile-slide");
    let _window = TestWindow::open_decorated(&compositor.wayland, "tile", (240, 140));
    let info = windows(&compositor).remove(0);
    let row = info.y + 70;
    // x20: the 380 ms tiling lasts 7.6 s; at ~3 s (150 ms, ~70 % of the way) the frame is
    // between its old size and the whole usable area.
    set_speed(&compositor, 20.0);
    compositor
        .client()
        .call(WINDOW_TILE_FILL, json!({ "id": info.id }))
        .unwrap();
    let start = shot(&compositor, "tile-start");
    let (a, b) = red_span(&start, row).expect("window at the start");
    assert!(
        (b - a + 1 - 240).abs() <= 6,
        "at the start it is as it was: {}",
        b - a + 1
    );
    std::thread::sleep(Duration::from_millis(3000));
    let middle = shot(&compositor, "tile-middle");
    let widest = (0..SIZE.1 as i32)
        .filter_map(|y| red_span(&middle, y))
        .map(|(a, b)| b - a + 1)
        .max()
        .expect("window halfway");
    assert!(
        widest > 240 + 60 && widest <= SIZE.0 as i32,
        "halfway the window grows towards the area: {widest}"
    );
    set_speed(&compositor, 0.0);
}
