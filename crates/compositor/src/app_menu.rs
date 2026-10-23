//! Server-side `silica-menu-v1`: the apps' menus in the global menu bar (silica-os/silica-desktop#25).
//!
//! An app publishes its menus as a JSON model in a file descriptor; the compositor checks it
//! (hostile input: size, depth, counts and lengths are bounded, a bad model is a protocol
//! error for that client only), rewrites it in its normal form and hands the frontmost app's
//! model to the shell components (`silica_shell_manager_v1.app_menu`, version 8). The item
//! the user chooses in the bar comes back from the shell (`activate_menu_item`) and goes to
//! the app (`silica_menu_v1.activated`). Keyboard shortcuts stay inside the app.

use std::io::Read;
use std::os::fd::OwnedFd;
use std::sync::Mutex;

use serde_json::{Value, json};
use silica_protocols::menu::MAX_MODEL_SIZE;
use silica_protocols::menu::model::parse_model;
use silica_protocols::menu::server::silica_menu_manager_v1::{self, SilicaMenuManagerV1};
use silica_protocols::menu::server::silica_menu_v1::{self, SilicaMenuV1};
use silica_protocols::shell::server::silica_shell_manager_v1::SilicaShellManagerV1;
use smithay::desktop::Window;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use tracing::warn;

use crate::state::State;

/// The first version of `silica_shell_manager_v1` with `app_menu`.
pub const SHELL_VERSION: u32 = 8;

/// Reads `size` bytes from offset 0 of the app's file. Only regular files (a memfd) are
/// read: a pipe or a socket could keep the compositor waiting.
fn read_model_file(fd: OwnedFd, size: u32) -> Result<Vec<u8>, String> {
    let size = usize::try_from(size).map_err(|_| "size too large".to_owned())?;
    if size == 0 || size > MAX_MODEL_SIZE {
        return Err(format!("size {size} outside 1–{MAX_MODEL_SIZE}"));
    }
    let stat = rustix::fs::fstat(&fd).map_err(|err| format!("fstat: {err}"))?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
        return Err("the model is not in a regular file".to_owned());
    }
    let file = std::fs::File::from(fd);
    let mut bytes = Vec::with_capacity(size);
    let mut reader = std::io::BufReader::new(file);
    {
        use std::io::Seek;
        reader
            .seek(std::io::SeekFrom::Start(0))
            .map_err(|err| format!("seek: {err}"))?;
    }
    reader
        .take(size as u64)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("read: {err}"))?;
    if bytes.len() != size {
        return Err(format!(
            "{} bytes in the file, {size} announced",
            bytes.len()
        ));
    }
    Ok(bytes)
}

/// A sealed, read-only memfd with these bytes, for the shell.
fn sealed_file(bytes: &[u8]) -> std::io::Result<OwnedFd> {
    use std::io::Write;
    let fd = rustix::fs::memfd_create(
        "silica-app-menu",
        rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
    )?;
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes)?;
    file.flush()?;
    // The readers share this offset: back at the start for them.
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0))?;
    let fd = OwnedFd::from(file);
    rustix::fs::fcntl_add_seals(
        &fd,
        rustix::fs::SealFlags::SHRINK
            | rustix::fs::SealFlags::GROW
            | rustix::fs::SealFlags::WRITE
            | rustix::fs::SealFlags::SEAL,
    )?;
    Ok(fd)
}

/// The menu object of an app.
pub struct MenuData {
    /// Names the object for the shell (`activate_menu_item`); never 0.
    pub serial: u32,
    pub client: ClientId,
    /// The model in normal form (JSON) and its version, after the first `set_model`.
    pub model: Mutex<Option<(u64, std::sync::Arc<Vec<u8>>)>>,
}

/// What the bar shows: the frontmost app, its menu object and the model version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announced {
    pub app_id: String,
    pub serial: u32,
    pub version: u64,
}

/// The menus of the apps.
#[derive(Default)]
pub struct AppMenus {
    pub menus: Vec<SilicaMenuV1>,
    next_serial: u32,
    next_version: u64,
    /// The last frontmost menus sent to the shell.
    pub announced: Option<Announced>,
}

impl AppMenus {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, SilicaMenuManagerV1, _>(2, ());
        Self::default()
    }

    fn serial(&mut self) -> u32 {
        self.next_serial = self.next_serial.wrapping_add(1).max(1);
        self.next_serial
    }

    fn version(&mut self) -> u64 {
        self.next_version = self.next_version.wrapping_add(1);
        self.next_version
    }

    /// The live menu object of a client.
    pub fn of_client(&self, client: &ClientId) -> Option<&SilicaMenuV1> {
        self.menus.iter().find(|menu| {
            menu.is_alive()
                && menu
                    .data::<MenuData>()
                    .is_some_and(|data| &data.client == client)
        })
    }

    /// The live menu object with this serial.
    pub fn by_serial(&self, serial: u32) -> Option<&SilicaMenuV1> {
        self.menus.iter().find(|menu| {
            menu.is_alive()
                && menu
                    .data::<MenuData>()
                    .is_some_and(|data| data.serial == serial)
        })
    }
}

/// The model and its version of a menu object, if the app sent one.
fn model_of(menu: &SilicaMenuV1) -> Option<(u32, u64, std::sync::Arc<Vec<u8>>)> {
    let data = menu.data::<MenuData>()?;
    let model = data.model.lock().ok()?;
    let (version, bytes) = model.as_ref()?;
    Some((data.serial, *version, bytes.clone()))
}

/// Sends the frontmost app's menus to one shell component.
fn send_app_menu(manager: &SilicaShellManagerV1, app_id: &str, serial: u32, bytes: &[u8]) {
    if manager.version() < SHELL_VERSION {
        return;
    }
    let size = u32::try_from(bytes.len()).unwrap_or(0);
    let bytes = if size == 0 { &[][..] } else { bytes };
    match sealed_file(bytes) {
        Ok(fd) => {
            use std::os::fd::AsFd;
            let serial = if size == 0 { 0 } else { serial };
            manager.app_menu(app_id.to_owned(), serial, fd.as_fd(), size);
        }
        Err(err) => warn!("app menu: {err}"),
    }
}

impl State {
    /// The window or surface whose app is in front, and that app: the focused window's (or
    /// the window on top while a shell menu has the keyboard, as `identifier_of`), else the
    /// windowless frontmost app (#142) or the desktop's owner (#147).
    fn front_app(&self, focus: Option<&WlSurface>) -> Option<(String, ClientId)> {
        let window = self.front_window_for(focus);
        if let Some(window) = window
            && let Some(toplevel) = window.toplevel()
            && let Some(client) = toplevel.wl_surface().client()
        {
            let app = crate::windows::window_app(&window).unwrap_or_default();
            return Some((app, client.id()));
        }
        let app = self.windowless_front.clone()?;
        let hidden = self
            .hidden
            .iter()
            .chain(self.minimized.iter())
            .map(|(w, _)| w)
            .find(|w| crate::windows::window_app(w).as_deref() == Some(app.as_str()))
            .and_then(|w| w.toplevel()?.wl_surface().client());
        let client = hidden.or_else(|| {
            self.desktop_layer()
                .filter(|(_, owner)| *owner == app)
                .and_then(|(layer, _)| layer.wl_surface().client())
        })?;
        Some((app, client.id()))
    }

    /// The window in front for the menus, as `identifier_of` finds it.
    fn front_window_for(&self, focus: Option<&WlSurface>) -> Option<Window> {
        // An open popup (a popover with the keyboard) belongs to the window it hangs from: the
        // bar keeps showing that app's menus (silica-os/silica-desktop#238).
        let root = self.focus_root(focus?);
        let focus = &root;
        if let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == focus))
        {
            return Some(window.clone());
        }
        let on_layer = smithay::desktop::layer_map_for_output(&self.output)
            .layers()
            .any(|layer| layer.wl_surface() == focus);
        (on_layer && self.windowless_front.is_none())
            .then(|| self.space.elements().last().cloned())
            .flatten()
    }

    /// What the bar should show now: app id, menu serial, model version and the model.
    fn front_menus(
        &self,
        focus: Option<&WlSurface>,
    ) -> (Announced, Option<std::sync::Arc<Vec<u8>>>) {
        let Some((app_id, client)) = self.front_app(focus) else {
            let none = Announced {
                app_id: String::new(),
                serial: 0,
                version: 0,
            };
            return (none, None);
        };
        match self.app_menus.of_client(&client).and_then(model_of) {
            Some((serial, version, bytes)) => (
                Announced {
                    app_id,
                    serial,
                    version,
                },
                Some(bytes),
            ),
            None => (
                Announced {
                    app_id,
                    serial: 0,
                    version: 0,
                },
                None,
            ),
        }
    }

    /// Sends the frontmost app's menus to the shell components if they changed. Called with
    /// the focused surface from the focus change callback (see `announce_focus`).
    pub fn announce_app_menu(&mut self, focus: Option<&WlSurface>) {
        let (announced, bytes) = self.front_menus(focus);
        if self.app_menus.announced.as_ref() == Some(&announced) {
            return;
        }
        self.shell_managers.retain(|manager| manager.is_alive());
        let bytes = bytes.as_deref().map_or(&[][..], Vec::as_slice);
        for manager in &self.shell_managers {
            send_app_menu(manager, &announced.app_id, announced.serial, bytes);
        }
        self.app_menus.announced = Some(announced);
    }

    /// The same, reading the focus from the keyboard (outside the focus callback).
    pub fn reannounce_app_menu(&mut self) {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        self.announce_app_menu(focus.as_ref());
    }

    /// The frontmost menus to a shell component that just bound.
    pub fn send_app_menu_to(&self, manager: &SilicaShellManagerV1) {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        let (announced, bytes) = self.front_menus(focus.as_ref());
        let bytes = bytes.as_deref().map_or(&[][..], Vec::as_slice);
        send_app_menu(manager, &announced.app_id, announced.serial, bytes);
    }

    /// A menu object is gone (destroyed, or its client left): the bar no longer shows it.
    fn forget_menu(&mut self, serial: u32) {
        self.app_menus.menus.retain(|menu| {
            menu.is_alive() && menu.data::<MenuData>().is_some_and(|d| d.serial != serial)
        });
        self.reannounce_app_menu();
    }

    /// An item chosen in the bar: the app receives it. False if the menu object is gone.
    pub fn activate_menu_item(&mut self, serial: u32, id: u32) -> bool {
        match self.app_menus.by_serial(serial) {
            Some(menu) => {
                menu.activated(id);
                true
            }
            None => false,
        }
    }

    /// `app.menu`: the frontmost app's menus, as the shell receives them.
    pub fn app_menu_state(&self) -> Value {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        let (announced, bytes) = self.front_menus(focus.as_ref());
        let model = bytes
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or(Value::Null);
        json!({
            "app_id": announced.app_id,
            "serial": announced.serial,
            "model": model,
        })
    }

    /// `app.menu_activate`: chooses an item of the frontmost app's menus.
    pub fn app_menu_activate(&mut self, id: u32) -> Result<Value, String> {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        let (announced, _) = self.front_menus(focus.as_ref());
        if announced.serial == 0 || !self.activate_menu_item(announced.serial, id) {
            return Err("the frontmost app has no menus".to_owned());
        }
        Ok(json!({ "app_id": announced.app_id, "id": id }))
    }
}

impl GlobalDispatch<SilicaMenuManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<SilicaMenuManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<SilicaMenuManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        manager: &SilicaMenuManagerV1,
        request: silica_menu_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            silica_menu_manager_v1::Request::GetMenu { id } => {
                if state.app_menus.of_client(&client.id()).is_some() {
                    manager.post_error(
                        silica_menu_manager_v1::Error::AlreadyHasMenu,
                        "the client already has a menu object",
                    );
                    return;
                }
                let serial = state.app_menus.serial();
                let menu = data_init.init(
                    id,
                    MenuData {
                        serial,
                        client: client.id(),
                        model: Mutex::new(None),
                    },
                );
                state.app_menus.menus.retain(Resource::is_alive);
                state.app_menus.menus.push(menu);
            }
            silica_menu_manager_v1::Request::Destroy => {}
            request @ silica_menu_manager_v1::Request::GetPopupMenu { .. } => {
                crate::popup_menu::get_popup_menu(manager, client, data_init, request);
            }
            _ => {}
        }
    }
}

impl Dispatch<SilicaMenuV1, MenuData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        menu: &SilicaMenuV1,
        request: silica_menu_v1::Request,
        data: &MenuData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            silica_menu_v1::Request::SetModel { fd, size } => {
                let model = read_model_file(fd, size).and_then(|bytes| {
                    let model = parse_model(&bytes)?;
                    serde_json::to_vec(&model).map_err(|err| err.to_string())
                });
                match model {
                    Ok(bytes) => {
                        let version = state.app_menus.version();
                        if let Ok(mut slot) = data.model.lock() {
                            *slot = Some((version, std::sync::Arc::new(bytes)));
                        }
                        state.reannounce_app_menu();
                    }
                    Err(err) => {
                        menu.post_error(silica_menu_v1::Error::InvalidModel, err);
                    }
                }
            }
            silica_menu_v1::Request::Destroy => state.forget_menu(data.serial),
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, _menu: &SilicaMenuV1, data: &MenuData) {
        state.forget_menu(data.serial);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn only_regular_files_are_read() {
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(read_model_file(OwnedFd::from(socket), 4).is_err());
        let file = sealed_file(br#"{"menus": []}"#).unwrap();
        assert_eq!(read_model_file(file, 13).unwrap(), br#"{"menus": []}"#);
        let file = sealed_file(b"{}").unwrap();
        assert!(read_model_file(file, 13).is_err(), "shorter than announced");
        let file = sealed_file(b"{}").unwrap();
        assert!(read_model_file(file, 0).is_err());
    }
}
