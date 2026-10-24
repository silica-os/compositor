//! The Dock's control (`silica-dock-v1`): nodes for windows and layers with their frame and
//! state, hiding, placing, gestures, hot corners, the forwarded commands. The Dock is a child
//! process (a shell component, `SILICA_SHELL=1`) driven line by line from the test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use common::client::TestWindow;
use common::{Headless, WIDTH, pixel_in, screenshot};
use serde_json::{Value, json};
use silica_control::commands::{INPUT_GESTURE, INPUT_POINTER_MOVE, WINDOW_LIST, WindowInfo};
use silica_protocols::dock::client::silica_dock_control_v1::{
    self, EVT_NODE_OPCODE, SilicaDockControlV1,
};
use silica_protocols::dock::client::silica_dock_manager_v1::SilicaDockManagerV1;
use silica_protocols::dock::client::silica_dock_node_v1::{self, SilicaDockNodeV1};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, event_created_child};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

const CHILD_SOCKET: &str = "SILICA_TEST_DOCK_SOCKET";

#[derive(Default)]
struct Dock {
    manager: Option<SilicaDockManagerV1>,
    control: Option<SilicaDockControlV1>,
    nodes: Vec<SilicaDockNodeV1>,
    /// The lines to print.
    out: Vec<String>,
}

/// The name of an enum value in a log line.
fn name<T: std::fmt::Debug>(value: WEnum<T>) -> String {
    match value {
        WEnum::Value(v) => format!("{v:?}"),
        WEnum::Unknown(v) => v.to_string(),
    }
}

impl Dispatch<WlRegistry, ()> for Dock {
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
            && interface == "silica_dock_manager_v1"
        {
            state.manager = Some(registry.bind(name, 2, qh, ()));
        }
    }
}

impl Dispatch<SilicaDockManagerV1, ()> for Dock {
    fn event(
        _: &mut Self,
        _: &SilicaDockManagerV1,
        _: <SilicaDockManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<SilicaDockControlV1, ()> for Dock {
    fn event(
        state: &mut Self,
        _: &SilicaDockControlV1,
        event: silica_dock_control_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            silica_dock_control_v1::Event::Node { node, kind, key } => {
                let index = state.nodes.len();
                let kind = match kind {
                    WEnum::Value(silica_dock_control_v1::Kind::Window) => "window",
                    WEnum::Value(silica_dock_control_v1::Kind::MenuBar) => "menu_bar",
                    _ => "layer",
                };
                state.out.push(format!("NODE {index} {kind} {key}"));
                state.nodes.push(node);
            }
            silica_dock_control_v1::Event::Swipe {
                phase,
                fingers,
                dx,
                dy,
            } => state
                .out
                .push(format!("SWIPE {} {fingers} {dx:.1} {dy:.1}", name(phase))),
            silica_dock_control_v1::Event::Pinch {
                phase,
                fingers,
                scale,
            } => state
                .out
                .push(format!("PINCH {} {fingers} {scale:.2}", name(phase))),
            silica_dock_control_v1::Event::Corner {
                corner,
                entered,
                modifiers,
                ..
            } => state.out.push(format!(
                "CORNER {} {entered} {}",
                name(corner),
                match modifiers {
                    WEnum::Value(m) => m.bits(),
                    WEnum::Unknown(v) => v,
                }
            )),
            _ => {}
        }
    }

    event_created_child!(Dock, SilicaDockControlV1, [
        EVT_NODE_OPCODE => (SilicaDockNodeV1, ()),
    ]);
}

impl Dispatch<SilicaDockNodeV1, ()> for Dock {
    fn event(
        state: &mut Self,
        node: &SilicaDockNodeV1,
        event: silica_dock_node_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let index = state
            .nodes
            .iter()
            .position(|n| n == node)
            .unwrap_or(usize::MAX);
        match event {
            silica_dock_node_v1::Event::Frame {
                x,
                y,
                width,
                height,
            } => state
                .out
                .push(format!("FRAME {index} {x} {y} {width} {height}")),
            silica_dock_node_v1::Event::State { state: bits } => {
                let bits = match bits {
                    WEnum::Value(b) => b.bits(),
                    WEnum::Unknown(v) => v,
                };
                state.out.push(format!("STATE {index} {bits}"));
            }
            silica_dock_node_v1::Event::Closed => state.out.push(format!("CLOSED {index}")),
            _ => {}
        }
    }
}

/// The Dock process: reads commands from stdin, prints what it receives.
fn dock_client(socket: &str) {
    let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue: EventQueue<Dock> = conn.new_event_queue();
    let qh = queue.handle();
    let mut dock = Dock::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut dock).unwrap();
    let Some(manager) = dock.manager.clone() else {
        println!("NOMANAGER");
        return;
    };
    let flush = |dock: &mut Dock| {
        for line in dock.out.drain(..) {
            println!("{line}");
        }
    };
    println!("READY");
    for line in std::io::stdin().lines() {
        let line = line.unwrap();
        let words: Vec<&str> = line.split_whitespace().collect();
        let number = |i: usize| -> f64 { words.get(i).and_then(|w| w.parse().ok()).unwrap_or(0.0) };
        let node = |i: usize, dock: &Dock| dock.nodes.get(number(i) as usize).cloned();
        match words.first().copied() {
            Some("bind") => {
                let control = manager.get_control(&qh, ());
                dock.control = Some(control);
            }
            Some("commit") => {
                if let Some(c) = &dock.control {
                    c.commit();
                }
            }
            Some("swipe_fingers") => {
                if let Some(c) = &dock.control {
                    c.set_swipe_fingers(number(1) as u32);
                }
            }
            Some("hide") => {
                if let Some(n) = node(1, &dock) {
                    n.set_visibility(silica_dock_node_v1::Visibility::Hidden);
                }
            }
            Some("show") => {
                if let Some(n) = node(1, &dock) {
                    n.set_visibility(silica_dock_node_v1::Visibility::Shown);
                }
            }
            // warp NODE X Y W H BEND TOP BOTTOM
            Some("warp") => {
                if let Some(n) = node(1, &dock) {
                    n.warp(
                        number(2),
                        number(3),
                        number(4),
                        number(5),
                        number(6),
                        number(7),
                        number(8),
                    );
                }
            }
            Some("clear_warp") => {
                if let Some(n) = node(1, &dock) {
                    n.clear_warp();
                }
            }
            Some("inherit") => {
                if let Some(n) = node(1, &dock) {
                    n.set_visibility(silica_dock_node_v1::Visibility::Inherit);
                }
            }
            // place NODE INDEX X Y SCALE [above]
            Some("place") => {
                if let Some(n) = node(1, &dock) {
                    let level = if words.get(6) == Some(&"above") {
                        silica_dock_node_v1::Level::AboveTop
                    } else {
                        silica_dock_node_v1::Level::Stack
                    };
                    n.place(
                        number(2) as u32,
                        level,
                        number(3),
                        number(4),
                        number(5),
                        1.0,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                    );
                }
            }
            // placefx NODE INDEX X Y SCALE OPACITY CLIP_X CLIP_Y CLIP_W CLIP_H
            Some("placefx") => {
                if let Some(n) = node(1, &dock) {
                    n.place(
                        number(2) as u32,
                        silica_dock_node_v1::Level::Stack,
                        number(3),
                        number(4),
                        number(5),
                        number(6),
                        number(7),
                        number(8),
                        number(9),
                        number(10),
                    );
                }
            }
            Some("unplace") => {
                if let Some(n) = node(1, &dock) {
                    n.remove_placement(number(2) as u32);
                }
            }
            Some("activate") => {
                if let Some(n) = node(1, &dock) {
                    n.activate();
                }
            }
            Some("release") => {
                if let Some(c) = dock.control.take() {
                    c.destroy();
                }
            }
            Some("quit") => break,
            _ => {}
        }
        // Everything the compositor sends by now.
        queue.roundtrip(&mut dock).unwrap();
        flush(&mut dock);
        println!("OK");
    }
}

/// The child process: runs only when the parent test launches it with the socket.
#[test]
fn dock_child() {
    if let Ok(socket) = std::env::var(CHILD_SOCKET) {
        dock_client(&socket);
    }
}

/// A Dock process driven from the test.
struct DockProcess {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    log: Vec<String>,
}

impl DockProcess {
    fn start(socket: &std::path::Path, shell: bool) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "dock_child",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_SOCKET, socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if shell {
            command.env("SILICA_SHELL", "1");
        } else {
            command.env_remove("SILICA_SHELL");
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut dock = Self {
            child,
            stdin,
            lines,
            log: Vec::new(),
        };
        // The harness prints its own lines too ("running 1 test"): skip to READY or NOMANAGER.
        loop {
            let line = dock.next_line();
            // The harness' "test dock_child ... " comes first on the same line.
            for word in ["READY", "NOMANAGER"] {
                if line.ends_with(word) {
                    dock.log.push(word.to_owned());
                    return dock;
                }
            }
        }
    }

    fn next_line(&mut self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(10))
            .expect("the Dock process said nothing")
    }

    /// Sends a command; returns the lines printed until it is done.
    fn run(&mut self, command: &str) -> Vec<String> {
        writeln!(self.stdin, "{command}").unwrap();
        self.stdin.flush().unwrap();
        let mut got = Vec::new();
        loop {
            let line = self.next_line();
            if line == "OK" {
                break;
            }
            got.push(line);
        }
        self.log.extend(got.iter().cloned());
        got
    }
}

impl Drop for DockProcess {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.child.wait();
    }
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

#[test]
fn only_shell_components_see_the_dock_control() {
    let compositor = Headless::start("dock-global");
    let outsider = DockProcess::start(&compositor.wayland, false);
    assert_eq!(outsider.log, ["NOMANAGER"]);
    let dock = DockProcess::start(&compositor.wayland, true);
    assert_eq!(dock.log, ["READY"]);
}

#[test]
fn the_control_announces_windows_and_layers_and_follows_them() {
    let compositor = Headless::start("dock-nodes");
    let _first = TestWindow::open_sized(&compositor.wayland, "first", (120, 80));
    let _bar = TestWindow::open_layer_at(
        &compositor.wayland,
        Layer::Top,
        "dock-test-layer",
        Anchor::Bottom | Anchor::Left | Anchor::Right,
        (0, 30),
        None,
        |_, _, _, _| [40, 40, 40, 255],
    );
    let mut dock = DockProcess::start(&compositor.wayland, true);
    let got = dock.run("bind");
    let nodes: Vec<&String> = got.iter().filter(|l| l.starts_with("NODE")).collect();
    assert!(
        nodes.iter().any(|l| l.contains("window")),
        "a node for the window: {got:?}"
    );
    assert!(
        nodes.iter().any(|l| l.contains("layer dock-test-layer")),
        "a node for the layer: {got:?}"
    );
    // The frame and the state come with the node; the window is the focused one.
    let window_index: usize = nodes
        .iter()
        .find(|l| l.contains("window"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|i| i.parse().ok())
        .unwrap();
    assert!(
        got.iter()
            .any(|l| l.starts_with(&format!("FRAME {window_index} "))),
        "{got:?}"
    );
    assert!(
        got.iter().any(|l| l == &format!("STATE {window_index} 8")),
        "focused: {got:?}"
    );
    // A second Dock cannot take the control.
    let mut second = DockProcess::start(&compositor.wayland, true);
    let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| second.run("bind")));
    assert!(rejected.is_err(), "the second control was refused");
}

#[test]
fn a_window_the_dock_hides_leaves_the_screen_and_comes_back_where_it_was() {
    let compositor = Headless::start("dock-hide");
    let _window = TestWindow::open_sized(&compositor.wayland, "away", (120, 80));
    let mut dock = DockProcess::start(&compositor.wayland, true);
    let got = dock.run("bind");
    let index = got
        .iter()
        .find(|l| l.starts_with("NODE") && l.contains("window"))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap()
        .to_owned();
    let before = windows(&compositor);
    assert_eq!(before.len(), 1);
    dock.run(&format!("hide {index}"));
    // Nothing happens before the commit.
    assert_eq!(windows(&compositor).len(), 1);
    dock.run("commit");
    assert!(
        windows(&compositor).is_empty(),
        "the window left the screen"
    );
    dock.run(&format!("inherit {index}"));
    dock.run("commit");
    let after = windows(&compositor);
    assert_eq!(after.len(), 1);
    assert_eq!((after[0].x, after[0].y), (before[0].x, before[0].y));
    // Letting go of the control also brings back what it hid.
    dock.run(&format!("hide {index}"));
    dock.run("commit");
    assert!(windows(&compositor).is_empty());
    dock.run("release");
    assert_eq!(windows(&compositor).len(), 1, "the control's overrides end");
}

#[test]
fn a_placed_window_is_drawn_where_the_dock_puts_it() {
    // A green desktop and a red window at (32, 32): placed at (200, 100), half size.
    let compositor = Headless::start_with(
        "dock-place",
        (640, 400),
        &["--background", "00ff00", "--window-origin", "32"],
    );
    let _window = TestWindow::open_sized(&compositor.wayland, "placed", (200, 120));
    let mut dock = DockProcess::start(&compositor.wayland, true);
    let got = dock.run("bind");
    let index = got
        .iter()
        .find(|l| l.starts_with("NODE") && l.contains("window"))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap()
        .to_owned();
    let red = |shot: &[u8], x, y| {
        let p = pixel_in(shot, 640, x, y);
        p[0] > 150 && p[1] < 90
    };
    let shot = screenshot(&compositor);
    assert!(red(&shot, 100, 80), "the window is at its place");
    dock.run(&format!("place {index} 0 300 200 0.5"));
    dock.run("commit");
    let shot = screenshot(&compositor);
    assert!(!red(&shot, 100, 80), "it left its place");
    // Its frame is 200 wide and 120 + the bar tall at half size: the middle is around
    // (300 + 50, 200 + 35).
    assert!(red(&shot, 350, 240), "it is drawn at the placement");
    assert!(!red(&shot, 500, 240), "and no bigger than half");
    dock.run(&format!("unplace {index} 0"));
    dock.run("commit");
    let shot = screenshot(&compositor);
    assert!(
        red(&shot, 100, 80),
        "without a placement it is where it was"
    );
}

#[test]
fn swipes_pinches_and_corners_go_to_the_dock() {
    let compositor = Headless::start("dock-input");
    let mut dock = DockProcess::start(&compositor.wayland, true);
    dock.run("bind");
    dock.run("swipe_fingers 4");
    let mut client = compositor.client();
    let mut swipe = |fingers: u32, phase: &str| {
        client
            .call(
                INPUT_GESTURE,
                json!({"kind": "swipe", "phase": phase, "fingers": fingers, "dx": -40.0, "dy": 0.0}),
            )
            .unwrap();
    };
    swipe(4, "begin");
    swipe(4, "update");
    swipe(4, "end");
    let log = dock.run("sync");
    assert!(
        log.iter().any(|l| l.starts_with("SWIPE Begin 4"))
            && log.iter().any(|l| l.starts_with("SWIPE End 4")),
        "{log:?}"
    );
    // Three fingers are not the Dock's.
    swipe(3, "begin");
    swipe(3, "end");
    let log = dock.run("sync");
    assert!(!log.iter().any(|l| l.starts_with("SWIPE")), "{log:?}");
    // The pointer in a corner, then away.
    client
        .call(INPUT_POINTER_MOVE, json!({"x": 0.0, "y": 0.0}))
        .unwrap();
    client
        .call(INPUT_POINTER_MOVE, json!({"x": 100.0, "y": 100.0}))
        .unwrap();
    let log = dock.run("sync");
    assert!(
        log.iter().any(|l| l == "CORNER TopLeft 1 0")
            && log.iter().any(|l| l == "CORNER TopLeft 0 0"),
        "{log:?}"
    );
}

/// The index of the first window node in what the Dock printed.
fn window_node(lines: &[String]) -> String {
    lines
        .iter()
        .find(|l| l.starts_with("NODE") && l.contains("window"))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap()
        .to_owned()
}

#[test]
fn the_dock_drives_the_minimize_effect() {
    use silica_control::commands::{WINDOW_FOCUS, WINDOW_MINIMIZE};
    let compositor = Headless::start_with(
        "dock-minimize",
        (640, 400),
        &["--background", "00ff00", "--window-origin", "32"],
    );
    let _window = TestWindow::open_sized(&compositor.wayland, "genie", (200, 120));
    let mut dock = DockProcess::start(&compositor.wayland, true);
    let node = window_node(&dock.run("bind"));
    let red = |shot: &[u8], x, y| {
        let p = pixel_in(shot, 640, x, y);
        p[0] > 150 && p[1] < 90
    };
    let mut client = compositor.client();
    client.call(WINDOW_MINIMIZE, Value::Null).unwrap();
    // The window is minimized, and the compositor ran no effect: the Dock is told.
    let log = dock.run("sync");
    assert!(
        log.iter().any(|l| l == &format!("STATE {node} 1")),
        "minimized: {log:?}"
    );
    // Until the Dock takes it over, the window stays where it was.
    assert!(red(&screenshot(&compositor), 100, 80), "held on screen");
    // The Dock shows it warped all the way into a 40 × 20 cell at (500, 340).
    dock.run(&format!("show {node}"));
    dock.run(&format!("warp {node} 500 340 40 20 1 1 1"));
    dock.run("commit");
    let shot = screenshot(&compositor);
    assert!(!red(&shot, 100, 80), "it left its place");
    assert!(red(&shot, 520, 350), "it is in the cell");
    // The effect ends: the window is minimized for good, and shows nowhere.
    dock.run(&format!("clear_warp {node}"));
    dock.run(&format!("inherit {node}"));
    dock.run("commit");
    let shot = screenshot(&compositor);
    assert!(!red(&shot, 100, 80) && !red(&shot, 520, 350), "gone");
    // Restoring: the window is back in the space but held off the screen for the Dock.
    let id = windows(&compositor)[0].id;
    client.call(WINDOW_FOCUS, json!({ "id": id })).unwrap();
    let log = dock.run("sync");
    assert!(
        log.iter().any(|l| l == &format!("STATE {node} 8")),
        "restored and focused: {log:?}"
    );
    assert!(!red(&screenshot(&compositor), 100, 80), "held off screen");
    // The Dock warps it out of the cell, then clears the warp: it is where it was.
    dock.run(&format!("warp {node} 500 340 40 20 1 1 1"));
    dock.run("commit");
    assert!(
        red(&screenshot(&compositor), 520, 350),
        "coming out of the cell"
    );
    dock.run(&format!("clear_warp {node}"));
    dock.run("commit");
    let shot = screenshot(&compositor);
    assert!(red(&shot, 100, 80), "back where it was");
}

#[test]
fn a_placement_fades_and_is_clipped() {
    // A green desktop and a red window at (32, 32) of 200 × 120 (plus its bar).
    let compositor = Headless::start_with(
        "dock-fade",
        (640, 400),
        &["--background", "00ff00", "--window-origin", "32"],
    );
    let _window = TestWindow::open_sized(&compositor.wayland, "fade", (200, 120));
    let mut dock = DockProcess::start(&compositor.wayland, true);
    let node = window_node(&dock.run("bind"));
    // Half opaque, in place: red over green.
    dock.run(&format!("placefx {node} 0 32 32 1 0.5 0 0 0 0"));
    dock.run("commit");
    let shot = screenshot(&compositor);
    let p = pixel_in(&shot, 640, 100, 100);
    assert!(
        (90..170).contains(&p[0]) && (90..170).contains(&p[1]),
        "half red over half green: {p:?}"
    );
    // Clipped to a 60-wide strip from x 32: the rest of the window is not drawn.
    dock.run(&format!("placefx {node} 0 32 32 1 1 32 0 60 400"));
    dock.run("commit");
    let shot = screenshot(&compositor);
    let inside = pixel_in(&shot, 640, 60, 100);
    let outside = pixel_in(&shot, 640, 150, 100);
    assert!(
        inside[0] > 150 && inside[1] < 90,
        "inside the clip: {inside:?}"
    );
    assert!(
        outside[0] < 90 && outside[1] > 150,
        "outside the clip: {outside:?}"
    );
}

fn node_of(lines: &[String], kind: &str) -> String {
    lines
        .iter()
        .find(|l| l.starts_with("NODE") && l.contains(kind))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap()
        .to_owned()
}

#[test]
fn the_dock_hides_and_places_a_layer() {
    // A green desktop and a red 100 × 60 wallpaper layer at the top left corner.
    let compositor = Headless::start_with("dock-layer", (640, 400), &["--background", "00ff00"]);
    let _wallpaper = TestWindow::open_layer_at(
        &compositor.wayland,
        Layer::Bottom,
        "wallpaper",
        Anchor::Top | Anchor::Left,
        (100, 60),
        None,
        |_, _, _, _| [0, 0, 255, 255],
    );
    let mut dock = DockProcess::start(&compositor.wayland, true);
    let node = node_of(&dock.run("bind"), "layer wallpaper");
    let red = |shot: &[u8], x, y| {
        let p = pixel_in(shot, 640, x, y);
        p[0] > 150 && p[1] < 90
    };
    assert!(red(&screenshot(&compositor), 50, 30), "the layer is drawn");
    dock.run(&format!("hide {node}"));
    dock.run("commit");
    assert!(!red(&screenshot(&compositor), 50, 30), "hidden by the Dock");
    dock.run(&format!("inherit {node}"));
    dock.run("commit");
    assert!(red(&screenshot(&compositor), 50, 30), "back");
    dock.run(&format!("place {node} 0 200 100 1"));
    dock.run("commit");
    let shot = screenshot(&compositor);
    assert!(!red(&shot, 50, 30), "it left its place");
    assert!(red(&shot, 250, 130), "it is drawn at the placement");
}

#[test]
fn the_dock_hides_the_menu_bar() {
    let compositor = Headless::start("dock-menu-bar");
    let mut app = TestWindow::open(&compositor.wayland, "app");
    app.add_to_menu_bar(None, 100, [0xff, 0, 0, 0xff]);
    let blue = |shot: &[u8]| {
        let p = pixel_in(shot, WIDTH, 50, 16);
        p[2] > 150 && p[0] < 90
    };
    assert!(blue(&screenshot(&compositor)), "the titles are drawn");
    let mut dock = DockProcess::start(&compositor.wayland, true);
    let node = node_of(&dock.run("bind"), "menu_bar menu-bar");
    dock.run(&format!("hide {node}"));
    dock.run("commit");
    assert!(!blue(&screenshot(&compositor)), "the strip is hidden");
    dock.run(&format!("inherit {node}"));
    dock.run("commit");
    assert!(blue(&screenshot(&compositor)), "and back");
}
