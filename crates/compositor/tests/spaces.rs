//! Workspaces (M5.1) on the headless compositor: windows in the spaces, space switch, black band
//! during the scroll (`plan/reference/spaces.md`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::time::Duration;

use common::client::TestWindow;
use common::{HEIGHT, Headless, WIDTH, pixel, screenshot};
use serde_json::{Value, json};
use silica_control::commands::{
    INPUT_KEY, SPACE_ADD, SPACE_LIST, SPACE_NEXT, SPACE_PREVIOUS, SPACE_REMOVE, SpaceInfo,
    WINDOW_FOCUS, WINDOW_LIST, WINDOW_MOVE_TO_SPACE, WindowInfo,
};

fn solid(size: (u32, u32), rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat((size.0 * size.1) as usize)
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

fn spaces(compositor: &Headless) -> Vec<SpaceInfo> {
    serde_json::from_value(compositor.client().call(SPACE_LIST, Value::Null).unwrap()).unwrap()
}

fn by_title<'a>(list: &'a [WindowInfo], title: &str) -> &'a WindowInfo {
    list.iter()
        .find(|w| w.title.as_deref() == Some(title))
        .unwrap_or_else(|| panic!("window {title} missing: {list:?}"))
}

/// The pixel at the center of a window.
fn center(window: &WindowInfo) -> (u32, u32) {
    (
        (window.x + window.width / 2) as u32,
        (window.y + window.height / 2) as u32,
    )
}

/// Red of the pixel of the red window (BGRA in the test buffer: the blue in memory).
fn shows(shot: &[u8], at: (u32, u32), rgba: [u8; 4]) -> bool {
    let [r, g, b, _] = pixel(shot, at.0, at.1);
    r.abs_diff(rgba[0]) < 10 && g.abs_diff(rgba[1]) < 10 && b.abs_diff(rgba[2]) < 10
}

const RED: [u8; 4] = [0, 0, 255, 255];
const BLUE: [u8; 4] = [255, 0, 0, 255];
const RED_RGB: [u8; 4] = [255, 0, 0, 255];
const BLUE_RGB: [u8; 4] = [0, 0, 255, 255];

#[test]
fn windows_live_in_their_space() {
    let compositor = Headless::start("spaces");
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

    assert_eq!(c.call(SPACE_ADD, Value::Null).unwrap(), json!({"index": 1}));
    let list = windows(&compositor);
    let blue = by_title(&list, "blue").clone();
    c.call(WINDOW_MOVE_TO_SPACE, json!({"id": blue.id, "index": 1}))
        .unwrap();

    let list = windows(&compositor);
    assert_eq!(by_title(&list, "red").space, 0);
    assert_eq!(by_title(&list, "blue").space, 1);
    let red = by_title(&list, "red").clone();
    let s = spaces(&compositor);
    assert_eq!(s.len(), 2);
    assert!(s[0].active);
    assert_eq!(s[0].windows, [red.id]);
    assert_eq!(s[1].windows, [blue.id]);

    // Only the active space is on screen.
    let shot = screenshot(&compositor);
    assert!(shows(&shot, center(&red), RED_RGB));
    assert!(!shows(&shot, center(&blue), BLUE_RGB));

    // Next space (animations off in the tests): the blue one, not the red one.
    assert_eq!(
        c.call(SPACE_NEXT, Value::Null).unwrap(),
        json!({"switched": true})
    );
    let shot = screenshot(&compositor);
    assert!(shows(&shot, center(&blue), BLUE_RGB));
    assert!(!shows(&shot, center(&red), RED_RGB));
    assert!(spaces(&compositor)[1].active);
    // Past the last space nothing happens.
    assert_eq!(
        c.call(SPACE_NEXT, Value::Null).unwrap(),
        json!({"switched": false})
    );

    // window.focus on a window of another space goes to that space.
    c.call(WINDOW_FOCUS, json!({"id": red.id})).unwrap();
    assert!(spaces(&compositor)[0].active);
    assert_eq!(
        c.call(SPACE_PREVIOUS, Value::Null).unwrap(),
        json!({"switched": false})
    );

    // Removing space 1: the blue one goes back to space 0, visible.
    c.call(SPACE_REMOVE, json!({"index": 1})).unwrap();
    let shot = screenshot(&compositor);
    assert!(shows(&shot, center(&blue), BLUE_RGB));
    assert_eq!(spaces(&compositor).len(), 1);
    assert!(c.call(SPACE_REMOVE, json!({"index": 0})).is_err());
}

#[test]
fn switching_slides_with_a_black_gap() {
    // Animations 40 times slower: the scroll lasts ~30 s and is captured halfway.
    let compositor = Headless::start_with("spaces-slide", (WIDTH, HEIGHT), &[]);
    let mut c = compositor.client();
    c.call("ui.animation_scale", json!({"scale": 40})).unwrap();
    let _red = TestWindow::open_pixels(
        &compositor.wayland,
        "red",
        (100, 60),
        &solid((100, 60), RED),
    );
    c.call(SPACE_ADD, Value::Null).unwrap();
    // With ⌃→ from the keyboard, as in the reference.
    c.call(INPUT_KEY, json!({"key": "Control_L", "pressed": true}))
        .unwrap();
    c.call(INPUT_KEY, json!({"key": "Right"})).unwrap();
    c.call(INPUT_KEY, json!({"key": "Control_L", "pressed": false}))
        .unwrap();
    std::thread::sleep(Duration::from_millis(4000));
    let shot = screenshot(&compositor);
    // On the center row a black band 64 wide (± the antialiased edge).
    let y = HEIGHT / 2;
    let black: Vec<u32> = (0..WIDTH)
        .filter(|&x| {
            let [r, g, b, _] = pixel(&shot, x, y);
            r < 4 && g < 4 && b < 4
        })
        .collect();
    assert!(!black.is_empty(), "no black band");
    let (first, last) = (black[0], *black.last().unwrap());
    assert!(
        black.len() as u32 == last - first + 1,
        "the band is not contiguous: {black:?}"
    );
    assert!(
        (62..=65).contains(&(last - first + 1)),
        "width {}",
        last - first + 1
    );
    assert!(spaces(&compositor)[1].active);
}
