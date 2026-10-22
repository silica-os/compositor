//! A drag that starts on a shell layer (the floating screenshot thumbnail): an overlay layer
//! surface that takes the pointer only on a small rectangle carries a file (`text/uri-list`)
//! into a window, as a real Wayland drag and drop.

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
use silica_control::commands::{
    INPUT_POINTER_BUTTON, INPUT_POINTER_MOVE, WINDOW_LIST, WINDOW_TILE_LEFT, WindowInfo,
};

const SIZE: (u32, u32) = (640, 400);
const URIS: &[u8] = b"file:///tmp/Screenshot%202026-10-14%20at%2004.12.33.png\r\n";
const BLUE_BGRA: [u8; 4] = [255, 0, 0, 255];

#[test]
fn a_layer_drags_a_file_into_a_window() {
    let compositor = Headless::start_with("layer-drag", SIZE, &["--background", "7a7b80"]);
    let mut client = compositor.client();
    let mut target = TestWindow::open_sized(&compositor.wayland, "target", (200, 150));
    client.call(WINDOW_TILE_LEFT, json!({})).unwrap();
    target.accept_drops();
    let windows: Vec<WindowInfo> =
        serde_json::from_value(client.call(WINDOW_LIST, Value::Null).unwrap()).unwrap();
    let to = windows
        .iter()
        .find(|w| w.title.as_deref() == Some("target"))
        .unwrap()
        .clone();

    // The thumbnail: 120 × 70 at the bottom right of the overlay.
    let thumb = (500, 310, 120, 70);
    let mut layer = TestWindow::open_overlay(&compositor.wayland, thumb);
    client
        .call(INPUT_POINTER_MOVE, json!({"x": 560, "y": 340}))
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": true}),
        )
        .unwrap();
    assert!(
        layer.pointer_log().presses >= 1,
        "the press reached the layer"
    );
    // Grabbed at its center, as the thumbnail is under the pointer.
    layer.start_drag_at("text/uri-list", URIS, 24, BLUE_BGRA, (12, 12));

    // Over the window: the offer with the file type.
    let (x, y) = (to.x + 60, to.y + 50);
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x, "y": y}))
        .unwrap();
    let mimes = target
        .drag_over()
        .expect("the drag did not enter the window");
    assert_eq!(mimes, vec!["text/uri-list".to_owned()]);
    target.accept_offer("text/uri-list");

    // The icon follows the pointer, its grabbed point under it: it spans 12 pt on each side.
    let shot = common::screenshot(&compositor);
    for (dx, dy) in [(-10, -10), (10, 10), (0, 0)] {
        let icon = common::pixel_in(&shot, SIZE.0, (x + dx) as u32, (y + dy) as u32);
        assert_eq!(&icon[..3], &[0, 0, 255], "drag icon at {dx}, {dy}");
    }
    let past = common::pixel_in(&shot, SIZE.0, (x + 14) as u32, (y + 14) as u32);
    assert_ne!(
        &past[..3],
        &[0, 0, 255],
        "the icon is not anchored at its corner"
    );

    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": false}),
        )
        .unwrap();
    let data = target.receive_drop("text/uri-list", || {
        layer.drag_outcome();
    });
    assert_eq!(data, URIS);
    assert_eq!(
        layer.drag_outcome(),
        Some(true),
        "dnd_finished to the layer"
    );
}

#[test]
fn a_layer_drag_dropped_nowhere_is_cancelled() {
    let compositor = Headless::start_with("layer-drag-cancel", SIZE, &["--background", "7a7b80"]);
    let mut client = compositor.client();
    let mut layer = TestWindow::open_overlay(&compositor.wayland, (500, 310, 120, 70));
    client
        .call(INPUT_POINTER_MOVE, json!({"x": 560, "y": 340}))
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": true}),
        )
        .unwrap();
    layer.start_drag("text/uri-list", URIS, 24, BLUE_BGRA);
    // Over the empty desktop, released: nobody took it.
    client
        .call(INPUT_POINTER_MOVE, json!({"x": 200, "y": 100}))
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": false}),
        )
        .unwrap();
    assert_eq!(layer.drag_outcome(), Some(false), "cancelled to the layer");
}
