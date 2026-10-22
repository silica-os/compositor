//! Server-side decorations: title bar, window buttons and their commands, with the colors
//! measured on macOS 27 (`plan/reference/windows.md`).

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
use silica_control::commands::{
    INPUT_CLICK, INPUT_POINTER_BUTTON, INPUT_POINTER_MOVE, WINDOW_FOCUS, WINDOW_FULLSCREEN,
    WINDOW_LIST, WindowInfo,
};
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::Mode;

const SIZE: (u32, u32) = (640, 400);
const BAR: i32 = 32;

fn start(name: &str) -> Headless {
    Headless::start_with(name, SIZE, &["--background", "7a7b80"])
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

fn shot(compositor: &Headless) -> Vec<u8> {
    let shot = common::screenshot(compositor);
    if let Some(dir) = std::env::var_os("SILICA_COMPOSITOR_SHOTS") {
        std::fs::copy(
            compositor.dir.join("shot.png"),
            std::path::Path::new(&dir).join("decorations.png"),
        )
        .unwrap();
    }
    shot
}

/// The pixel is within `tolerance` of the expected color, channel by channel.
fn assert_near(shot: &[u8], (x, y): (i32, i32), expected: [u8; 3], tolerance: u8, what: &str) {
    let got = pixel_in(shot, SIZE.0, x as u32, y as u32);
    let close = got
        .iter()
        .zip(expected)
        .all(|(g, e)| g.abs_diff(e) <= tolerance);
    assert!(
        close,
        "{what} at ({x}, {y}): {got:?}, expected {expected:?}"
    );
}

/// Center of a bar button (0 close, 1 minimize, 2 full screen) of the window.
fn button(window: &WindowInfo, index: i32) -> (i32, i32) {
    (window.x + 16 + 23 * index, window.y - BAR + 16)
}

#[test]
fn titlebar_is_negotiated_and_drawn_with_the_measured_buttons() {
    let compositor = start("decor-draw");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "bar", (240, 140));
    assert_eq!(window.decoration_mode(), Some(Mode::ServerSide));
    let info = windows(&compositor).remove(0);
    // The bar sits above the content, inside the usable area.
    assert!(info.y >= BAR, "bar off screen: content at y {}", info.y);
    let shot = shot(&compositor);

    // White background of the active bar; top-left corner cut (gray background).
    assert_near(
        &shot,
        (info.x + 150, info.y - 16),
        [255, 255, 255],
        1,
        "bar background",
    );
    assert_near(
        &shot,
        (info.x, info.y - BAR),
        [0x7a, 0x7b, 0x80],
        40,
        "bar corner",
    );
    // Active buttons: close #f87d77, minimize #fcc93b, full screen #85d660 at the center.
    assert_near(&shot, button(&info, 0), [0xf8, 0x7d, 0x77], 8, "close");
    assert_near(&shot, button(&info, 1), [0xfc, 0xc9, 0x3b], 8, "minimize");
    assert_near(
        &shot,
        button(&info, 2),
        [0x85, 0xd6, 0x60],
        8,
        "full screen",
    );
    // The content stays red below the bar.
    assert_near(
        &shot,
        (info.x + 120, info.y + 70),
        [255, 0, 0],
        0,
        "content",
    );
}

#[test]
fn hovering_the_buttons_shows_the_symbols_and_inactive_buttons_are_gray() {
    let compositor = start("decor-hover");
    let _first = TestWindow::open_decorated(&compositor.wayland, "first", (240, 140));
    let first = windows(&compositor).remove(0);
    let mut client = compositor.client();
    let (cx, cy) = button(&first, 0);
    client
        .call(INPUT_POINTER_MOVE, json!({"x": cx, "y": cy}))
        .unwrap();
    // At the center of the close button there is the crossing of the X, `#a30000`.
    assert_near(
        &shot(&compositor),
        (cx, cy),
        [0xa3, 0x00, 0x00],
        12,
        "close symbol",
    );

    // A second window far away makes the first inactive: near-white buttons (TL-GLASS: lum 240 at the top to 246 at the bottom).
    let _second = TestWindow::open_decorated(&compositor.wayland, "second", (120, 80));
    let second = windows(&compositor).remove(0);
    client
        .call(
            silica_control::commands::WINDOW_MOVE,
            json!({"id": second.id, "x": 450, "y": 250}),
        )
        .unwrap();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": 5, "y": 395}))
        .unwrap();
    let inactive = shot(&compositor);
    for index in 0..3 {
        assert_near(
            &inactive,
            button(&first, index),
            [0xf3, 0xf3, 0xf3],
            6,
            "inactive button",
        );
    }

    // Pointer on the group of the inactive window: active colors and symbols, it stays inactive.
    let (mx, my) = button(&first, 1);
    client
        .call(INPUT_POINTER_MOVE, json!({"x": mx, "y": my}))
        .unwrap();
    assert_near(
        &shot(&compositor),
        (mx, my),
        [0xc1, 0x44, 0x00],
        14,
        "minimize symbol",
    );
    // Pressed on minimize: the symbol becomes lighter and saturated (`#ff5a00`).
    client
        .call(INPUT_POINTER_BUTTON, json!({"pressed": true}))
        .unwrap();
    assert_near(
        &shot(&compositor),
        (mx, my),
        [0xff, 0x5a, 0x00],
        14,
        "minimize pressed",
    );
    // Leaving before the release cancels the action.
    client
        .call(INPUT_POINTER_MOVE, json!({"x": mx + 60, "y": my + 60}))
        .unwrap();
    client
        .call(INPUT_POINTER_BUTTON, json!({"pressed": false}))
        .unwrap();
    assert!(
        !windows(&compositor)
            .iter()
            .find(|w| w.id == first.id)
            .unwrap()
            .minimized
    );
}

#[test]
fn titlebar_buttons_close_minimize_and_fullscreen() {
    let compositor = start("decor-buttons");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "buttons", (240, 140));
    let info = windows(&compositor).remove(0);
    let mut client = compositor.client();
    let click = |client: &mut silica_control::Client, (x, y): (i32, i32)| {
        client.call(INPUT_CLICK, json!({"x": x, "y": y})).unwrap();
    };

    // Minimize: it disappears from the screen, window.focus brings it back where it was.
    click(&mut client, button(&info, 1));
    let list = windows(&compositor);
    assert!(list[0].minimized, "{list:?}");
    client.call(WINDOW_FOCUS, json!({"id": info.id})).unwrap();
    let list = windows(&compositor);
    assert!(!list[0].minimized && list[0].focused, "{list:?}");
    assert_eq!((list[0].x, list[0].y), (info.x, info.y));

    // Full screen and back (green button, then the command).
    // The state reaches the client with a configure; it applies from the next commit.
    click(&mut client, button(&info, 2));
    window.decoration_mode();
    window.commit_frames(1, std::time::Duration::ZERO);
    let list = windows(&compositor);
    assert!(list[0].fullscreen, "{list:?}");
    client.call(WINDOW_FULLSCREEN, Value::Null).unwrap();
    window.decoration_mode();
    window.commit_frames(1, std::time::Duration::ZERO);
    let list = windows(&compositor);
    assert!(!list[0].fullscreen, "{list:?}");
    assert_eq!((list[0].x, list[0].y), (info.x, info.y));

    // Close: the request reaches the client.
    click(&mut client, button(&info, 0));
    assert!(window.close_requested());
}

#[test]
fn a_fixed_size_window_keeps_minimize_and_disables_full_screen() {
    let compositor = start("decor-fixed");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "fixed", (240, 140));
    window.set_fixed_size((240, 140));
    let info = windows(&compositor).remove(0);
    let shot = shot(&compositor);
    // Minimize stays yellow; full screen is a gray disc (calculator.md).
    assert_near(&shot, button(&info, 1), [0xfc, 0xc9, 0x3b], 8, "minimize");
    let zoom = pixel_in(
        &shot,
        SIZE.0,
        button(&info, 2).0 as u32,
        button(&info, 2).1 as u32,
    );
    assert!(
        zoom[0].abs_diff(zoom[1]) < 12 && zoom[1].abs_diff(zoom[2]) < 12,
        "full screen should be gray: {zoom:?}"
    );
    // The green button does nothing; minimize still works.
    let mut client = compositor.client();
    let (x, y) = button(&info, 2);
    client.call(INPUT_CLICK, json!({"x": x, "y": y})).unwrap();
    window.decoration_mode();
    window.commit_frames(1, std::time::Duration::ZERO);
    assert!(!windows(&compositor)[0].fullscreen);
    let (x, y) = button(&info, 1);
    client.call(INPUT_CLICK, json!({"x": x, "y": y})).unwrap();
    assert!(windows(&compositor)[0].minimized);
}

#[test]
fn dragging_the_titlebar_moves_the_window() {
    let compositor = start("decor-drag");
    let _window = TestWindow::open_decorated(&compositor.wayland, "drag", (240, 140));
    let info = windows(&compositor).remove(0);
    let mut client = compositor.client();
    let (x, y) = (info.x + 150, info.y - 16);
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x, "y": y}))
        .unwrap();
    client
        .call(INPUT_POINTER_BUTTON, json!({"pressed": true}))
        .unwrap();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x + 40, "y": y + 30}))
        .unwrap();
    client
        .call(INPUT_POINTER_BUTTON, json!({"pressed": false}))
        .unwrap();
    let moved = windows(&compositor).remove(0);
    assert_eq!((moved.x, moved.y), (info.x + 40, info.y + 30));
}

/// The frame never goes above the top of the usable area during the move (#188).
#[test]
fn dragging_the_titlebar_up_stops_at_the_top_of_the_usable_area() {
    let compositor = start("decor-drag-top");
    let _window = TestWindow::open_decorated(&compositor.wayland, "drag", (240, 140));
    let info = windows(&compositor).remove(0);
    let mut client = compositor.client();
    let (x, y) = (info.x + 150, info.y - 16);
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x, "y": y}))
        .unwrap();
    client
        .call(INPUT_POINTER_BUTTON, json!({"pressed": true}))
        .unwrap();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x, "y": 0}))
        .unwrap();
    let during = windows(&compositor).remove(0);
    assert_eq!(during.y, BAR);
    client
        .call(INPUT_POINTER_BUTTON, json!({"pressed": false}))
        .unwrap();
    assert_eq!(windows(&compositor).remove(0).y, BAR);
}

#[test]
fn full_size_content_keeps_only_the_buttons_over_the_app() {
    let compositor = start("decor-full-size");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "full", (240, 140));
    let before = windows(&compositor).remove(0);
    // The app draws a toolbar 52 tall: The compositor removes its bar.
    window.set_full_size_content(52);
    window.commit_frames(1, std::time::Duration::ZERO);
    let info = windows(&compositor).remove(0);
    let image = shot(&compositor);
    // Above the content there is no white bar any more: the gray background.
    assert_near(
        &image,
        (info.x + 150, info.y - 16),
        [0x7a, 0x7b, 0x80],
        6,
        "no bar above the content",
    );
    // The buttons sit above the content, centered in the app's bar (y 26).
    let on_content = |i: i32| (info.x + 26 + 23 * i, info.y + 26);
    assert_near(&image, on_content(0), [0xf8, 0x7d, 0x77], 8, "close");
    assert_near(&image, on_content(2), [0x85, 0xd6, 0x60], 8, "full screen");
    // The rest of the strip belongs to the app (red).
    assert_near(
        &image,
        (info.x + 150, info.y + 26),
        [255, 0, 0],
        0,
        "app bar",
    );
    // The click on the red one closes; elsewhere it reaches the app.
    let mut client = compositor.client();
    client
        .call(INPUT_CLICK, json!({"x": info.x + 26, "y": info.y + 26}))
        .unwrap();
    assert!(window.close_requested());
    // 0: The compositor's bar again.
    window.set_full_size_content(0);
    window.commit_frames(1, std::time::Duration::ZERO);
    let image = shot(&compositor);
    let info = windows(&compositor).remove(0);
    assert_near(
        &image,
        (info.x + 150, info.y - 16),
        [255, 255, 255],
        1,
        "the bar comes back",
    );
    let _ = before;
}

/// The buttons keep the same margin and sit on the centre of whatever bar the app declares:
/// 32 pt (a title band like the Terminal's) gives (16, 16), a 52 pt toolbar (26, 26).
#[test]
fn full_size_buttons_follow_the_bar_height_with_a_constant_margin() {
    let compositor = start("decor-bar-heights");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "short", (240, 140));
    window.set_full_size_content(32);
    window.commit_frames(1, std::time::Duration::ZERO);
    let info = windows(&compositor).remove(0);
    let image = shot(&compositor);
    let at = |i: i32| (info.x + 16 + 23 * i, info.y + 16);
    assert_near(&image, at(0), [0xf8, 0x7d, 0x77], 8, "close in a 32 pt bar");
    assert_near(
        &image,
        at(2),
        [0x85, 0xd6, 0x60],
        8,
        "full screen in a 32 pt bar",
    );
    // The app's red 2 pt before the disc (which starts 9 pt from the edge).
    assert_near(
        &image,
        (info.x + 6, info.y + 16),
        [255, 0, 0],
        0,
        "app bar left of the disc",
    );
    window.set_full_size_content(52);
    window.commit_frames(1, std::time::Duration::ZERO);
    let info = windows(&compositor).remove(0);
    let image = shot(&compositor);
    assert_near(
        &image,
        (info.x + 26, info.y + 26),
        [0xf8, 0x7d, 0x77],
        8,
        "close in a 52 pt bar",
    );
}

/// The app moves the window from its toolbar (`xdg_toplevel.move`): only with the button
/// still pressed, and the release leaves it where it is.
#[test]
fn the_app_moves_its_window_from_the_toolbar() {
    let compositor = start("decor-app-move");
    let mut window = TestWindow::open_decorated(&compositor.wayland, "move", (240, 140));
    window.set_full_size_content(52);
    let info = windows(&compositor).remove(0);
    let mut client = compositor.client();
    let (x, y) = (info.x + 150, info.y + 26);
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x, "y": y}))
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": true}),
        )
        .unwrap();
    window.start_move();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x + 60, "y": y + 40}))
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": false}),
        )
        .unwrap();
    let moved = windows(&compositor).remove(0);
    assert_eq!((moved.x, moved.y), (info.x + 60, info.y + 40));
    // After the release the request no longer applies: the window does not follow the pointer.
    window.start_move();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x + 200, "y": y + 100}))
        .unwrap();
    let still = windows(&compositor).remove(0);
    assert_eq!((still.x, still.y), (moved.x, moved.y));
}

/// The arrangement commands: the halves and the fill of the usable area, frame included (the
/// 32 bar above the content); two windows side by side.
#[test]
fn tile_commands_place_the_frame_in_the_usable_area() {
    use silica_control::commands::{WINDOW_ARRANGE_2, WINDOW_TILE_LEFT, WINDOW_TILE_RIGHT};
    let compositor = start("decor-tile");
    let _a = TestWindow::open_decorated(&compositor.wayland, "a", (300, 200));
    let _b = TestWindow::open_decorated(&compositor.wayland, "b", (300, 200));
    let half = (SIZE.0 / 2) as i32;
    let mut client = compositor.client();
    let by_title = |title: &str| {
        windows(&compositor)
            .into_iter()
            .find(|w| w.title.as_deref() == Some(title))
            .unwrap()
    };
    client.call(WINDOW_TILE_LEFT, json!({})).unwrap();
    let b = by_title("b");
    assert_eq!((b.x, b.y), (0, BAR));
    client.call(WINDOW_TILE_RIGHT, json!({})).unwrap();
    let b = by_title("b");
    assert_eq!((b.x, b.y), (half, BAR));
    // Arrange two: "b" (with the focus) on the left, "a" on the right.
    client.call(WINDOW_ARRANGE_2, json!({})).unwrap();
    assert_eq!(by_title("b").x, 0);
    assert_eq!(by_title("a").x, half);
}
