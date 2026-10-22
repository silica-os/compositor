//! Popups never outlive what they belong to (silica-os/mustache#25): a client that goes away
//! with a menu open, a window that closes with one, and a menu that opens beside another one
//! leave no surface and no glass behind, and the app is told when a popup stops being grabbed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::time::{Duration, Instant};

use common::Headless;
use common::client::{APP_ID, PopupSpec, TestWindow};
use serde_json::{Value, json};
use silica_control::commands::{GLASS_LIST, INPUT_CLICK, WINDOW_LIST};
use silica_protocols::glass::client::silica_glass_v1::Material;

const OWNER_APP: &str = "dev.silica.test.menu-owner";

fn glass_surfaces(compositor: &Headless) -> Vec<String> {
    let list = compositor.client().call(GLASS_LIST, Value::Null).unwrap();
    list.as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["surface"].as_str().map(str::to_owned))
        .collect()
}

fn window_count(compositor: &Headless) -> usize {
    let list = compositor.client().call(WINDOW_LIST, Value::Null).unwrap();
    list.as_array().unwrap().len()
}

/// Waits (a client going away is noticed on the compositor's next turn) until `done` holds.
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("still there after 5 s: {what}");
}

fn window_with_glass_popup(compositor: &Headless) -> TestWindow {
    APP_ID.with(|id| *id.borrow_mut() = OWNER_APP.to_owned());
    let mut window = TestWindow::open(&compositor.wayland, "menu-owner");
    APP_ID.with(|id| id.borrow_mut().clear());
    compositor
        .client()
        .call(INPUT_CLICK, json!({"x": 40, "y": 40, "button": "right"}))
        .unwrap();
    window.open_popup(PopupSpec {
        anchor_rect: (10, 10, 1, 1),
        size: (50, 40),
        adjustment: 0,
        grab: true,
        nested: false,
        transparent: false,
    });
    window
        .set_popup_glass(&[(0, 0, 50, 40, 10.0, Material::Thick)])
        .unwrap();
    assert!(
        glass_surfaces(compositor)
            .iter()
            .any(|s| s.starts_with("popup of window")),
        "the popup's glass is not listed"
    );
    window
}

#[test]
fn a_client_that_disconnects_with_a_popup_open_leaves_no_surface_or_glass() {
    let compositor = Headless::start("popup-gone-disconnect");
    let window = window_with_glass_popup(&compositor);
    drop(window);
    eventually("the window of the client that went away", || {
        window_count(&compositor) == 0
    });
    eventually("the glass of its popup", || {
        glass_surfaces(&compositor).is_empty()
    });
}

#[test]
fn a_window_that_closes_with_a_popup_open_takes_its_glass_with_it() {
    let compositor = Headless::start("popup-gone-toplevel");
    let mut window = window_with_glass_popup(&compositor);
    window.destroy_toplevel();
    eventually("the glass of the popup of a closed window", || {
        glass_surfaces(&compositor).is_empty()
    });
}

#[test]
fn a_menu_opened_beside_another_one_ends_the_first() {
    let compositor = Headless::start("popup-sibling");
    APP_ID.with(|id| *id.borrow_mut() = OWNER_APP.to_owned());
    let mut window = TestWindow::open(&compositor.wayland, "menu-owner");
    APP_ID.with(|id| id.borrow_mut().clear());
    compositor
        .client()
        .call(INPUT_CLICK, json!({"x": 40, "y": 40, "button": "right"}))
        .unwrap();
    window.open_grabbing_popup((5, 5), (40, 30));
    assert!(!window.popup_done(), "the first popup closed by itself");
    // The app opens another menu (a toolbar pop-up, the Actions menu) with the first still
    // open: that one loses its grab, so nothing would ever close it unless the compositor does.
    window.open_sibling_grabbing_popup((60, 5), (40, 30));
    assert!(
        window.popup_done(),
        "the first popup was left open without a grab"
    );
}
