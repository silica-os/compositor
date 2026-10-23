//! The actions socket: a few user-facing commands that apps and launchers may run in any session,
//! without the control channel (which a real session doesn't open). The compositor listens on
//! `$XDG_RUNTIME_DIR/silica/actions-<wayland display>.sock` (mode 0600, the user only; one per
//! compositor, so nested and headless sessions don't collide) and accepts only the
//! commands in [`ACTIONS`], with the control channel's JSON lines. `silica-action` is its
//! client: the Apps and Screenshot applications are `silica-action launcher.toggle` and
//! `screenshot.toolbar`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::commands::{
    CONTROLCENTER_TOGGLE, FARO_TOGGLE, LAUNCHER_TOGGLE, NOTIFICATIONS_TOGGLE,
    OUTPUT_COLOR_TEMPERATURE, OUTPUT_CONFIGURE, OUTPUT_LIST, SCREENSHOT_SCREEN,
    SCREENSHOT_SCREEN_CLIPBOARD, SCREENSHOT_SELECTION, SCREENSHOT_SELECTION_CLIPBOARD,
    SCREENSHOT_TOOLBAR,
};
use crate::{Error, Request, Response};

/// The commands the actions socket runs: things a user could do with a click or a shortcut,
/// nothing that reads the screen or moves windows.
pub const ACTIONS: &[&str] = &[
    LAUNCHER_TOGGLE,
    FARO_TOGGLE,
    SCREENSHOT_TOOLBAR,
    SCREENSHOT_SCREEN,
    SCREENSHOT_SCREEN_CLIPBOARD,
    SCREENSHOT_SELECTION,
    SCREENSHOT_SELECTION_CLIPBOARD,
    NOTIFICATIONS_TOGGLE,
    CONTROLCENTER_TOGGLE,
    // For silica-daemon's System1, which gives apps (Settings) the displays and the night light.
    OUTPUT_LIST,
    OUTPUT_CONFIGURE,
    OUTPUT_COLOR_TEMPERATURE,
];

/// Whether the actions socket runs `command`.
pub fn is_action(command: &str) -> bool {
    ACTIONS.contains(&command)
}

/// The socket of the compositor on Wayland display `display` (`wayland-1`), or none without a
/// runtime folder.
pub fn socket_path(display: &str) -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let name = display.rsplit('/').next().unwrap_or(display);
    Some(
        PathBuf::from(runtime)
            .join("silica")
            .join(format!("actions-{name}.sock")),
    )
}

/// Runs `command` through the actions socket of the compositor in `WAYLAND_DISPLAY`.
pub fn run(command: &str) -> Result<Value, Error> {
    run_with(command, Value::Object(Default::default()))
}

/// Like `run`, with parameters.
pub fn run_with(command: &str, params: Value) -> Result<Value, Error> {
    let display = std::env::var("WAYLAND_DISPLAY")
        .map_err(|_| Error::Protocol("WAYLAND_DISPLAY is not set".into()))?;
    let path = socket_path(&display)
        .ok_or_else(|| Error::Protocol("XDG_RUNTIME_DIR is not set".into()))?;
    run_at(&path, command, params)
}

/// Runs `command` with `params` through the actions socket at `path`.
pub fn run_at(path: &std::path::Path, command: &str, params: Value) -> Result<Value, Error> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut writer = stream.try_clone()?;
    let request = Request {
        id: 1,
        command: command.to_owned(),
        params,
    };
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    if answer.is_empty() {
        return Err(Error::Protocol(
            "the actions socket closed without an answer".into(),
        ));
    }
    let response: Response = serde_json::from_str(&answer)?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(Error::Command(response.error.unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_user_actions() {
        assert!(is_action("launcher.toggle"));
        assert!(is_action("screenshot.toolbar"));
        assert!(!is_action("ui.screenshot"));
        assert!(!is_action("window.move"));
        assert!(!is_action("app.launch"));
    }

    #[test]
    fn one_socket_per_display() {
        let Some(path) = socket_path("wayland-1") else {
            return;
        };
        assert!(path.ends_with("silica/actions-wayland-1.sock"));
        let Some(absolute) = socket_path("/run/user/1000/wayland-2") else {
            return;
        };
        assert!(absolute.ends_with("silica/actions-wayland-2.sock"));
    }
}
