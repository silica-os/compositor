//! Server-side `silica-window-v1`: an app asks for full-size content. The compositor does not add the
//! title bar above the content and only draws the window buttons over the app, centered in
//! its toolbar. The state is double-buffered (it applies on `wl_surface.commit`); a client
//! asking for two objects for the same surface receives a protocol error, never an the compositor
//! crash.

use std::sync::atomic::{AtomicBool, Ordering};

use silica_protocols::window::server::silica_window_manager_v1::{self, SilicaWindowManagerV1};
use silica_protocols::window::server::silica_window_v1::{self, SilicaWindowV1};
use smithay::reexports::wayland_server::WEnum;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
};
use smithay::wayland::compositor::{Cacheable, with_states};

use crate::state::State;

/// The tallest toolbar accepted (logical pixels).
pub const MAX_TOOLBAR: u32 = 200;

/// The double-buffered state: the height of the app's toolbar (0: The compositor's bar) and the
/// appearance the app overrides the system's with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppearanceState {
    pub toolbar_height: u32,
    /// `None` follows the system (`set_appearance(system)`).
    pub dark: Option<bool>,
    /// Interactive resizing in whole steps (`set_size_increment`): base and step, per axis.
    pub increment: Option<crate::resize::Increment>,
}

impl Cacheable for AppearanceState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

#[derive(Default)]
struct HasWindow(AtomicBool);

/// The full-size content of the surface: the height of the toolbar.
pub fn full_size_toolbar(surface: &WlSurface) -> Option<u32> {
    let height = with_states(surface, |states| {
        states
            .cached_state
            .get::<AppearanceState>()
            .current()
            .toolbar_height
    });
    (height > 0).then_some(height)
}

/// The window's own appearance: `Some(true)` dark, `Some(false)` light, `None` the system's.
pub fn override_of(surface: &WlSurface) -> Option<bool> {
    with_states(surface, |states| {
        states.cached_state.get::<AppearanceState>().current().dark
    })
}

/// The step sizes the surface asked for (`set_size_increment`), if any.
pub fn increment_of(surface: &WlSurface) -> Option<crate::resize::Increment> {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<AppearanceState>()
            .current()
            .increment
    })
}

/// Whether what the compositor draws for the surface's window is dark: its override, otherwise the
/// system appearance `system_dark`.
pub fn surface_dark(surface: &WlSurface, system_dark: bool) -> bool {
    override_of(surface).unwrap_or(system_dark)
}

/// [`surface_dark`] for a window (the system's if it has no toplevel).
pub fn window_dark(window: &smithay::desktop::Window, system_dark: bool) -> bool {
    window.toplevel().map_or(system_dark, |toplevel| {
        surface_dark(toplevel.wl_surface(), system_dark)
    })
}

pub struct WindowManagerState;

impl WindowManagerState {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, SilicaWindowManagerV1, _>(4, ());
        Self
    }
}

impl GlobalDispatch<SilicaWindowManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<SilicaWindowManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<SilicaWindowManagerV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        manager: &SilicaWindowManagerV1,
        request: silica_window_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            silica_window_manager_v1::Request::GetWindow { id, surface } => {
                let already = with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(HasWindow::default);
                    states
                        .data_map
                        .get::<HasWindow>()
                        .is_some_and(|has| has.0.swap(true, Ordering::SeqCst))
                });
                if already {
                    manager.post_error(
                        silica_window_manager_v1::Error::AlreadyHasWindow,
                        "the surface already has a window object",
                    );
                    return;
                }
                data_init.init(id, surface.downgrade());
            }
            silica_window_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<SilicaWindowV1, Weak<WlSurface>> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        window: &SilicaWindowV1,
        request: silica_window_v1::Request,
        surface: &Weak<WlSurface>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Ok(surface) = surface.upgrade() else {
            return;
        };
        let set = |height: u32| {
            with_states(&surface, |states| {
                states
                    .cached_state
                    .get::<AppearanceState>()
                    .pending()
                    .toolbar_height = height.min(MAX_TOOLBAR);
            });
        };
        match request {
            silica_window_v1::Request::SetFullSizeContent { toolbar_height } => {
                set(toolbar_height);
            }
            silica_window_v1::Request::SetAppearance { appearance } => {
                let dark = match appearance {
                    WEnum::Value(silica_window_v1::Appearance::System) => None,
                    WEnum::Value(silica_window_v1::Appearance::Light) => Some(false),
                    WEnum::Value(silica_window_v1::Appearance::Dark) => Some(true),
                    WEnum::Value(_) | WEnum::Unknown(_) => {
                        window.post_error(
                            silica_window_v1::Error::InvalidAppearance,
                            "unknown appearance",
                        );
                        return;
                    }
                };
                with_states(&surface, |states| {
                    states.cached_state.get::<AppearanceState>().pending().dark = dark;
                });
            }
            silica_window_v1::Request::SetSizeIncrement {
                base_width,
                base_height,
                step_width,
                step_height,
            } => {
                let side = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
                let increment = crate::resize::Increment {
                    base: (side(base_width), side(base_height)),
                    step: (side(step_width), side(step_height)),
                };
                with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<AppearanceState>()
                        .pending()
                        .increment = (increment.step != (0, 0)).then_some(increment);
                });
            }
            silica_window_v1::Request::SetHidden { hidden } => {
                if hidden != 0 {
                    state.hide_window(&surface);
                } else {
                    state.show_window(&surface);
                }
            }
            silica_window_v1::Request::Destroy => {
                set(0);
                with_states(&surface, |states| {
                    let mut cached = states.cached_state.get::<AppearanceState>();
                    cached.pending().dark = None;
                    cached.pending().increment = None;
                });
                with_states(&surface, |states| {
                    if let Some(has) = states.data_map.get::<HasWindow>() {
                        has.0.store(false, Ordering::SeqCst);
                    }
                });
            }
            _ => {}
        }
    }
}
