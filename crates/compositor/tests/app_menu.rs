//! The apps' menus in the global menu bar (silica-menu-v1, silica-os/silica-desktop#25): an app
//! publishes its model, the shell receives the frontmost app's model (silica-shell-v1
//! version 8) and its choice goes back to the app.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{BufRead, BufReader, Read};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::Headless;
use common::client::{APP_ID, TestWindow};
use serde_json::{Value, json};
use silica_control::commands::{
    APP_HIDE, APP_HIDE_OTHERS, APP_MENU, APP_MENU_ACTIVATE, APP_SHOW_ALL, WINDOW_FOCUS,
    WINDOW_LIST, WindowInfo,
};
use silica_protocols::shell::client::silica_shell_manager_v1::{self, SilicaShellManagerV1};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

const CHILD_SOCKET: &str = "SILICA_TEST_MENU_SOCKET";
const CHILD_LISTEN: &str = "SILICA_TEST_MENU_LISTEN";

/// The item the shell child chooses when it sees it.
const CHOSEN: u32 = 7;

#[derive(Default)]
struct Shell {
    manager: Option<SilicaShellManagerV1>,
    /// The `app_menu` events: app id, serial, model (empty without one).
    menus: Vec<(String, u32, String)>,
}

impl Dispatch<WlRegistry, ()> for Shell {
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
            && interface == "silica_shell_manager_v1"
        {
            state.manager = Some(registry.bind(name, 8, qh, ()));
        }
    }
}

impl Dispatch<SilicaShellManagerV1, ()> for Shell {
    fn event(
        state: &mut Self,
        _: &SilicaShellManagerV1,
        event: silica_shell_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let silica_shell_manager_v1::Event::AppMenu {
            app_id,
            serial,
            fd,
            size,
        } = event
        {
            let mut file = std::fs::File::from(fd);
            let mut model = String::new();
            file.read_to_string(&mut model).unwrap();
            assert_eq!(model.len(), size as usize);
            // The file is sealed: the shell cannot change what the others read.
            assert!(std::io::Write::write_all(&mut file, b"x").is_err());
            state.menus.push((app_id, serial, model));
        }
    }
}

/// The shell child: listens for a while, chooses `CHOSEN` the first time it shows up, then
/// prints the menus received, one per line.
fn shell_child(socket: &str, ms: u64) {
    let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut shell = Shell::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut shell).unwrap();
    queue.roundtrip(&mut shell).unwrap();
    println!("READY");
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut chosen = false;
    while Instant::now() < deadline {
        queue.roundtrip(&mut shell).unwrap();
        if !chosen
            && let Some((_, serial, _)) = shell
                .menus
                .iter()
                .find(|(_, _, model)| model.contains(&format!("\"id\":{CHOSEN}")))
            && let Some(manager) = &shell.manager
        {
            manager.activate_menu_item(*serial, CHOSEN);
            // An old serial does nothing.
            manager.activate_menu_item(serial.wrapping_add(1000), CHOSEN);
            chosen = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for (app_id, serial, model) in &shell.menus {
        let labels: Vec<String> = serde_json::from_str::<Value>(model)
            .ok()
            .and_then(|m| m["menus"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .flat_map(|menu| menu["items"].as_array().cloned().unwrap_or_default())
            .filter_map(|item| item["label"].as_str().map(str::to_owned))
            .collect();
        println!(
            "APP_MENU {app_id} {} {}",
            if *serial == 0 { "none" } else { "some" },
            labels.join(",")
        );
    }
}

#[test]
fn menu_shell_child() {
    if let (Ok(socket), Ok(ms)) = (std::env::var(CHILD_SOCKET), std::env::var(CHILD_LISTEN)) {
        shell_child(&socket, ms.parse().unwrap());
    }
}

fn model(label: &str) -> String {
    json!({"menus": [
        {"label": "Calc", "role": "app", "items": [
            {"label": "", "role": "quit", "shortcut": {"key": "Q", "meta": true}}
        ]},
        {"label": "View", "items": [
            {"id": CHOSEN, "label": label, "checked": true, "shortcut": {"key": "1", "meta": true}}
        ]}
    ]})
    .to_string()
}

fn window_id(compositor: &Headless, title: &str) -> u64 {
    let list: Vec<WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    list.into_iter()
        .find(|w| w.title.as_deref() == Some(title))
        .unwrap()
        .id
}

#[test]
fn the_bar_receives_the_frontmost_apps_menus_and_the_choice_goes_back() {
    let compositor = Headless::start("app-menu");
    APP_ID.with(|id| *id.borrow_mut() = "org.silica.Calc".to_owned());
    let mut calc = TestWindow::open(&compositor.wayland, "calc");
    calc.set_menu_model(&model("Basic"));
    APP_ID.with(|id| *id.borrow_mut() = "org.silica.Other".to_owned());
    let _other = TestWindow::open(&compositor.wayland, "other");
    APP_ID.with(|id| id.borrow_mut().clear());

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "menu_shell_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, &compositor.wayland)
        .env(CHILD_LISTEN, "2500")
        .env("SILICA_SHELL", "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    for line in lines.by_ref() {
        // The harness writes "test menu_shell_child ... " before the child's own output.
        if line.unwrap().ends_with("READY") {
            break;
        }
    }
    // "other" is in front, without menus; then "calc" comes forward with its model.
    let calc_id = window_id(&compositor, "calc");
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({"id": calc_id}))
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    // The shell chose the item: the app receives it, once (the old serial did nothing).
    let chosen = calc.menu_activations();
    // A live update reaches the bar; without a model the bar goes back to nothing.
    calc.set_menu_model(&model("Scientific"));
    std::thread::sleep(Duration::from_millis(300));
    calc.destroy_menu();
    let out: Vec<String> = lines.map(Result::unwrap).collect();
    child.wait().unwrap();
    assert_eq!(chosen, [CHOSEN], "{out:?}");
    let menus: Vec<&str> = out
        .iter()
        .filter_map(|l| l.strip_prefix("APP_MENU "))
        .collect();
    assert_eq!(
        menus,
        [
            "org.silica.Other none ",
            "org.silica.Calc some Basic",
            "org.silica.Calc some Scientific",
            "org.silica.Calc none ",
        ],
        "{out:?}"
    );
}

#[test]
fn the_menus_are_commands_too() {
    let compositor = Headless::start("app-menu-commands");
    let mut c = compositor.client();
    // No app in front: no menus, and nothing to choose.
    let state = c.call(APP_MENU, Value::Null).unwrap();
    assert_eq!(state["serial"], 0);
    assert!(c.call(APP_MENU_ACTIVATE, json!({"id": CHOSEN})).is_err());

    APP_ID.with(|id| *id.borrow_mut() = "org.silica.Calc".to_owned());
    let mut calc = TestWindow::open(&compositor.wayland, "calc");
    APP_ID.with(|id| id.borrow_mut().clear());
    calc.set_menu_model(&model("Basic"));
    let state = c.call(APP_MENU, Value::Null).unwrap();
    assert_eq!(state["app_id"], "org.silica.Calc");
    assert_ne!(state["serial"], 0);
    // In normal form: the defaults written out, the empty label of a role item dropped.
    assert_eq!(
        state["model"]["menus"][1]["items"][0],
        json!({"id": CHOSEN, "label": "Basic", "enabled": true, "checked": true,
               "shortcut": {"key": "1", "meta": true}})
    );
    assert_eq!(
        state["model"]["menus"][0]["items"][0],
        json!({"enabled": true, "role": "quit", "shortcut": {"key": "Q", "meta": true}})
    );
    c.call(APP_MENU_ACTIVATE, json!({"id": CHOSEN})).unwrap();
    assert_eq!(calc.menu_activations(), [CHOSEN]);
}

/// A popover of the app (an xdg_popup with a grab, which takes the keyboard) leaves the app in
/// front: the bar keeps its menus (silica-os/silica-desktop#238, the bar switched to "Silica").
#[test]
fn an_open_popup_keeps_its_app_in_front() {
    let compositor = Headless::start("app-menu-popup");
    APP_ID.with(|id| *id.borrow_mut() = "org.silica.Calc".to_owned());
    let mut calc = TestWindow::open(&compositor.wayland, "calc");
    APP_ID.with(|id| id.borrow_mut().clear());
    calc.set_menu_model(&model("Basic"));
    let mut c = compositor.client();
    assert_eq!(
        c.call(APP_MENU, Value::Null).unwrap()["app_id"],
        "org.silica.Calc"
    );
    // A press in the window gives the popup's grab a serial, then the popup opens.
    let list: Vec<silica_control::commands::WindowInfo> = serde_json::from_value(
        c.call(silica_control::commands::WINDOW_LIST, Value::Null)
            .unwrap(),
    )
    .unwrap();
    let w = &list[0];
    c.call(
        silica_control::commands::INPUT_CLICK,
        json!({"x": w.x + w.width / 2, "y": w.y + w.height / 2, "button": "right"}),
    )
    .unwrap();
    calc.open_grabbing_popup((5, 5), (40, 30));
    let state = c.call(APP_MENU, Value::Null).unwrap();
    assert_eq!(state["app_id"], "org.silica.Calc");
    assert_ne!(state["serial"], 0);
}

#[test]
fn a_bad_model_disconnects_only_its_app() {
    let compositor = Headless::start("app-menu-bad");
    let mut bad = TestWindow::open(&compositor.wayland, "bad");
    let mut good = TestWindow::open(&compositor.wayland, "good");
    good.set_menu_model(&model("Basic"));
    bad.set_menu_model("{\"menus\": [ not json");
    assert!(bad.disconnected());
    assert!(!good.disconnected());
    assert_eq!(good.menu_activations(), Vec::<u32>::new());
    // The compositor keeps going and still serves the good app's menus.
    let state = compositor.client().call(APP_MENU, Value::Null).unwrap();
    assert_ne!(state["serial"], 0, "{state}");
}

/// The windows on screen, top first, with the focused one marked.
fn on_screen(compositor: &Headless) -> Vec<(String, bool)> {
    let list: Vec<WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    list.into_iter()
        .filter(|w| !w.minimized)
        .map(|w| (w.title.unwrap_or_default(), w.focused))
        .collect()
}

fn open_as(compositor: &Headless, app: &str, title: &str) -> TestWindow {
    APP_ID.with(|id| *id.borrow_mut() = app.to_owned());
    let window = TestWindow::open(&compositor.wayland, title);
    APP_ID.with(|id| id.borrow_mut().clear());
    window
}

#[test]
fn hide_hide_others_and_show_all() {
    let compositor = Headless::start("app-hide");
    let _a1 = open_as(&compositor, "org.silica.A", "a1");
    let _a2 = open_as(&compositor, "org.silica.A", "a2");
    let _b = open_as(&compositor, "org.silica.B", "b");
    let _c = open_as(&compositor, "org.silica.C", "c");
    let mut client = compositor.client();

    // C in front: Hide takes it off the screen and B comes to the front.
    let hidden = client.call(APP_HIDE, Value::Null).unwrap();
    assert_eq!(hidden, json!({"app_id": "org.silica.C", "hidden": 1}));
    assert_eq!(
        on_screen(&compositor),
        [
            ("b".to_owned(), true),
            ("a2".to_owned(), false),
            ("a1".to_owned(), false)
        ]
    );
    // Hide Others with B in front: only B stays.
    let others = client.call(APP_HIDE_OTHERS, Value::Null).unwrap();
    assert_eq!(others["hidden"], 2);
    assert_eq!(on_screen(&compositor), [("b".to_owned(), true)]);
    // Show All: everything back, B still in front.
    let shown = client.call(APP_SHOW_ALL, Value::Null).unwrap();
    assert_eq!(shown["shown"], 3);
    let windows = on_screen(&compositor);
    assert_eq!(windows.len(), 4, "{windows:?}");
    assert_eq!(windows[0], ("b".to_owned(), true));
}

#[test]
fn a_window_of_a_hidden_app_brings_it_back() {
    let compositor = Headless::start("app-unhide");
    let _a1 = open_as(&compositor, "org.silica.A", "a1");
    let _a2 = open_as(&compositor, "org.silica.A", "a2");
    let _b = open_as(&compositor, "org.silica.B", "b");
    let a1 = window_id(&compositor, "a1");
    compositor
        .client()
        .call(APP_HIDE, json!({"app_id": "org.silica.A"}))
        .unwrap();
    assert_eq!(on_screen(&compositor), [("b".to_owned(), true)]);
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({"id": a1}))
        .unwrap();
    let windows = on_screen(&compositor);
    assert_eq!(windows.len(), 3, "{windows:?}");
    assert_eq!(windows[0], ("a1".to_owned(), true));
}
