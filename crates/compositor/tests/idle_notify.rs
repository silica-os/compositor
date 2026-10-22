//! ext-idle-notify-v1: `idled` after the timeout without input, `resumed` on input.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use common::Headless;
use serde_json::json;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

#[derive(Default)]
struct State {
    notifier: Option<ExtIdleNotifierV1>,
    seat: Option<WlSeat>,
    /// Per notification (0: normal, 1: input only): idle now, and how many events came.
    idle: [bool; 2],
    events: [u32; 2],
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "ext_idle_notifier_v1" => state.notifier = Some(registry.bind(name, 2, qh, ())),
                "wl_seat" => state.seat = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.idle[*index] = matches!(event, ext_idle_notification_v1::Event::Idled);
        state.events[*index] += 1;
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ignore ExtIdleNotifierV1);

fn wait(
    queue: &mut EventQueue<State>,
    state: &mut State,
    what: &str,
    done: impl Fn(&State) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        queue.roundtrip(state).unwrap();
        if done(state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: {:?} after {:?} events",
            state.idle,
            state.events
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn notifications_idle_and_resume() {
    let compositor = Headless::start("idle-notify");
    let stream = UnixStream::connect(&compositor.wayland).unwrap();
    let connection = Connection::from_socket(stream).unwrap();
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).unwrap();
    let notifier = state
        .notifier
        .clone()
        .expect("ext_idle_notifier_v1 is announced");
    let seat = state.seat.clone().unwrap();
    let _normal = notifier.get_idle_notification(300, &seat, &qh, 0usize);
    let _input = notifier.get_input_idle_notification(300, &seat, &qh, 1usize);

    wait(&mut queue, &mut state, "idled", |s| s.idle == [true, true]);
    compositor
        .client()
        .call("input.pointer_move", json!({"x": 10.0, "y": 10.0}))
        .unwrap();
    wait(&mut queue, &mut state, "resumed", |s| {
        s.idle == [false, false]
    });
    assert_eq!(state.events, [2, 2], "one idled and one resumed each");
}
