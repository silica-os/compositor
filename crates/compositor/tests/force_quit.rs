//! Force quit: `app.force_quit` terminates the processes of an app's windows, and only those.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::process::Command;

use common::Headless;
use common::client::{APP_ID, TestWindow};
use serde_json::json;
use silica_control::commands::{APP_FORCE_QUIT, WINDOW_LIST, WindowInfo};

const CHILD_SOCKET: &str = "SILICA_TEST_FORCE_QUIT_SOCKET";

/// The child: opens a window of the app "dev.test.Stuck" and waits.
#[test]
fn force_quit_child() {
    if let Ok(socket) = std::env::var(CHILD_SOCKET) {
        APP_ID.with(|id| *id.borrow_mut() = "dev.test.Stuck".to_owned());
        let _window = TestWindow::open(std::path::Path::new(&socket), "stuck");
        std::thread::sleep(std::time::Duration::from_secs(20));
    }
}

#[test]
fn force_quit_kills_only_the_processes_of_that_app() {
    let compositor = Headless::start("force-quit");
    let _mine = TestWindow::open(&compositor.wayland, "mine");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "force_quit_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_SOCKET, &compositor.wayland)
        .spawn()
        .unwrap();
    let mut client = compositor.client();
    // The child's window arrives.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let list: Vec<WindowInfo> =
            serde_json::from_value(client.call(WINDOW_LIST, serde_json::Value::Null).unwrap())
                .unwrap();
        if list.iter().any(|w| w.title.as_deref() == Some("stuck")) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the child's window does not arrive"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let reply = client
        .call(APP_FORCE_QUIT, json!({"app_id": "dev.test.Stuck"}))
        .unwrap();
    assert_eq!(reply["killed"], 1);
    let status = child.wait().unwrap();
    assert!(!status.success(), "the child should have been terminated");
    // An app that does not exist: error, no process touched (the test is still alive).
    assert!(
        client
            .call(APP_FORCE_QUIT, json!({"app_id": "dev.test.Nobody"}))
            .is_err()
    );
}
