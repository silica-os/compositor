//! The app zooms or minimizes its own window (`xdg_toplevel.set_maximized`, `unset_maximized`,
//! `set_minimized`): the title bar double click of an app that draws its bar (#169).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::Headless;
use common::client::TestWindow;
use serde_json::Value;
use silica_control::commands::{WINDOW_LIST, WindowInfo};

const SIZE: (u32, u32) = (1280, 800);

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

#[test]
fn maximize_fills_the_screen_and_unmaximize_goes_back() {
    let compositor = Headless::start_with("zoom-toggle", SIZE, &[]);
    let mut window = TestWindow::open_decorated(&compositor.wayland, "zoom", (300, 200));
    let before = windows(&compositor).remove(0);
    window.request_maximized(true);
    assert!(window.maximized(), "the client is told it is maximized");
    let (w, h) = window.configured_size().unwrap();
    assert!(w > 1000 && h > 600, "the content fills the area: {w}x{h}");
    window.request_maximized(false);
    assert!(!window.maximized());
    let (w, h) = window.configured_size().unwrap();
    assert_eq!(w, 300);
    assert_eq!(h, 200);
    let after = windows(&compositor).remove(0);
    assert_eq!((after.x, after.y), (before.x, before.y));
}

/// A window that cannot be resized (min = max, Calculator) is not zoomed by the app's title bar
/// double click (`set_maximized`), as its zoom button is disabled (silica-os/silica-desktop#245).
#[test]
fn a_fixed_size_window_does_not_zoom() {
    let compositor = Headless::start_with("zoom-fixed", SIZE, &[]);
    let mut window = TestWindow::open_decorated(&compositor.wayland, "fixed", (248, 412));
    window.set_fixed_size((248, 412));
    let before = windows(&compositor).remove(0);
    window.request_maximized(true);
    assert!(!window.maximized(), "the client stays unmaximized");
    window.commit_frames(1, std::time::Duration::ZERO);
    let after = windows(&compositor).remove(0);
    assert_eq!(
        (after.x, after.y, after.width, after.height),
        (before.x, before.y, before.width, before.height)
    );
    // Minimize is still allowed.
    window.request_minimized();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(windows(&compositor).iter().all(|w| w.minimized));
}

#[test]
fn minimize_request_takes_the_window_off_the_screen() {
    let compositor = Headless::start_with("zoom-minimize", SIZE, &[]);
    let mut window = TestWindow::open_decorated(&compositor.wayland, "min", (300, 200));
    assert_eq!(windows(&compositor).len(), 1);
    window.request_minimized();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(windows(&compositor).iter().all(|w| w.minimized));
}

#[test]
fn an_app_can_fill_the_screen_in_place_and_leave_again() {
    let compositor = Headless::start_with("fullscreen-request", SIZE, &[]);
    let mut window = TestWindow::open_decorated(&compositor.wayland, "slides", (300, 200));
    let before = windows(&compositor).remove(0);
    window.request_fullscreen(true);
    assert!(window.fullscreen(), "the client is told it is full screen");
    assert_eq!(
        window.configured_size(),
        Some((SIZE.0 as i32, SIZE.1 as i32))
    );
    window.commit_frames(1, std::time::Duration::ZERO);
    let list = windows(&compositor);
    assert!(list[0].fullscreen);
    assert_eq!((list[0].x, list[0].y), (0, 0));
    window.request_fullscreen(false);
    assert!(!window.fullscreen());
    window.commit_frames(1, std::time::Duration::ZERO);
    assert_eq!(window.configured_size(), Some((300, 200)));
    let after = windows(&compositor).remove(0);
    assert!(!after.fullscreen);
    assert_eq!((after.x, after.y), (before.x, before.y));
}
