//! `--session-bus`: The compositor starts a private session bus and passes it to the clients.

#![allow(clippy::expect_used)]

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::Headless;
use serde_json::{Value, json};
use silica_control::commands::{APP_LAUNCH, SYSTEM_DND, SYSTEM_STATUS, SYSTEM_VOLUME};

/// Waits for `path` to exist and not be empty.
fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "{} did not appear",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The names present on the bus, with `dbus-send`.
fn list_names(address: &str) -> String {
    let output = Command::new("dbus-send")
        .args([
            &format!("--bus={address}"),
            "--print-reply",
            "--dest=org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus.ListNames",
        ])
        .output()
        .expect("dbus-send (package dbus) is required for this test");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn clients_get_the_private_bus() {
    let compositor = Headless::start_with("session-bus", (320, 240), &["--session-bus"]);
    let file = compositor.dir.join("bus-address");
    compositor
        .client()
        .call(
            APP_LAUNCH,
            json!({
                "command": format!("printf %s \"$DBUS_SESSION_BUS_ADDRESS\" > {}", file.display())
            }),
        )
        .unwrap();
    let address = wait_for_file(&file);
    assert!(address.starts_with("unix:"), "address: {address}");
    // The bus is the compositor's, not the one of the hosting session.
    assert_ne!(
        Some(address.as_str()),
        std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref()
    );
    list_names(&address);

    // With `cargo test --workspace` silica-daemon is built next to the compositor: then it must take
    // the notifications name on the private bus.
    let daemon = std::path::Path::new(env!("CARGO_BIN_EXE_silica-compositor"))
        .with_file_name("silica-daemon");
    if daemon.is_file() {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !list_names(&address).contains("org.freedesktop.Notifications") {
            assert!(
                Instant::now() < deadline,
                "silica-daemon did not take the name"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn without_the_option_clients_keep_the_host_bus() {
    let compositor = Headless::start_with("no-session-bus", (320, 240), &[]);
    let file = compositor.dir.join("bus-address");
    compositor
        .client()
        .call(
            APP_LAUNCH,
            json!({
                "command": format!("printf 'x%s' \"$DBUS_SESSION_BUS_ADDRESS\" > {}", file.display())
            }),
        )
        .unwrap();
    let seen = wait_for_file(&file);
    // The bus the compositor was started with (the test harness gives it one that does not exist).
    let host = format!("unix:path={}", compositor.dir.join("no-bus").display());
    assert_eq!(seen, format!("x{host}"));
}

#[test]
fn system_commands_reach_the_daemon() {
    // silica-daemon is needed next to the compositor (cargo test --workspace builds it).
    let daemon = std::path::Path::new(env!("CARGO_BIN_EXE_silica-compositor"))
        .with_file_name("silica-daemon");
    if !daemon.is_file() {
        eprintln!("silica-daemon not built: test skipped");
        return;
    }
    let compositor = Headless::start_with("system", (320, 240), &["--session-bus"]);
    let mut client = compositor.client();
    // Only Do Not Disturb: the other actions would change the machine running the tests.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if client.call(SYSTEM_STATUS, Value::Null).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "system.status never available");
        std::thread::sleep(Duration::from_millis(100));
    }
    let queued = client.call(SYSTEM_DND, json!({"enabled": true})).unwrap();
    assert_eq!(queued["queued"], true);
    loop {
        let status = client.call(SYSTEM_STATUS, Value::Null).unwrap();
        if status["dnd"] == true {
            break;
        }
        assert!(Instant::now() < deadline, "dnd never active: {status}");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Wrong parameters: error, nothing queued.
    assert!(
        client
            .call(SYSTEM_VOLUME, json!({"value": "alto"}))
            .is_err()
    );
    assert!(client.call(SYSTEM_DND, Value::Null).is_err());
}
