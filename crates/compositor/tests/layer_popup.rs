//! Menus and popovers of a shell layer as `xdg_popup` surfaces above it
//! (`zwlr_layer_surface_v1.get_popup`, silica-os/silica-desktop#222): placed inside the output,
//! drawn with their own glass, closed by a press outside.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::Headless;
use common::client::{PopupSpec, TestWindow};
use serde_json::{Value, json};
use silica_control::commands::{GLASS_LIST, INPUT_CLICK};
use silica_protocols::glass::client::silica_glass_v1::Material;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;

/// A transparent full-screen overlay, like the Faro layer.
fn layer(compositor: &Headless) -> TestWindow {
    TestWindow::open_layer(
        &compositor.wayland,
        Layer::Overlay,
        "test-faro",
        None,
        |_, _, _, _| [0; 4],
    )
}

fn click(compositor: &Headless, x: i32, y: i32) {
    compositor
        .client()
        .call(INPUT_CLICK, json!({"x": x, "y": y, "button": "left"}))
        .unwrap();
}

fn spec(anchor_rect: (i32, i32, u32, u32), size: (u32, u32), adjustment: u32) -> PopupSpec {
    PopupSpec {
        anchor_rect,
        size,
        adjustment,
        grab: false,
        nested: false,
        transparent: false,
    }
}

#[test]
fn a_layer_popup_past_the_right_edge_slides_back_on_the_output() {
    let compositor = Headless::start("lp-slide");
    let mut faro = layer(&compositor);
    // The output is 320 wide: 200 wide from x = 250 does not fit.
    faro.open_popup(spec((250, 10, 1, 1), (200, 30), 1));
    let configure = *faro.popup_configures().last().expect("no configure");
    assert_eq!((configure.2, configure.3), (200, 30));
    assert!(configure.0 < 250, "did not slide: {configure:?}");
    assert!(configure.0 + configure.2 <= 320, "leaves the output");
    assert!(configure.0 >= 0);
}

#[test]
fn a_layer_popup_past_the_bottom_edge_flips_above_its_anchor() {
    let compositor = Headless::start("lp-flip");
    let mut faro = layer(&compositor);
    faro.open_popup(spec((10, 150, 10, 10), (40, 80), 8));
    let configure = *faro.popup_configures().last().expect("no configure");
    assert_eq!((configure.2, configure.3), (40, 80));
    assert_eq!(configure.1, 150 - 80, "did not flip: {configure:?}");
}

#[test]
fn a_layer_popup_declares_its_own_glass() {
    let compositor = Headless::start("lp-glass");
    let mut faro = layer(&compositor);
    faro.open_popup(PopupSpec {
        transparent: true,
        ..spec((10, 10, 1, 1), (50, 40), 0)
    });
    faro.set_popup_glass(&[(0, 0, 50, 40, 10.0, Material::Thick)])
        .unwrap();
    let configure = *faro.popup_configures().last().unwrap();
    let list = compositor.client().call(GLASS_LIST, Value::Null).unwrap();
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["surface"] == "popup of layer test-faro")
        .unwrap_or_else(|| panic!("the popup's glass is not listed: {list}"));
    assert_eq!(entry["regions"][0]["width"], 50, "{entry}");
    assert_eq!(entry["x"], configure.0, "{entry}");
    assert_eq!(entry["y"], configure.1, "{entry}");
}

#[test]
fn a_press_outside_a_layer_popup_closes_it_and_inside_does_not() {
    let compositor = Headless::start("lp-dismiss");
    let mut faro = layer(&compositor);
    // The press that gives the serial for the grab.
    click(&compositor, 5, 5);
    faro.open_popup(PopupSpec {
        grab: true,
        ..spec((100, 60, 1, 1), (40, 30), 0)
    });
    let configure = *faro.popup_configures().last().unwrap();
    click(&compositor, configure.0 + 10, configure.1 + 10);
    assert!(!faro.popup_done(), "a press inside closed the popup");
    click(&compositor, 5, 5);
    assert!(faro.popup_done(), "a press outside left the popup open");
}
