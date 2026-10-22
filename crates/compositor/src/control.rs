//! Control channel server: JSON lines on a user-only Unix socket (see `silica-control`), and
//! the actions socket: the same protocol on another user-only socket, limited to a few user-facing commands
//! (`silica_control::actions`), always open in every session.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use silica_control::{LineBuffer, Request, Response, actions};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use tracing::{info, warn};

use crate::state::Compositor;

/// Maximum time to deliver a response before closing the client.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens the control channel at `path`. It is a debug and test channel: only the user who runs
/// the compositor can reach it.
pub fn listen(handle: &LoopHandle<'static, Compositor>, path: &Path) -> std::io::Result<()> {
    listen_unix(handle, path, false)?;
    info!("control channel on {}", path.display());
    Ok(())
}

/// Opens the actions socket at `path`, limited to the user-facing commands.
pub fn listen_actions(
    handle: &LoopHandle<'static, Compositor>,
    path: &Path,
) -> std::io::Result<()> {
    listen_unix(handle, path, true)?;
    info!("actions socket on {}", path.display());
    Ok(())
}

/// Binds a user-only Unix socket at `path` (its folder 0700, the socket 0600) and serves it.
fn listen_unix(
    handle: &LoopHandle<'static, Compositor>,
    path: &Path,
    actions_only: bool,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // A stale socket from an earlier session.
    if path.exists() && UnixStream::connect(path).is_err() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let name = if actions_only {
        "actions socket"
    } else {
        "control channel"
    };
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |_, listener, data| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // Only the session's own user, whatever the socket's mode says.
                            let same_user = rustix::net::sockopt::socket_peercred(&stream)
                                .is_ok_and(|peer| peer.uid == rustix::process::getuid());
                            if !same_user {
                                warn!("{name}: a client of another user refused");
                                continue;
                            }
                            if let Err(err) = serve(&data.state.loop_handle, stream, actions_only) {
                                warn!("{name} client refused: {err}");
                            }
                        }
                        Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                        Err(err) => {
                            warn!("accept on the {name}: {err}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    Ok(())
}

/// A connected client stream.
trait ClientStream: AsFd + 'static
where
    for<'a> &'a Self: Read + Write,
{
    fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
}

impl ClientStream for UnixStream {
    fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()> {
        UnixStream::set_nonblocking(self, nonblocking)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_write_timeout(self, timeout)
    }
}

/// Serves one client; with `actions_only`, commands outside `actions::ACTIONS` are refused.
fn serve<S>(
    handle: &LoopHandle<'static, Compositor>,
    stream: S,
    actions_only: bool,
) -> std::io::Result<()>
where
    S: ClientStream,
    for<'a> &'a S: Read + Write,
{
    stream.set_nonblocking(true)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut lines = LineBuffer::default();
    handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, data| {
                // A command can change what is on screen.
                data.state.wake();
                let stream: &S = stream;
                let mut chunk = [0u8; 4096];
                loop {
                    let read = match (&*stream).read(&mut chunk) {
                        Ok(0) => return Ok(PostAction::Remove),
                        Ok(read) => read,
                        Err(err) if err.kind() == ErrorKind::WouldBlock => {
                            return Ok(PostAction::Continue);
                        }
                        Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => return Ok(PostAction::Remove),
                    };
                    let requests = match lines.push(chunk.get(..read).unwrap_or_default()) {
                        Ok(requests) => requests,
                        Err(err) => {
                            let _ = reply(stream, &Response::err(0, err));
                            return Ok(PostAction::Remove);
                        }
                    };
                    for line in requests {
                        let response = match serde_json::from_str::<Request>(&line) {
                            Ok(request)
                                if actions_only && !actions::is_action(&request.command) =>
                            {
                                Response::err(
                                    request.id,
                                    format!("{} is not an action", request.command),
                                )
                            }
                            Ok(request) => data.state.execute(request),
                            Err(err) => Response::err(0, format!("invalid request: {err}")),
                        };
                        if reply(stream, &response).is_err() {
                            return Ok(PostAction::Remove);
                        }
                    }
                }
            },
        )
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    Ok(())
}

fn reply<S>(stream: &S, response: &Response) -> std::io::Result<()>
where
    S: ClientStream,
    for<'a> &'a S: Read + Write,
{
    let mut line = serde_json::to_vec(response).map_err(std::io::Error::other)?;
    line.push(b'\n');
    stream.set_nonblocking(false)?;
    let written = (&*stream).write_all(&line);
    stream.set_nonblocking(true)?;
    written
}
