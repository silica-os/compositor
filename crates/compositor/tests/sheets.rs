//! Sheets: a window that is a modal dialog of its parent (xdg-dialog `set_modal`, as a
//! Vetro app's sheets) opens centred over its parent, stays above it, and keeps the focus:
//! asking for the parent gives the focus to the sheet.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::Headless;
use common::client::{APP_ID, TestWindow};
use serde_json::{Value, json};
use silica_control::commands::{WINDOW_FOCUS, WINDOW_LIST, WindowInfo};

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    let list = compositor.client().call(WINDOW_LIST, Value::Null).unwrap();
    serde_json::from_value(list).unwrap()
}

fn titled<'a>(list: &'a [WindowInfo], title: &str) -> &'a WindowInfo {
    list.iter()
        .find(|w| w.title.as_deref() == Some(title))
        .unwrap_or_else(|| panic!("no window {title}: {list:?}"))
}

fn focused(compositor: &Headless) -> Option<String> {
    windows(compositor)
        .into_iter()
        .find(|w| w.focused)
        .and_then(|w| w.title)
}

#[test]
fn a_modal_child_is_a_sheet_centred_on_its_parent_that_keeps_the_focus() {
    let compositor = Headless::start_with("sheets", (1000, 700), &[]);
    APP_ID.with(|id| *id.borrow_mut() = "dev.silica.VetroGallery".to_owned());
    let mut doc = TestWindow::open_sized(&compositor.wayland, "document", (600, 400));
    doc.open_child("sheet", (300, 150), true);
    APP_ID.with(|id| id.borrow_mut().clear());
    let list = windows(&compositor);
    let parent = titled(&list, "document");
    let sheet = titled(&list, "sheet");
    assert_eq!((sheet.width, sheet.height), (300, 150));
    assert_eq!(
        (sheet.x + sheet.width / 2, sheet.y + sheet.height / 2),
        (parent.x + parent.width / 2, parent.y + parent.height / 2),
        "{list:?}"
    );
    assert_eq!(list.first().and_then(|w| w.title.as_deref()), Some("sheet"));
    assert_eq!(focused(&compositor).as_deref(), Some("sheet"));
    // The parent cannot take the focus while its sheet shows.
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({"id": parent.id}))
        .unwrap();
    assert_eq!(focused(&compositor).as_deref(), Some("sheet"));
}

#[test]
fn a_child_that_is_not_modal_takes_its_own_place_and_lets_the_parent_focus() {
    let compositor = Headless::start_with("sheets-plain", (1000, 700), &[]);
    let mut doc = TestWindow::open_sized(&compositor.wayland, "document", (600, 400));
    doc.open_child("info", (300, 150), false);
    let list = windows(&compositor);
    let parent = titled(&list, "document");
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({"id": parent.id}))
        .unwrap();
    assert_eq!(focused(&compositor).as_deref(), Some("document"));
}

#[test]
fn a_sheet_follows_its_window_when_it_moves() {
    let compositor = Headless::start_with("sheets-move", (1000, 700), &[]);
    let mut doc = TestWindow::open_sized(&compositor.wayland, "document", (600, 400));
    doc.open_child("sheet", (300, 150), true);
    let before = windows(&compositor);
    let parent = titled(&before, "document");
    let sheet = titled(&before, "sheet");
    let (dx, dy) = (sheet.x - parent.x, sheet.y - parent.y);
    compositor
        .client()
        .call(
            silica_control::commands::WINDOW_MOVE,
            json!({"id": parent.id, "x": parent.x + 40, "y": parent.y + 25}),
        )
        .unwrap();
    let after = windows(&compositor);
    let parent = titled(&after, "document");
    let sheet = titled(&after, "sheet");
    assert_eq!(
        (sheet.x - parent.x, sheet.y - parent.y),
        (dx, dy),
        "{after:?}"
    );
}
