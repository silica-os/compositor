//! ext-image-copy-capture-v1: the shell captures the screen and one window into its shm
//! buffers; other clients do not see the capture globals.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{Read, Seek};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::process::Command;

use common::Headless;
use common::client::{RED_BGRA, TestWindow, WINDOW};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::{
    self, ExtForeignToplevelListV1,
};
use wayland_protocols::ext::image_capture_source::v1::client::ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{
    self, ExtImageCopyCaptureFrameV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::{
    ExtImageCopyCaptureManagerV1, Options,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{
    self, ExtImageCopyCaptureSessionV1,
};

const CHILD_SOCKET: &str = "SILICA_TEST_CAPTURE_SOCKET";
const MANAGER: &str = "ext_image_copy_capture_manager_v1";

#[derive(Default)]
struct State {
    globals: Vec<String>,
    output: Option<WlOutput>,
    shm: Option<WlShm>,
    output_sources: Option<ExtOutputImageCaptureSourceManagerV1>,
    toplevel_sources: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    manager: Option<ExtImageCopyCaptureManagerV1>,
    toplevels: Vec<ExtForeignToplevelHandleV1>,
    size: Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    done: bool,
    stopped: bool,
    /// Some(true): ready, Some(false): failed.
    result: Option<bool>,
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
        let wl_registry::Event::Global {
            name, interface, ..
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_output" => state.output = Some(registry.bind(name, 1, qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "ext_output_image_capture_source_manager_v1" => {
                state.output_sources = Some(registry.bind(name, 1, qh, ()));
            }
            "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                state.toplevel_sources = Some(registry.bind(name, 1, qh, ()));
            }
            MANAGER => state.manager = Some(registry.bind(name, 1, qh, ())),
            "ext_foreign_toplevel_list_v1" => {
                let _: ExtForeignToplevelListV1 = registry.bind(name, 1, qh, ());
            }
            _ => {}
        }
        state.globals.push(interface);
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(toplevel);
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.size = Some((width, height));
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                if let Ok(format) = format.into_result() {
                    state.formats.push(format);
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => state.done = true,
            ext_image_copy_capture_session_v1::Event::Stopped => state.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => state.result = Some(true),
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => state.result = Some(false),
            _ => {}
        }
    }
}

delegate_noop!(State: ignore WlOutput);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore ExtForeignToplevelHandleV1);
delegate_noop!(State: ignore ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ignore ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(State: ignore ExtImageCaptureSourceV1);
delegate_noop!(State: ignore ExtImageCopyCaptureManagerV1);

fn connect(socket: &std::path::Path) -> (EventQueue<State>, State) {
    let connection = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = connection.new_event_queue();
    let _registry = connection.display().get_registry(&queue.handle(), ());
    let mut state = State::default();
    queue.roundtrip(&mut state).unwrap();
    queue.roundtrip(&mut state).unwrap();
    (queue, state)
}

/// One frame of `source`: its size and how many of its pixels are the test window's red.
fn capture(
    queue: &mut EventQueue<State>,
    state: &mut State,
    source: &ExtImageCaptureSourceV1,
) -> (u32, u32, usize) {
    let qh = queue.handle();
    state.size = None;
    state.done = false;
    state.result = None;
    let session = state
        .manager
        .as_ref()
        .unwrap()
        .create_session(source, Options::empty(), &qh, ());
    while !state.done && !state.stopped {
        queue.blocking_dispatch(state).unwrap();
    }
    assert!(!state.stopped, "the session stopped");
    assert!(state.formats.contains(&wl_shm::Format::Argb8888));
    let (width, height) = state.size.unwrap();
    let len = (width * height * 4) as usize;
    let mut file = tempfile::tempfile().unwrap();
    file.set_len(len as u64).unwrap();
    let pool = state
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), len as i32, &qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        width as i32 * 4,
        wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&buffer);
    frame.damage_buffer(0, 0, width as i32, height as i32);
    frame.capture();
    while state.result.is_none() {
        queue.blocking_dispatch(state).unwrap();
    }
    assert_eq!(state.result, Some(true), "the frame failed");
    let mut pixels = Vec::new();
    file.rewind().unwrap();
    file.read_to_end(&mut pixels).unwrap();
    let red = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| **p == RED_BGRA)
        .count();
    frame.destroy();
    session.destroy();
    (width, height, red)
}

/// Runs in a child process that the compositor takes for the shell (`SILICA_SHELL=1`).
#[test]
fn capture_child() {
    let Ok(socket) = std::env::var(CHILD_SOCKET) else {
        return;
    };
    let (mut queue, mut state) = connect(std::path::Path::new(&socket));
    let qh = queue.handle();
    let output = state.output.clone().unwrap();
    let source = state
        .output_sources
        .as_ref()
        .expect("the shell sees the output sources")
        .create_source(&output, &qh, ());
    let (width, height, red) = capture(&mut queue, &mut state, &source);
    println!("OUTPUT {width} {height} {red}");
    let toplevel = state.toplevels.first().cloned().expect("a toplevel");
    let source = state
        .toplevel_sources
        .as_ref()
        .expect("the shell sees the toplevel sources")
        .create_source(&toplevel, &qh, ());
    let (width, height, red) = capture(&mut queue, &mut state, &source);
    println!("TOPLEVEL {width} {height} {red}");
}

fn told(stdout: &str, key: &str) -> Vec<u32> {
    stdout
        .lines()
        .find_map(|line| line.split_once(key).map(|(_, rest)| rest))
        .unwrap_or_else(|| panic!("the child printed no {key}: {stdout}"))
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect()
}

#[test]
fn the_shell_captures_the_screen_and_a_window() {
    let compositor = Headless::start("image-copy");
    let _window = TestWindow::open(&compositor.wayland, "captured");
    let (_queue, state) = connect(&compositor.wayland);
    assert!(
        !state.globals.iter().any(|g| g == MANAGER),
        "an app sees the capture manager"
    );

    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "capture_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, &compositor.wayland)
        .env("SILICA_SHELL", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let area = (WINDOW.0 * WINDOW.1) as usize;
    let screen = told(&stdout, "OUTPUT ");
    assert_eq!((screen[0], screen[1]), (common::WIDTH, common::HEIGHT));
    assert!(
        screen[2] as usize >= area / 2,
        "the screen shows the window: {screen:?}"
    );
    let window = told(&stdout, "TOPLEVEL ");
    assert!(window[0] >= WINDOW.0 && window[1] >= WINDOW.1, "{window:?}");
    assert!(
        window[2] as usize >= area / 2,
        "the window capture: {window:?}"
    );
}
