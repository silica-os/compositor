//! xdg-foreign v2: a window of another app becomes the child of an exported window (the
//! portal's Open dialog, a Mustache helper, attached to the app that asked for it). It opens
//! centered over its parent, stays above it, and gives the focus back to it when it closes,
//! not to another window of its own app.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::Headless;
use common::client::{APP_ID, FOREIGN_PARENT, TestWindow};
use serde_json::{Value, json};
use silica_control::commands::{WINDOW_FOCUS, WINDOW_LIST, WindowInfo};

const FILES: &str = "dev.frascella.Mustache";
const VIEWER: &str = "dev.frascella.Preview";

/// The windows on screen, top first.
fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    let list = compositor.client().call(WINDOW_LIST, Value::Null).unwrap();
    serde_json::from_value(list).unwrap()
}

fn titled<'a>(list: &'a [WindowInfo], title: &str) -> &'a WindowInfo {
    list.iter()
        .find(|w| w.title.as_deref() == Some(title))
        .unwrap_or_else(|| panic!("no window {title}: {list:?}"))
}

fn titles(list: &[WindowInfo]) -> Vec<&str> {
    list.iter().filter_map(|w| w.title.as_deref()).collect()
}

fn focused(compositor: &Headless) -> Option<String> {
    windows(compositor)
        .into_iter()
        .find(|w| w.focused)
        .and_then(|w| w.title)
}

fn open_as(compositor: &Headless, app: &str, title: &str, size: (u32, u32)) -> TestWindow {
    APP_ID.with(|id| *id.borrow_mut() = app.to_owned());
    let window = TestWindow::open_sized(&compositor.wayland, title, size);
    APP_ID.with(|id| id.borrow_mut().clear());
    window
}

/// A window of `app` whose parent is the exported window `handle`.
fn open_dialog(compositor: &Headless, app: &str, title: &str, handle: &str) -> TestWindow {
    FOREIGN_PARENT.with(|parent| *parent.borrow_mut() = handle.to_owned());
    let window = open_as(compositor, app, title, (200, 100));
    FOREIGN_PARENT.with(|parent| parent.borrow_mut().clear());
    window
}

/// Another window of the dialog's app, the viewer that exports its window and the dialog
/// attached to it.
fn scene(compositor: &Headless) -> (TestWindow, TestWindow, TestWindow) {
    let files = open_as(compositor, FILES, "files", (300, 200));
    let mut viewer = open_as(compositor, VIEWER, "viewer", (400, 300));
    let handle = viewer.export();
    assert!(!handle.is_empty());
    let dialog = open_dialog(compositor, FILES, "open", &handle);
    (files, viewer, dialog)
}

#[test]
fn a_dialog_attached_with_a_foreign_handle_opens_centered_over_its_parent() {
    let compositor = Headless::start_with("foreign-place", (1000, 700), &[]);
    let (_files, _viewer, _dialog) = scene(&compositor);
    let list = windows(&compositor);
    assert_eq!(titles(&list), ["open", "viewer", "files"]);
    assert_eq!(focused(&compositor).as_deref(), Some("open"));
    let viewer = titled(&list, "viewer");
    let dialog = titled(&list, "open");
    assert_eq!((dialog.width, dialog.height), (200, 100));
    assert_eq!(
        (dialog.x + dialog.width / 2, dialog.y + dialog.height / 2),
        (viewer.x + viewer.width / 2, viewer.y + viewer.height / 2),
        "{list:?}"
    );
}

/// The scene of #180 on the real output: Preview (708 × 500 at 213,267 under a 30 pt bar), the
/// dialog's app with its own window. Returns the viewer, its handle and the files window.
fn big_scene(compositor: &Headless) -> (TestWindow, String, TestWindow) {
    let files = open_as(compositor, FILES, "files", (300, 200));
    let mut viewer = open_as(compositor, VIEWER, "viewer", (708, 500));
    let handle = viewer.export();
    compositor
        .client()
        .call(
            silica_control::commands::WINDOW_MOVE,
            json!({"id": titled(&windows(compositor), "viewer").id, "x": 213, "y": 267}),
        )
        .unwrap();
    (viewer, handle, files)
}

fn assert_centered(compositor: &Headless) {
    let list = windows(compositor);
    let viewer = titled(&list, "viewer");
    let dialog = titled(&list, "open");
    assert_eq!((dialog.width, dialog.height), (1000, 700), "{list:?}");
    assert_eq!(
        (dialog.x + dialog.width / 2, dialog.y + dialog.height / 2),
        (viewer.x + viewer.width / 2, viewer.y + viewer.height / 2),
        "{list:?}"
    );
}

#[test]
fn a_dialog_that_grows_after_its_first_buffer_is_centered_at_its_final_size() {
    let compositor = Headless::start_with("foreign-grow", (1920, 1200), &[]);
    let (_viewer, handle, _files) = big_scene(&compositor);
    let mut dialog = open_dialog(&compositor, FILES, "open", &handle);
    dialog.commit_size((1000, 700));
    assert_centered(&compositor);
}

#[test]
fn a_parent_set_after_the_first_buffer_still_centers_the_dialog() {
    let compositor = Headless::start_with("foreign-late", (1920, 1200), &[]);
    let (_viewer, handle, _files) = big_scene(&compositor);
    let mut dialog = open_as(&compositor, FILES, "open", (1000, 700));
    dialog.set_foreign_parent(&handle);
    assert_centered(&compositor);
}

#[test]
fn the_dialog_stays_above_its_parent() {
    let compositor = Headless::start_with("foreign-above", (1000, 700), &[]);
    let (_files, _viewer, _dialog) = scene(&compositor);
    let viewer = titled(&windows(&compositor), "viewer").id;
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({"id": viewer}))
        .unwrap();
    let list = windows(&compositor);
    assert_eq!(titles(&list), ["open", "viewer", "files"]);
    assert_eq!(focused(&compositor).as_deref(), Some("viewer"));
}

#[test]
fn closing_the_dialog_gives_the_focus_back_to_its_parent() {
    let compositor = Headless::start_with("foreign-close", (1000, 700), &[]);
    let (_files, _viewer, mut dialog) = scene(&compositor);
    // The files window goes over the viewer, then the dialog takes the focus again: without
    // the parent, its app's other window would get the focus back.
    let list = windows(&compositor);
    let files = titled(&list, "files").id;
    let open = titled(&list, "open").id;
    let mut client = compositor.client();
    client.call(WINDOW_FOCUS, json!({"id": files})).unwrap();
    client.call(WINDOW_FOCUS, json!({"id": open})).unwrap();
    assert_eq!(focused(&compositor).as_deref(), Some("open"));
    // The dialog takes in (and acks) the configures of the focus changes first.
    assert!(!dialog.close_requested());
    dialog.destroy_toplevel();
    assert_eq!(focused(&compositor).as_deref(), Some("viewer"));
}

#[test]
fn the_focus_goes_back_to_the_parent_even_if_the_import_went_first() {
    let compositor = Headless::start_with("foreign-import-gone", (1000, 700), &[]);
    let (_files, _viewer, mut dialog) = scene(&compositor);
    // The dialog's process ends: its imported handle may be destroyed before its window.
    dialog.drop_imported();
    dialog.destroy_toplevel();
    assert_eq!(focused(&compositor).as_deref(), Some("viewer"));
}
