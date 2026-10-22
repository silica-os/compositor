//! Shell protocols: only the shell components (`SILICA_SHELL=1`) see the window list and
//! `silica_shell_manager_v1`, which activates a window.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::os::unix::net::UnixStream;
use std::process::Command;

use common::Headless;
use common::client::TestWindow;
use serde_json::{Value, json};
use silica_control::commands::{INPUT_KEY, WINDOW_LIST, WindowInfo};
use silica_protocols::shell::client::silica_shell_manager_v1::{self, SilicaShellManagerV1};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::{
    self, ExtForeignToplevelHandleV1,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::{
    self, EVT_TOPLEVEL_OPCODE, ExtForeignToplevelListV1,
};

const CHILD_SOCKET: &str = "SILICA_TEST_SHELL_SOCKET";
const CHILD_LISTEN: &str = "SILICA_TEST_SHELL_LISTEN";
const CHILD_THUMBNAIL: &str = "SILICA_TEST_SHELL_THUMBNAIL";
const CHILD_BROADCAST: &str = "SILICA_TEST_SHELL_BROADCAST";
/// Set: the child activates the whole app of "first" (`activate_app`) instead of the window.
const CHILD_WHOLE_APP: &str = "SILICA_TEST_SHELL_WHOLE_APP";

#[derive(Default)]
struct Shell {
    globals: Vec<String>,
    list: Option<ExtForeignToplevelListV1>,
    manager: Option<SilicaShellManagerV1>,
    /// (identifier, title) of the announced windows.
    toplevels: Vec<(String, String)>,
    /// The identifiers announced with `focused`.
    focused: Vec<String>,
    /// The `minimized` events (identifier, state) and the thumbnail received.
    minimized: Vec<(String, u32)>,
    thumbnail: Option<(u32, u32)>,
    /// The commands forwarded by the compositor.
    commands: Vec<String>,
    /// The arrangement popovers requested (identifier, x, y).
    window_menus: Vec<(String, i32, i32)>,
    /// The states announced by the components (topic, value).
    broadcasts: Vec<(String, String)>,
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
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "ext_foreign_toplevel_list_v1" => {
                    state.list = Some(registry.bind(name, 1, qh, ()));
                }
                "silica_shell_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(10), qh, ()));
                }
                _ => {}
            }
            state.globals.push(interface);
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for Shell {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { .. } = event {
            state.toplevels.push((String::new(), String::new()));
        }
    }

    event_created_child!(Shell, ExtForeignToplevelListV1, [
        EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Shell {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(last) = state.toplevels.last_mut() else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => last.0 = identifier,
            ext_foreign_toplevel_handle_v1::Event::Title { title } => last.1 = title,
            _ => {}
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
        match event {
            silica_shell_manager_v1::Event::Focused { identifier } => {
                state.focused.push(identifier)
            }
            silica_shell_manager_v1::Event::Command { id } => state.commands.push(id),
            silica_shell_manager_v1::Event::Minimized {
                identifier,
                minimized,
            } => state.minimized.push((identifier, minimized)),
            silica_shell_manager_v1::Event::Thumbnail { width, height, .. } => {
                state.thumbnail = Some((width, height));
            }
            silica_shell_manager_v1::Event::WindowMenu { identifier, x, y } => {
                state.window_menus.push((identifier, x, y));
            }
            silica_shell_manager_v1::Event::Broadcast { topic, value } => {
                state.broadcasts.push((topic, value));
            }
            _ => {}
        }
    }
}

/// Connects, reads the globals and the windows; if it is a shell component it activates the
/// window "first". Prints the globals found, one per line.
fn shell_client(socket: &str) {
    let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut shell = Shell::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut shell).unwrap();
    queue.roundtrip(&mut shell).unwrap();
    // An announcement: sends it (if there is a value) and prints the received ones, also on connection.
    if let Ok(value) = std::env::var(CHILD_BROADCAST) {
        if let (Some(manager), false) = (shell.manager.clone(), value.is_empty()) {
            manager.broadcast("controlcenter.open".to_owned(), value);
            queue.roundtrip(&mut shell).unwrap();
        }
        for (topic, value) in &shell.broadcasts {
            println!("BROADCAST {topic}={value}");
        }
        return;
    }
    // The minimized windows announced on connection and the thumbnail of the first.
    if std::env::var_os(CHILD_THUMBNAIL).is_some() {
        if let (Some(manager), Some((identifier, _))) =
            (shell.manager.clone(), shell.minimized.first().cloned())
        {
            let path = std::env::temp_dir().join(format!("silica-thumb-{}", std::process::id()));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            file.set_len(256 * 256 * 4).unwrap();
            manager.thumbnail(identifier.clone(), std::os::fd::AsFd::as_fd(&file));
            queue.roundtrip(&mut shell).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let _ = std::fs::remove_file(&path);
            let (w, h) = shell.thumbnail.unwrap_or_default();
            let opaque = bytes
                .chunks(4)
                .take((w * h) as usize)
                .filter(|px| px[3] > 200)
                .count();
            println!("MINIMIZED {}", shell.minimized.len());
            println!("THUMBNAIL {w} {h} {opaque}");
        }
        return;
    }
    // Listening for a few seconds: meanwhile the parent test presses the keys.
    if let Ok(ms) = std::env::var(CHILD_LISTEN) {
        println!("READY");
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(ms.parse().unwrap());
        while std::time::Instant::now() < deadline {
            queue.roundtrip(&mut shell).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for id in &shell.commands {
            println!("COMMAND {id}");
        }
        for (_, x, y) in &shell.window_menus {
            println!("WINDOW_MENU {x} {y}");
        }
        return;
    }
    let target = shell
        .toplevels
        .iter()
        .find(|(_, title)| title == "first")
        .map(|(identifier, _)| identifier.clone());
    if let (Some(manager), Some(identifier)) = (shell.manager.clone(), target) {
        if std::env::var_os(CHILD_WHOLE_APP).is_some() {
            manager.activate_app(identifier);
        } else {
            manager.activate(identifier);
        }
        queue.roundtrip(&mut shell).unwrap();
        // A catalog command requested by the shell goes back to the whole shell.
        manager.execute("faro.toggle".to_owned());
        manager.execute("notifications.toggle".to_owned());
        manager.execute("launcher.toggle".to_owned());
        queue.roundtrip(&mut shell).unwrap();
    }
    for id in &shell.commands {
        println!("COMMAND {id}");
    }
    for global in &shell.globals {
        println!("GLOBAL {global}");
    }
    for (_, title) in &shell.toplevels {
        println!("TOPLEVEL {title}");
    }
    for identifier in &shell.focused {
        let title = shell
            .toplevels
            .iter()
            .find(|(id, _)| id == identifier)
            .map_or("", |(_, title)| title.as_str());
        println!("FOCUSED {title}");
    }
}

/// The child process: runs only when the parent test launches it with the socket.
#[test]
fn shell_child() {
    if let Ok(socket) = std::env::var(CHILD_SOCKET) {
        shell_client(&socket);
    }
}

fn run_child(socket: &std::path::Path, shell: bool) -> String {
    run_child_with(socket, shell, false)
}

fn run_child_with(socket: &std::path::Path, shell: bool, whole_app: bool) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    if whole_app {
        command.env(CHILD_WHOLE_APP, "1");
    } else {
        command.env_remove(CHILD_WHOLE_APP);
    }
    command
        .args([
            "--exact",
            "shell_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, socket);
    if shell {
        command.env("SILICA_SHELL", "1");
    } else {
        command.env_remove("SILICA_SHELL");
    }
    let output = command.output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn only_shell_components_see_the_window_list_and_can_activate() {
    let compositor = Headless::start("shell");
    let _first = TestWindow::open(&compositor.wayland, "first");
    let _second = TestWindow::open(&compositor.wayland, "second");

    let outsider = run_child(&compositor.wayland, false);
    assert!(outsider.contains("GLOBAL wl_compositor"), "{outsider}");
    assert!(!outsider.contains("silica_shell_manager_v1"), "{outsider}");
    assert!(
        !outsider.contains("ext_foreign_toplevel_list_v1"),
        "{outsider}"
    );

    let shell = run_child(&compositor.wayland, true);
    assert!(shell.contains("GLOBAL silica_shell_manager_v1"), "{shell}");
    assert!(
        shell.contains("TOPLEVEL first") && shell.contains("TOPLEVEL second"),
        "{shell}"
    );

    // On connection the focus was on "second"; after the activation on "first".
    let focused: Vec<&str> = shell.lines().filter(|l| l.starts_with("FOCUSED")).collect();
    assert_eq!(focused, ["FOCUSED second", "FOCUSED first"], "{shell}");

    assert!(shell.contains("COMMAND faro.toggle"), "{shell}");
    assert!(shell.contains("COMMAND notifications.toggle"), "{shell}");
    assert!(shell.contains("COMMAND launcher.toggle"), "{shell}");

    // The shell's child activated "first": now it has the focus and is on top.
    let list: Vec<WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    assert_eq!(list[0].title.as_deref(), Some("first"), "{list:?}");
    assert!(list[0].focused);
}

#[test]
fn the_logo_key_drives_the_app_switcher() {
    let compositor = Headless::start("shell-switcher");
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "shell_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, &compositor.wayland)
        .env(CHILD_LISTEN, "2500")
        .env("SILICA_SHELL", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    let mut c = compositor.client();
    let mut key = |key: &str, pressed: Option<bool>| {
        let params = match pressed {
            Some(pressed) => json!({"key": key, "pressed": pressed}),
            None => json!({"key": key}),
        };
        c.call(INPUT_KEY, params).unwrap();
    };
    // ⌘Tab Tab, then ⌘ released: two steps and the confirmation.
    key("Super_L", Some(true));
    key("Tab", None);
    key("Tab", None);
    key("Super_L", Some(false));
    // ⌘⇧Tab and Esc: back and cancelled; the release afterwards confirms nothing.
    key("Super_L", Some(true));
    key("Shift_L", Some(true));
    key("Tab", None);
    key("Shift_L", Some(false));
    key("Escape", None);
    key("Super_L", Some(false));
    // Up and Down open App Exposé of the chosen app; the release afterwards confirms nothing.
    key("Super_L", Some(true));
    key("Tab", None);
    key("Up", None);
    key("Super_L", Some(false));
    key("Super_L", Some(true));
    key("Tab", None);
    key("Down", None);
    key("Super_L", Some(false));
    let output = child.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    let commands: Vec<&str> = out
        .lines()
        .filter_map(|l| l.strip_prefix("COMMAND "))
        .collect();
    assert_eq!(
        commands,
        [
            "switcher.next",
            "switcher.next",
            "switcher.commit",
            "switcher.previous",
            "switcher.cancel",
            "switcher.next",
            "switcher.expose",
            "switcher.next",
            "switcher.expose"
        ],
        "{out}"
    );
}

/// Two windows of app "one" with a window of "due" between them, "due" on top:
/// first < third < other.
fn three_windows(compositor: &Headless) -> [TestWindow; 3] {
    use common::client::APP_ID;
    use silica_control::commands::WINDOW_FOCUS;
    APP_ID.with(|id| *id.borrow_mut() = "one".to_owned());
    let first = TestWindow::open(&compositor.wayland, "first");
    APP_ID.with(|id| *id.borrow_mut() = "due".to_owned());
    let other = TestWindow::open(&compositor.wayland, "other");
    APP_ID.with(|id| *id.borrow_mut() = "one".to_owned());
    let third = TestWindow::open(&compositor.wayland, "third");
    APP_ID.with(|id| id.borrow_mut().clear());
    let id = windows(compositor)
        .into_iter()
        .find(|w| w.title.as_deref() == Some("other"))
        .unwrap()
        .id;
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({ "id": id }))
        .unwrap();
    assert_eq!(titles(compositor), ["other", "third", "first"]);
    [first, other, third]
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

/// The titles from the top window down.
fn titles(compositor: &Headless) -> Vec<String> {
    windows(compositor)
        .iter()
        .filter_map(|w| w.title.clone())
        .collect()
}

#[test]
fn activating_a_window_brings_only_that_window() {
    // A window chosen in the Window menu or the Dock menu (silica-ui#101): the app's other
    // window stays behind "other".
    let compositor = Headless::start("shell-window");
    let _windows = three_windows(&compositor);
    run_child(&compositor.wayland, true);
    assert_eq!(titles(&compositor), ["first", "other", "third"]);
}

#[test]
fn activating_an_app_brings_all_its_windows() {
    // The app switcher, Bring All to Front (D-03): both windows of "one" come to the front,
    // keeping their order, with "first" (the requested one) on top.
    let compositor = Headless::start("shell-app");
    let _windows = three_windows(&compositor);
    run_child_with(&compositor.wayland, true, true);
    assert_eq!(titles(&compositor), ["first", "third", "other"]);
}

#[test]
fn clicking_a_window_raises_only_that_window() {
    use silica_control::commands::INPUT_CLICK;
    // The reference: a click raises the window under it, not the rest of its app.
    let compositor = Headless::start("shell-click");
    let _windows = three_windows(&compositor);
    // Apart from the others: the cascade can place windows of different apps on top of each
    // other, which would leave no point of "first" to click.
    let first = windows(&compositor)
        .into_iter()
        .find(|w| w.title.as_deref() == Some("first"))
        .unwrap();
    compositor
        .client()
        .call(
            silica_control::commands::WINDOW_MOVE,
            json!({"id": first.id, "x": 700, "y": 450}),
        )
        .unwrap();
    // A point of "first" that no other window covers.
    let list = windows(&compositor);
    let frame = |title: &str| {
        let w = list
            .iter()
            .find(|w| w.title.as_deref() == Some(title))
            .unwrap();
        (w.x, w.y, w.width, w.height)
    };
    let (fx, fy, fw, fh) = frame("first");
    let covered = |x: i32, y: i32| {
        ["other", "third"].iter().any(|t| {
            let (ox, oy, ow, oh) = frame(t);
            x >= ox && x < ox + ow && y >= oy && y < oy + oh
        })
    };
    let point = (fy + 40..fy + fh - 10)
        .step_by(10)
        .flat_map(|y| (fx + 10..fx + fw - 10).step_by(10).map(move |x| (x, y)))
        .find(|&(x, y)| !covered(x, y))
        .expect("a visible point of the first window");
    compositor
        .client()
        .call(INPUT_CLICK, json!({"x": point.0, "y": point.1}))
        .unwrap();
    assert_eq!(titles(&compositor), ["first", "other", "third"]);
}

#[test]
fn minimized_windows_are_announced_with_a_thumbnail() {
    let compositor =
        Headless::start_with("shell-thumbnail", (640, 400), &["--animation-scale", "1"]);
    let _window = TestWindow::open_sized(&compositor.wayland, "minimized", (300, 200));
    compositor
        .client()
        .call(silica_control::commands::WINDOW_MINIMIZE, Value::Null)
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "shell_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, &compositor.wayland)
        .env(CHILD_THUMBNAIL, "1")
        .env("SILICA_SHELL", "1")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("MINIMIZED 1"), "{out}");
    // The 300 × 200 frame (without shadow) fits in the 256 pt: reduced to 256 × ~171.
    let line = out
        .lines()
        .find(|l| l.starts_with("THUMBNAIL"))
        .unwrap_or_else(|| panic!("{out}"));
    let numbers: Vec<u32> = line
        .split_whitespace()
        .skip(1)
        .map(|n| n.parse().unwrap())
        .collect();
    assert_eq!(numbers[0], 256, "{line}");
    assert!((160..=180).contains(&numbers[1]), "{line}");
    // Almost all the pixels are full (the red window, without a shadow around it).
    assert!(numbers[2] > 256 * 160 * 9 / 10, "{line}");
}

/// Long press on the green button: the shell receives the arrangement popover with the tip
/// below the button, and the release does not take the window to full screen.
#[test]
fn a_long_press_on_the_zoom_button_asks_the_shell_for_the_window_menu() {
    use silica_control::commands::{INPUT_POINTER_BUTTON, INPUT_POINTER_MOVE};
    let compositor = Headless::start("shell-window-menu");
    let _window = TestWindow::open_decorated(&compositor.wayland, "arrange", (300, 200));
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "shell_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, &compositor.wayland)
        .env(CHILD_LISTEN, "2500")
        .env("SILICA_SHELL", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let mut c = compositor.client();
    let list: Vec<WindowInfo> =
        serde_json::from_value(c.call(WINDOW_LIST, Value::Null).unwrap()).unwrap();
    let info = &list[0];
    // The green button: center x 62, y 16 of the bar, which sits above the content.
    let (bx, by) = (info.x + 62, info.y - 32 + 16);
    c.call(INPUT_POINTER_MOVE, json!({"x": bx, "y": by}))
        .unwrap();
    c.call(
        INPUT_POINTER_BUTTON,
        json!({"button": "left", "pressed": true}),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    c.call(
        INPUT_POINTER_BUTTON,
        json!({"button": "left", "pressed": false}),
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    let menu = out
        .lines()
        .find(|l| l.starts_with("WINDOW_MENU"))
        .unwrap_or_else(|| panic!("no popover: {out}"));
    // The tip below the button: x of the center, y of the base of the disc (radius 7).
    assert_eq!(menu, format!("WINDOW_MENU {} {}", bx, by + 7));
    let list: Vec<WindowInfo> =
        serde_json::from_value(c.call(WINDOW_LIST, Value::Null).unwrap()).unwrap();
    assert!(!list[0].fullscreen, "{list:?}");
}

/// A state announced by a component goes back to everyone (the sender too) and whoever
/// connects later receives the last value.
#[test]
fn shell_components_share_their_state_with_broadcast() {
    let compositor = Headless::start("shell-broadcast");
    let run = |value: &str| {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "shell_child",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_SOCKET, &compositor.wayland)
            .env(CHILD_BROADCAST, value)
            .env("SILICA_SHELL", "1")
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let sender = run("1");
    assert!(
        sender.contains("BROADCAST controlcenter.open=1"),
        "{sender}"
    );
    // A component connecting later, without announcing anything.
    let later = run("");
    assert!(later.contains("BROADCAST controlcenter.open=1"), "{later}");
}
