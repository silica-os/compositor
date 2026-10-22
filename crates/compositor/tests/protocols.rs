//! Every standard protocol apps look for is announced in the registry.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::os::unix::net::UnixStream;

use common::Headless;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

#[derive(Default)]
struct Globals(Vec<String>);

impl Dispatch<WlRegistry, ()> for Globals {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { interface, .. } = event {
            state.0.push(interface);
        }
    }
}

const EXPECTED: &[&str] = &[
    "zwp_primary_selection_device_manager_v1",
    "xdg_activation_v1",
    "wp_presentation",
    "wp_single_pixel_buffer_manager_v1",
    "wp_alpha_modifier_v1",
    "wp_fifo_manager_v1",
    "wp_commit_timing_manager_v1",
    "wp_content_type_manager_v1",
    "zwp_pointer_constraints_v1",
    "zwp_relative_pointer_manager_v1",
    "zwp_pointer_gestures_v1",
    "zwp_text_input_manager_v3",
    "zwp_input_method_manager_v2",
    "zwp_virtual_keyboard_manager_v1",
    "zwp_idle_inhibit_manager_v1",
    "zwp_keyboard_shortcuts_inhibit_manager_v1",
    "xdg_toplevel_icon_manager_v1",
    "xdg_toplevel_tag_manager_v1",
    "xdg_system_bell_v1",
    "zwp_tablet_manager_v2",
    "wp_fractional_scale_manager_v1",
];

#[test]
fn the_standard_protocols_are_announced() {
    let compositor = Headless::start("protocols");
    let stream = UnixStream::connect(&compositor.wayland).unwrap();
    let connection = Connection::from_socket(stream).unwrap();
    let mut queue = connection.new_event_queue();
    let _registry = connection.display().get_registry(&queue.handle(), ());
    let mut globals = Globals::default();
    queue.roundtrip(&mut globals).unwrap();
    let missing: Vec<_> = EXPECTED
        .iter()
        .filter(|name| !globals.0.iter().any(|g| g == *name))
        .collect();
    assert!(missing.is_empty(), "not announced: {missing:?}");
}
