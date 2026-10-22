//! Screenshots: the screen, a region, a single window (⇧⌘3, ⇧⌘4); the temporary PNG files
//! that the shell shows as a thumbnail and then moves to their place; the pointer drawn in,
//! the window's shadow and the clipboard flag.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::client::{FIRST_WINDOW_AT, TestWindow};
use common::{Headless, decode, pixel_in};
use serde_json::{Value, json};
use silica_control::commands::{
    INPUT_POINTER_MOVE, SCREENSHOT_REGION, SCREENSHOT_SCREEN, SCREENSHOT_SCREEN_CLIPBOARD,
    SCREENSHOT_SETTINGS, SCREENSHOT_WINDOW_AT,
};

fn png_size(path: &str) -> (u32, u32) {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let reader = decoder.read_info().unwrap();
    let info = reader.info();
    (info.width, info.height)
}

/// Runs a capture command; the size of the PNG it wrote (removed afterwards) and its reply.
fn capture(client: &mut silica_control::Client, id: &str, params: Value) -> ((u32, u32), Value) {
    let reply = client.call(id, params).unwrap();
    let path = reply["path"].as_str().unwrap().to_owned();
    let size = png_size(&path);
    assert_eq!(
        size,
        (
            reply["width"].as_u64().unwrap() as u32,
            reply["height"].as_u64().unwrap() as u32
        )
    );
    std::fs::remove_file(&path).unwrap();
    (size, reply)
}

/// The pixels of a capture, decoded (the file is removed).
fn capture_pixels(client: &mut silica_control::Client, id: &str, params: Value) -> Vec<u8> {
    let reply = client.call(id, params).unwrap();
    let path = reply["path"].as_str().unwrap().to_owned();
    let pixels = decode(&std::fs::read(&path).unwrap()).1;
    std::fs::remove_file(&path).unwrap();
    pixels
}

#[test]
fn screen_region_and_window_captures_write_png_files() {
    let compositor = Headless::start_with("screenshots", (640, 400), &[]);
    let _window = TestWindow::open_decorated(&compositor.wayland, "to capture", (200, 120));
    let mut client = compositor.client();
    assert_eq!(
        capture(&mut client, SCREENSHOT_SCREEN, Value::Null).0,
        (640, 400)
    );
    assert_eq!(
        capture(
            &mut client,
            SCREENSHOT_REGION,
            json!({"x": 10, "y": 20, "width": 100, "height": 50})
        )
        .0,
        (100, 50)
    );
    // The window without its shadow: the frame with its 32 bar, 200 × (120 + 32).
    let at = f64::from(FIRST_WINDOW_AT) + 10.0;
    assert_eq!(
        capture(
            &mut client,
            SCREENSHOT_WINDOW_AT,
            json!({"x": at, "y": at, "shadow": false})
        )
        .0,
        (200, 152)
    );
    // With its shadow (the default): the frame plus a transparent margin of 22 / 23 on the
    // sides, 15 on top and 30 at the bottom (SCREENSHOT-2).
    let ((w, h), reply) = capture(&mut client, SCREENSHOT_WINDOW_AT, json!({"x": at, "y": at}));
    assert_eq!((w, h), (200 + 22 + 23, 152 + 15 + 30), "with the shadow");
    assert_eq!(reply["clipboard"], json!(false));
    // Outside every window: an error, no file; with or_screen, the whole screen.
    assert!(
        client
            .call(SCREENSHOT_WINDOW_AT, json!({"x": 630, "y": 390}))
            .is_err()
    );
    assert_eq!(
        capture(
            &mut client,
            SCREENSHOT_WINDOW_AT,
            json!({"x": 630, "y": 390, "or_screen": true})
        )
        .0,
        (640, 400)
    );
    // Unknown parameters are refused.
    assert!(
        client
            .call(SCREENSHOT_SCREEN, json!({"cursor": true}))
            .is_err()
    );
}

#[test]
fn the_window_shadow_is_transparent_around_the_frame() {
    let compositor = Headless::start_with("screenshot-shadow", (640, 400), &[]);
    let _window = TestWindow::open_decorated(&compositor.wayland, "shadowed", (200, 120));
    let mut client = compositor.client();
    let at = f64::from(FIRST_WINDOW_AT) + 10.0;
    let reply = client
        .call(SCREENSHOT_WINDOW_AT, json!({"x": at, "y": at}))
        .unwrap();
    let path = reply["path"].as_str().unwrap().to_owned();
    let width = reply["width"].as_u64().unwrap() as u32;
    let height = reply["height"].as_u64().unwrap() as u32;
    let pixels = decode(&std::fs::read(&path).unwrap()).1;
    std::fs::remove_file(&path).unwrap();
    // The bottom-right corner is beyond the frame: shadow or nothing, never opaque.
    let corner = pixel_in(&pixels, width, width - 1, height - 1);
    assert!(corner[3] < 255, "corner {corner:?}");
    // The margins are exactly 22 / 23 wide on the sides, 15 on top and 30 at the bottom: the
    // frame starts and ends there, opaque in the middle of each edge.
    assert_eq!((width, height), (245, 197));
    let mid_y = 15 + 152 / 2;
    assert_eq!(pixel_in(&pixels, width, 22 + 100, 15 + 80)[3], 255);
    assert!(pixel_in(&pixels, width, 5, mid_y)[3] < 255, "left margin");
    assert!(
        pixel_in(&pixels, width, width - 5, mid_y)[3] < 255,
        "right margin"
    );
    assert!(
        pixel_in(&pixels, width, width / 2, 5)[3] < 255,
        "top margin"
    );
    assert!(
        pixel_in(&pixels, width, width / 2, height - 5)[3] < 255,
        "bottom margin"
    );
    // Somewhere in the middle: the window, opaque.
    let middle = pixel_in(&pixels, width, width / 2, height / 2);
    assert_eq!(middle[3], 255, "middle {middle:?}");
}

#[test]
fn the_pointer_is_drawn_in_when_asked_or_set() {
    let compositor = Headless::start_with("screenshot-pointer", (320, 200), &[]);
    let mut client = compositor.client();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": 100, "y": 80}))
        .unwrap();
    let plain = capture_pixels(&mut client, SCREENSHOT_SCREEN, Value::Null);
    let with = capture_pixels(&mut client, SCREENSHOT_SCREEN, json!({"pointer": true}));
    // The arrow's tip (white outline) and its black body just below it.
    assert_ne!(
        pixel_in(&plain, 320, 101, 83),
        pixel_in(&with, 320, 101, 83)
    );
    assert_eq!(&pixel_in(&with, 320, 101, 83)[..3], &[0, 0, 0]);
    assert_eq!(&pixel_in(&with, 320, 100, 80)[..3], &[255, 255, 255]);
    // Far from the pointer nothing changes.
    assert_eq!(pixel_in(&plain, 320, 10, 10), pixel_in(&with, 320, 10, 10));

    // The setting applies to the shortcut (no parameters).
    client
        .call(SCREENSHOT_SETTINGS, json!({"pointer": true}))
        .unwrap();
    let set = capture_pixels(&mut client, SCREENSHOT_SCREEN, Value::Null);
    assert_eq!(&pixel_in(&set, 320, 101, 83)[..3], &[0, 0, 0]);
    // A region: the pointer at its place inside the region.
    let region = capture_pixels(
        &mut client,
        SCREENSHOT_REGION,
        json!({"x": 90, "y": 70, "width": 40, "height": 40}),
    );
    assert_eq!(&pixel_in(&region, 40, 11, 13)[..3], &[0, 0, 0]);
    // An explicit false wins over the setting.
    let off = capture_pixels(&mut client, SCREENSHOT_SCREEN, json!({"pointer": false}));
    assert_eq!(pixel_in(&off, 320, 101, 83), pixel_in(&plain, 320, 101, 83));
}

#[test]
fn the_clipboard_shortcut_marks_the_capture() {
    let compositor = Headless::start_with("screenshot-clipboard", (320, 200), &[]);
    let mut client = compositor.client();
    let (size, reply) = capture(&mut client, SCREENSHOT_SCREEN_CLIPBOARD, Value::Null);
    assert_eq!(size, (320, 200));
    assert_eq!(reply["clipboard"], json!(true));
    let (_, reply) = capture(
        &mut client,
        SCREENSHOT_REGION,
        json!({"x": 0, "y": 0, "width": 10, "height": 10, "clipboard": true}),
    );
    assert_eq!(reply["clipboard"], json!(true));
}
