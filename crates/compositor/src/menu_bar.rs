//! Server-side `silica-menu-bar-v1`: the menu bar strip is the compositor's, as WindowServer's
//! is on macOS (silica-os/compositor#12). The strip has no surface of its own: it is
//! transparent, over the tone that softens the top of the wallpaper (`render.rs`). Clients put
//! their parts in it:
//!
//! - an app's menu titles, at the start of the strip while that app is frontmost; the last
//!   buffer stays on screen when the app hangs;
//! - status items, lined up from the right: the system items (Control Center) in their order,
//!   then the apps' items, the newest leftmost.
//!
//! Input in these surfaces goes to their owners, which open their menus as their own
//! `xdg_popup`s attached with `get_popup` (silica-os/compositor#13 removed the shell-drawn ones).

use silica_protocols::menu_bar::server::silica_menu_bar_manager_v1::{
    self, Placement, SilicaMenuBarManagerV1,
};
use silica_protocols::menu_bar::server::silica_menu_titles_v1::{self, SilicaMenuTitlesV1};
use silica_protocols::menu_bar::server::silica_status_item_v1::{self, SilicaStatusItemV1};
use smithay::desktop::utils::{bbox_from_surface_tree, under_from_surface_tree};
use smithay::desktop::{PopupKind, PopupManager, Window, WindowSurfaceType};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_popup::XdgPopup;
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::wayland::compositor;
use smithay::wayland::shell::xdg::XdgPopupSurfaceData;
use tracing::warn;

use crate::state::State;

/// The strip's height (kit: Menu Bar, 32 pt).
pub const HEIGHT: i32 = 32;
/// The status items end this far from the right edge (kit: Status Bar).
const TRAILING: i32 = 8;

const TITLES_ROLE: &str = "silica_menu_titles_v1";
const ITEM_ROLE: &str = "silica_status_item_v1";

/// The surface of a titles or status item object.
pub struct StripData {
    surface: WlSurface,
}

struct Titles {
    object: SilicaMenuTitlesV1,
    surface: WlSurface,
    /// The room last sent in `configure`.
    room: Option<u32>,
}

struct Item {
    object: SilicaStatusItemV1,
    surface: WlSurface,
    system: bool,
    order: u32,
    seq: u64,
    visible: bool,
}

/// The surfaces in the menu bar.
#[derive(Default)]
pub struct MenuBar {
    titles: Vec<Titles>,
    items: Vec<Item>,
    next_seq: u64,
    /// The client whose titles show: the frontmost app's.
    front: Option<ClientId>,
    /// The titles last told they are shown.
    shown: Option<ObjectId>,
    /// Where the keyboard was before a menu of the bar took it, to go back there when the
    /// menu closes.
    return_focus: Option<WlSurface>,
}

/// The strip as laid out: each surface and where it is in the output.
pub struct Layout {
    pub surfaces: Vec<(WlSurface, Point<i32, Logical>)>,
    /// The room the titles have before the status items.
    pub room: i32,
}

fn surface_width(surface: &WlSurface) -> i32 {
    bbox_from_surface_tree(surface, (0, 0)).size.w.max(0)
}

impl MenuBar {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, SilicaMenuBarManagerV1, _>(1, ());
        Self::default()
    }

    /// Whether any client has a part in the strip: only then is it on screen.
    pub fn active(&self) -> bool {
        !self.titles.is_empty() || !self.items.is_empty()
    }

    /// Every titles and status item surface, shown or not (they keep drawing).
    pub fn surfaces(&self) -> Vec<WlSurface> {
        let titles = self.titles.iter().map(|t| t.surface.clone());
        titles
            .chain(self.items.iter().map(|i| i.surface.clone()))
            .collect()
    }

    /// Whether `surface` is a titles or status item surface.
    pub fn owns(&self, surface: &WlSurface) -> bool {
        self.titles.iter().any(|t| t.surface == *surface)
            || self.items.iter().any(|i| i.surface == *surface)
    }

    fn front_titles(&self) -> Option<&Titles> {
        let front = self.front.as_ref()?;
        self.titles
            .iter()
            .rev()
            .find(|t| t.object.client().is_some_and(|c| c.id() == *front))
    }

    /// The strip on an output `width` wide: the frontmost app's titles at the start, the
    /// visible status items from the right.
    pub fn layout(&self, width: i32) -> Layout {
        let mut items: Vec<&Item> = self.items.iter().filter(|i| i.visible).collect();
        items.sort_by_key(|i| {
            (
                !i.system,
                if i.system { u64::from(i.order) } else { 0 },
                i.seq,
            )
        });
        let mut surfaces = Vec::new();
        let mut x = width - TRAILING;
        for item in items {
            let w = surface_width(&item.surface);
            if w == 0 {
                continue;
            }
            x -= w;
            surfaces.push((item.surface.clone(), Point::from((x, 0))));
        }
        let room = x.max(0);
        if let Some(titles) = self.front_titles() {
            surfaces.push((titles.surface.clone(), Point::from((0, 0))));
        }
        Layout { surfaces, room }
    }

    fn forget(&mut self, id: &ObjectId) {
        self.titles.retain(|t| t.object.id() != *id);
        self.items.retain(|i| i.object.id() != *id);
        if self.shown.as_ref() == Some(id) {
            self.shown = None;
        }
    }
}

impl State {
    /// Whether the strip is on screen: someone has a part in it and no window is full screen
    /// in front.
    pub fn menu_bar_visible(&self) -> bool {
        self.menu_bar.active() && !self.front_is_fullscreen()
    }

    /// The front window is full screen: the menu bar and the Dock step aside.
    pub fn front_is_fullscreen(&self) -> bool {
        self.space
            .elements()
            .last()
            .is_some_and(crate::windows::is_fullscreen)
    }

    fn output_width(&self) -> i32 {
        self.space
            .output_geometry(&self.output)
            .map_or(0, |geo| geo.size.w)
    }

    /// The strip's surfaces and where they are in the output, when it is on screen.
    pub fn menu_bar_view(&self) -> Vec<(WlSurface, Point<i32, Logical>)> {
        if !self.menu_bar_visible() {
            return Vec::new();
        }
        self.menu_bar.layout(self.output_width()).surfaces
    }

    /// Where a surface of the strip is in the output.
    pub fn menu_bar_location(&self, surface: &WlSurface) -> Option<Point<i32, Logical>> {
        self.menu_bar_view()
            .into_iter()
            .find(|(s, _)| s == surface)
            .map(|(_, at)| at)
    }

    /// The surface of the strip (or of its menus) under `pos`, and its origin, in the space.
    pub fn menu_bar_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let origin = self
            .space
            .output_geometry(&self.output)
            .map(|geo| geo.loc)
            .unwrap_or_default();
        let view = self.menu_bar_view();
        for (surface, at) in &view {
            for (popup, location) in PopupManager::popups_for_surface(surface) {
                let offset = origin + *at + location - popup.geometry().loc;
                if let Some((s, loc)) =
                    under_from_surface_tree(popup.wl_surface(), pos, offset, WindowSurfaceType::ALL)
                {
                    return Some((s, loc.to_f64()));
                }
            }
        }
        view.iter().find_map(|(surface, at)| {
            under_from_surface_tree(surface, pos, origin + *at, WindowSurfaceType::ALL)
                .map(|(s, loc)| (s, loc.to_f64()))
        })
    }

    /// The app in front changed (the keyboard moved): its titles show. A menu or popover of
    /// the strip taking the keyboard leaves the app in front as it is.
    pub fn menu_bar_follow_focus(&mut self, focus: Option<&WlSurface>) {
        if let Some(focus) = focus
            && self.menu_bar.owns(&self.focus_root(focus))
        {
            return;
        }
        self.menu_bar.front = self.front_app(focus).map(|(_, client)| client);
        self.menu_bar_update();
    }

    /// The same, reading the focus from the keyboard (outside the focus callback).
    pub fn menu_bar_refresh(&mut self) {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        self.menu_bar_follow_focus(focus.as_ref());
    }

    /// Tells the titles their room and whether they show, where that changed.
    fn menu_bar_update(&mut self) {
        let layout = self.menu_bar.layout(self.output_width());
        let room = u32::try_from(layout.room).unwrap_or(0);
        for titles in &mut self.menu_bar.titles {
            if titles.room != Some(room) {
                titles.room = Some(room);
                let serial = SERIAL_COUNTER.next_serial();
                titles.object.configure(serial.into(), room, HEIGHT as u32);
            }
        }
        let shown = self
            .menu_bar_visible()
            .then(|| self.menu_bar.front_titles().map(|t| t.object.clone()))
            .flatten();
        let shown_id = shown.as_ref().map(Resource::id);
        if self.menu_bar.shown != shown_id {
            if let Some(old) = self.menu_bar.shown.take()
                && let Some(titles) = self.menu_bar.titles.iter().find(|t| t.object.id() == old)
            {
                titles.object.shown(0);
            }
            if let Some(titles) = &shown {
                titles.shown(1);
            }
            self.menu_bar.shown = shown_id;
        }
        self.wake();
    }

    /// A commit of a strip surface: an item that changed width moves the others.
    pub fn menu_bar_commit(&mut self, surface: &WlSurface) {
        if self.menu_bar.owns(surface) {
            self.menu_bar_update();
        }
    }

    /// A menu of the strip takes the keyboard: where it was is kept for when it closes.
    pub fn menu_bar_grab(&mut self, root: &WlSurface) {
        if !self.menu_bar.owns(root) {
            return;
        }
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        if let Some(focus) = focus
            && !self.menu_bar.owns(&self.focus_root(&focus))
        {
            self.menu_bar.return_focus = Some(focus);
        }
    }

    /// Where the keyboard goes when a menu hanging from `parent` closes: back where it was
    /// before a menu of the strip took it, else to `parent`.
    pub fn keyboard_after_popup(&mut self, parent: WlSurface) -> Option<WlSurface> {
        if self.menu_bar.owns(&parent) {
            return self.menu_bar.return_focus.take().filter(Resource::is_alive);
        }
        Some(parent)
    }

    /// The window or surface whose app is in front, and that app: the focused window's (or
    /// the window on top while a shell menu has the keyboard, as `identifier_of`), else the
    /// windowless frontmost app (#142) or the desktop's owner (#147).
    pub fn front_app(&self, focus: Option<&WlSurface>) -> Option<(String, ClientId)> {
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

    /// The window in front, as `identifier_of` finds it.
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

    /// The strip for the control socket: whether it shows, the titles in front and the items.
    pub fn menu_bar_state(&self) -> serde_json::Value {
        let layout = self.menu_bar.layout(self.output_width());
        let app_of = |surface: &WlSurface| {
            surface
                .client()
                .and_then(|c| c.get_credentials(&self.display_handle).ok())
                .map(|c| c.pid)
        };
        let surfaces: Vec<serde_json::Value> = layout
            .surfaces
            .iter()
            .map(|(surface, at)| {
                let item = self.menu_bar.items.iter().find(|i| i.surface == *surface);
                serde_json::json!({
                    "kind": if item.is_some() { "item" } else { "titles" },
                    "system": item.is_some_and(|i| i.system),
                    "order": item.map(|i| i.order),
                    "x": at.x,
                    "width": surface_width(surface),
                    "pid": app_of(surface),
                })
            })
            .collect();
        serde_json::json!({
            "visible": self.menu_bar_visible(),
            "room": layout.room,
            "surfaces": surfaces,
        })
    }
}

/// Attaches `popup`, made with a null parent, to a strip surface; it is placed again now that
/// its parent is known.
fn attach_popup(state: &mut State, parent: &WlSurface, popup: &XdgPopup) {
    let Some(handle) = state.xdg_shell_state.get_popup(popup) else {
        warn!("menu bar: get_popup with an unknown popup");
        return;
    };
    compositor::with_states(handle.wl_surface(), |states| {
        if let Some(data) = states.data_map.get::<XdgPopupSurfaceData>()
            && let Ok(mut data) = data.lock()
        {
            data.parent = Some(parent.clone());
        }
    });
    let positioner = handle.with_pending_state(|s| s.positioner);
    let geometry = state.popup_geometry(&PopupKind::from(handle.clone()), positioner);
    handle.with_pending_state(|s| s.geometry = geometry);
}

impl GlobalDispatch<SilicaMenuBarManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<SilicaMenuBarManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<SilicaMenuBarManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        manager: &SilicaMenuBarManagerV1,
        request: silica_menu_bar_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            silica_menu_bar_manager_v1::Request::GetMenuTitles { id, surface } => {
                if compositor::give_role(&surface, TITLES_ROLE).is_err() {
                    manager.post_error(
                        silica_menu_bar_manager_v1::Error::Role,
                        "the surface already has a role",
                    );
                    return;
                }
                let object = data_init.init(
                    id,
                    StripData {
                        surface: surface.clone(),
                    },
                );
                // A second one replaces the first.
                state
                    .menu_bar
                    .titles
                    .retain(|t| t.object.client().is_none_or(|c| c.id() != client.id()));
                state.output.enter(&surface);
                state.menu_bar.titles.push(Titles {
                    object,
                    surface,
                    room: None,
                });
                state.menu_bar_refresh();
            }
            silica_menu_bar_manager_v1::Request::GetStatusItem {
                id,
                surface,
                placement,
                order,
            } => {
                if compositor::give_role(&surface, ITEM_ROLE).is_err() {
                    manager.post_error(
                        silica_menu_bar_manager_v1::Error::Role,
                        "the surface already has a role",
                    );
                    return;
                }
                let object = data_init.init(
                    id,
                    StripData {
                        surface: surface.clone(),
                    },
                );
                object.configure(SERIAL_COUNTER.next_serial().into(), HEIGHT as u32);
                state.output.enter(&surface);
                let seq = state.menu_bar.next_seq;
                state.menu_bar.next_seq += 1;
                state.menu_bar.items.push(Item {
                    object,
                    surface,
                    system: matches!(placement, WEnum::Value(Placement::System)),
                    order,
                    seq,
                    visible: true,
                });
                state.menu_bar_update();
            }
            silica_menu_bar_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<SilicaMenuTitlesV1, StripData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _titles: &SilicaMenuTitlesV1,
        request: silica_menu_titles_v1::Request,
        data: &StripData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            silica_menu_titles_v1::Request::GetPopup { popup } => {
                attach_popup(state, &data.surface, &popup);
            }
            silica_menu_titles_v1::Request::AckConfigure { .. }
            | silica_menu_titles_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        titles: &SilicaMenuTitlesV1,
        _data: &StripData,
    ) {
        state.menu_bar.forget(&titles.id());
        state.menu_bar_update();
    }
}

impl Dispatch<SilicaStatusItemV1, StripData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        item: &SilicaStatusItemV1,
        request: silica_status_item_v1::Request,
        data: &StripData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            silica_status_item_v1::Request::SetVisible { visible } => {
                if let Some(entry) = state.menu_bar.items.iter_mut().find(|i| i.object == *item) {
                    entry.visible = visible != 0;
                }
                state.menu_bar_update();
            }
            silica_status_item_v1::Request::GetPopup { popup } => {
                attach_popup(state, &data.surface, &popup);
            }
            silica_status_item_v1::Request::AckConfigure { .. }
            | silica_status_item_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        item: &SilicaStatusItemV1,
        _data: &StripData,
    ) {
        state.menu_bar.forget(&item.id());
        state.menu_bar_update();
    }
}
