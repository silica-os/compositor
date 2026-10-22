//! Rules for several displays (BACKLOG-1 / #54, `displays.md`), independent of the backend so
//! the headless and drm paths with more than one output can use them. The compositor drives a single
//! display in the real session today; this is the logic the multi-output code will call.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::utils::{Logical, Point, Rectangle};

/// The active menu bar switches in a single frame, 43–74 ms after the click that changes the
/// active display (the middle of the range).
pub const ACTIVE_BAR_DELAY: Duration = Duration::from_millis(58);

/// The display whose area contains the point (Faro opens on the display under the pointer).
/// A point outside every display falls on the nearest one; None without displays.
pub fn display_at(
    displays: &[Rectangle<i32, Logical>],
    point: Point<f64, Logical>,
) -> Option<usize> {
    let distance = |d: &Rectangle<i32, Logical>| {
        let (x, y) = (f64::from(d.loc.x), f64::from(d.loc.y));
        let (w, h) = (f64::from(d.size.w), f64::from(d.size.h));
        let dx = (x - point.x).max(point.x - (x + w)).max(0.0);
        let dy = (y - point.y).max(point.y - (y + h)).max(0.0);
        dx.hypot(dy)
    };
    displays
        .iter()
        .enumerate()
        .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))
        .map(|(index, _)| index)
}

/// While a window is dragged it is drawn on every display it overlaps: the display index
/// and the part of the frame that falls on it.
pub fn dragged_parts(
    displays: &[Rectangle<i32, Logical>],
    frame: Rectangle<i32, Logical>,
) -> Vec<(usize, Rectangle<i32, Logical>)> {
    displays
        .iter()
        .enumerate()
        .filter_map(|(index, display)| Some((index, display.intersection(frame)?)))
        .collect()
}

/// The last frame each window had on each display, to restore with "Move to".
#[derive(Debug, Default)]
pub struct FrameMemory {
    frames: HashMap<(u64, String), Rectangle<i32, Logical>>,
}

impl FrameMemory {
    /// Remembers the frame the window has on the display it is leaving.
    pub fn remember(&mut self, window: u64, display: &str, frame: Rectangle<i32, Logical>) {
        self.frames.insert((window, display.to_owned()), frame);
    }

    /// The frame for the window arriving on `display`: the last one it had there, or else
    /// the current one moved by the displays' offset (same size and relative position).
    pub fn move_to(
        &self,
        window: u64,
        display: &str,
        current: Rectangle<i32, Logical>,
        from: Rectangle<i32, Logical>,
        to: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        self.frames
            .get(&(window, display.to_owned()))
            .copied()
            .unwrap_or_else(|| Rectangle::new(current.loc - from.loc + to.loc, current.size))
    }

    /// The window is gone.
    pub fn forget(&mut self, window: u64) {
        self.frames.retain(|(id, _), _| *id != window);
    }
}

/// Which display has the active menu bar (the other one is dimmed): it changes in one frame,
/// [`ACTIVE_BAR_DELAY`] after the click.
#[derive(Debug, Clone)]
pub struct ActiveBar {
    active: usize,
    pending: Option<(usize, Instant)>,
}

impl ActiveBar {
    pub fn new(active: usize) -> Self {
        Self {
            active,
            pending: None,
        }
    }

    /// A click on `display` at `now`.
    pub fn click(&mut self, display: usize, now: Instant) {
        if display == self.active {
            self.pending = None;
        } else if self.pending.is_none_or(|(pending, _)| pending != display) {
            self.pending = Some((display, now));
        }
    }

    /// The display with the active bar at `now`: no intermediate state.
    pub fn at(&mut self, now: Instant) -> usize {
        if let Some((display, click)) = self.pending
            && now.saturating_duration_since(click) >= ACTIVE_BAR_DELAY
        {
            self.active = display;
            self.pending = None;
        }
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    /// The reference's layout: the secondary on the left of the primary.
    fn layout() -> [Rectangle<i32, Logical>; 2] {
        [rect(0, 0, 2560, 1440), rect(-2560, 0, 2560, 1440)]
    }

    #[test]
    fn faro_opens_on_the_display_under_the_pointer() {
        let displays = layout();
        assert_eq!(display_at(&displays, (100.0, 100.0).into()), Some(0));
        assert_eq!(display_at(&displays, (-100.0, 700.0).into()), Some(1));
        // Outside every display: the nearest one.
        assert_eq!(display_at(&displays, (-3000.0, 2000.0).into()), Some(1));
        assert_eq!(display_at(&[], (0.0, 0.0).into()), None);
    }

    #[test]
    fn a_dragged_window_is_drawn_on_both_displays() {
        let displays = layout();
        let parts = dragged_parts(&displays, rect(-300, 100, 900, 600));
        assert_eq!(
            parts,
            vec![(0, rect(0, 100, 600, 600)), (1, rect(-300, 100, 300, 600))]
        );
        let inside = dragged_parts(&displays, rect(100, 100, 900, 600));
        assert_eq!(inside, vec![(0, rect(100, 100, 900, 600))]);
    }

    #[test]
    fn move_to_restores_the_last_frame_on_that_display() {
        let [primary, secondary] = layout();
        let mut memory = FrameMemory::default();
        // First time: same size and relative position.
        let current = rect(150, 150, 800, 600);
        assert_eq!(
            memory.move_to(1, "second", current, primary, secondary),
            rect(-2410, 150, 800, 600)
        );
        // It had a frame on the secondary before: that one comes back.
        memory.remember(1, "second", rect(-2000, 300, 700, 500));
        assert_eq!(
            memory.move_to(1, "second", current, primary, secondary),
            rect(-2000, 300, 700, 500)
        );
        // Another window is not affected.
        assert_eq!(
            memory.move_to(2, "second", current, primary, secondary),
            rect(-2410, 150, 800, 600)
        );
        memory.forget(1);
        assert_eq!(
            memory.move_to(1, "second", current, primary, secondary),
            rect(-2410, 150, 800, 600)
        );
    }

    #[test]
    fn the_active_bar_switches_in_one_frame_after_the_click() {
        let t0 = Instant::now();
        let mut bar = ActiveBar::new(0);
        bar.click(1, t0);
        assert_eq!(bar.at(t0 + Duration::from_millis(42)), 0);
        assert_eq!(bar.at(t0 + ACTIVE_BAR_DELAY), 1);
        // A click on the active display changes nothing.
        bar.click(1, t0 + Duration::from_millis(200));
        assert_eq!(bar.at(t0 + Duration::from_millis(400)), 1);
        assert!(ACTIVE_BAR_DELAY >= Duration::from_millis(43));
        assert!(ACTIVE_BAR_DELAY <= Duration::from_millis(74));
    }
}
