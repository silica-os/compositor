//! The lock screen is an `ext_session_lock_v1` client (the shell's locker). While the session is
//! locked only its surfaces are drawn and get input; if it dies the session stays locked; the
//! lock ends only when the client that holds it unlocks.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::client::{FIRST_WINDOW_AT, TestWindow};
use common::{Headless, pixel, screenshot};
use serde_json::{Value, json};
use silica_control::commands::{INPUT_CLICK, INPUT_KEY, INPUT_POINTER_MOVE};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::{
    self, ExtSessionLockSurfaceV1,
};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::{
    self, ExtSessionLockV1,
};

const CHILD_SOCKET: &str = "SILICA_TEST_LOCKER_SOCKET";

#[derive(Default)]
struct Locker {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    output: Option<WlOutput>,
    manager: Option<ExtSessionLockManagerV1>,
    locked: bool,
    finished: bool,
    size: Option<(u32, u32)>,
}

impl Dispatch<WlRegistry, ()> for Locker {
    fn event(
        app: &mut Self,
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
                "wl_compositor" => app.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_output" => app.output = Some(registry.bind(name, 1, qh, ())),
                "ext_session_lock_manager_v1" => {
                    app.manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Locker {
    fn event(
        app: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => app.locked = true,
            ext_session_lock_v1::Event::Finished => app.finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for Locker {
    fn event(
        app: &mut Self,
        surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
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

impl Dispatch<WlShm, ()> for Locker {
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

delegate_noop!(Locker: ignore WlCompositor);
delegate_noop!(Locker: ignore WlOutput);
delegate_noop!(Locker: ignore WlSurface);
delegate_noop!(Locker: ignore WlBuffer);
delegate_noop!(Locker: WlShmPool);
delegate_noop!(Locker: ignore ExtSessionLockManagerV1);

/// The child: takes the lock, draws a white lock surface, and unlocks when told to.
fn locker(socket: &str) {
    let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut app = Locker::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut app).unwrap();
    let Some(manager) = app.manager.clone() else {
        println!("NO MANAGER");
        return;
    };
    let lock = manager.lock(&qh, ());
    queue.roundtrip(&mut app).unwrap();
    if app.finished {
        println!("FINISHED");
        return;
    }
    while !app.locked {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    let surface = app.compositor.as_ref().unwrap().create_surface(&qh, ());
    let _lock_surface = lock.get_lock_surface(&surface, app.output.as_ref().unwrap(), &qh, ());
    while app.size.is_none() {
        queue.blocking_dispatch(&mut app).unwrap();
    }
    let (w, h) = app.size.unwrap();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&vec![0xFFu8; (w * h * 4) as usize]).unwrap();
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
    queue.roundtrip(&mut app).unwrap();
    println!("LOCKED!");
    std::io::stdout().flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "unlock" => {
                lock.unlock_and_destroy();
                queue.roundtrip(&mut app).unwrap();
                println!("UNLOCKED");
                return;
            }
            // A wrong password: the locker stays as it is.
            "wrong" => {
                println!("WRONG");
                std::io::stdout().flush().unwrap();
            }
            _ => {}
        }
    }
}

/// The child process: runs only when the parent test launches it with the socket.
#[test]
fn locker_child() {
    if let Ok(socket) = std::env::var(CHILD_SOCKET) {
        locker(&socket);
    }
}

struct LockerProcess {
    child: Child,
    out: BufReader<std::process::ChildStdout>,
}

impl LockerProcess {
    fn start(socket: &std::path::Path, shell: bool) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "locker_child",
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
        let out = BufReader::new(child.stdout.take().unwrap());
        Self { child, out }
    }

    /// The next line the child prints that is one of ours.
    fn wait_for(&mut self, word: &str) -> bool {
        let mut line = String::new();
        loop {
            line.clear();
            if self.out.read_line(&mut line).unwrap() == 0 {
                return false;
            }
            if line.contains(word) {
                return true;
            }
        }
    }

    fn say(&mut self, word: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{word}").unwrap();
        stdin.flush().unwrap();
    }
}

fn lock_info(compositor: &Headless) -> Value {
    compositor.client().call("idle.state", Value::Null).unwrap()["lock"].clone()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What the two windows received (counts only: losing the focus to the lock surface is a
/// leave, not input).
fn received(first: &mut TestWindow, second: &mut TestWindow) -> [u32; 10] {
    let mut counts = [0; 10];
    for (i, window) in [first, second].into_iter().enumerate() {
        let (p, k) = (window.pointer_log(), window.key_log());
        counts[i * 5..i * 5 + 5].copy_from_slice(&[p.enters, p.motions, p.presses, p.axes, k.keys]);
    }
    counts
}

/// Moves over the first window, clicks and types.
fn poke(compositor: &Headless, shortcut: bool) {
    let mut c = compositor.client();
    let at = f64::from(FIRST_WINDOW_AT);
    c.call(INPUT_POINTER_MOVE, json!({"x": at + 20.0, "y": at + 40.0}))
        .unwrap();
    c.call(INPUT_POINTER_MOVE, json!({"x": at + 25.0, "y": at + 45.0}))
        .unwrap();
    c.call(INPUT_CLICK, json!({"x": at + 25.0, "y": at + 45.0}))
        .unwrap();
    c.call(INPUT_KEY, json!({"key": "a"})).unwrap();
    if shortcut {
        c.call(INPUT_KEY, json!({"key": "Super_L", "pressed": true}))
            .unwrap();
        c.call(INPUT_KEY, json!({"key": "m"})).unwrap();
        c.call(INPUT_KEY, json!({"key": "Super_L", "pressed": false}))
            .unwrap();
    }
}

#[test]
fn only_the_shells_locker_may_lock_and_it_needs_a_shell_to_lock_from_the_menu() {
    let compositor = Headless::start("lock-who");
    // An app that is not the shell does not see the lock manager.
    let mut outsider = LockerProcess::start(&compositor.wayland, false);
    assert!(outsider.wait_for("NO MANAGER"));
    // `session.lock` without a shell is refused: nobody could unlock.
    assert!(
        compositor
            .client()
            .call("session.lock", Value::Null)
            .is_err()
    );
    assert_eq!(lock_info(&compositor)["locked"], false);
    // And there is no command to unlock.
    assert!(
        compositor
            .client()
            .call("session.unlock", Value::Null)
            .is_err()
    );
}

#[test]
fn a_locked_session_draws_and_feeds_only_the_lock_surface_and_stays_locked() {
    let compositor = Headless::start("lock");
    let mut first = TestWindow::open(&compositor.wayland, "first");
    let mut second = TestWindow::open(&compositor.wayland, "second");
    // Before the lock the windows do get input.
    poke(&compositor, false);
    let before = received(&mut first, &mut second);
    assert!(before[1] + before[2] + before[6] + before[7] > 0);
    assert!(before[4] + before[9] > 0);

    let mut locker = LockerProcess::start(&compositor.wayland, true);
    assert!(locker.wait_for("LOCKED!"));
    assert_eq!(lock_info(&compositor)["locked"], true);
    assert_eq!(lock_info(&compositor)["locker"], true);
    // Only the lock surface is on the screen: white everywhere, the windows are gone.
    let shot = screenshot(&compositor);
    assert_eq!(pixel(&shot, 40, 60), [255, 255, 255, 255]);

    // No input of any kind reaches the windows: not the motion, the click, the keys nor the
    // shortcut (Logo+M would minimize).
    poke(&compositor, true);
    assert_eq!(received(&mut first, &mut second), before);
    let list = compositor
        .client()
        .call("window.list", Value::Null)
        .unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|w| w["minimized"] == false)
    );

    // A wrong password leaves the lock as it is.
    locker.say("wrong");
    assert!(locker.wait_for("WRONG"));
    assert_eq!(lock_info(&compositor)["locked"], true);

    // The locker dies: the session stays locked, plain black, still no input.
    locker.child.kill().unwrap();
    locker.child.wait().unwrap();
    wait_until("the locker is gone", || {
        lock_info(&compositor)["locker"] == false
    });
    assert_eq!(lock_info(&compositor)["locked"], true);
    poke(&compositor, true);
    assert_eq!(received(&mut first, &mut second), before);
    let shot = screenshot(&compositor);
    assert_eq!(pixel(&shot, 40, 60), [0, 0, 0, 255]);

    // A new locker takes the lock over, and the right password unlocks.
    let mut again = LockerProcess::start(&compositor.wayland, true);
    assert!(again.wait_for("LOCKED!"));
    assert_eq!(lock_info(&compositor)["locker"], true);
    // Its lock surface is mapped: the black screen is white again (silica-os/silica-desktop#252).
    wait_until("the new lock surface is drawn", || {
        pixel(&screenshot(&compositor), 40, 60) == [255, 255, 255, 255]
    });
    again.say("unlock");
    assert!(again.wait_for("UNLOCKED"));
    wait_until("the session is unlocked", || {
        lock_info(&compositor)["locked"] == false
    });
    // The windows are there and get input again.
    poke(&compositor, false);
    let after = received(&mut first, &mut second);
    assert!(after[..3] > before[..3] || after[5..8] > before[5..8]);
}

#[test]
fn a_second_client_cannot_take_a_lock_that_is_held() {
    let compositor = Headless::start("lock-twice");
    let mut holder = LockerProcess::start(&compositor.wayland, true);
    assert!(holder.wait_for("LOCKED!"));
    let mut intruder = LockerProcess::start(&compositor.wayland, true);
    assert!(intruder.wait_for("FINISHED"));
    assert_eq!(lock_info(&compositor)["locked"], true);
    holder.say("unlock");
    assert!(holder.wait_for("UNLOCKED"));
}
