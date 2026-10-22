//! Drag between apps (`wl_data_device`): a window drags a list of files (`text/uri-list`)
//! onto another, the compositor draws the icon on the pointer, the target receives the data on the
//! drop and the icon disappears.

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
    INPUT_POINTER_BUTTON, INPUT_POINTER_MOVE, WINDOW_LIST, WINDOW_TILE_LEFT, WINDOW_TILE_RIGHT,
    WindowInfo,
};

const SIZE: (u32, u32) = (640, 400);
const GREEN_BGRA: [u8; 4] = [0, 255, 0, 255];
const URIS: &[u8] = b"file:///tmp/test.txt\r\n";

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

fn by_title(compositor: &Headless, title: &str) -> WindowInfo {
    windows(compositor)
        .into_iter()
        .find(|w| w.title.as_deref() == Some(title))
        .unwrap()
}

#[test]
fn a_drag_carries_uris_to_another_window_with_its_icon() {
    let compositor = Headless::start_with("drag-uris", SIZE, &["--background", "7a7b80"]);
    let mut client = compositor.client();
    let mut source = TestWindow::open_sized(&compositor.wayland, "source", (200, 150));
    client.call(WINDOW_TILE_LEFT, json!({})).unwrap();
    let mut target = TestWindow::open_sized(&compositor.wayland, "target", (200, 150));
    client.call(WINDOW_TILE_RIGHT, json!({})).unwrap();
    target.accept_drops();
    let from = by_title(&compositor, "source");
    let to = by_title(&compositor, "target");

    // Press in the source, then the drag with its serial.
    client
        .call(
            INPUT_POINTER_MOVE,
            json!({"x": from.x + 50, "y": from.y + 50}),
        )
        .unwrap();
    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": true}),
        )
        .unwrap();
    source.start_drag("text/uri-list", URIS, 20, GREEN_BGRA);

    let (x, y) = (to.x + 80, to.y + 60);
    client
        .call(INPUT_POINTER_MOVE, json!({"x": x, "y": y}))
        .unwrap();
    let mimes = target.drag_over().expect("the drag did not enter");
    assert_eq!(mimes, vec!["text/uri-list".to_owned()]);
    target.accept_offer("text/uri-list");

    // The icon follows the pointer, above the target.
    let shot = common::screenshot(&compositor);
    let icon = pixel_in(&shot, SIZE.0, (x + 10) as u32, (y + 10) as u32);
    assert_eq!(&icon[..3], &[0, 255, 0], "drag icon");

    client
        .call(
            INPUT_POINTER_BUTTON,
            json!({"button": "left", "pressed": false}),
        )
        .unwrap();
    let data = target.receive_drop("text/uri-list", || {
        source.drag_outcome();
    });
    assert_eq!(data, URIS);
    assert_eq!(
        source.drag_outcome(),
        Some(true),
        "dnd_finished to the source"
    );

    // Dropped: the icon is gone.
    let shot = common::screenshot(&compositor);
    let after = pixel_in(&shot, SIZE.0, (x + 10) as u32, (y + 10) as u32);
    assert_ne!(&after[..3], &[0, 255, 0], "the icon stays after the drop");
}
