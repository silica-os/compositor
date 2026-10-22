//! Hiding apps (the app menu's Hide, Hide Others and Show All; silica-os/silica-desktop#25): the
//! windows of a hidden app leave the screen at once, without an animation, and the app below
//! comes to the front. They stay in the shell's window list (the Dock keeps the app's dot):
//! activating one of them (the Dock, the app switcher, the Window menu) brings the whole app
//! back. Minimized windows stay in the Dock; windows of the other spaces are not touched.

use smithay::desktop::Window;
use smithay::utils::{Logical, Point};

use crate::panorama::window_app;
use crate::state::State;

/// The windows of hidden apps, with the position they come back to.
#[derive(Default)]
pub struct HiddenApps(pub Vec<(Window, Point<i32, Logical>)>);

impl HiddenApps {
    /// Whether the window belongs to a hidden app.
    pub fn contains(&self, window: &Window) -> bool {
        self.0.iter().any(|(w, _)| w == window)
    }

    pub fn windows(&self) -> impl Iterator<Item = &Window> {
        self.0.iter().map(|(w, _)| w)
    }
}

impl State {
    /// The frontmost app: the focused window's, or the windowless one.
    pub fn front_app_id(&self) -> Option<String> {
        self.focused_window()
            .and_then(|w| window_app(&w))
            .or_else(|| self.windowless_front.clone())
    }

    /// Hides the app's windows on screen; the window on top of what remains takes the
    /// focus. Returns how many windows left the screen.
    pub fn hide_app(&mut self, app: &str) -> usize {
        let windows: Vec<Window> = self
            .space
            .elements()
            .filter(|w| window_app(w).as_deref() == Some(app))
            .cloned()
            .collect();
        for window in &windows {
            let location = self.space.element_location(window).unwrap_or_default();
            window.set_activated(false);
            self.space.unmap_elem(window);
            self.hidden_apps.0.push((window.clone(), location));
        }
        self.prune_popup_menus();
        if self.windowless_front.as_deref() == Some(app) {
            self.set_windowless_front(None);
        }
        // A hidden app does not come back to the front by itself.
        self.windowless_history.retain(|a| a != app);
        let front = self.focused_window();
        if front.is_none() || windows.iter().any(|w| Some(w) == front.as_ref()) {
            let next = self.space.elements().last().cloned();
            self.focus_window(next);
            self.desktop_owner_fallback(None);
        }
        windows.len()
    }

    /// Hides every app but the frontmost one. Returns how many windows left the screen.
    pub fn hide_other_apps(&mut self) -> usize {
        let front = self.front_app_id();
        let mut apps: Vec<String> = self
            .space
            .elements()
            .filter_map(window_app)
            .filter(|app| Some(app) != front.as_ref())
            .collect();
        apps.sort();
        apps.dedup();
        apps.iter().map(|app| self.hide_app(app)).sum()
    }

    /// Brings the app's hidden windows back where they were, in their order, without
    /// changing the focus. Returns them.
    pub fn unhide_app(&mut self, app: &str) -> Vec<Window> {
        let (back, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.hidden_apps.0)
            .into_iter()
            .partition(|(w, _)| window_app(w).as_deref() == Some(app));
        self.hidden_apps.0 = keep;
        for (window, location) in &back {
            self.space.map_element(window.clone(), *location, false);
        }
        back.into_iter().map(|(w, _)| w).collect()
    }

    /// Show All: every hidden app comes back, under the windows already on screen.
    pub fn show_all_apps(&mut self) -> usize {
        let hidden = std::mem::take(&mut self.hidden_apps.0);
        let count = hidden.len();
        let focused = self.focused_window();
        for (window, location) in hidden {
            self.space.map_element(window, location, false);
        }
        // The app in front stays in front.
        if let Some(front) = focused {
            self.space.raise_element(&front, true);
        }
        count
    }

    /// A window of a hidden app is asked for (activated): its app comes back first.
    pub fn unhide_for(&mut self, window: &Window) {
        if self.hidden_apps.contains(window)
            && let Some(app) = window_app(window)
        {
            self.unhide_app(&app);
        }
    }

    /// A window closed: it is no longer waiting to come back.
    pub fn forget_hidden_window(&mut self, window: &Window) {
        self.hidden_apps.0.retain(|(w, _)| w != window);
    }
}
