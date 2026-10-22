//! The actions socket: always open, user-only, and limited to the user-facing actions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::Headless;
use serde_json::Value;

fn socket(compositor: &Headless) -> PathBuf {
    let display = compositor.wayland.file_name().unwrap().to_string_lossy();
    compositor
        .dir
        .join("silica")
        .join(format!("actions-{display}.sock"))
}

fn call(path: &PathBuf, command: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = loop {
        match UnixStream::connect(path) {
            Ok(stream) => break stream,
            Err(err) if Instant::now() < deadline => {
                let _ = err;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => panic!("{}: {err}", path.display()),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    writeln!(writer, r#"{{"id":7,"command":"{command}","params":{{}}}}"#).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn actions_run_and_other_commands_are_refused() {
    let compositor = Headless::start("actions");
    let path = socket(&compositor);

    let shown = call(&path, "launcher.toggle");
    assert_eq!(shown["ok"], Value::Bool(true), "{shown}");
    assert_eq!(shown["id"], 7);

    let refused = call(&path, "window.list");
    assert_eq!(refused["ok"], Value::Bool(false), "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("not an action"),
        "{refused}"
    );

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
