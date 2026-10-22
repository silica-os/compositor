//! Server-side `silica_popup_menu_v1`: context menus and pop-up buttons drawn by the shell
//! (silica-os/silica-desktop#241, `docs/spec/menus.md`).
//!
//! The app describes a menu with typed requests and shows it; the compositor turns the anchor into
//! output coordinates and hands the rows to the shell components
//! (`silica_shell_manager_v1.popup_menu`, version 9). From the moment the menu is shown it is
//! also a transient owned by the app (modal for the app, `pass` elsewhere), so every way of
//! ending it goes through `transient.rs`: a press outside, a change of context, the shell's
//! own choice or Esc. The result goes back once, as `selected` or `dismissed`.

use std::collections::HashSet;
use std::sync::Mutex;

use silica_protocols::menu::server::silica_menu_manager_v1::{self, SilicaMenuManagerV1};
use silica_protocols::menu::server::silica_popup_menu_v1::{self, SilicaPopupMenuV1};
use silica_protocols::shell::server::silica_shell_manager_v1::SilicaShellManagerV1;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_region::WlRegion;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, Resource, WEnum,
};
use smithay::utils::{Logical, Point, Rectangle};

use crate::state::State;
use crate::transient::{OwnerApp, Rule, Source, Transient};

/// The first version of `silica_shell_manager_v1` with popup menus.
pub const SHELL_VERSION: u32 = 9;

/// Limits of a menu (`silica_popup_menu_v1`).
pub const MAX_ROWS: usize = 1024;
pub const MAX_DEPTH: usize = 4;
pub const MAX_TEXT: usize = 256;

/// `silica_shell_manager_v1.popup_row_kind`.
pub const ROW_ITEM: u32 = 0;
pub const ROW_SEPARATOR: u32 = 1;
pub const ROW_SECTION: u32 = 2;
pub const ROW_SUBMENU_BEGIN: u32 = 3;
pub const ROW_SUBMENU_END: u32 = 4;

/// One row as the shell receives it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub kind: u32,
    pub id: u32,
    pub label: String,
    pub flags: u32,
    pub icon: String,
    pub key: String,
    pub modifiers: u32,
}

/// The rows while the app builds the menu.
#[derive(Debug, Default)]
struct Build {
    rows: Vec<Row>,
    depth: usize,
    ids: HashSet<u32>,
    shown: bool,
}

/// The data of a `silica_popup_menu_v1` object.
pub struct PopupMenuData {
    client: ClientId,
    surface: WlSurface,
    kind: u32,
    /// The anchor in the surface's coordinates.
    anchor: Rectangle<i32, Logical>,
    selected: u32,
    build: Mutex<Build>,
}

/// A menu that was shown and is not over yet.
pub struct OpenPopupMenu {
    pub serial: u32,
    pub object: SilicaPopupMenuV1,
    pub client: ClientId,
    pub surface: WlSurface,
    /// The ids of the items the shell may choose.
    pub ids: HashSet<u32>,
}

/// The popup menus that are open.
#[derive(Default)]
pub struct PopupMenus {
    pub open: Vec<OpenPopupMenu>,
    next_serial: u32,
}

impl PopupMenus {
    fn serial(&mut self) -> u32 {
        self.next_serial = self.next_serial.wrapping_add(1).max(1);
        self.next_serial
    }
}

/// How a menu ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The app is told `dismissed`.
    Dismissed,
    /// The app is told `selected(id)`.
    Selected(u32),
    /// The app destroyed it or left: nothing to tell.
    Silent,
}

fn too_long(text: &str) -> bool {
    text.len() > MAX_TEXT
}

impl State {
    /// Where a surface of an app sits on the output, logical: a window's surface, a popup of
    /// it, or a shell layer. None when the surface is not on screen.
    fn surface_screen_origin(&self, surface: &WlSurface) -> Option<Point<i32, Logical>> {
        let mut root = surface.clone();
        for _ in 0..16 {
            match smithay::wayland::compositor::get_parent(&root) {
                Some(parent) => root = parent,
                None => break,
            }
        }
        if let Some(window) = self.window_for(&root) {
            let at = self.space.element_location(&window)?;
            return Some(at - window.geometry().loc);
        }
        if let Some(popup) = self.popups.find_popup(&root) {
            let toplevel = smithay::desktop::find_popup_root_surface(&popup).ok()?;
            let base = self.surface_screen_origin(&toplevel)?;
            let geometry = self
                .window_for(&toplevel)
                .map_or_else(Point::default, |window| window.geometry().loc);
            let at = smithay::desktop::get_popup_toplevel_coords(&popup);
            return Some(base + geometry + at);
        }
        let map = smithay::desktop::layer_map_for_output(&self.output);
        map.layer_for_surface(&root, smithay::desktop::WindowSurfaceType::TOPLEVEL)?;
        drop(map);
        Some(self.surface_origin(&root))
    }

    /// The app that owns a surface: the window's app id, else the desktop owner of a layer.
    fn surface_app(&self, surface: &WlSurface) -> String {
        let mut root = surface.clone();
        for _ in 0..16 {
            match smithay::wayland::compositor::get_parent(&root) {
                Some(parent) => root = parent,
                None => break,
            }
        }
        if let Some(popup) = self.popups.find_popup(&root)
            && let Ok(top) = smithay::desktop::find_popup_root_surface(&popup)
        {
            root = top;
        }
        if let Some(window) = self.window_for(&root) {
            return crate::panorama::window_app(&window).unwrap_or_default();
        }
        let map = smithay::desktop::layer_map_for_output(&self.output);
        map.layer_for_surface(&root, smithay::desktop::WindowSurfaceType::TOPLEVEL)
            .and_then(|layer| crate::layers::desktop_owner(layer.namespace()).map(str::to_owned))
            .unwrap_or_default()
    }

    /// `show`: the menu opens, or is dismissed at once when it cannot be shown.
    fn open_popup_menu(
        &mut self,
        object: &SilicaPopupMenuV1,
        data: &PopupMenuData,
        rows: Vec<Row>,
        ids: HashSet<u32>,
    ) {
        // One menu per client: the newer one replaces the open one.
        let previous: Vec<u32> = self
            .popup_menus
            .open
            .iter()
            .filter(|menu| menu.client == data.client)
            .map(|menu| menu.serial)
            .collect();
        for serial in previous {
            self.finish_popup_menu(serial, Ended::Dismissed);
        }
        self.shell_managers.retain(|manager| manager.is_alive());
        let managers: Vec<SilicaShellManagerV1> = self
            .shell_managers
            .iter()
            .filter(|manager| manager.version() >= SHELL_VERSION)
            .cloned()
            .collect();
        let origin = self.surface_screen_origin(&data.surface);
        let (Some(origin), false) = (origin, managers.is_empty()) else {
            object.dismissed();
            return;
        };
        let serial = self.popup_menus.serial();
        let app_id = self.surface_app(&data.surface);
        let anchor = Rectangle::new(data.anchor.loc + origin, data.anchor.size);
        let appearance = data
            .surface
            .client()
            .and_then(|_| crate::window_appearance::override_of(&data.surface))
            .map_or(0, |dark| if dark { 2 } else { 1 });
        for manager in &managers {
            manager.popup_menu(
                serial,
                app_id.clone(),
                data.kind,
                anchor.loc.x,
                anchor.loc.y,
                anchor.size.w,
                anchor.size.h,
                data.selected,
                appearance,
            );
            for row in &rows {
                manager.popup_menu_row(
                    serial,
                    row.kind,
                    row.id,
                    row.label.clone(),
                    row.flags,
                    row.icon.clone(),
                    row.key.clone(),
                    row.modifiers,
                );
            }
            manager.popup_menu_end(serial);
        }
        self.popup_menus.open.push(OpenPopupMenu {
            serial,
            object: object.clone(),
            client: data.client.clone(),
            surface: data.surface.clone(),
            ids,
        });
        self.transients.push(Transient {
            source: Source::PopupMenu {
                serial,
                object: object.clone(),
                surface: None,
            },
            region: Vec::new(),
            owner: OwnerApp::Client(data.client.clone()),
            owner_rule: Rule::MODAL,
            other_rule: Rule::PASS,
        });
        self.refresh_pointer();
    }

    /// The end of a menu, whatever the reason: the app is told (unless it left), the shell
    /// closes its drawing, the transient goes. False if the menu was not open.
    pub fn finish_popup_menu(&mut self, serial: u32, ended: Ended) -> bool {
        let Some(index) = self
            .popup_menus
            .open
            .iter()
            .position(|menu| menu.serial == serial)
        else {
            return false;
        };
        let menu = self.popup_menus.open.remove(index);
        self.transients
            .retain(|t| !matches!(&t.source, Source::PopupMenu { serial: s, .. } if *s == serial));
        if menu.object.is_alive() {
            match ended {
                Ended::Dismissed => menu.object.dismissed(),
                Ended::Selected(id) => menu.object.selected(id),
                Ended::Silent => {}
            }
        }
        self.shell_managers.retain(|manager| manager.is_alive());
        for manager in &self.shell_managers {
            if manager.version() >= SHELL_VERSION {
                manager.popup_menu_closed(serial);
            }
        }
        self.refresh_pointer();
        true
    }

    /// The shell chose an item: the app receives it and the menu is over.
    pub fn choose_popup_menu(&mut self, serial: u32, id: u32) -> bool {
        let known = self
            .popup_menus
            .open
            .iter()
            .any(|menu| menu.serial == serial && menu.ids.contains(&id));
        known && self.finish_popup_menu(serial, Ended::Selected(id))
    }

    /// Where the shell drew the menu: a press there keeps it open.
    pub fn set_popup_menu_region(
        &mut self,
        serial: u32,
        surface: WlSurface,
        region: Option<WlRegion>,
    ) {
        let region = self.transient_region(&surface, region);
        for transient in &mut self.transients {
            if let Source::PopupMenu {
                serial: s,
                surface: slot,
                ..
            } = &mut transient.source
                && *s == serial
            {
                *slot = Some(surface.clone());
                transient.region = region.clone();
            }
        }
    }

    /// Ends every open menu (a change of context).
    pub fn dismiss_popup_menus(&mut self) {
        let all: Vec<u32> = self.popup_menus.open.iter().map(|m| m.serial).collect();
        for serial in all {
            self.finish_popup_menu(serial, Ended::Dismissed);
        }
    }

    /// Menus whose surface is no longer on screen (unmapped, minimized, hidden, gone) end.
    pub fn prune_popup_menus(&mut self) {
        let gone: Vec<u32> = self
            .popup_menus
            .open
            .iter()
            .filter(|menu| {
                !menu.surface.is_alive() || self.surface_screen_origin(&menu.surface).is_none()
            })
            .map(|menu| menu.serial)
            .collect();
        for serial in gone {
            self.finish_popup_menu(serial, Ended::Dismissed);
        }
    }

    /// The surface a menu hangs from is being destroyed.
    pub fn dismiss_popup_menus_on(&mut self, surface: &WlSurface) {
        let gone: Vec<u32> = self
            .popup_menus
            .open
            .iter()
            .filter(|menu| menu.surface == *surface)
            .map(|menu| menu.serial)
            .collect();
        for serial in gone {
            self.finish_popup_menu(serial, Ended::Dismissed);
        }
    }

    /// The keyboard went to `focus`: a menu ends when it is a surface of neither the menu's
    /// app nor a shell component (the shell takes the keyboard to navigate the menu).
    pub fn popup_menus_follow_focus(&mut self, focus: Option<&WlSurface>) {
        if self.popup_menus.open.is_empty() {
            return;
        }
        let Some(client) = focus.and_then(|surface| surface.client()).map(|c| c.id()) else {
            return;
        };
        if self
            .shell_managers
            .iter()
            .any(|m| m.client().is_some_and(|c| c.id() == client))
        {
            return;
        }
        let gone: Vec<u32> = self
            .popup_menus
            .open
            .iter()
            .filter(|menu| menu.client != client)
            .map(|menu| menu.serial)
            .collect();
        for serial in gone {
            self.finish_popup_menu(serial, Ended::Dismissed);
        }
    }
}

/// `get_popup_menu`.
pub fn get_popup_menu(
    manager: &SilicaMenuManagerV1,
    client: &Client,
    data_init: &mut DataInit<'_, State>,
    request: silica_menu_manager_v1::Request,
) {
    let silica_menu_manager_v1::Request::GetPopupMenu {
        id,
        surface,
        kind,
        x,
        y,
        width,
        height,
        selected,
    } = request
    else {
        return;
    };
    if surface.client().map(|c| c.id()) != Some(client.id()) {
        manager.post_error(
            silica_menu_manager_v1::Error::InvalidSurface,
            "the surface belongs to another client",
        );
        return;
    }
    let kind = match kind {
        WEnum::Value(value) => value as u32,
        WEnum::Unknown(value) => value.min(1),
    };
    data_init.init(
        id,
        PopupMenuData {
            client: client.id(),
            surface,
            kind,
            anchor: Rectangle::new(Point::from((x, y)), (width.max(0), height.max(0)).into()),
            selected,
            build: Mutex::new(Build::default()),
        },
    );
}

impl Dispatch<SilicaPopupMenuV1, PopupMenuData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        menu: &SilicaPopupMenuV1,
        request: silica_popup_menu_v1::Request,
        data: &PopupMenuData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use silica_popup_menu_v1::{Error, Request};
        let Ok(mut build) = data.build.lock() else {
            return;
        };
        // Rows are added before `show`, once.
        let push = |build: &mut Build, row: Row| {
            if build.shown {
                menu.post_error(Error::InvalidState, "the menu is already shown");
                return;
            }
            if build.rows.len() >= MAX_ROWS {
                menu.post_error(Error::TooManyRows, "too many rows");
                return;
            }
            build.rows.push(row);
        };
        match request {
            Request::AddItem {
                id,
                label,
                flags,
                icon,
                key,
                modifiers,
            } => {
                if id == 0 || !build.ids.insert(id) {
                    menu.post_error(Error::InvalidRow, "an item id is zero or repeated");
                    return;
                }
                if too_long(&label) || too_long(&icon) || too_long(&key) {
                    menu.post_error(Error::InvalidRow, "text past the limit");
                    return;
                }
                let flags = flags_of(flags);
                push(
                    &mut build,
                    Row {
                        kind: ROW_ITEM,
                        id,
                        label,
                        flags,
                        icon,
                        key,
                        modifiers: flags_of(modifiers) & 0xf,
                    },
                );
            }
            Request::AddSeparator => push(
                &mut build,
                Row {
                    kind: ROW_SEPARATOR,
                    ..Row::default()
                },
            ),
            Request::AddSection { label } => {
                if too_long(&label) {
                    menu.post_error(Error::InvalidRow, "text past the limit");
                    return;
                }
                push(
                    &mut build,
                    Row {
                        kind: ROW_SECTION,
                        label,
                        ..Row::default()
                    },
                );
            }
            Request::BeginSubmenu { label, flags, icon } => {
                if too_long(&label) || too_long(&icon) {
                    menu.post_error(Error::InvalidRow, "text past the limit");
                    return;
                }
                if build.depth >= MAX_DEPTH {
                    menu.post_error(Error::TooManyRows, "too many levels of submenu");
                    return;
                }
                build.depth += 1;
                let flags = flags_of(flags);
                push(
                    &mut build,
                    Row {
                        kind: ROW_SUBMENU_BEGIN,
                        label,
                        flags,
                        icon,
                        ..Row::default()
                    },
                );
            }
            Request::EndSubmenu => {
                if build.depth == 0 {
                    menu.post_error(Error::InvalidState, "no submenu to end");
                    return;
                }
                build.depth -= 1;
                push(
                    &mut build,
                    Row {
                        kind: ROW_SUBMENU_END,
                        ..Row::default()
                    },
                );
            }
            Request::Show => {
                if build.shown || build.depth != 0 {
                    menu.post_error(Error::InvalidState, "show twice, or a submenu left open");
                    return;
                }
                build.shown = true;
                let rows = std::mem::take(&mut build.rows);
                let ids = std::mem::take(&mut build.ids);
                drop(build);
                if ids.is_empty() {
                    menu.dismissed();
                    return;
                }
                state.open_popup_menu(menu, data, rows, ids);
            }
            Request::Destroy => {
                drop(build);
                state.end_popup_menu_object(menu);
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        menu: &SilicaPopupMenuV1,
        _data: &PopupMenuData,
    ) {
        state.end_popup_menu_object(menu);
    }
}

impl State {
    /// The app destroyed the object (or left): its menu ends without an event.
    fn end_popup_menu_object(&mut self, object: &SilicaPopupMenuV1) {
        let serial = self
            .popup_menus
            .open
            .iter()
            .find(|menu| menu.object == *object)
            .map(|menu| menu.serial);
        if let Some(serial) = serial {
            self.finish_popup_menu(serial, Ended::Silent);
        }
    }
}

/// A bit set argument of a request as a plain number.
fn flags_of<T: Into<u32>>(value: WEnum<T>) -> u32 {
    match value {
        WEnum::Value(flags) => flags.into(),
        WEnum::Unknown(bits) => bits,
    }
}
