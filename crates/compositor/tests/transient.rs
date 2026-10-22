//! Transient UI (#149): while a shell menu or panel is open the windows keep the pointer, and
//! a press outside ends it (`dismissed`). Since silica-shell-v1 version 7 the outside is split
//! between the owner app and the other surfaces, each with its rule (the reference's
//! INPUT-01): a menu is modal only for its app, the Dock menu only reactivates the app that
//! was active, a panel never blocks; the wheel never ends anything.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{BufRead, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

use common::Headless;
use common::client::{APP_ID, PointerLog, PopupSpec, TestWindow};
use serde_json::{Value, json};
use silica_control::commands::{
    INPUT_CLICK, INPUT_POINTER_MOVE, INPUT_SCROLL, WINDOW_FOCUS, WINDOW_LIST, WindowInfo,
};
use silica_protocols::shell::client::silica_shell_manager_v1::{
    self, Outside, Rule, SilicaShellManagerV1,
};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{self, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, ZwlrLayerSurfaceV1,
};

const CHILD_SOCKET: &str = "SILICA_TEST_TRANSIENT_SOCKET";
const CHILD_MODE: &str = "SILICA_TEST_TRANSIENT_MODE";

/// The menu: a small overlay in the bottom right corner, away from the windows.
const MENU: (u32, u32) = (60, 40);

/// The owner app ("front", on top and focused) and another app ("back", below it).
const OWNER_APP: &str = "dev.test.Owner";
const OTHER_APP: &str = "dev.test.Other";

/// Only on "back" (the first window, at 32, 32, partly under "front" at 64, 64).
const ON_BACK: (f64, f64) = (45.0, 80.0);
/// Only on "front".
const ON_FRONT: (f64, f64) = (150.0, 115.0);

#[derive(Default)]
struct App {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    manager: Option<SilicaShellManagerV1>,
    size: Option<(u32, u32)>,
    dismissed: u32,
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
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
                "wl_compositor" => app.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => app.layer_shell = Some(registry.bind(name, 1, qh, ())),
                "silica_shell_manager_v1" => {
                    app.manager = Some(registry.bind(name, version.min(7), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
        surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            surface.ack_configure(serial);
            app.size = Some((width, height));
        }
    }
}

impl Dispatch<SilicaShellManagerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &SilicaShellManagerV1,
        event: silica_shell_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let silica_shell_manager_v1::Event::Dismissed { .. } = event {
            app.dismissed += 1;
        }
    }
}

impl Dispatch<WlShm, ()> for App {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore wayland_client::protocol::wl_region::WlRegion);
delegate_noop!(App: ignore WlBuffer);
delegate_noop!(App: WlShmPool);
delegate_noop!(App: ignore ZwlrLayerShellV1);

/// How the child opens its transient.
#[derive(Clone, Copy)]
enum Mode {
    /// Version 6: one rule for every surface.
    V6(Outside),
    /// Version 7: the owner app and the two rules.
    V7(&'static str, Rule, Rule),
    /// Version 7 on a surface as big as the screen whose input region and transient region
    /// are only the menu's corner, like the Dock with its menu open (#206).
    V7Wide(&'static str, Rule, Rule),
}

impl Mode {
    fn encode(self) -> String {
        let rule = |rule: Rule| match rule {
            Rule::Pass => "pass",
            Rule::Activate => "activate",
            _ => "modal",
        };
        match self {
            Mode::V6(Outside::PassThrough) => "v6 pass_through".to_owned(),
            Mode::V6(_) => "v6 consume".to_owned(),
            Mode::V7(owner, a, b) => format!("v7 {} {} {owner}", rule(a), rule(b)),
            Mode::V7Wide(owner, a, b) => format!("v7wide {} {} {owner}", rule(a), rule(b)),
        }
    }
}

/// The shell component: opens the menu with its transient, prints READY, then listens until
/// its stdin closes and prints how many `dismissed` it received.
fn menu_client(socket: &str, mode: &str) {
    let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut app = App::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut app).unwrap();
    let surface = app.compositor.as_ref().unwrap().create_surface(&qh, ());
    let layer = app.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Overlay,
        "menu".to_owned(),
        &qh,
        (),
    );
    let wide = mode.starts_with("v7wide");
    if wide {
        layer.set_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right | Anchor::Top);
        layer.set_size(0, 0);
    } else {
        layer.set_anchor(Anchor::Bottom | Anchor::Right);
        layer.set_size(MENU.0, MENU.1);
    }
    surface.commit();
    while app.size.is_none() {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    let (w, h) = app.size.unwrap();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&vec![255u8; (w * h * 4) as usize]).unwrap();
    let pool = app
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), (w * h * 4) as i32, &qh, ());
    let buffer = pool.create_buffer(
        0,
        w as i32,
        h as i32,
        (w * 4) as i32,
        wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    let manager = app.manager.clone().expect("silica_shell_manager_v1");
    // The wide surface takes the pointer only in the menu's corner, and so does the transient.
    let corner = app.compositor.as_ref().unwrap().create_region(&qh, ());
    corner.add(
        w as i32 - MENU.0 as i32,
        h as i32 - MENU.1 as i32,
        MENU.0 as i32,
        MENU.1 as i32,
    );
    if wide {
        surface.set_input_region(Some(&corner));
        surface.commit();
    }
    let words: Vec<&str> = mode.split(' ').collect();
    let rule = |word: &str| match word {
        "pass" => Rule::Pass,
        "activate" => Rule::Activate,
        _ => Rule::Modal,
    };
    match words.as_slice() {
        ["v7wide", owner_rule, other_rule, owner @ ..] => {
            manager.set_owned_transient(
                &surface,
                Some(&corner),
                owner.join(" "),
                rule(owner_rule),
                rule(other_rule),
            );
        }
        ["v7", owner_rule, other_rule, owner @ ..] => {
            assert!(wayland_client::Proxy::version(&manager) >= 7);
            manager.set_owned_transient(
                &surface,
                None,
                owner.join(" "),
                rule(owner_rule),
                rule(other_rule),
            );
        }
        ["v6", "pass_through"] => {
            manager.set_transient(&surface, None, Outside::PassThrough, Outside::PassThrough);
        }
        _ => manager.set_transient(&surface, None, Outside::Consume, Outside::PassThrough),
    }
    queue.roundtrip(&mut app).unwrap();
    println!("READY");
    std::io::stdout().flush().unwrap();
    // The parent closes stdin when it is done.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done.load(std::sync::atomic::Ordering::SeqCst) && std::time::Instant::now() < deadline {
        queue.roundtrip(&mut app).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    queue.roundtrip(&mut app).unwrap();
    println!("DISMISSED {}", app.dismissed);
}

/// The child process: runs only when the parent test launches it.
#[test]
fn transient_child() {
    if let (Ok(socket), Ok(mode)) = (std::env::var(CHILD_SOCKET), std::env::var(CHILD_MODE)) {
        menu_client(&socket, &mode);
    }
}

/// What the windows saw and whether the transient ended.
struct Outcome {
    dismissed: u32,
    back: PointerLog,
    front: PointerLog,
    back_focused: bool,
    front_focused: bool,
}

/// One of the control channel's input commands.
enum Act {
    Click((f64, f64), &'static str),
    Scroll((f64, f64)),
    Move((f64, f64)),
    /// Activates the "back" window through the control channel (no pointer).
    FocusBack,
}

/// Opens "back" (app `OTHER_APP` unless `back_app`) and "front" (`OWNER_APP`, on top and
/// focused), the menu in this mode, then runs the actions.
fn run(name: &str, mode: Mode, back_app: &str, acts: &[Act]) -> Outcome {
    let compositor = Headless::start(name);
    APP_ID.with(|id| *id.borrow_mut() = back_app.to_owned());
    let mut back = TestWindow::open(&compositor.wayland, "back");
    APP_ID.with(|id| *id.borrow_mut() = OWNER_APP.to_owned());
    let mut front = TestWindow::open(&compositor.wayland, "front");
    APP_ID.with(|id| id.borrow_mut().clear());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "transient_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, &compositor.wayland)
        .env(CHILD_MODE, mode.encode())
        .env("SILICA_SHELL", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = std::io::BufReader::new(stdout).lines();
    let mut out = String::new();
    for line in lines.by_ref() {
        let line = line.unwrap();
        out.push_str(&line);
        out.push('\n');
        // libtest prints "test transient_child ... " on the same line.
        if line.ends_with("READY") {
            break;
        }
    }
    // The pointer starts away from both windows (and the menu).
    let mut control = compositor.client();
    control
        .call(INPUT_POINTER_MOVE, json!({"x": 5.0, "y": 190.0}))
        .unwrap();
    let base_back = back.pointer_log();
    let base_front = front.pointer_log();
    for act in acts {
        match act {
            Act::Click(at, button) => control
                .call(INPUT_CLICK, json!({"x": at.0, "y": at.1, "button": button}))
                .unwrap(),
            Act::Scroll(at) => control
                .call(INPUT_SCROLL, json!({"x": at.0, "y": at.1, "dy": 15.0}))
                .unwrap(),
            Act::Move(at) => control
                .call(INPUT_POINTER_MOVE, json!({"x": at.0, "y": at.1}))
                .unwrap(),
            Act::FocusBack => {
                let list: Vec<WindowInfo> =
                    serde_json::from_value(control.call(WINDOW_LIST, Value::Null).unwrap())
                        .unwrap();
                let id = list
                    .iter()
                    .find(|w| w.title.as_deref() == Some("back"))
                    .map(|w| w.id)
                    .unwrap();
                control.call(WINDOW_FOCUS, json!({"id": id})).unwrap()
            }
        };
    }
    let list: Vec<WindowInfo> =
        serde_json::from_value(control.call(WINDOW_LIST, Value::Null).unwrap()).unwrap();
    let focused = |title: &str| {
        list.iter()
            .any(|w| w.title.as_deref() == Some(title) && w.focused)
    };
    let since = |now: PointerLog, base: PointerLog| PointerLog {
        inside: now.inside,
        enters: now.enters - base.enters,
        motions: now.motions - base.motions,
        presses: now.presses - base.presses,
        axes: now.axes - base.axes,
    };
    let back_log = since(back.pointer_log(), base_back);
    let front_log = since(front.pointer_log(), base_front);
    drop(child.stdin.take());
    for line in lines {
        out.push_str(&line.unwrap());
        out.push('\n');
    }
    child.wait().unwrap();
    let dismissed = out
        .lines()
        .find_map(|line| line.split("DISMISSED ").nth(1))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("no count from the child: {out}"));
    Outcome {
        dismissed,
        back: back_log,
        front: front_log,
        back_focused: focused("back"),
        front_focused: focused("front"),
    }
}

/// A menu of the frontmost app (INPUT-01 (a), (b)).
const APP_MENU: Mode = Mode::V7(OWNER_APP, Rule::Modal, Rule::Pass);
/// A system panel (Faro, Control Center, status menus…).
const PANEL: Mode = Mode::V7("", Rule::Pass, Rule::Pass);
/// The Dock menu, with the app that was active as owner.
const DOCK_MENU: Mode = Mode::V7(OTHER_APP, Rule::Activate, Rule::Pass);

#[test]
fn a_menu_click_in_another_app_closes_it_and_goes_through() {
    let o = run(
        "t-menu-other",
        APP_MENU,
        OTHER_APP,
        &[Act::Click(ON_BACK, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.back.presses, 1, "the click did not reach the other app");
    assert!(o.back_focused, "the other app did not activate");
}

#[test]
fn a_menu_click_in_its_own_app_only_closes_it() {
    let o = run(
        "t-menu-owner",
        APP_MENU,
        OTHER_APP,
        &[Act::Click(ON_FRONT, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.front.presses, 0, "the click reached the menu's own app");
}

#[test]
fn a_menu_right_click_in_another_app_goes_through() {
    let o = run(
        "t-menu-right",
        APP_MENU,
        OTHER_APP,
        &[Act::Click(ON_BACK, "right")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.back.presses, 1);
    assert!(o.back_focused);
}

#[test]
fn a_menu_lets_the_other_app_scroll_but_not_its_own_and_stays_open() {
    let o = run(
        "t-menu-scroll",
        APP_MENU,
        OTHER_APP,
        &[Act::Scroll(ON_BACK), Act::Scroll(ON_FRONT)],
    );
    assert_eq!(o.dismissed, 0, "the wheel closed the menu");
    assert_eq!(o.back.axes, 1, "the other app did not scroll");
    assert_eq!(o.front.axes, 0, "the menu's own app scrolled");
}

#[test]
fn a_menu_keeps_the_hover_of_other_apps_only() {
    let o = run(
        "t-menu-hover",
        APP_MENU,
        OTHER_APP,
        &[Act::Move(ON_FRONT), Act::Move(ON_BACK), Act::Move(ON_BACK)],
    );
    assert_eq!(o.dismissed, 0);
    assert_eq!(o.front.enters, 0, "the menu's own app got the pointer");
    assert!(o.back.inside && o.back.enters == 1, "{:?}", o.back);
}

#[test]
fn the_dock_menu_only_reactivates_the_app_that_was_active() {
    // "back" is the owner here: the app the Dock menu found active.
    let o = run(
        "t-dock-owner",
        DOCK_MENU,
        OTHER_APP,
        &[Act::Click(ON_BACK, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert!(o.back_focused, "the window was not activated");
    assert_eq!(o.back.presses, 0, "the click reached the window's content");
}

#[test]
fn the_dock_menu_lets_clicks_through_elsewhere_and_right_clicks_everywhere() {
    let o = run(
        "t-dock-other",
        DOCK_MENU,
        OTHER_APP,
        &[Act::Click(ON_FRONT, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.front.presses, 1);
    let o = run(
        "t-dock-right",
        DOCK_MENU,
        OTHER_APP,
        &[Act::Click(ON_BACK, "right")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.back.presses, 1);
}

#[test]
fn the_dock_menu_lets_every_window_scroll() {
    let o = run(
        "t-dock-scroll",
        DOCK_MENU,
        OTHER_APP,
        &[Act::Scroll(ON_BACK), Act::Scroll(ON_FRONT)],
    );
    assert_eq!(o.dismissed, 0);
    assert_eq!((o.back.axes, o.front.axes), (1, 1));
}

#[test]
fn a_panel_never_blocks() {
    let o = run(
        "t-panel",
        PANEL,
        OTHER_APP,
        &[Act::Scroll(ON_FRONT), Act::Move(ON_FRONT)],
    );
    assert_eq!(o.dismissed, 0, "the wheel closed the panel");
    assert_eq!(o.front.axes, 1);
    assert!(o.front.inside);
    let o = run(
        "t-panel-click",
        PANEL,
        OTHER_APP,
        &[Act::Click(ON_BACK, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.back.presses, 1);
    assert!(o.back_focused && !o.front_focused);
}

#[test]
fn activating_another_window_closes_a_menu() {
    // #219: switching context without the pointer (⌘Tab, a command) ends the menu.
    let o = run("t-focus-closes", APP_MENU, OTHER_APP, &[Act::FocusBack]);
    assert_eq!(o.dismissed, 1);
    assert!(o.back_focused);
}

#[test]
fn activating_another_window_closes_a_panel_and_the_dock_menu() {
    assert_eq!(
        run("t-focus-panel", PANEL, OTHER_APP, &[Act::FocusBack]).dismissed,
        1
    );
    assert_eq!(
        run("t-focus-dock", DOCK_MENU, OTHER_APP, &[Act::FocusBack]).dismissed,
        1
    );
}

#[test]
fn a_press_on_the_menu_itself_keeps_it_open() {
    let x = f64::from(common::WIDTH - MENU.0 / 2);
    let y = f64::from(common::HEIGHT - MENU.1 / 2);
    let o = run(
        "t-inside",
        APP_MENU,
        OTHER_APP,
        &[Act::Click((x, y), "left")],
    );
    assert_eq!(o.dismissed, 0);
}

#[test]
fn version_6_still_consumes_or_lets_through() {
    let o = run(
        "t-v6-consume",
        Mode::V6(Outside::Consume),
        OTHER_APP,
        &[Act::Click(ON_BACK, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.back.presses, 0);
    assert!(!o.back_focused);
    let o = run(
        "t-v6-pass",
        Mode::V6(Outside::PassThrough),
        OTHER_APP,
        &[Act::Click(ON_BACK, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.back.presses, 1);
    assert!(o.back_focused);
}

/// An app's popup with a grab (INPUT-01 (a)): The compositor closes it on a click outside; the click
/// stops in its own app and goes through in another one. Returns (back, front, done).
fn popup_click(name: &str, at: (f64, f64)) -> (PointerLog, PointerLog, bool, bool) {
    let compositor = Headless::start(name);
    APP_ID.with(|id| *id.borrow_mut() = OTHER_APP.to_owned());
    let mut back = TestWindow::open(&compositor.wayland, "back");
    APP_ID.with(|id| *id.borrow_mut() = OWNER_APP.to_owned());
    let mut front = TestWindow::open(&compositor.wayland, "front");
    APP_ID.with(|id| id.borrow_mut().clear());
    let mut control = compositor.client();
    // The right click that opens the context menu.
    control
        .call(
            INPUT_CLICK,
            json!({"x": ON_FRONT.0, "y": ON_FRONT.1, "button": "right"}),
        )
        .unwrap();
    // Away from ON_FRONT and ON_BACK: at about (69, 69) on the screen.
    front.open_grabbing_popup((5, 5), (40, 30));
    let base_back = back.pointer_log();
    let base_front = front.pointer_log();
    // The wheel on the own window is dropped and leaves the popup open.
    control
        .call(
            INPUT_SCROLL,
            json!({"x": ON_FRONT.0, "y": ON_FRONT.1, "dy": 15.0}),
        )
        .unwrap();
    assert!(!front.popup_done(), "the wheel closed the popup");
    control
        .call(INPUT_CLICK, json!({"x": at.0, "y": at.1}))
        .unwrap();
    let list: Vec<WindowInfo> =
        serde_json::from_value(control.call(WINDOW_LIST, Value::Null).unwrap()).unwrap();
    let back_focused = list
        .iter()
        .any(|w| w.title.as_deref() == Some("back") && w.focused);
    let back_log = back.pointer_log();
    let front_log = front.pointer_log();
    assert_eq!(
        front_log.axes, base_front.axes,
        "the popup's own app scrolled"
    );
    (
        PointerLog {
            presses: back_log.presses - base_back.presses,
            ..back_log
        },
        PointerLog {
            presses: front_log.presses - base_front.presses,
            ..front_log
        },
        front.popup_done(),
        back_focused,
    )
}

#[test]
fn an_app_popup_click_in_its_own_app_only_closes_it() {
    let (_, front, done, _) = popup_click("t-popup-owner", ON_FRONT);
    assert!(done, "the popup stayed open");
    assert_eq!(front.presses, 0, "the click reached the popup's own window");
}

#[test]
fn an_app_popup_click_in_another_app_closes_it_and_goes_through() {
    let (back, _, done, back_focused) = popup_click("t-popup-other", ON_BACK);
    assert!(done, "the popup stayed open");
    assert_eq!(back.presses, 1, "the click did not reach the other app");
    assert!(back_focused);
}

/// An app popup is kept on the output (#4): the positioner's constraint adjustment moves a
/// menu that would leave the screen (adjustment bits: 1 slide x, 2 slide y, 4 flip x, 8 flip
/// y). Returns the popup's configure and the window's origin.
fn constrained_popup(
    name: &str,
    anchor_rect: (i32, i32, u32, u32),
    size: (u32, u32),
    adjustment: u32,
) -> ((i32, i32, i32, i32), (i32, i32)) {
    let compositor = Headless::start(name);
    APP_ID.with(|id| *id.borrow_mut() = OWNER_APP.to_owned());
    let mut window = TestWindow::open(&compositor.wayland, "menu-owner");
    APP_ID.with(|id| id.borrow_mut().clear());
    let mut control = compositor.client();
    let list: Vec<WindowInfo> =
        serde_json::from_value(control.call(WINDOW_LIST, Value::Null).unwrap()).unwrap();
    let info = list
        .iter()
        .find(|w| w.title.as_deref() == Some("menu-owner"))
        .expect("the window is listed");
    control
        .call(
            INPUT_CLICK,
            json!({"x": info.x + 10, "y": info.y + 10, "button": "right"}),
        )
        .unwrap();
    window.open_popup(PopupSpec {
        anchor_rect,
        size,
        adjustment,
        grab: true,
        nested: false,
        transparent: false,
    });
    let configure = *window
        .popup_configures()
        .last()
        .expect("the popup was not configured");
    (
        (configure.0, configure.1, configure.2, configure.3),
        (info.x, info.y),
    )
}

#[test]
fn an_app_popup_past_the_right_edge_slides_back_on_the_output() {
    // The output is 320 wide: 200 wide from x = 60 of the window does not fit.
    let (popup, window) = constrained_popup("t-popup-slide", (60, 10, 1, 1), (200, 30), 1);
    assert_eq!((popup.2, popup.3), (200, 30), "the size changed");
    assert!(popup.0 < 60, "the popup did not slide left: {popup:?}");
    assert!(
        window.0 + popup.0 + popup.2 <= 320,
        "the popup leaves the output: {popup:?} in a window at {window:?}"
    );
    assert!(window.0 + popup.0 >= 0, "the popup slid off the left edge");
}

#[test]
fn an_app_popup_past_the_bottom_edge_flips_above_its_anchor() {
    // 80 tall under an anchor in the lower half of the output does not fit below it, and fits
    // above it.
    let (popup, window) = constrained_popup("t-popup-flip", (10, 80, 10, 10), (40, 80), 8);
    assert_eq!((popup.2, popup.3), (40, 80));
    assert_eq!(
        popup.1,
        80 - 80,
        "the popup did not flip above the anchor: {popup:?}"
    );
    assert!(
        window.1 + popup.1 >= 0,
        "the flipped popup leaves the output"
    );
}

#[test]
fn an_app_popup_that_fits_stays_where_it_was_asked() {
    let (popup, _) = constrained_popup("t-popup-fits", (5, 5, 1, 1), (40, 30), 1 | 2 | 4 | 8);
    assert_eq!(popup, (5, 6, 40, 30));
}

#[test]
fn a_submenu_popup_keeps_its_menu_open_and_grabbed() {
    let compositor = Headless::start("t-popup-nested");
    APP_ID.with(|id| *id.borrow_mut() = OWNER_APP.to_owned());
    let mut front = TestWindow::open(&compositor.wayland, "front");
    APP_ID.with(|id| id.borrow_mut().clear());
    let mut control = compositor.client();
    control
        .call(
            INPUT_CLICK,
            json!({"x": 100.0, "y": 100.0, "button": "right"}),
        )
        .unwrap();
    let menu = PopupSpec {
        anchor_rect: (5, 5, 1, 1),
        size: (40, 30),
        adjustment: 0,
        grab: true,
        nested: false,
        transparent: false,
    };
    front.open_popup(menu);
    front.open_popup(PopupSpec {
        anchor_rect: (30, 5, 1, 1),
        nested: true,
        ..menu
    });
    assert!(!front.popup_done(), "the submenu closed its menu");
    // The submenu goes away (the pointer left it): its menu is still grabbed.
    front.close_child_popup();
    assert!(!front.popup_done());
    // A click away from the app ends the menu that is left.
    control
        .call(INPUT_CLICK, json!({"x": 300.0, "y": 190.0}))
        .unwrap();
    assert!(
        front.popup_done(),
        "the menu is no longer grabbed after its submenu closed"
    );
}

#[test]
fn a_pointer_over_a_submenu_reaches_it_and_a_click_there_keeps_the_menus_open() {
    let compositor = Headless::start("t-popup-nested-hit");
    APP_ID.with(|id| *id.borrow_mut() = OWNER_APP.to_owned());
    let mut front = TestWindow::open(&compositor.wayland, "front");
    APP_ID.with(|id| id.borrow_mut().clear());
    let mut control = compositor.client();
    let list: Vec<WindowInfo> =
        serde_json::from_value(control.call(WINDOW_LIST, Value::Null).unwrap()).unwrap();
    let info = list
        .iter()
        .find(|w| w.title.as_deref() == Some("front"))
        .expect("the window is listed")
        .clone();
    control
        .call(
            INPUT_CLICK,
            json!({"x": info.x + 10, "y": info.y + 10, "button": "right"}),
        )
        .unwrap();
    let menu = PopupSpec {
        anchor_rect: (5, 5, 1, 1),
        size: (40, 30),
        adjustment: 0,
        grab: true,
        nested: false,
        transparent: false,
    };
    front.open_popup(menu);
    front.open_popup(PopupSpec {
        anchor_rect: (30, 5, 1, 1),
        nested: true,
        ..menu
    });
    let configures = front.popup_configures();
    let (root, sub) = (
        configures[configures.len() - 2],
        configures[configures.len() - 1],
    );
    // The middle of the submenu, in the window's coordinates, on the screen.
    let x = f64::from(info.x + root.0 + sub.0 + sub.2 / 2);
    let y = f64::from(info.y + root.1 + sub.1 + sub.3 / 2);
    let before = front.pointer_log();
    control
        .call(INPUT_POINTER_MOVE, json!({"x": x, "y": y}))
        .unwrap();
    let moved = front.pointer_log();
    assert!(
        moved.enters > before.enters,
        "the pointer did not reach the popups: {configures:?}"
    );
    control.call(INPUT_CLICK, json!({"x": x, "y": y})).unwrap();
    let after = front.pointer_log();
    assert!(
        !front.popup_done(),
        "a click on the submenu closed the menus: {configures:?}"
    );
    assert_eq!(
        after.presses,
        before.presses + 1,
        "the press did not reach the submenu"
    );
}

#[test]
fn a_full_screen_dock_with_a_small_region_still_ends_on_a_press_outside() {
    // The Dock's surface is as tall as the screen; only its menu's corner takes the pointer.
    let wide = Mode::V7Wide(OTHER_APP, Rule::Activate, Rule::Pass);
    let o = run(
        "t-dock-wide-other",
        wide,
        OWNER_APP,
        &[Act::Click(ON_FRONT, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(
        o.front.presses, 1,
        "the click did not reach the window below"
    );
    let o = run(
        "t-dock-wide-owner",
        wide,
        OTHER_APP,
        &[Act::Click(ON_BACK, "left")],
    );
    assert_eq!(o.dismissed, 1);
    assert_eq!(o.back.presses, 0);
}
