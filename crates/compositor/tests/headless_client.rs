//! A real Wayland client opens an xdg-shell window on the headless compositor, and the window shows
//! up in the control channel's screenshot.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::client::{FIRST_WINDOW_AT, TestWindow, WINDOW};
use common::{Headless, assert_outside_window, pixel, screenshot};

#[test]
fn xdg_toplevel_appears_in_screenshot() {
    let compositor = Headless::start("client");
    let _window = TestWindow::open(&compositor.wayland, "test");
    let shot = screenshot(&compositor);

    let (w, h) = WINDOW;
    let at = FIRST_WINDOW_AT;
    // Center and the middle of the sides: the corners are cut (window_corners_follow_the_reference).
    // The outermost pixel row or column is the 1 pt rim, so the points are 1 pt inside.
    for (x, y) in [
        (at + w / 2, at + h / 2),
        (at + 1, at + h / 2),
        (at + w - 2, at + h / 2),
        (at + w / 2, at + 1),
        (at + w / 2, at + h - 2),
    ] {
        assert_eq!(
            pixel(&shot, x, y),
            [255, 0, 0, 255],
            "window missing at ({x}, {y})"
        );
    }
    assert_outside_window(&shot, at - 1, at + h / 2);
    assert_outside_window(&shot, at + w, at + h / 2);
}
