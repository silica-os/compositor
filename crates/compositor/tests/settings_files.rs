//! The user's settings: those of the Settings service (silica-daemon's, here on a private bus,
//! silica-os/silica-desktop#240) and the files still in `$XDG_CONFIG_HOME/silica` (`power.json`,
//! `displays.json`). The compositor applies them at start and when they change.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Headless, SettingsBus};
use serde_json::{Value, json};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use zbus::zvariant::Value as Variant;

/// Writes `name` like Settings does: a temporary file renamed over the real one.
fn write_setting(compositor: &Headless, name: &str, value: &Value) {
    let dir = compositor.dir.join("config/silica");
    std::fs::create_dir_all(&dir).unwrap();
    let temporary = dir.join(format!("{name}.tmp"));
    std::fs::write(&temporary, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    std::fs::rename(&temporary, dir.join(name)).unwrap();
}

/// Calls `command` until `check` accepts the result (5 s at most).
fn wait_for(compositor: &Headless, command: &str, check: impl Fn(&Value) -> bool) -> Value {
    let mut client = compositor.client();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = client.call(command, Value::Null).unwrap();
        if check(&value) {
            return value;
        }
        assert!(Instant::now() < deadline, "{command}: still {value}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Default)]
struct Keyboard {
    repeat: Vec<(i32, i32)>,
    keymaps: usize,
}

impl Dispatch<WlRegistry, ()> for Keyboard {
    fn event(
        _: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "wl_seat"
        {
            let seat: WlSeat = registry.bind(name, 7, qh, ());
            seat.get_keyboard(qh, ());
        }
    }
}

impl Dispatch<WlSeat, ()> for Keyboard {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlKeyboard, ()> for Keyboard {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::RepeatInfo { rate, delay } => state.repeat.push((rate, delay)),
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                ..
            } => state.keymaps += 1,
            _ => {}
        }
    }
}

fn connect(socket: &Path) -> (Connection, wayland_client::EventQueue<Keyboard>, Keyboard) {
    let connection = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut keyboard = Keyboard::default();
    queue.roundtrip(&mut keyboard).unwrap();
    queue.roundtrip(&mut keyboard).unwrap();
    (connection, queue, keyboard)
}

/// The compositor on `bus`, which has the Settings service.
fn with_settings(name: &str, bus: &SettingsBus) -> Headless {
    Headless::start_with_env(name, (common::WIDTH, common::HEIGHT), &[], &bus.env())
}

#[test]
fn keyboard_settings_set_repeat_and_sources_live() {
    let bus = SettingsBus::start(&[
        ("keyboard.repeatDelay", Variant::I64(375)),
        ("keyboard.repeatRate", Variant::I64(11)),
        ("keyboard.layouts", Variant::from(Vec::<String>::new())),
    ]);
    let compositor = with_settings("keyboard-settings", &bus);
    let (_connection, mut queue, mut keyboard) = connect(&compositor.wayland);
    // The defaults first (no file).
    let state = compositor
        .client()
        .call("keyboard.state", Value::Null)
        .unwrap();
    assert_eq!(state["repeatDelay"], 375);
    assert_eq!(state["repeatRate"], 11);
    assert_eq!(keyboard.repeat, [(11, 375)]);
    assert_eq!(keyboard.keymaps, 1);
    // With one source the shortcut does not switch.
    let next = compositor
        .client()
        .call("keyboard.next_source", Value::Null)
        .unwrap();
    assert_eq!(next["switched"], false);

    bus.set("keyboard.repeatDelay", Variant::I64(250));
    bus.set(
        "keyboard.layouts",
        Variant::from(vec!["us".to_owned(), "it".to_owned()]),
    );
    bus.set("keyboard.repeatRate", Variant::I64(30));
    let state = wait_for(&compositor, "keyboard.state", |s| {
        s["repeatRate"] == 30 && s["layouts"] == json!(["us", "it"])
    });
    assert_eq!(state["repeatDelay"], 250);
    assert_eq!(state["layouts"], json!(["us", "it"]));
    assert_eq!(state["active"], 0);
    // The bound keyboard got the new repeat and a new keymap.
    queue.roundtrip(&mut keyboard).unwrap();
    assert_eq!(keyboard.repeat.last(), Some(&(30, 250)));
    assert!(keyboard.keymaps >= 2);

    let next = compositor
        .client()
        .call("keyboard.next_source", Value::Null)
        .unwrap();
    assert_eq!(next, json!({"switched": true, "active": 1}));
    // The input menu chooses a source by its index; past the last one nothing changes.
    let select = |index: u64| {
        compositor
            .client()
            .call("keyboard.select_source", json!({ "index": index }))
            .unwrap()
    };
    assert_eq!(select(0), json!({"switched": true, "active": 0}));
    assert_eq!(select(3), json!({"switched": false, "active": 0}));
    assert_eq!(select(1), json!({"switched": true, "active": 1}));

    // A layout xkb does not know keeps the keymap; the repeat still applies.
    bus.set(
        "keyboard.layouts",
        Variant::from(vec!["us".to_owned(), "nosuchlayout".to_owned()]),
    );
    bus.set("keyboard.repeatRate", Variant::I64(0));
    bus.set("keyboard.repeatDelay", Variant::I64(600));
    let state = wait_for(&compositor, "keyboard.state", |s| s["repeatDelay"] == 600);
    assert_eq!(state["repeatRate"], 0);
    assert_eq!(state["layouts"], json!(["us", "it"]));

    // Back to the defaults.
    for key in [
        "keyboard.repeatDelay",
        "keyboard.repeatRate",
        "keyboard.layouts",
    ] {
        bus.reset(key);
    }
    let state = wait_for(&compositor, "keyboard.state", |s| {
        s["repeatDelay"] == 375 && s["layouts"] == json!([])
    });
    assert_eq!(state["repeatRate"], 11);
}

#[test]
fn displays_are_listed_configured_and_saved() {
    let compositor = Headless::start("displays");
    let mut client = compositor.client();
    let list = client.call("output.list", Value::Null).unwrap();
    let outputs = list.as_array().unwrap();
    assert_eq!(outputs.len(), 1);
    let output = &outputs[0];
    let connector = output["connector"].as_str().unwrap().to_owned();
    assert_eq!(output["enabled"], true);
    assert_eq!(output["scale"], 1.0);
    assert_eq!(output["transform"], "normal");
    assert_eq!(output["current"]["width"], common::WIDTH);

    let configured = client
        .call(
            "output.configure",
            json!({"connector": connector, "scale": 2, "transform": "90"}),
        )
        .unwrap();
    assert_eq!(configured["scale"], 2.0);
    assert_eq!(configured["transform"], "90");
    // Saved for this display, by its identity (the connector without EDID).
    let saved: Value = serde_json::from_slice(
        &std::fs::read(compositor.dir.join("config/silica/displays.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["outputs"][&connector]["scale"], 2.0);
    assert_eq!(saved["outputs"][&connector]["transform"], "90");

    // Bad parameters and modes the display does not have are refused.
    for bad in [
        json!({"scale": 1.1}),
        json!({"transform": "sideways"}),
        json!({"mode": {"width": 7, "height": 7}}),
        json!({"connector": "nope", "scale": 1}),
    ] {
        assert!(
            client.call("output.configure", bad.clone()).is_err(),
            "{bad}"
        );
    }

    // A change made to the file applies at once.
    write_setting(
        &compositor,
        "displays.json",
        &json!({"outputs": {connector.clone(): {"scale": 1.5, "transform": "normal"}}}),
    );
    let list = wait_for(&compositor, "output.list", |l| l[0]["scale"] == 1.5);
    assert_eq!(list[0]["transform"], "normal");

    // Headless has no gamma: the night light is accepted but not applied.
    let night = client
        .call("output.color_temperature", json!({"kelvin": 3400}))
        .unwrap();
    assert_eq!(night, json!({"kelvin": 3400, "applied": false}));
    assert!(
        client
            .call("output.color_temperature", json!({"kelvin": 200}))
            .is_err()
    );
}

#[test]
fn power_json_turns_the_display_off_until_input() {
    let compositor = Headless::start("power-json");
    let state = compositor.client().call("idle.state", Value::Null).unwrap();
    assert_eq!(state["displayOff"], false);
    assert_eq!(state["displayOffMinutes"], Value::Null);

    // 0.02 minutes: 1.2 s.
    write_setting(
        &compositor,
        "power.json",
        &json!({"displayOffMinutes": 0.02, "preventSleep": true}),
    );
    let state = wait_for(&compositor, "idle.state", |s| s["displayOff"] == true);
    assert_eq!(state["preventSleep"], true);
    assert!(state["idleSeconds"].as_u64().unwrap() >= 1);
    // Input wakes it.
    compositor
        .client()
        .call("input.pointer_move", json!({"x": 10.0, "y": 10.0}))
        .unwrap();
    let state = compositor.client().call("idle.state", Value::Null).unwrap();
    assert_eq!(state["displayOff"], false);
    assert_eq!(state["idleSeconds"], 0);
}

#[test]
fn input_settings_are_applied_live() {
    let bus = SettingsBus::start(&[
        ("input.mouse.speed", Variant::F64(0.0)),
        ("input.mouse.scrollSpeed", Variant::F64(1.0)),
        ("input.mouse.naturalScroll", Variant::Bool(false)),
        ("input.mouse.secondaryClick", Variant::from("right")),
        ("input.touchpad.speed", Variant::F64(0.0)),
        ("input.touchpad.scrollSpeed", Variant::F64(1.0)),
        ("input.touchpad.secondaryClick", Variant::from("twoFinger")),
        ("input.touchpad.tapToClick", Variant::Bool(false)),
        ("input.touchpad.disableWhileTyping", Variant::Bool(true)),
    ]);
    let compositor = with_settings("input-settings", &bus);
    // The service's defaults, once it has answered.
    let state = wait_for(&compositor, "input.state", |s| s["mouse"]["speed"] == 0.0);
    assert_eq!(state["mouse"]["scrollSpeed"], 1.0);
    assert_eq!(state["touchpad"]["disableWhileTyping"], true);
    // Headless has no libinput devices.
    assert_eq!(state["devices"], json!({"mouse": false, "touchpad": false}));

    bus.set("input.mouse.naturalScroll", Variant::Bool(true));
    bus.set("input.mouse.secondaryClick", Variant::from("left"));
    bus.set("input.touchpad.tapToClick", Variant::Bool(true));
    bus.set("input.touchpad.scrollSpeed", Variant::F64(1.5));
    bus.set("input.mouse.speed", Variant::F64(0.25));
    let state = wait_for(&compositor, "input.state", |s| s["mouse"]["speed"] == 0.25);
    assert_eq!(state["mouse"]["naturalScroll"], true);
    assert_eq!(state["mouse"]["leftHanded"], true);
    assert_eq!(state["touchpad"]["tapToClick"], true);
    assert_eq!(state["touchpad"]["secondaryClick"], "twoFinger");
    assert_eq!(state["touchpad"]["scrollSpeed"], 1.5);

    // Back to the default.
    bus.reset("input.mouse.speed");
    wait_for(&compositor, "input.state", |s| s["mouse"]["speed"] == 0.0);
}

#[test]
fn without_the_settings_service_the_defaults_apply() {
    let compositor = Headless::start("no-settings");
    let state = compositor
        .client()
        .call("keyboard.state", Value::Null)
        .unwrap();
    assert_eq!(state["repeatDelay"], 375);
    let state = compositor
        .client()
        .call("input.state", Value::Null)
        .unwrap();
    assert_eq!(state["mouse"]["speed"], Value::Null);
    // The files of the settings folder no longer drive what moved to the service.
    write_setting(&compositor, "keyboard.json", &json!({"repeatDelay": 250}));
    std::thread::sleep(Duration::from_millis(300));
    let state = compositor
        .client()
        .call("keyboard.state", Value::Null)
        .unwrap();
    assert_eq!(state["repeatDelay"], 375);
}

#[test]
fn input_type_follows_the_active_keymap_with_a_temporary_key_fallback() {
    let bus = SettingsBus::start(&[("keyboard.layouts", Variant::from(Vec::<String>::new()))]);
    let compositor = with_settings("keyboard-type", &bus);
    let (_connection, mut queue, mut keyboard) = connect(&compositor.wayland);
    bus.set("keyboard.layouts", Variant::from(vec!["it".to_owned()]));
    wait_for(&compositor, "keyboard.state", |s| {
        s["layouts"] == json!(["it"])
    });
    queue.roundtrip(&mut keyboard).unwrap();
    // Characters the Italian layout has (the ones that put "ò" for ";" on US keycodes), some
    // with AltGr: no error, and no keymap change.
    let before = keyboard.keymaps;
    compositor
        .client()
        .call("input.type", json!({"text": "clear; ls @ò€"}))
        .unwrap();
    queue.roundtrip(&mut keyboard).unwrap();
    assert_eq!(keyboard.keymaps, before);
    // A character no layout has: a temporary keymap is sent, then the real one again.
    compositor
        .client()
        .call("input.type", json!({"text": "aǆb"}))
        .unwrap();
    queue.roundtrip(&mut keyboard).unwrap();
    assert_eq!(keyboard.keymaps, before + 2);
    // A control character other than the keys cannot be typed, and nothing is pressed.
    assert!(
        compositor
            .client()
            .call("input.type", json!({"text": "a\u{1}"}))
            .is_err()
    );
    assert_eq!(
        compositor
            .client()
            .call("keyboard.state", Value::Null)
            .unwrap()["layouts"],
        json!(["it"])
    );
}

#[test]
fn hot_corners_run_their_action_when_the_pointer_arrives() {
    let bus = SettingsBus::start(&[("hotcorners.topLeft", Variant::from("none"))]);
    let compositor = with_settings("hot-corners", &bus);
    let mut client = compositor.client();
    let pointer = |client: &mut silica_control::Client, x: f64, y: f64| {
        client
            .call("input.pointer_move", json!({"x": x, "y": y}))
            .unwrap();
    };
    // An app's menu is open: the launcher, like every shell panel, ends it.
    let mut window = common::client::TestWindow::open(&compositor.wayland, "menu");
    let frame = &client.call("window.list", Value::Null).unwrap()[0];
    let at = (
        frame["x"].as_f64().unwrap_or_default() + 20.0,
        frame["y"].as_f64().unwrap_or_default() + 20.0,
    );
    client
        .call(
            "input.click",
            json!({"x": at.0, "y": at.1, "button": "right"}),
        )
        .unwrap();
    window.open_grabbing_popup((5, 5), (40, 30));
    // By default, the corner does nothing.
    pointer(&mut client, 160.0, 120.0);
    pointer(&mut client, 0.0, 0.0);
    assert!(!window.popup_done());
    bus.set("hotcorners.topLeft", Variant::from("launcher"));
    // Applied as it lands: arrive again until the corner is armed with its action.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pointer(&mut client, 160.0, 120.0);
        pointer(&mut client, 0.0, 0.0);
        if window.popup_done() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the corner did not run its action"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
