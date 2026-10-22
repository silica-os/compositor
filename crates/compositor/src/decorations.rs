//! Server-side decorations (`xdg-decoration`): The compositor's title bar above the window content,
//! with the close, minimize and full-screen buttons.
//!
//! The window position in the space stays the content's; the frame is the content plus the
//! bar, on top. Corners and shadow follow the frame.

use compositor_render::titlebar::{self, Part};
use smithay::delegate_xdg_decoration;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::desktop::Window;
use smithay::desktop::space::SpaceElement;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use tracing::{info, warn};

use crate::state::State;

/// Delay before the arrangement popover on the green button with the pointer resting on it:
/// 810–817 ms measured (BACKLOG-1 / W-08).
pub const WINDOW_MENU_HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(813);

/// Delay of the popover with the button pressed: 620–635 ms measured.
pub const WINDOW_MENU_PRESS_DELAY: std::time::Duration = std::time::Duration::from_millis(628);

/// The window has the compositor's bar: server-side mode already confirmed to the client, and it is
/// not full screen (there the bar disappears, as in the recordings).
pub fn is_server_side(window: &Window) -> bool {
    window
        .toplevel()
        .is_some_and(|toplevel| toplevel.current_state().decoration_mode == Some(Mode::ServerSide))
        && !crate::windows::is_fullscreen(window)
}

/// The app asked for full-size content (`silica-window-v1`): the height of its toolbar.
/// The compositor then does not add the bar, it only draws the buttons.
pub fn full_size_toolbar(window: &Window) -> Option<u32> {
    window
        .toplevel()
        .and_then(|toplevel| crate::window_appearance::full_size_toolbar(toplevel.wl_surface()))
}

/// Height of the bar above the content (0 without server-side decorations or with full-size
/// content).
pub fn titlebar_height(window: &Window) -> i32 {
    if is_server_side(window) && full_size_toolbar(window).is_none() {
        titlebar::HEIGHT
    } else {
        0
    }
}

/// The height of the bar the buttons are placed in: The compositor's own bar, or the app's toolbar
/// with full-size content (`SilicaWindow.setFullSizeContent(toolbarHeight)`). The buttons
/// are centred on it and take their left margin from it (`titlebar::button_centers`).
pub fn button_bar_height(window: &Window) -> f64 {
    full_size_toolbar(window).map_or(f64::from(titlebar::HEIGHT), f64::from)
}

/// The frame given the content rectangle: the bar is added on top.
pub fn frame_of(window: &Window, content: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let bar = titlebar_height(window);
    Rectangle::new(
        content.loc - Point::from((0, bar)),
        content.size + Size::from((0, bar)),
    )
}

/// Where the pointer is over a window: bar (with the part) or content.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Titlebar(Window, Part, Point<f64, Logical>),
    Content(Window),
}

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // The compositor prefers to draw the bar: a uniform look across apps.
        toplevel.with_pending_state(|state| state.decoration_mode = Some(Mode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        // A client that draws its own bar (GTK4) keeps it.
        toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| state.decoration_mode = Some(Mode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}

delegate_xdg_decoration!(State);

impl State {
    /// The frame of a window in the space, if mapped.
    pub fn window_frame(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        self.space
            .element_geometry(window)
            .map(|content| frame_of(window, content))
    }

    /// The window under a point, from the top, with the bar counted as part of the window.
    pub fn window_hit(&self, location: Point<f64, Logical>) -> Option<Hit> {
        for window in self.space.elements().rev() {
            if let Some(frame) = self.window_frame(window)
                && is_server_side(window)
            {
                let local = location - frame.loc.to_f64();
                if frame.to_f64().contains(location)
                    && let Some(part) = titlebar::part_at(local, button_bar_height(window))
                    // With full-size content the rest of the band belongs to the app (its
                    // toolbar): only the buttons are the compositor's.
                    && (full_size_toolbar(window).is_none() || part != Part::Bar)
                {
                    return Some(Hit::Titlebar(window.clone(), part, local));
                }
            }
            let Some(render_location) = self
                .space
                .element_location(window)
                .map(|loc| loc - window.geometry().loc)
            else {
                continue;
            };
            if window.is_in_input_region(&(location - render_location.to_f64())) {
                return Some(Hit::Content(window.clone()));
            }
        }
        None
    }

    /// A window that just switched to server-side decorations: if the bar would go out of
    /// the usable area, the window moves down.
    pub fn make_room_for_titlebar(&mut self, window: &Window) {
        let (Some(location), Some(frame)) = (
            self.space.element_location(window),
            self.window_frame(window),
        ) else {
            return;
        };
        let top = self.usable_area().loc.y;
        if frame.loc.y < top {
            let shift = top.saturating_sub(frame.loc.y);
            self.space
                .map_element(window.clone(), location + Point::from((0, shift)), false);
        }
    }

    /// Runs the bar button released over the same button.
    /// Press on the green button: after `WINDOW_MENU_PRESS_DELAY` with the button still held on it,
    /// the shell opens the arrangement popover and the release no longer toggles full screen
    /// (windows.md: at 0.4 s it is not there, at 1.4 s it is; measured: ~628 ms).
    pub fn arm_window_menu(&mut self, window: &Window) {
        self.titlebar_press_count = self.titlebar_press_count.wrapping_add(1);
        let press = self.titlebar_press_count;
        let window = window.clone();
        let timer = Timer::from_duration(WINDOW_MENU_PRESS_DELAY);
        let inserted = self.loop_handle.insert_source(timer, move |_, _, data| {
            data.state.wake();
            let state = &mut data.state;
            let same = state.titlebar_press_count == press
                && state.titlebar_press_inside
                && matches!(&state.titlebar_press, Some((w, Part::Zoom)) if *w == window);
            if same && let Some(frame) = state.window_frame(&window) {
                // The tip below the button: its center x, bottom of the disc (radius 7).
                let (x, y) = titlebar::button_centers(button_bar_height(&window))[2];
                let point = frame.loc + Point::from((x.round() as i32, (y + 7.0).round() as i32));
                if state.announce_window_menu(&window, point) > 0 {
                    state.titlebar_press = None;
                    state.titlebar_press_inside = false;
                }
            }
            TimeoutAction::Drop
        });
        if let Err(err) = inserted {
            warn!("popover timer: {err}");
        }
    }

    /// The pointer came onto the green button: after `WINDOW_MENU_HOVER_DELAY` still resting on
    /// it (no press needed), the shell opens the arrangement popover. Once per entry.
    pub fn arm_zoom_hover(&mut self, window: &Window) {
        self.titlebar_press_count = self.titlebar_press_count.wrapping_add(1);
        let id = self.titlebar_press_count;
        self.zoom_hover = Some((window.clone(), id));
        let window = window.clone();
        let timer = Timer::from_duration(WINDOW_MENU_HOVER_DELAY);
        let inserted = self.loop_handle.insert_source(timer, move |_, _, data| {
            data.state.wake();
            let state = &mut data.state;
            let same = state.zoom_hover.as_ref() == Some(&(window.clone(), id))
                && state.titlebar_press.is_none();
            if same && let Some(frame) = state.window_frame(&window) {
                let (x, y) = titlebar::button_centers(button_bar_height(&window))[2];
                let point = frame.loc + Point::from((x.round() as i32, (y + 7.0).round() as i32));
                state.announce_window_menu(&window, point);
            }
            TimeoutAction::Drop
        });
        if let Err(err) = inserted {
            warn!("hover popover timer: {err}");
        }
    }

    pub fn titlebar_action(&mut self, window: &Window, part: Part) {
        let disabled = crate::render::buttons_disabled(window);
        if (part == Part::Minimize && disabled.minimize())
            || (part == Part::Zoom && disabled.zoom())
        {
            return;
        }
        let result = match part {
            Part::Close => {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_close();
                }
                Ok(())
            }
            Part::Minimize => self.minimize_window(window),
            Part::Zoom => self.toggle_fullscreen(window),
            Part::Bar => Ok(()),
        };
        if let Err(err) = result {
            warn!("button {part:?}: {err}");
        } else {
            info!("button {part:?}");
        }
    }
}
