//! Resizing from the frame edges (mustache-spec 1.1, MU-08): 2 pt outside and 3 inside on the
//! left and at the top, 4 outside and 3 inside on the right and at the bottom, diagonal corners for 16 pt.
//! Live resize (#160): one configure per client answer, the `resizing` state while the button
//! is held, the left and top edges anchored on the size the client commits.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::Headless;
use common::client::TestWindow;
use serde_json::{Value, json};
use silica_control::commands::{INPUT_POINTER_BUTTON, INPUT_POINTER_MOVE, WINDOW_LIST, WindowInfo};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;

const SIZE: (u32, u32) = (1280, 800);
const BAR: i32 = 32;
const W: i32 = 300;
const H: i32 = 200;

fn window(compositor: &Headless) -> WindowInfo {
    let list: Vec<WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    list.into_iter().next().unwrap()
}

fn press(compositor: &Headless, at: (f64, f64)) {
    let mut client = compositor.client();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": at.0, "y": at.1}))
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": true}),
        )
        .unwrap();
}

fn move_to(compositor: &Headless, at: (f64, f64)) {
    compositor
        .client()
        .call(INPUT_POINTER_MOVE, json!({"x": at.0, "y": at.1}))
        .unwrap();
}

fn release(compositor: &Headless) {
    compositor
        .client()
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": false}),
        )
        .unwrap();
}

/// Right and bottom sides of the window's content.
fn far_sides(info: &WindowInfo) -> (i32, i32) {
    (info.x + info.width, info.y + info.height)
}

/// Presses at `from`, drags by `by` and releases.
fn drag(compositor: &Headless, from: (f64, f64), by: (f64, f64)) {
    let mut client = compositor.client();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": from.0, "y": from.1}))
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": true}),
        )
        .unwrap();
    client
        .call(
            INPUT_POINTER_MOVE,
            json!({"x": from.0 + by.0, "y": from.1 + by.1}),
        )
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": false}),
        )
        .unwrap();
}

/// A decorated 300 × 200 window and its frame (bar included): x, y, width, height.
fn open(name: &str) -> (Headless, TestWindow, (f64, f64, f64, f64)) {
    let compositor = Headless::start_with(name, SIZE, &["--background", "7a7b80"]);
    let mut win = TestWindow::open_decorated(&compositor.wayland, "edges", (W as u32, H as u32));
    win.configured_size();
    let info = window(&compositor);
    let frame = (
        f64::from(info.x),
        f64::from(info.y - BAR),
        f64::from(W),
        f64::from(H + BAR),
    );
    (compositor, win, frame)
}

#[test]
fn the_left_edge_also_takes_2_pt_outside() {
    let (compositor, mut win, (x, y, _, _)) = open("resize-left");
    let before = window(&compositor);
    drag(&compositor, (x - 1.5, y + 120.0), (-50.0, 0.0));
    assert_eq!(win.configured_size(), Some((W + 50, H)));
    // The origin moves when the client commits the new size, not before.
    assert_eq!(
        window(&compositor).x,
        before.x,
        "no jump before the client's frame"
    );
    win.answer();
    assert_eq!(
        window(&compositor).x,
        before.x - 50,
        "the right side stays still"
    );
}

#[test]
fn the_right_edge_takes_4_pt_outside_and_no_more() {
    let (compositor, mut win, (x, y, w, _)) = open("resize-right");
    let before = window(&compositor);
    drag(&compositor, (x + w + 3.5, y + 120.0), (40.0, 0.0));
    assert_eq!(win.configured_size(), Some((W + 40, H)));
    assert_eq!(window(&compositor).x, before.x);
    // Just past the zone (4 pt outside): nothing.
    let (compositor, mut win, (x, y, w, _)) = open("resize-right-out");
    let asked = win.configured_size();
    drag(&compositor, (x + w + 4.5, y + 120.0), (40.0, 0.0));
    assert_eq!(win.configured_size(), asked);
}

#[test]
fn the_corner_resizes_in_two_directions() {
    let (compositor, mut win, (x, y, w, h)) = open("resize-corner");
    // 1 pt outside on the right, 5 pt above the bottom: inside the 16 pt of the corner.
    drag(&compositor, (x + w + 1.0, y + h - 5.0), (30.0, 20.0));
    assert_eq!(win.configured_size(), Some((W + 30, H + 20)));
}

#[test]
fn with_full_size_content_the_top_edge_is_the_top_of_the_app() {
    let compositor = Headless::start_with("resize-full", SIZE, &["--background", "7a7b80"]);
    let mut win = TestWindow::open_decorated(&compositor.wayland, "edges", (W as u32, H as u32));
    win.set_full_size_content(52);
    let before = window(&compositor);
    // The frame is the content: the top is at the window's y.
    let (x, y) = (f64::from(before.x), f64::from(before.y));
    drag(&compositor, (x + 150.0, y - 1.0), (0.0, -30.0));
    assert_eq!(win.configured_size(), Some((W, H + 30)));
    win.answer();
    assert_eq!(window(&compositor).y, before.y - 30);
}

#[test]
fn live_left_edge_follows_the_pointer_and_the_right_side_stays() {
    let (compositor, mut win, (x, y, _, _)) = open("resize-live-left");
    let before = window(&compositor);
    let anchor = far_sides(&before);
    let grab = (x - 1.0, y + 120.0);
    press(&compositor, grab);
    assert!(win.resizing(), "the press sends the resizing state");
    win.answer();
    for dx in [-10.0, -35.0, -80.0, -20.0, 40.0] {
        move_to(&compositor, (grab.0 + dx, grab.1));
        let asked = win.configured_size().unwrap();
        assert_eq!(asked, (W - dx as i32, H), "the size follows the pointer");
        assert!(win.resizing());
        // Before the client's frame the window is where it was: same size, same place.
        let shown = window(&compositor);
        assert_eq!(far_sides(&shown), anchor);
        win.answer();
        let shown = window(&compositor);
        assert_eq!((shown.width, shown.height), asked);
        assert_eq!(far_sides(&shown), anchor, "dx {dx}: the right side moved");
    }
    release(&compositor);
    assert!(!win.resizing(), "the release clears the resizing state");
    win.answer();
    assert_eq!(far_sides(&window(&compositor)), anchor);
}

#[test]
fn live_top_left_corner_keeps_the_bottom_right_corner() {
    let (compositor, mut win, (x, y, _, _)) = open("resize-live-corner");
    // Down from the top of the screen first (dragged by the bar), so the top edge has room.
    drag(&compositor, (x + 150.0, y + 10.0), (100.0, 200.0));
    let info = window(&compositor);
    let (x, y) = (f64::from(info.x), f64::from(info.y - BAR));
    let anchor = far_sides(&window(&compositor));
    let grab = (x - 1.0, y - 1.0);
    press(&compositor, grab);
    win.answer();
    for (dx, dy) in [(-15.0, -10.0), (-40.0, -45.0), (30.0, 25.0)] {
        move_to(&compositor, (grab.0 + dx, grab.1 + dy));
        let asked = win.answer();
        assert_eq!(asked, (W - dx as i32, H - dy as i32));
        let shown = window(&compositor);
        assert_eq!(far_sides(&shown), anchor, "({dx}, {dy})");
    }
    release(&compositor);
    win.answer();
    assert_eq!(far_sides(&window(&compositor)), anchor);
}

#[test]
fn one_configure_in_flight_until_the_client_answers() {
    let (compositor, mut win, (x, y, w, _)) = open("resize-throttle");
    let grab = (x + w + 1.0, y + 120.0);
    press(&compositor, grab);
    let after_press = win.configure_count();
    // The press's configure is not answered yet: the movements wait.
    for dx in 1..=5 {
        move_to(&compositor, (grab.0 + f64::from(dx) * 10.0, grab.1));
    }
    assert_eq!(win.configure_count(), after_press);
    // The answer lets the newest size leave, only that one.
    win.answer();
    assert_eq!(win.configure_count(), after_press + 1);
    assert_eq!(win.configured_size(), Some((W + 50, H)));
    release(&compositor);
}

#[test]
fn a_slow_client_never_shows_a_jump() {
    let (compositor, mut win, (x, y, _, _)) = open("resize-slow");
    let before = window(&compositor);
    let anchor = far_sides(&before);
    win.hold_acks(true);
    let grab = (x - 1.0, y + 120.0);
    press(&compositor, grab);
    move_to(&compositor, (grab.0 - 60.0, grab.1));
    // Nothing acked, then a frame of the old size: the window stays as it was.
    win.commit_size((W, H));
    assert_eq!(window(&compositor).x, before.x);
    win.ack_held();
    win.commit_size((W, H));
    let shown = window(&compositor);
    assert_eq!(
        (shown.x, shown.width),
        (before.x, W),
        "an old frame stays in place"
    );
    // The client draws a size of its own (not the one asked): still anchored on the right.
    win.ack_held();
    win.commit_size((W + 37, H));
    assert_eq!(far_sides(&window(&compositor)), anchor);
    // The size asked last arrives.
    let asked = win.answer();
    let shown = window(&compositor);
    assert_eq!((shown.width, shown.height), asked);
    assert_eq!(far_sides(&shown), anchor);
    release(&compositor);
    win.answer();
    assert_eq!(far_sides(&window(&compositor)), anchor);
}

#[test]
fn the_minimum_size_stops_the_edge() {
    let (compositor, mut win, (x, y, _, _)) = open("resize-min");
    win.set_min_size((220, 150));
    let anchor = far_sides(&window(&compositor));
    let grab = (x - 1.0, y + 120.0);
    press(&compositor, grab);
    win.answer();
    // Far past the minimum, to the right.
    move_to(&compositor, (grab.0 + 250.0, grab.1));
    let asked = win.answer();
    assert_eq!(asked, (220, H), "the edge stops at the minimum width");
    let shown = window(&compositor);
    assert_eq!(far_sides(&shown), anchor);
    // A hard stop: coming back, the edge picks the pointer up again as soon as it passes the
    // stop, with nothing accumulated (300 - 60 = 240 wide, the pointer 60 pt in).
    move_to(&compositor, (grab.0 + 60.0, grab.1));
    assert_eq!(win.answer(), (W - 60, H));
    assert_eq!(far_sides(&window(&compositor)), anchor);
    // The bottom edge stops at the minimum height too.
    release(&compositor);
    win.answer();
    let (x, y, w, h) = {
        let info = window(&compositor);
        (
            f64::from(info.x),
            f64::from(info.y - BAR),
            f64::from(info.width),
            f64::from(info.height + BAR),
        )
    };
    let grab = (x + w / 2.0, y + h + 1.0);
    press(&compositor, grab);
    win.answer();
    move_to(&compositor, (grab.0, grab.1 - 300.0));
    assert_eq!(win.answer(), (W - 60, 150));
    release(&compositor);
}

#[test]
fn a_resize_grab_is_not_seen_by_the_desktop_below() {
    // The desktop layer (Mustache's icons) under the window's edge: the press, the motion and
    // the release of a resize belong to the compositor (#214).
    let compositor = Headless::start_with("resize-desktop", SIZE, &["--background", "7a7b80"]);
    let mut desktop = TestWindow::open_layer(
        &compositor.wayland,
        Layer::Background,
        "desktop",
        None,
        |_, _, _, _| [0, 0, 0, 0],
    );
    let mut win = TestWindow::open_decorated(&compositor.wayland, "edges", (W as u32, H as u32));
    win.configured_size();
    let info = window(&compositor);
    let (x, y) = (f64::from(info.x), f64::from(info.y - BAR));
    let before = desktop.pointer_log();
    // The right edge takes 4 pt outside the window: there the pointer is over the desktop.
    drag(
        &compositor,
        (x + f64::from(W) + 2.0, y + 120.0),
        (40.0, 30.0),
    );
    assert!(win.configured_size().is_some());
    let after = desktop.pointer_log();
    assert_eq!(
        after.presses, before.presses,
        "no press reached the desktop"
    );
    assert_eq!(
        after.motions, before.motions,
        "no motion reached the desktop"
    );
}
