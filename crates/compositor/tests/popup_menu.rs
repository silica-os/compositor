//! The menu service (silica-os/silica-desktop#241, `docs/spec/menus.md`): an app shows a context menu or a
//! pop-up button's menu with typed requests, the shell receives the rows and the anchor in
//! output coordinates, and the result goes back once. The compositor ends the menu on every change of
//! context (#237): one test for each trigger.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use common::Headless;
use common::client::{APP_ID, PopupResult, TestWindow};
use serde_json::{Value, json};
use silica_control::commands::{
    APP_HIDE, FARO_TOGGLE, INPUT_CLICK, LAUNCHER_TOGGLE, SESSION_LOCK, SWITCHER_NEXT, WINDOW_FOCUS,
    WINDOW_LIST, WINDOW_MINIMIZE, WindowInfo,
};
use silica_protocols::shell::client::silica_shell_manager_v1::{self, SilicaShellManagerV1};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

const CHILD_SOCKET: &str = "SILICA_TEST_POPUP_SOCKET";
const CHILD_LISTEN: &str = "SILICA_TEST_POPUP_LISTEN";
/// What the shell child does with a menu: `choose:<id>`, `close` or nothing.
const CHILD_ACTION: &str = "SILICA_TEST_POPUP_ACTION";

#[derive(Default)]
struct Shell {
    manager: Option<SilicaShellManagerV1>,
    lines: Vec<String>,
    /// The serial of the menu being received, and whether its rows are complete.
    open: Option<u32>,
    complete: bool,
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
            state.manager = Some(registry.bind(name, 9, qh, ()));
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
        use silica_shell_manager_v1::Event;
        match event {
            Event::PopupMenu {
                serial,
                app_id,
                kind,
                x,
                y,
                width,
                height,
                selected,
                ..
            } => {
                state.open = Some(serial);
                state.complete = false;
                state.lines.push(format!(
                    "POPUP {app_id} kind={kind} at={x},{y},{width},{height} selected={selected}"
                ));
            }
            Event::PopupMenuRow {
                kind,
                id,
                label,
                flags,
                icon,
                key,
                modifiers,
                ..
            } => state.lines.push(format!(
                "ROW {kind} {id} {label} {flags} {icon} {key} {modifiers}"
            )),
            Event::PopupMenuEnd { .. } => {
                state.complete = true;
                state.lines.push("END".to_owned());
            }
            Event::PopupMenuClosed { .. } => {
                state.open = None;
                state.lines.push("CLOSED".to_owned());
            }
            _ => {}
        }
    }
}

/// The shell child: prints what it receives, one line each, and does `ACTION` with the first
/// menu once its rows are complete.
fn shell_child(socket: &str, ms: u64, action: &str) {
    let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut shell = Shell::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut shell).unwrap();
    queue.roundtrip(&mut shell).unwrap();
    println!("READY");
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut printed = 0;
    let mut acted = false;
    while Instant::now() < deadline {
        queue.roundtrip(&mut shell).unwrap();
        if !acted
            && shell.complete
            && let (Some(serial), Some(manager)) = (shell.open, &shell.manager)
        {
            if let Some(id) = action.strip_prefix("choose:") {
                // An id the menu does not have does nothing.
                manager.popup_menu_choose(serial, 9999);
                manager.popup_menu_choose(serial, id.parse().unwrap());
            } else if action == "close" {
                manager.popup_menu_close(serial);
            }
            acted = true;
        }
        while printed < shell.lines.len() {
            println!("{}", shell.lines[printed]);
            printed += 1;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn popup_shell_child() {
    if let (Ok(socket), Ok(ms)) = (std::env::var(CHILD_SOCKET), std::env::var(CHILD_LISTEN)) {
        shell_child(
            &socket,
            ms.parse().unwrap(),
            &std::env::var(CHILD_ACTION).unwrap_or_default(),
        );
    }
}

/// The shell child and its lines.
struct ShellProcess {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl ShellProcess {
    fn start(compositor: &Headless, action: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "popup_shell_child",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_SOCKET, &compositor.wayland)
            .env(CHILD_LISTEN, "6000")
            .env(CHILD_ACTION, action)
            .env("SILICA_SHELL", "1")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // The harness writes "test popup_shell_child ... " before the child's output.
                let line = line.rsplit("... ").next().unwrap_or("").to_owned();
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let mut shell = Self {
            child,
            lines,
            seen: Vec::new(),
        };
        assert!(shell.wait_for("READY"), "the shell child did not start");
        shell
    }

    /// Waits for a line that starts with `prefix`; false after 4 s.
    fn wait_for(&mut self, prefix: &str) -> bool {
        if self.seen.iter().any(|l| l.starts_with(prefix)) {
            return true;
        }
        let deadline = Instant::now() + Duration::from_secs(4);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    let found = line.starts_with(prefix);
                    self.seen.push(line);
                    if found {
                        return true;
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        false
    }

    /// Whether the shell has not been told of a close within a moment.
    fn closed(&mut self) -> bool {
        self.wait_for("CLOSED")
    }
}

impl Drop for ShellProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn open_as(compositor: &Headless, app: &str, title: &str) -> TestWindow {
    APP_ID.with(|id| *id.borrow_mut() = app.to_owned());
    let window = TestWindow::open(&compositor.wayland, title);
    APP_ID.with(|id| id.borrow_mut().clear());
    window
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

fn window(compositor: &Headless, title: &str) -> WindowInfo {
    windows(compositor)
        .into_iter()
        .find(|w| w.title.as_deref() == Some(title))
        .unwrap()
}

/// A context menu with two items, one with a submenu.
fn rows(menu: &silica_protocols::menu::client::silica_popup_menu_v1::SilicaPopupMenuV1) {
    use silica_protocols::menu::client::silica_popup_menu_v1::{Flags, Modifiers};
    menu.add_section("Edit".to_owned());
    menu.add_item(
        1,
        "Copy".to_owned(),
        Flags::Checked,
        "doc.on.doc".to_owned(),
        "C".to_owned(),
        Modifiers::Meta,
    );
    menu.add_separator();
    menu.begin_submenu("Share".to_owned(), Flags::empty(), String::new());
    menu.add_item(
        2,
        "Mail".to_owned(),
        Flags::empty(),
        String::new(),
        String::new(),
        Modifiers::empty(),
    );
    menu.end_submenu();
}

/// Two apps: "owner" with a menu open on its window (shell child running), "other" on top
/// of nothing in particular. Returns the compositor, the shell, the windows.
fn setup(name: &str, action: &str) -> (Headless, ShellProcess, TestWindow, TestWindow) {
    let compositor = Headless::start(name);
    let owner = open_as(&compositor, "org.silica.Owner", "owner");
    let other = open_as(&compositor, "org.silica.Other", "other");
    let shell = ShellProcess::start(&compositor, action);
    let mut owner = owner;
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({"id": window(&compositor, "owner").id}))
        .unwrap();
    let _menu = owner.show_popup_menu(0, (10, 20, 0, 0), 0, rows);
    std::mem::forget(_menu);
    let mut shell = shell;
    assert!(
        shell.wait_for("END"),
        "the shell got no menu: {:?}",
        shell.seen
    );
    (compositor, shell, owner, other)
}

#[test]
fn the_shell_receives_the_rows_and_the_anchor_on_the_output() {
    let (compositor, shell, _owner, _other) = setup("popup-rows", "");
    let at = window(&compositor, "owner");
    // The window's surface starts at the window's position on the output (no frame here).
    let popup = shell.seen.iter().find(|l| l.starts_with("POPUP")).unwrap();
    assert!(
        popup.starts_with("POPUP org.silica.Owner kind=0 at="),
        "{popup}"
    );
    let at_values: Vec<i32> = popup
        .split("at=")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect();
    assert_eq!(at_values[2..], [0, 0]);
    assert!(
        (at_values[0] - (at.x + 10)).abs() <= 40 && (at_values[1] - (at.y + 20)).abs() <= 60,
        "{popup} for the window at {},{}",
        at.x,
        at.y
    );
    let rows: Vec<&str> = shell
        .seen
        .iter()
        .filter(|l| l.starts_with("ROW"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        rows,
        [
            "ROW 2 0 Edit 0   0",
            "ROW 0 1 Copy 2 doc.on.doc C 1",
            "ROW 1 0  0   0",
            "ROW 3 0 Share 0   0",
            "ROW 0 2 Mail 0   0",
            "ROW 4 0  0   0",
        ]
    );
}

#[test]
fn the_shells_choice_goes_back_once() {
    let compositor = Headless::start("popup-choose");
    let mut owner = open_as(&compositor, "org.silica.Owner", "owner");
    let mut shell = ShellProcess::start(&compositor, "choose:2");
    let _menu = owner.show_popup_menu(0, (10, 20, 0, 0), 0, rows);
    assert!(shell.closed());
    assert_eq!(owner.popup_results(), [PopupResult::Selected(2)]);
    // Over: nothing more arrives.
    assert_eq!(owner.popup_results(), []);
}

#[test]
fn the_shells_close_dismisses() {
    let compositor = Headless::start("popup-close");
    let mut owner = open_as(&compositor, "org.silica.Owner", "owner");
    let mut shell = ShellProcess::start(&compositor, "close");
    let _menu = owner.show_popup_menu(1, (10, 20, 80, 22), 2, rows);
    assert!(shell.closed());
    assert!(
        shell
            .seen
            .iter()
            .any(|l| l.contains("kind=1") && l.contains("selected=2"))
    );
    assert_eq!(owner.popup_results(), [PopupResult::Dismissed]);
}

#[test]
fn a_menu_without_a_shell_or_without_items_is_dismissed_at_once() {
    let compositor = Headless::start("popup-none");
    let mut owner = open_as(&compositor, "org.silica.Owner", "owner");
    let _menu = owner.show_popup_menu(0, (1, 1, 0, 0), 0, rows);
    assert_eq!(owner.popup_results(), [PopupResult::Dismissed]);
    let _shell = ShellProcess::start(&compositor, "");
    let _empty = owner.show_popup_menu(0, (1, 1, 0, 0), 0, |menu| menu.add_separator());
    assert_eq!(owner.popup_results(), [PopupResult::Dismissed]);
}

#[test]
fn a_newer_menu_of_the_same_app_replaces_the_open_one() {
    let (_compositor, mut shell, mut owner, _other) = setup("popup-replace", "");
    let _second = owner.show_popup_menu(0, (30, 30, 0, 0), 0, rows);
    assert!(shell.closed());
    assert_eq!(owner.popup_results(), [PopupResult::Dismissed]);
}

#[test]
fn destroying_the_object_closes_the_menu_without_an_event() {
    let compositor = Headless::start("popup-destroy");
    let mut owner = open_as(&compositor, "org.silica.Owner", "owner");
    let mut shell = ShellProcess::start(&compositor, "");
    let menu = owner.show_popup_menu(0, (10, 20, 0, 0), 0, rows);
    assert!(shell.wait_for("END"));
    menu.destroy();
    // Reads the events too: the request is sent.
    assert_eq!(owner.popup_results(), []);
    assert!(shell.closed());
    assert_eq!(owner.popup_results(), []);
}

#[test]
fn a_bad_menu_disconnects_only_its_app() {
    let compositor = Headless::start("popup-bad");
    let mut bad = open_as(&compositor, "org.silica.Bad", "bad");
    let mut good = open_as(&compositor, "org.silica.Good", "good");
    let _shell = ShellProcess::start(&compositor, "");
    let _menu = bad.show_popup_menu(0, (1, 1, 0, 0), 0, |menu| {
        use silica_protocols::menu::client::silica_popup_menu_v1::{Flags, Modifiers};
        for _ in 0..2 {
            menu.add_item(
                7,
                "Same".to_owned(),
                Flags::empty(),
                String::new(),
                String::new(),
                Modifiers::empty(),
            );
        }
    });
    assert!(bad.disconnected());
    assert!(!good.disconnected());
}

// The dismissal triggers (#237): each ends the menu with `dismissed` for the app and
// `popup_menu_closed` for the shell.

fn ended(shell: &mut ShellProcess, owner: &mut TestWindow, why: &str) {
    assert!(
        shell.closed(),
        "{why}: the shell was not told: {:?}",
        shell.seen
    );
    assert_eq!(owner.popup_results(), [PopupResult::Dismissed], "{why}");
}

#[test]
fn a_press_outside_ends_the_menu_in_the_app_and_in_another_app() {
    let (compositor, mut shell, mut owner, _other) = setup("popup-press", "");
    let w = window(&compositor, "owner");
    compositor
        .client()
        .call(INPUT_CLICK, json!({"x": w.x + 20, "y": w.y + 40}))
        .unwrap();
    ended(&mut shell, &mut owner, "press in the owner app");

    let (compositor, mut shell, mut owner, _other) = setup("popup-press-other", "");
    let w = window(&compositor, "other");
    compositor
        .client()
        .call(INPUT_CLICK, json!({"x": w.x + 20, "y": w.y + 40}))
        .unwrap();
    ended(&mut shell, &mut owner, "press in another app");
}

#[test]
fn another_window_taking_the_focus_ends_the_menu() {
    let (compositor, mut shell, mut owner, _other) = setup("popup-focus", "");
    compositor
        .client()
        .call(WINDOW_FOCUS, json!({"id": window(&compositor, "other").id}))
        .unwrap();
    ended(&mut shell, &mut owner, "focus");
}

#[test]
fn the_app_switcher_ends_the_menu() {
    let (compositor, mut shell, mut owner, _other) = setup("popup-switcher", "");
    compositor
        .client()
        .call(SWITCHER_NEXT, Value::Null)
        .unwrap();
    ended(&mut shell, &mut owner, "switcher");
}

#[test]
fn faro_and_the_launcher_end_the_menu() {
    for command in [FARO_TOGGLE, LAUNCHER_TOGGLE] {
        let (compositor, mut shell, mut owner, _other) = setup(&format!("popup-{command}"), "");
        compositor.client().call(command, Value::Null).unwrap();
        ended(&mut shell, &mut owner, command);
    }
}

#[test]
fn locking_the_session_ends_the_menu() {
    let (compositor, mut shell, mut owner, _other) = setup("popup-lock", "");
    compositor.client().call(SESSION_LOCK, Value::Null).unwrap();
    ended(&mut shell, &mut owner, "lock");
}

#[test]
fn the_parent_minimizing_or_hiding_ends_the_menu() {
    let (compositor, mut shell, mut owner, other) = setup("popup-minimize", "");
    let _other = other;
    compositor
        .client()
        .call(
            WINDOW_MINIMIZE,
            json!({"id": window(&compositor, "owner").id}),
        )
        .unwrap();
    ended(&mut shell, &mut owner, "minimize");

    let (compositor, mut shell, mut owner, _other) = setup("popup-hide", "");
    compositor
        .client()
        .call(APP_HIDE, json!({"app_id": "org.silica.Owner"}))
        .unwrap();
    ended(&mut shell, &mut owner, "hide");
}

#[test]
fn the_app_closing_its_window_and_leaving_ends_the_menu() {
    let (_compositor, mut shell, owner, _other) = setup("popup-leave", "");
    drop(owner);
    assert!(shell.closed(), "leave: {:?}", shell.seen);
}
