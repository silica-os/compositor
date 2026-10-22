//! The systemd user session. Started by `silica-compositor.service`, the compositor gives its
//! Wayland display to the user manager and to the session bus, so the services started after it
//! (the shell, the portals) find it, then tells systemd it is ready (`Type=notify`).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::net::UnixDatagram;

use tracing::{info, warn};
use zbus::blocking::Connection;

/// Exports `WAYLAND_DISPLAY` and reports readiness. Outside a systemd unit it does nothing.
pub fn announce(display: &OsStr) {
    if std::env::var_os("NOTIFY_SOCKET").is_none() {
        return;
    }
    let name = display.to_string_lossy();
    if let Err(err) = export(&name) {
        warn!("exporting WAYLAND_DISPLAY to the session: {err}");
    }
    match notify("READY=1") {
        Ok(()) => info!("ready, WAYLAND_DISPLAY={name}"),
        Err(err) => warn!("sd_notify: {err}"),
    }
}

fn export(display: &str) -> zbus::Result<()> {
    let bus = Connection::session()?;
    let assignment = format!("WAYLAND_DISPLAY={display}");
    bus.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        "SetEnvironment",
        &(vec![assignment.as_str()],),
    )?;
    let environment = HashMap::from([("WAYLAND_DISPLAY", display)]);
    bus.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "UpdateActivationEnvironment",
        &(environment,),
    )?;
    Ok(())
}

/// Sends `state` to the service manager's notification socket.
fn notify(state: &str) -> std::io::Result<()> {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    let socket = UnixDatagram::unbound()?;
    let bytes = path.as_encoded_bytes();
    if let Some(name) = bytes.strip_prefix(b"@") {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
        socket.send_to_addr(state.as_bytes(), &addr)?;
    } else {
        socket.send_to(state.as_bytes(), &path)?;
    }
    Ok(())
}

/// Tells systemd the compositor is stopping, so a restart is not mistaken for a hang.
pub fn stopping() {
    if let Err(err) = notify("STOPPING=1") {
        warn!("sd_notify: {err}");
    }
}
