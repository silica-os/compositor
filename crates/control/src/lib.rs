//! Silica's JSON control channel.
//!
//! The protocol is one JSON line per message over a user-only Unix socket: the client sends a
//! [`Request`], the server answers with a [`Response`] carrying the same `id`.
//!
//! ```text
//! → {"id":1,"command":"ui.screenshot","params":{"path":"/tmp/shot.png"}}
//! ← {"id":1,"ok":true,"result":{"width":1280,"height":800,"path":"/tmp/shot.png"}}
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod actions;
pub mod apps;
pub mod commands;
pub mod mcp;

pub use commands::{CATALOG, CommandSpec, Shortcut, UI_SCREENSHOT};

/// Maximum length of a request: beyond it, the server closes the connection.
pub const MAX_REQUEST_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub command: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotParams {
    /// Where to write the PNG. Without it, the PNG comes back as base64 in `png_base64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A region of the screen in logical points (`screenshot.region`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionParams {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clipboard: bool,
}

impl RegionParams {
    pub fn options(&self) -> CaptureOptions {
        CaptureOptions {
            pointer: self.pointer,
            clipboard: self.clipboard,
        }
    }
}

/// An app by app_id (`app.force_quit`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppParams {
    pub app_id: String,
}

/// `app.quit`: the app to quit, or the frontmost one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuitParams {
    #[serde(default)]
    pub app_id: Option<String>,
}

/// The window under a point, in logical points (`screenshot.window_at`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowShotParams {
    pub x: f64,
    pub y: f64,
    /// With the window's shadow on a transparent background (default true); false: the
    /// frame only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<bool>,
    /// No window there (the desktop): the whole screen instead of an error.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub or_screen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clipboard: bool,
}

impl WindowShotParams {
    pub fn options(&self) -> CaptureOptions {
        CaptureOptions {
            pointer: self.pointer,
            clipboard: self.clipboard,
        }
    }
}

/// What every screenshot command may add: the pointer drawn into the image, and the
/// clipboard as the destination (the shell copies the image instead of saving a file).
/// The parameters of `screenshot.screen`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureOptions {
    /// Draw the pointer into the image; without it, the compositor's own setting
    /// (`screenshot.settings`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<bool>,
    /// The capture goes to the clipboard.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clipboard: bool,
}

/// The screenshot settings the compositor keeps for its own shortcuts (`screenshot.settings`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotSettings {
    /// Draw the pointer into the captures.
    pub pointer: bool,
}

/// What the compositor announces to the shell after a capture (broadcast topic `screenshot`, as
/// JSON): the temporary PNG, its size, whether it goes to the clipboard and a sequence
/// number (two equal captures are still two announcements).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureAnnouncement {
    pub path: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub clipboard: bool,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub png_base64: Option<String>,
}

/// Splits a byte stream into complete lines.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    /// Adds bytes read from the socket and returns the complete lines.
    /// Error if a line exceeds [`MAX_REQUEST_BYTES`].
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line).trim().to_owned();
            if !text.is_empty() {
                lines.push(text);
            }
        }
        if self.pending.len() > MAX_REQUEST_BYTES {
            return Err(format!("request over {MAX_REQUEST_BYTES} bytes"));
        }
        Ok(lines)
    }
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// The server answered with `ok: false`.
    Command(String),
    /// The connection closed or the response does not match the request.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(err) => write!(f, "I/O: {err}"),
            Error::Json(err) => write!(f, "JSON: {err}"),
            Error::Command(err) => write!(f, "command failed: {err}"),
            Error::Protocol(err) => write!(f, "protocol: {err}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Json(err)
    }
}

/// The usual path of the control channel's socket: `$XDG_RUNTIME_DIR/silica/control.sock`.
pub fn default_socket_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join("silica").join("control.sock"))
}

/// Synchronous client of the control channel, for tests, CLI and agents.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    /// Connects to the control channel's socket at `path`.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, Error> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
        })
    }

    /// Sends a command and waits for the response.
    pub fn call(&mut self, command: &str, params: Value) -> Result<Value, Error> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let request = Request {
            id,
            command: command.to_owned(),
            params,
        };
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;

        let mut reply = String::new();
        if self.reader.read_line(&mut reply)? == 0 {
            return Err(Error::Protocol(
                "connection closed by the server".to_owned(),
            ));
        }
        let response: Response = serde_json::from_str(&reply)?;
        if response.id != id {
            return Err(Error::Protocol(format!(
                "response {} for request {id}",
                response.id
            )));
        }
        if response.ok {
            Ok(response.result.unwrap_or(Value::Null))
        } else {
            Err(Error::Command(response.error.unwrap_or_default()))
        }
    }

    pub fn screenshot(&mut self, params: &ScreenshotParams) -> Result<Screenshot, Error> {
        let result = self.call(UI_SCREENSHOT, serde_json::to_value(params)?)?;
        Ok(serde_json::from_value(result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_buffer_splits_and_keeps_partial_lines() {
        let mut buffer = LineBuffer::default();
        assert_eq!(
            buffer.push(b"{\"a\":1}\n{\"b\"").unwrap(),
            vec!["{\"a\":1}"]
        );
        assert_eq!(buffer.push(b":2}\n\n").unwrap(), vec!["{\"b\":2}"]);
    }

    #[test]
    fn line_buffer_rejects_oversized_requests() {
        let mut buffer = LineBuffer::default();
        assert!(buffer.push(&vec![b'x'; MAX_REQUEST_BYTES + 1]).is_err());
    }

    #[test]
    fn request_params_default_to_null() {
        let request: Request =
            serde_json::from_str(r#"{"id":3,"command":"ui.screenshot"}"#).unwrap();
        assert_eq!(request.params, Value::Null);
    }
}
