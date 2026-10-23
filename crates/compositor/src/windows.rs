//! Minimized and full-screen windows.

use std::cell::{Cell, RefCell};

use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::render::MorphKind;
use crate::state::State;

/// Where the window was before full screen, in its data: the geometry and the frame (with
/// the bar), to go back and for the exit animation.
#[derive(Default)]
struct BeforeFullscreen(
    Cell<Option<Rectangle<i32, Logical>>>,
    Cell<Option<Rectangle<f64, Logical>>>,
);

/// The frame (bar included) a window had before the app zoomed it (`xdg_toplevel.set_maximized`),
/// to go back on `unset_maximized`.
#[derive(Default)]
struct BeforeZoom(Cell<Option<Rectangle<i32, Logical>>>);

pub fn is_fullscreen(window: &Window) -> bool {
    window.toplevel().is_some_and(|toplevel| {
        toplevel
            .current_state()
            .states
            .contains(xdg_toplevel::State::Fullscreen)
    })
}

/// The app of a window (xdg-toplevel `app_id`).
pub fn window_app(window: &Window) -> Option<String> {
    let toplevel = window.toplevel()?;
    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .and_then(|data| data.app_id.clone())
    })
}

/// An arrangement of the green button's popover (windows.md, W-08): a half of the usable area
/// or all of it, to the exact frame with no margins (BACKLOG-1 / W-08).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    Left,
    Right,
    Top,
    Bottom,
    Fill,
}

/// The frames of `count` windows arranged in the area (the first is the focused one): 2 in
/// halves, 3 with the first on the left and two stacked on the right, 4 in quarters.
pub fn arrangement(area: Rectangle<i32, Logical>, count: usize) -> Vec<Rectangle<i32, Logical>> {
    let (x, y) = (area.loc.x, area.loc.y);
    let (w, h) = (area.size.w, area.size.h);
    let (hw, hh) = (w / 2, h / 2);
    let rect = |x: i32, y: i32, w: i32, h: i32| Rectangle::new((x, y).into(), (w, h).into());
    match count {
        0 => Vec::new(),
        1 => vec![area],
        2 => vec![rect(x, y, hw, h), rect(x + hw, y, w - hw, h)],
        3 => vec![
            rect(x, y, hw, h),
            rect(x + hw, y, w - hw, hh),
            rect(x + hw, y + hh, w - hw, h - hh),
        ],
        _ => vec![
            rect(x, y, hw, hh),
            rect(x + hw, y, w - hw, hh),
            rect(x, y + hh, hw, h - hh),
            rect(x + hw, y + hh, w - hw, h - hh),
        ],
    }
}

impl State {
    /// Moves the window to a half of the usable area, or to all of it (outside full screen).
    pub fn tile_window(&mut self, window: &Window, tile: Tile) -> Result<(), String> {
        // Fill is a zoom: not for a window that cannot be resized (silica-os/silica-desktop#245).
        if tile == Tile::Fill && crate::render::buttons_disabled(window).zoom() {
            return Ok(());
        }
        let area = self.usable_area();
        let (x, y) = (area.loc.x, area.loc.y);
        let (w, h) = (area.size.w, area.size.h);
        let (hw, hh) = (w / 2, h / 2);
        let frame = match tile {
            Tile::Left => Rectangle::new((x, y).into(), (hw, h).into()),
            Tile::Right => Rectangle::new((x + hw, y).into(), (w - hw, h).into()),
            Tile::Top => Rectangle::new((x, y).into(), (w, hh).into()),
            Tile::Bottom => Rectangle::new((x, y + hh).into(), (w, h - hh).into()),
            Tile::Fill => area,
        };
        self.place_frame(window, frame)
    }

    /// The app asks to be maximized (`xdg_toplevel.set_maximized`: the zoom of a double click
    /// on its title bar, silica-os/silica-desktop#169): the window fills the usable area and remembers its
    /// frame; `on` false (`unset_maximized`) puts it back. Full-screen windows leave full screen
    /// first, as with the tiles.
    pub fn zoom_window(&mut self, window: &Window, on: bool) -> Result<(), String> {
        // A window that cannot be resized (min size = max size, Calculator) does not zoom,
        // whoever asks: its zoom button is disabled, and the app's title bar double click
        // (`set_maximized`) follows the same rule (silica-os/silica-desktop#245). The client learns it
        // stays as it is.
        if on && crate::render::buttons_disabled(window).zoom() {
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_pending_configure();
            }
            return Ok(());
        }
        window.user_data().insert_if_missing(BeforeZoom::default);
        let before = window
            .user_data()
            .get::<BeforeZoom>()
            .ok_or("window state missing")?;
        if on {
            let frame = self
                .space
                .element_geometry(window)
                .map(|geometry| crate::decorations::frame_of(window, geometry));
            // A window zoomed twice keeps the frame it had before the first zoom.
            if !window.toplevel().is_some_and(|t| {
                t.current_state()
                    .states
                    .contains(xdg_toplevel::State::Maximized)
            }) && !is_fullscreen(window)
            {
                before.0.set(frame);
            }
            let area = self.usable_area();
            self.place_frame_as(window, area, true)
        } else {
            match before.0.take() {
                Some(frame) => self.place_frame_as(window, frame, false),
                None => {
                    // Nothing to go back to: the client only learns it is not maximized.
                    let toplevel = window.toplevel().ok_or("window without a toplevel")?;
                    toplevel.with_pending_state(|state| {
                        state.states.unset(xdg_toplevel::State::Maximized);
                    });
                    toplevel.send_pending_configure();
                    Ok(())
                }
            }
        }
    }

    /// Arranges the window and the `count − 1` most recently used ones of the space (from the
    /// top of the stack) according to [`arrangement`].
    pub fn arrange_windows(&mut self, first: &Window, count: usize) -> Result<(), String> {
        let mut windows = vec![first.clone()];
        windows.extend(
            self.space
                .elements()
                .rev()
                .filter(|w| *w != first)
                .take(count.saturating_sub(1))
                .cloned(),
        );
        let frames = arrangement(self.usable_area(), windows.len());
        for (window, frame) in windows.iter().zip(frames) {
            self.place_frame(window, frame)?;
        }
        self.focus_window(Some(first.clone()));
        Ok(())
    }

    /// Puts the window frame (bar included) in `frame`: position, and content size requested
    /// from the client.
    fn place_frame(
        &mut self,
        window: &Window,
        frame: Rectangle<i32, Logical>,
    ) -> Result<(), String> {
        self.place_frame_as(window, frame, false)
    }

    /// [`Self::place_frame`] with the `maximized` state the client is told it has.
    fn place_frame_as(
        &mut self,
        window: &Window,
        frame: Rectangle<i32, Logical>,
        maximized: bool,
    ) -> Result<(), String> {
        let was_fullscreen = is_fullscreen(window);
        if was_fullscreen {
            self.toggle_fullscreen(window)?;
        }
        let before = self
            .space
            .element_geometry(window)
            .map(|geometry| crate::decorations::frame_of(window, geometry).to_f64());
        let toplevel = window.toplevel().ok_or("window without a toplevel")?;
        let bar = crate::decorations::titlebar_height(window);
        let size = (frame.size.w, frame.size.h.saturating_sub(bar).max(1));
        toplevel.with_pending_state(|state| {
            state.size = Some(size.into());
            if maximized {
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Maximized);
            }
        });
        toplevel.send_pending_configure();
        self.space
            .map_element(window.clone(), frame.loc + Point::from((0, bar)), false);
        // The frame slides to its exact place (a window that left full screen has its own
        // animation).
        if let Some(before) = before.filter(|_| !was_fullscreen) {
            self.start_morph(window, before, frame.to_f64(), MorphKind::Tile);
        }
        Ok(())
    }

    /// Where the thumbnail of a minimized window ends up, in output points: the cell before
    /// the Trash in the Dock (the widest glass panel of the `dock` layer). From the minimize
    /// recording (`windows.md`): the Dock grows by one cell (816–1743 → 784–1775) and the
    /// thumbnail stops centered at x 1672, that is 71 pt to the left of the previous right
    /// edge; vertically at the center of the Dock. Thumbnail 57 wide and at most 47 tall
    /// (`dock.md`, D-08). Without a Dock: at the bottom center of the screen.
    pub fn minimize_target(&self, window: &Window) -> Option<Rectangle<f64, Logical>> {
        const FROM_RIGHT: f64 = 71.0;
        const WIDTH: f64 = 57.0;
        const MAX_HEIGHT: f64 = 47.0;
        /// The window shrinks to about the icon's size.
        const ICON: f64 = 40.0;
        let geometry = self.space.element_geometry(window)?;
        let frame = crate::decorations::frame_of(window, geometry).to_f64();
        let height = if frame.size.w > 0.0 {
            (WIDTH * frame.size.h / frame.size.w).min(MAX_HEIGHT)
        } else {
            MAX_HEIGHT
        };
        let map = smithay::desktop::layer_map_for_output(&self.output);
        let panel = map
            .layers()
            .filter(|layer| layer.namespace() == "dock")
            .filter_map(|layer| {
                let origin = map.layer_geometry(layer)?.loc.to_f64();
                crate::glass::current_regions(layer.wl_surface())
                    .into_iter()
                    .map(|r| {
                        Rectangle::<f64, Logical>::new(
                            (origin.x + f64::from(r.x), origin.y + f64::from(r.y)).into(),
                            (f64::from(r.width), f64::from(r.height)).into(),
                        )
                    })
                    .max_by(|a, b| a.size.w.total_cmp(&b.size.w))
            })
            .next();
        let output = self.space.output_geometry(&self.output)?.to_f64();
        // Into the icon of the window's app (Dock › Minimize windows into application icon).
        let icon = self
            .dock_config
            .minimize_to_app
            .then(|| window_app(window))
            .flatten()
            .and_then(|app| {
                self.dock_icons
                    .iter()
                    .find(|(known, _)| *known == app)
                    .map(|(_, at)| *at)
            })
            .and_then(|(x, y)| {
                let origin = map
                    .layers()
                    .find(|layer| layer.namespace() == "dock")
                    .and_then(|layer| map.layer_geometry(layer))?
                    .loc
                    .to_f64();
                Some(Rectangle::<f64, Logical>::new(
                    (origin.x + x - ICON / 2.0, origin.y + y - ICON / 2.0).into(),
                    (ICON, ICON).into(),
                ))
            });
        if let Some(icon) = icon {
            return Some(icon);
        }
        let center: Point<f64, Logical> = match panel {
            Some(panel) => (
                panel.loc.x + panel.size.w - FROM_RIGHT,
                panel.loc.y + panel.size.h / 2.0,
            )
                .into(),
            None => (
                output.loc.x + output.size.w / 2.0,
                output.loc.y + output.size.h - 40.0,
            )
                .into(),
        };
        Some(Rectangle::new(
            (center.x - WIDTH / 2.0, center.y - height / 2.0).into(),
            (WIDTH, height).into(),
        ))
    }

    /// Removes the window from the screen and gives the focus to the one below.
    pub fn minimize_window(&mut self, window: &Window) -> Result<(), String> {
        let location = self
            .space
            .element_location(window)
            .ok_or("window not on screen")?;
        // The thumbnail for the Dock and the genie, while the window is still on screen.
        self.capture_thumbnail(window);
        self.start_genie(window, false);
        self.space.unmap_elem(window);
        self.minimized.push((window.clone(), location));
        self.prune_popup_menus();
        self.announce_minimized(window, true);
        let next = self.space.elements().last().cloned();
        self.focus_window(next);
        self.desktop_owner_fallback(None);
        Ok(())
    }

    /// Brings a minimized window back where it was. False if it was not minimized.
    pub fn restore_window(&mut self, window: &Window) -> bool {
        let Some(index) = self.minimized.iter().position(|(w, _)| w == window) else {
            return false;
        };
        let (window, location) = self.minimized.remove(index);
        self.announce_minimized(&window, false);
        self.space.map_element(window.clone(), location, true);
        // It comes out of the Dock as a genie; until the genie ends the window stays hidden.
        self.start_genie(&window, true);
        true
    }

    /// The app closed its last window but keeps running (#142): the window leaves the screen,
    /// the shell's list and the focus, fading out like a closed window, and waits in `hidden`
    /// with its position. A minimized window is hidden too, from the Dock.
    pub fn hide_window(&mut self, surface: &WlSurface) {
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .is_some_and(|focus| &focus == surface);
        let on_screen = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned();
        let entry = if let Some(window) = on_screen {
            let location = self.space.element_location(&window).unwrap_or_default();
            self.start_closing(&window);
            self.space.unmap_elem(&window);
            self.prune_popup_menus();
            (window, location)
        } else if let Some(index) = self
            .minimized
            .iter()
            .position(|(w, _)| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        {
            let (window, location) = self.minimized.remove(index);
            self.announce_minimized(&window, false);
            (window, location)
        } else {
            return;
        };
        self.withdraw_toplevel(&entry.0);
        let app = window_app(&entry.0);
        // Another window of the app (its Get Info, #146) takes the focus it had.
        let next = if focused {
            self.next_focus_after(&entry.0, None)
        } else {
            None
        };
        self.hidden.push(entry);
        if next.is_some() {
            self.focus_window(next);
            return;
        }
        // As on the reference (APP-LC) the app stays frontmost with no window: no other
        // window takes the focus, and the menu bar keeps showing the app.
        self.set_windowless_front(app);
        self.focus_window(None);
    }

    /// Tells the shell whether the focused window is full screen (`fullscreen.front`, `1` or
    /// `0`): the menu bar hides itself then when Settings says so. Sent when it changes.
    pub fn sync_fullscreen_front(&mut self) {
        let on = self.focused_window().is_some_and(|w| is_fullscreen(&w));
        let value = if on { "1" } else { "0" };
        if self
            .shell_states
            .get("fullscreen.front")
            .map(String::as_str)
            != Some(value)
        {
            self.broadcast_to_shell("fullscreen.front".to_owned(), value.to_owned());
        }
    }

    /// The windowless frontmost app (#142), announced to the shell as `app.frontmost`.
    pub fn set_windowless_front(&mut self, app: Option<String>) {
        if self.windowless_front == app {
            return;
        }
        if let Some(previous) = self.windowless_front.take() {
            self.windowless_history.retain(|a| *a != previous);
            self.windowless_history.push(previous);
        }
        if let Some(app) = &app {
            self.windowless_history.retain(|a| a != app);
        }
        self.windowless_front = app;
        let value = self.windowless_front.clone().unwrap_or_default();
        self.broadcast_to_shell("app.frontmost".to_owned(), value);
        self.reannounce_focus();
    }

    /// The app shows its hidden window again: back where it was, in the shell's list, focused.
    /// A window that is not hidden is brought to the front (a minimized one comes out of the
    /// Dock): the app was opened again while its window sat behind others.
    pub fn show_window(&mut self, surface: &WlSurface) {
        let Some(index) = self
            .hidden
            .iter()
            .position(|(w, _)| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        else {
            let minimized = self
                .minimized
                .iter()
                .find(|(w, _)| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
                .map(|(w, _)| w.clone());
            if let Some(window) = minimized {
                self.restore_window(&window);
                return;
            }
            let on_screen = self
                .space
                .elements()
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
                .cloned();
            if on_screen.is_some() {
                self.focus_window(on_screen);
            }
            return;
        };
        let (window, location) = self.hidden.remove(index);
        self.space.map_element(window.clone(), location, true);
        self.publish_toplevel(&window);
        self.focus_window(Some(window));
    }

    /// The app asks for full screen or leaves it (`xdg_toplevel.set_fullscreen`, a slideshow,
    /// #173).
    pub fn set_fullscreen_in_place(&mut self, window: &Window, on: bool) -> Result<(), String> {
        if is_fullscreen(window) == on {
            return Ok(());
        }
        self.toggle_fullscreen(window)
    }

    /// Full screen on the output, or back to the previous geometry. The window fills the output
    /// where it is: a space of its own belongs to the Dock, as on macOS.
    pub fn toggle_fullscreen(&mut self, window: &Window) -> Result<(), String> {
        let toplevel = window.toplevel().ok_or("window without a toplevel")?;
        window
            .user_data()
            .insert_if_missing(BeforeFullscreen::default);
        let before = window
            .user_data()
            .get::<BeforeFullscreen>()
            .ok_or("window state missing")?;
        let output = self
            .space
            .output_geometry(&self.output)
            .ok_or("output without geometry")?;
        if is_fullscreen(window) {
            let previous = before.0.take();
            let frame = before.1.take();
            toplevel.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.size = previous.map(|rect| rect.size);
            });
            toplevel.send_pending_configure();
            if let Some(previous) = previous {
                self.space.map_element(window.clone(), previous.loc, true);
            }
            if let Some(frame) = frame {
                self.start_morph(window, output.to_f64(), frame, MorphKind::Exit);
            }
        } else {
            let current = self
                .space
                .element_geometry(window)
                .ok_or("window not on screen")?;
            let frame = crate::decorations::frame_of(window, current).to_f64();
            before.0.set(Some(current));
            before.1.set(Some(frame));
            self.start_morph(window, frame, output.to_f64(), MorphKind::Enter);
            toplevel.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.size = Some(output.size);
            });
            toplevel.send_pending_configure();
            self.space.map_element(window.clone(), output.loc, true);
            self.focus_window(Some(window.clone()));
        }
        Ok(())
    }

    /// The full-screen animation: the frame goes from `from` to `to` (off with animations
    /// at 0).
    fn start_morph(
        &mut self,
        window: &Window,
        from: Rectangle<f64, Logical>,
        to: Rectangle<f64, Logical>,
        kind: crate::render::MorphKind,
    ) {
        if self.animation_speed <= 0.0 {
            return;
        }
        window
            .user_data()
            .insert_if_missing(crate::render::FullscreenMorph::default);
        if let Some(morph) = window.user_data().get::<crate::render::FullscreenMorph>() {
            morph.0.set(Some(crate::render::Morph {
                from,
                to,
                start: std::time::Instant::now(),
                kind,
            }));
        }
    }

    /// A minimized window with that id.
    pub fn minimized_window(&self, id: u64) -> Option<Window> {
        self.minimized
            .iter()
            .find(|(window, _)| Self::window_id(window) == Some(id))
            .map(|(window, _)| window.clone())
    }
}

/// Saved position of a minimized window.
pub type Minimized = (Window, Point<i32, Logical>);

/// How far a child window (Get Info, a compose window) opens from its parent's corner: the
/// parent's bar and buttons stay visible.
const CHILD_OFFSET: i32 = 40;

/// Where a child window of a parent at `parent` opens, kept on screen (64 points of it at
/// least inside the usable area, as the app's remembered place).
pub fn child_location(
    parent: Point<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    let x = parent.x.saturating_add(CHILD_OFFSET).clamp(
        area.loc.x,
        area.loc.x.saturating_add((area.size.w - 64).max(0)),
    );
    let y = parent.y.saturating_add(CHILD_OFFSET).clamp(
        area.loc.y,
        area.loc.y.saturating_add((area.size.h - 64).max(0)),
    );
    (x, y).into()
}

/// Where the frame of a dialog `size` big goes to be centered over its parent's frame, kept
/// in the usable area (its top never under the menu bar).
pub fn centered_over(
    parent: Rectangle<i32, Logical>,
    size: Size<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    let center =
        |start: i32, outer: i32, inner: i32| start.saturating_add(outer.saturating_sub(inner) / 2);
    let x = center(parent.loc.x, parent.size.w, size.w);
    let y = center(parent.loc.y, parent.size.h, size.h);
    let x = x
        .min(
            area.loc
                .x
                .saturating_add(area.size.w.saturating_sub(size.w)),
        )
        .max(area.loc.x);
    let y = y
        .min(
            area.loc
                .y
                .saturating_add(area.size.h.saturating_sub(size.h)),
        )
        .max(area.loc.y);
    (x, y).into()
}

/// The parent's surface of a window (`xdg_toplevel.set_parent`, or an xdg-foreign handle).
/// A modal dialog of its parent (xdg-dialog `set_modal`: a Vetro app's sheet), and where
/// its parent was when it was last placed (it follows its parent's moves).
struct Modal(Cell<bool>, Cell<Option<Point<i32, Logical>>>);

/// Marks `window` as a modal dialog of its parent, or not.
pub fn set_modal(window: &Window, modal: bool) {
    let data = window.user_data();
    data.insert_if_missing(|| Modal(Cell::new(false), Cell::new(None)));
    if let Some(m) = data.get::<Modal>() {
        m.0.set(modal);
    }
}

/// Whether `window` is a modal dialog of its parent.
pub fn is_modal(window: &Window) -> bool {
    window.user_data().get::<Modal>().is_some_and(|m| m.0.get())
}

pub fn parent_of(window: &Window) -> Option<WlSurface> {
    window.toplevel()?.parent()
}

/// The last parent a window had: the focus goes back to it when the window closes, even when
/// the relationship ended first (an xdg-foreign import destroyed before the toplevel).
struct LastParent(RefCell<Option<WlSurface>>);

/// The size and place a dialog was centered at.
type Placed = (Size<i32, Logical>, Point<i32, Logical>);

/// A dialog of another app (xdg-foreign parent) waiting for its first buffer to be centered
/// over its parent. `placed` is the size and place it was given: if the app then changes the
/// size (a first small frame, then the real one) before the user moves the window, it is
/// centered again.
struct CenterOnParent {
    waiting: Cell<bool>,
    placed: Cell<Option<Placed>>,
}

/// Remembers `parent` as the parent of `window` ([`last_parent`]).
pub fn remember_parent(window: &Window, parent: &WlSurface) {
    let data = window.user_data();
    data.insert_if_missing(|| LastParent(RefCell::new(None)));
    if let Some(last) = data.get::<LastParent>() {
        last.0.replace(Some(parent.clone()));
    }
}

/// The parent `window` last had, if any.
pub fn last_parent(window: &Window) -> Option<WlSurface> {
    window
        .user_data()
        .get::<LastParent>()
        .and_then(|last| last.0.borrow().clone())
}

/// `window` is centered over its parent when its first buffer arrives
/// ([`State::center_on_parent`]).
pub fn center_at_first_buffer(window: &Window) {
    let data = window.user_data();
    data.insert_if_missing(|| CenterOnParent {
        waiting: Cell::new(true),
        placed: Cell::new(None),
    });
    if let Some(pending) = data.get::<CenterOnParent>() {
        pending.waiting.set(true);
    }
}

impl State {
    /// A dialog of another app (xdg-foreign parent, [`center_at_first_buffer`]) that now has
    /// its size goes to the middle of its parent, the whole frame (bar included) on screen.
    pub fn center_on_parent(&mut self, window: &Window) {
        let Some(pending) = window.user_data().get::<CenterOnParent>() else {
            return;
        };
        let size = window.geometry().size;
        if size.is_empty() {
            return;
        }
        let at = self.space.element_location(window);
        let resized = pending
            .placed
            .get()
            .is_some_and(|(placed_size, placed_at)| placed_size != size && Some(placed_at) == at);
        if !pending.waiting.get() && !resized {
            return;
        }
        pending.waiting.set(false);
        // A modal dialog without a parent (a system alert) is centred on the screen.
        let parent = parent_of(window).and_then(|parent| self.window_for(&parent));
        let parent_frame = match &parent {
            Some(p) => self.window_frame(p),
            None => Some(self.usable_area()),
        };
        let (Some(parent_frame), Some(frame), Some(location)) = (
            parent_frame,
            self.window_frame(window),
            self.space.element_location(window),
        ) else {
            return;
        };
        let to = centered_over(parent_frame, frame.size, self.usable_area());
        let at = location + (to - frame.loc);
        pending.placed.set(Some((size, at)));
        self.space.map_element(window.clone(), at, false);
        // A sheet follows its parent from where it was centred.
        if let Some(parent) = parent
            && let Some(modal) = window.user_data().get::<Modal>()
        {
            modal.1.set(self.space.element_location(&parent));
        }
    }

    /// Moves every sheet by as much as its parent moved since it was placed: a sheet stays
    /// attached to its window (called before each frame and after moves).
    pub fn follow_sheets(&mut self) {
        let sheets: Vec<Window> = self
            .space
            .elements()
            .filter(|w| is_modal(w))
            .cloned()
            .collect();
        for sheet in sheets {
            let Some(parent) = parent_of(&sheet).and_then(|p| self.window_for(&p)) else {
                continue;
            };
            let (Some(parent_at), Some(at)) = (
                self.space.element_location(&parent),
                self.space.element_location(&sheet),
            ) else {
                continue;
            };
            let Some(modal) = sheet.user_data().get::<Modal>() else {
                continue;
            };
            // Not placed yet: centring sets it first.
            if sheet.geometry().size.is_empty() {
                continue;
            }
            match modal.1.get() {
                Some(before) if before != parent_at => {
                    let moved = at + (parent_at - before);
                    modal.1.set(Some(parent_at));
                    self.space.map_element(sheet.clone(), moved, false);
                }
                Some(_) => {}
                None => modal.1.set(Some(parent_at)),
            }
        }
    }

    /// The modal dialog shown on `window` (the topmost, and its own modal dialog first):
    /// the window the focus goes to when `window` is asked for it.
    pub fn modal_child(&self, window: &Window) -> Option<Window> {
        let mut current = window.clone();
        let mut found = None;
        for _ in 0..8 {
            let surface = current.toplevel().map(|t| t.wl_surface().clone())?;
            let child = self
                .space
                .elements()
                .rev()
                .find(|w| is_modal(w) && parent_of(w).as_ref() == Some(&surface))
                .cloned();
            match child {
                Some(c) => {
                    found = Some(c.clone());
                    current = c;
                }
                None => break,
            }
        }
        found
    }

    /// The window that takes the keyboard when `closing` (focused) goes: its parent if on
    /// screen, else the topmost other window of the same app (macOS gives the focus to the
    /// app's next window); none otherwise.
    pub fn next_focus_after(&self, closing: &Window, parent: Option<WlSurface>) -> Option<Window> {
        let on_screen = |surface: &WlSurface| {
            self.space
                .elements()
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
                .cloned()
        };
        if let Some(parent) = parent.as_ref().and_then(on_screen) {
            return Some(parent);
        }
        let app = window_app(closing)?;
        self.space
            .elements()
            .rev()
            .filter(|w| *w != closing)
            .find(|w| window_app(w).as_deref() == Some(app.as_str()))
            .cloned()
    }

    /// Keeps the child windows of `window` (and theirs) above it after it was raised: an
    /// Info or compose window never goes behind its parent.
    pub fn raise_children(&mut self, window: &Window) {
        let mut parents = vec![window.clone()];
        while let Some(parent) = parents.pop() {
            let Some(surface) = parent.toplevel().map(|t| t.wl_surface().clone()) else {
                continue;
            };
            // Bottom to top: raising them in this order keeps their own stacking.
            let children: Vec<Window> = self
                .space
                .elements()
                .filter(|w| parent_of(w).as_ref() == Some(&surface))
                .cloned()
                .collect();
            for child in children {
                self.space.raise_element(&child, false);
                parents.push(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrangements_split_the_area() {
        let area = Rectangle::new((0, 30).into(), (1600, 900).into());
        let two = arrangement(area, 2);
        assert_eq!(two[0], Rectangle::new((0, 30).into(), (800, 900).into()));
        assert_eq!(two[1], Rectangle::new((800, 30).into(), (800, 900).into()));
        let three = arrangement(area, 3);
        assert_eq!(
            three[1],
            Rectangle::new((800, 30).into(), (800, 450).into())
        );
        assert_eq!(
            three[2],
            Rectangle::new((800, 480).into(), (800, 450).into())
        );
        let four = arrangement(area, 4);
        assert_eq!(
            four[3],
            Rectangle::new((800, 480).into(), (800, 450).into())
        );
        assert!(arrangement(area, 0).is_empty());
    }

    #[test]
    fn child_windows_open_over_their_parent_and_on_screen() {
        let area = Rectangle::new((0, 30).into(), (1600, 900).into());
        assert_eq!(child_location((100, 80).into(), area), (140, 120).into());
        // A parent near the bottom right: the child stays reachable.
        assert_eq!(child_location((1580, 900).into(), area), (1536, 866).into());
        // A parent above the area (under the menu bar): the child comes down into it.
        assert_eq!(child_location((-200, -100).into(), area), (0, 30).into());
    }

    #[test]
    fn dialogs_of_other_apps_are_centered_over_their_parent_and_on_screen() {
        let area = Rectangle::new((0, 30).into(), (1600, 900).into());
        let parent = Rectangle::new((200, 100).into(), (800, 600).into());
        assert_eq!(
            centered_over(parent, (400, 300).into(), area),
            (400, 250).into()
        );
        // A dialog bigger than its parent spills over evenly.
        assert_eq!(
            centered_over(parent, (1000, 700).into(), area),
            (100, 50).into()
        );
        // A parent near the top left: the dialog stays under the menu bar and on screen.
        let corner = Rectangle::new((-300, 0).into(), (400, 100).into());
        assert_eq!(
            centered_over(corner, (400, 300).into(), area),
            (0, 30).into()
        );
        // Near the bottom right.
        let low = Rectangle::new((1500, 850).into(), (400, 200).into());
        assert_eq!(
            centered_over(low, (400, 300).into(), area),
            (1200, 630).into()
        );
    }
}
