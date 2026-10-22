//! Running apps, with or without windows (silica-os/silica-desktop#142).
//!
//! As on the reference, closing an app's last window does not end the app. A windowless
//! app is still running: the Dock shows its dot, and a click on its icon asks it for a
//! window again. The compositor's toplevel list cannot show such an app, so every app process
//! registers a unix socket of its own:
//!
//! ```text
//! $XDG_RUNTIME_DIR/silica-apps/<app id>.<pid>.sock    one per running process
//! $XDG_RUNTIME_DIR/silica-apps/<app id>.parked        ended by the system while windowless
//! ```
//!
//! The protocol is one line per request, answered with one line:
//!
//! - `status` → `hidden` (no window) or `visible`;
//! - `reopen` → `ok`: the app shows a window again (the one it hid, or a new one);
//! - `quit` → `ok`: the app ends (Cmd+Q, Quit in the Dock menu).
//!
//! A `.parked` file marks an app the system ended while it had no window, under memory
//! pressure or after a long idle time. The Dock keeps showing it as running, and a click
//! starts it again. The app removes the file when it starts.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a request waits for the app's answer.
const TIMEOUT: Duration = Duration::from_millis(500);

/// The longest request or answer line.
pub const MAX_LINE: usize = 64;
/// Longest `open` request line (the file list of a second launch).
pub const MAX_OPEN_LINE: usize = 64 * 1024;

/// The registry folder: `$XDG_RUNTIME_DIR/silica-apps`, or none without a runtime folder.
pub fn dir() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join("silica-apps"))
}

/// An app id usable in a file name: reverse-DNS style characters only.
pub fn valid_app_id(app_id: &str) -> bool {
    !app_id.is_empty()
        && app_id.len() <= 200
        && !app_id.starts_with('.')
        && app_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The socket of a process of `app_id`.
pub fn socket_path(dir: &Path, app_id: &str, pid: u32) -> PathBuf {
    dir.join(format!("{app_id}.{pid}.sock"))
}

/// The marker of an app the system ended while it had no window.
pub fn parked_path(dir: &Path, app_id: &str) -> PathBuf {
    dir.join(format!("{app_id}.parked"))
}

/// One registered process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub app_id: String,
    pub pid: u32,
    pub path: PathBuf,
}

/// Splits `<app id>.<pid>.sock`; none for other names.
pub fn parse_socket_name(name: &str) -> Option<(String, u32)> {
    let stem = name.strip_suffix(".sock")?;
    let (app_id, pid) = stem.rsplit_once('.')?;
    let pid = pid.parse().ok()?;
    valid_app_id(app_id).then(|| (app_id.to_owned(), pid))
}

/// The registered processes in `dir`, of every app or only of `app_id`.
pub fn instances(dir: &Path, app_id: Option<&str>) -> Vec<Instance> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Instance> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (id, pid) = parse_socket_name(&name)?;
            (app_id.is_none() || app_id == Some(id.as_str())).then(|| Instance {
                app_id: id,
                pid,
                path: entry.path(),
            })
        })
        .collect();
    found.sort_by_key(|i| (i.app_id.clone(), i.pid));
    found
}

/// The apps the system ended while windowless (`.parked` files).
pub fn parked(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_suffix(".parked")?;
            valid_app_id(id).then(|| id.to_owned())
        })
        .collect();
    ids.sort();
    ids
}

/// Sends one request line to a registered process and returns its answer. A socket without
/// a listener (the process is gone) is removed.
pub fn request(path: &Path, line: &str) -> std::io::Result<String> {
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::ConnectionRefused {
                let _ = std::fs::remove_file(path);
            }
            return Err(err);
        }
    };
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut answer = String::new();
    BufReader::new(std::io::Read::take(stream, MAX_LINE as u64)).read_line(&mut answer)?;
    Ok(answer.trim().to_owned())
}

/// Hands `args` (the files of a second launch) to a running process of `app_id`, preferring
/// one without windows; true if one took them. The request is `open <JSON array>`.
pub fn open(app_id: &str, args: &[String]) -> bool {
    let Some(dir) = dir() else {
        return false;
    };
    let Ok(list) = serde_json::to_string(args) else {
        return false;
    };
    let line = format!("open {list}");
    if line.len() >= MAX_OPEN_LINE {
        return false;
    }
    let live: Vec<(Instance, String)> = instances(&dir, Some(app_id))
        .into_iter()
        .filter_map(|i| request(&i.path, "status").ok().map(|status| (i, status)))
        .collect();
    let target = live
        .iter()
        .find(|(_, status)| status == "hidden")
        .or_else(|| live.first());
    target.is_some_and(|(i, _)| request(&i.path, &line).is_ok_and(|a| a == "ok"))
}

/// Asks every process of `app_id` to quit; returns how many answered.
pub fn quit_all(app_id: &str) -> usize {
    let Some(dir) = dir() else {
        return 0;
    };
    instances(&dir, Some(app_id))
        .iter()
        .filter(|i| request(&i.path, "quit").is_ok())
        .count()
}

/// Asks a windowless process of `app_id` for a window; true if one answered. With
/// `any`, a process that still has windows also counts (it brings them forward).
pub fn reopen(app_id: &str, any: bool) -> bool {
    let Some(dir) = dir() else {
        return false;
    };
    let live: Vec<(Instance, String)> = instances(&dir, Some(app_id))
        .into_iter()
        .filter_map(|i| request(&i.path, "status").ok().map(|status| (i, status)))
        .collect();
    let target = live
        .iter()
        .find(|(_, status)| status == "hidden")
        .or_else(|| live.first().filter(|_| any));
    target.is_some_and(|(i, _)| request(&i.path, "reopen").is_ok_and(|a| a == "ok"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn socket_names_round_trip() {
        let dir = Path::new("/run/user/1000/silica-apps");
        let path = socket_path(dir, "dev.frascella.Mustache", 4242);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap();
        assert_eq!(
            parse_socket_name(name),
            Some(("dev.frascella.Mustache".to_owned(), 4242))
        );
        assert_eq!(parse_socket_name("dev.frascella.Mustache.parked"), None);
        assert_eq!(parse_socket_name("x.sock"), None);
        assert_eq!(parse_socket_name("../evil.12.sock"), None);
    }

    #[test]
    fn app_ids_are_checked() {
        assert!(valid_app_id("dev.frascella.Mail"));
        assert!(valid_app_id("org.gnome.Ptyxis-devel"));
        assert!(!valid_app_id(""));
        assert!(!valid_app_id(".hidden"));
        assert!(!valid_app_id("a/b"));
        assert!(!valid_app_id("a b"));
    }

    #[test]
    fn requests_reach_a_listener_and_stale_sockets_go() {
        let dir = std::env::temp_dir().join(format!("silica-apps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = socket_path(&dir, "dev.example.App", 7);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let mut stream = stream;
            stream
                .write_all(if line.trim() == "status" {
                    b"hidden\n"
                } else {
                    b"?\n"
                })
                .unwrap();
        });
        assert_eq!(request(&path, "status").unwrap(), "hidden");
        server.join().unwrap();
        assert_eq!(instances(&dir, Some("dev.example.App")).len(), 1);
        assert!(instances(&dir, Some("dev.example.Other")).is_empty());
        // The listener is gone: the socket file is stale and the request removes it.
        assert!(request(&path, "status").is_err());
        assert!(!path.exists());
        std::fs::write(parked_path(&dir, "dev.example.App"), b"").unwrap();
        assert_eq!(parked(&dir), ["dev.example.App"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
