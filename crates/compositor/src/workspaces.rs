//! Workspaces (M5.1): several desktops, every window in its own.
//!
//! The windows of the active space are in Smithay's `Space`; those of the other spaces are
//! kept here with their position, out of the scene. During a space switch
//! (plan/reference/spaces.md) the two desktops scroll horizontally together, rigidly: the
//! space's windows, wallpaper and menu bar; the Dock and the shell panels stay still. Between
//! the two desktops a 64 pt black band; the travel is the screen width plus the band. The
//! movement follows a critically damped spring (ω = 0.0132 rad/ms, no overshoot). Past the
//! first or the last space the command is ignored, without a bounce.
//!
//! During the transition the windows of the two spaces stay mapped together and move on
//! every frame: shadows, glass and animations work as always. At the end those of the space
//! left behind go out of the scene.

use std::time::Instant;

use smithay::desktop::Window;
use smithay::utils::{IsAlive, Logical, Point};

use crate::state::State;

/// The black band between two spaces while scrolling, in pt.
pub const SPACE_GAP: i32 = 64;

/// Pulsation of the space-switch spring, in rad/ms (fit of the measurements).
pub const SPRING_OMEGA: f64 = 0.0132;

/// Spaces at most: beyond that, `space.add` refuses.
pub const MAX_SPACES: usize = 16;

/// Progress of the space switch `ms` after the start: `1 − (1 + ωt) e^(−ωt)`.
pub fn spring_progress(ms: f64) -> f64 {
    if ms <= 0.0 {
        return 0.0;
    }
    let x = SPRING_OMEGA * ms;
    (1.0 - (1.0 + x) * (-x).exp()).clamp(0.0, 1.0)
}

/// Windows with their position in the space, from bottom to top.
pub type Placed = Vec<(Window, Point<i32, Logical>)>;

/// A scroll in progress between two spaces.
pub struct Transition {
    pub from: usize,
    pub to: usize,
    pub start: Instant,
    /// +1 to the right (the new space enters from the right), −1 to the left.
    pub direction: i32,
    /// The travel: output width plus the band.
    pub travel: i32,
    pub outgoing: Placed,
    pub incoming: Placed,
    /// Progress at the last frame (0–1).
    pub progress: f64,
}

/// The offset of the two desktops at this moment, in pt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpaceShift {
    /// How far the leaving desktop is moved.
    pub outgoing: f64,
    /// How far the entering desktop is moved.
    pub incoming: f64,
    /// Where the black band starts.
    pub gap: f64,
}

pub struct Workspaces {
    /// The windows of the non-active spaces; the active space's entry stays empty.
    pub stored: Vec<Placed>,
    /// Per space: the full-screen window that owns it (a full-screen window is a space of its
    /// own, right after the one it came from; spaces › full screen), None for a desktop.
    pub owners: Vec<Option<Window>>,
    pub active: usize,
    pub transition: Option<Transition>,
    /// A full-screen space to remove once the scroll away from it ends.
    pub pending_removal: Option<Window>,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self {
            stored: vec![Vec::new()],
            owners: vec![None],
            active: 0,
            transition: None,
            pending_removal: None,
        }
    }
}

impl Workspaces {
    pub fn count(&self) -> usize {
        self.stored.len()
    }

    /// The offset for the frame, if a scroll is in progress.
    pub fn shift(&self, output_width: f64) -> Option<SpaceShift> {
        let t = self.transition.as_ref()?;
        let travel = f64::from(t.travel);
        let moved = t.progress * travel * f64::from(t.direction);
        let outgoing = -moved;
        let incoming = f64::from(t.direction) * travel - moved;
        let gap = if t.direction > 0 {
            outgoing + output_width
        } else {
            incoming + output_width
        };
        Some(SpaceShift {
            outgoing,
            incoming,
            gap,
        })
    }
}

impl State {
    /// Which space a window is in.
    pub fn window_space(&self, window: &Window) -> Option<usize> {
        if self.space.elements().any(|w| w == window) {
            let in_transition = self
                .workspaces
                .transition
                .as_ref()
                .is_some_and(|t| t.outgoing.iter().any(|(w, _)| w == window));
            return Some(if in_transition {
                self.workspaces.transition.as_ref().map_or(0, |t| t.from)
            } else {
                self.workspaces.active
            });
        }
        self.workspaces
            .stored
            .iter()
            .position(|placed| placed.iter().any(|(w, _)| w == window))
    }

    /// Switches to space `to` with the scroll. False if it is already the active one or does
    /// not exist (past the edge there is no bounce: as in the reference, the command is ignored).
    pub fn switch_space(&mut self, to: usize, now: Instant) -> bool {
        self.settle_transition();
        let from = self.workspaces.active;
        if to == from || to >= self.workspaces.count() {
            return false;
        }
        self.dismiss_transients();
        let output_width = self
            .space
            .output_geometry(&self.output)
            .map_or(0, |geo| geo.size.w);
        let travel = output_width.saturating_add(SPACE_GAP);
        let direction: i32 = if to > from { 1 } else { -1 };
        let outgoing: Placed = self
            .space
            .elements()
            .filter_map(|w| Some((w.clone(), self.space.element_location(w)?)))
            .collect();
        let incoming = self
            .workspaces
            .stored
            .get_mut(to)
            .map(std::mem::take)
            .unwrap_or_default();
        for (window, location) in &incoming {
            let start = *location + Point::from((direction.saturating_mul(travel), 0));
            self.space.map_element(window.clone(), start, false);
        }
        self.workspaces.active = to;
        self.workspaces.transition = Some(Transition {
            from,
            to,
            start: now,
            direction,
            travel,
            outgoing,
            incoming,
            progress: 0.0,
        });
        let top = self
            .workspaces
            .transition
            .as_ref()
            .and_then(|t| t.incoming.last().map(|(w, _)| w.clone()));
        self.focus_window(top);
        if self.animation_speed <= 0.0 {
            self.settle_transition();
        }
        true
    }

    /// Advances the scroll in progress (on every frame).
    pub fn advance_spaces(&mut self, now: Instant) {
        self.follow_sheets();
        self.sync_fullscreen_front();
        let speed = self.animation_speed;
        let Some(t) = self.workspaces.transition.as_mut() else {
            return;
        };
        let ms = now.saturating_duration_since(t.start).as_secs_f64() * 1000.0;
        let progress = if speed <= 0.0 {
            1.0
        } else {
            spring_progress(ms / speed)
        };
        t.progress = progress;
        let travel = f64::from(t.travel);
        // Half a point from the end: still.
        if (1.0 - progress) * travel < 0.5 {
            self.settle_transition();
            return;
        }
        let moved = (progress * travel).round() as i32 * t.direction;
        let ahead = t.direction.saturating_mul(t.travel);
        let mut placements: Vec<(Window, Point<i32, Logical>)> = Vec::new();
        for (window, base) in &t.outgoing {
            placements.push((window.clone(), *base - Point::from((moved, 0))));
        }
        for (window, base) in &t.incoming {
            placements.push((window.clone(), *base + Point::from((ahead - moved, 0))));
        }
        for (window, location) in placements {
            if self.space.element_location(&window).is_some() {
                self.space.map_element(window, location, false);
            }
        }
    }

    /// Ends the scroll: the windows of the space left behind go out of the scene.
    pub fn settle_transition(&mut self) {
        let Some(t) = self.workspaces.transition.take() else {
            return;
        };
        let mut left = Placed::new();
        for (window, base) in t.outgoing {
            // A window closed during the scroll has already left the Space.
            if self.space.element_location(&window).is_some() {
                self.space.unmap_elem(&window);
                left.push((window, base));
            }
        }
        if let Some(slot) = self.workspaces.stored.get_mut(t.from) {
            slot.extend(left);
        }
        for (window, base) in t.incoming {
            if self.space.element_location(&window).is_some() {
                self.space.map_element(window, base, false);
            }
        }
        // A full-screen space left behind on leaving full screen: gone once off screen.
        if let Some(owner) = self.workspaces.pending_removal.take()
            && let Some(index) = self.owned_space(&owner)
            && index != self.workspaces.active
        {
            if let Some(slot) = self.workspaces.owners.get_mut(index) {
                *slot = None;
            }
            if let Err(err) = self.remove_space(index) {
                tracing::warn!("full-screen space: {err}");
            }
        }
    }

    /// Adds a space at the end and returns its index.
    pub fn add_space(&mut self) -> Result<usize, String> {
        if self.workspaces.count() >= MAX_SPACES {
            return Err(format!("at most {MAX_SPACES} spaces"));
        }
        self.workspaces.stored.push(Vec::new());
        self.workspaces.owners.push(None);
        self.panorama_space_added(Instant::now());
        Ok(self.workspaces.count() - 1)
    }

    /// Inserts an empty space at `index` (the active index follows its space).
    pub fn insert_space(&mut self, index: usize) -> Result<usize, String> {
        self.settle_transition();
        if self.workspaces.count() >= MAX_SPACES {
            return Err(format!("at most {MAX_SPACES} spaces"));
        }
        let index = index.min(self.workspaces.count());
        self.workspaces.stored.insert(index, Vec::new());
        self.workspaces.owners.insert(index, None);
        if self.workspaces.active >= index {
            self.workspaces.active += 1;
        }
        Ok(index)
    }

    /// The space owned by a full-screen window.
    pub fn owned_space(&self, window: &Window) -> Option<usize> {
        self.workspaces
            .owners
            .iter()
            .position(|owner| owner.as_ref() == Some(window))
    }

    /// The labels of the spaces bar: "Desktop n" counting only desktops, the window title for a
    /// full-screen space (title or app name: not determined in the reference; provisional).
    pub fn space_labels(&self) -> Vec<String> {
        // A single desktop has no number (missioncontrol.md › PA-14).
        let alone = self
            .workspaces
            .owners
            .iter()
            .filter(|owner| owner.is_none())
            .count()
            == 1;
        let mut desktops = 0;
        (0..self.workspaces.count())
            .map(
                |index| match self.workspaces.owners.get(index).cloned().flatten() {
                    Some(window) => crate::panorama::window_label(&window),
                    None => {
                        desktops += 1;
                        if alone {
                            crate::panorama::lone_desktop_label()
                        } else {
                            crate::panorama::desktop_label(desktops - 1)
                        }
                    }
                },
            )
            .collect()
    }

    /// Removes a space: its windows move to the previous one (or to the first). Removing the
    /// active space switches right away to the one receiving the windows.
    pub fn remove_space(&mut self, index: usize) -> Result<(), String> {
        self.settle_transition();
        let count = self.workspaces.count();
        if count <= 1 {
            return Err("there is only one space".to_owned());
        }
        if index >= count {
            return Err(format!("space {index} absent (there are {count})"));
        }
        let active = self.workspaces.active;
        // The windows of the removed space (out of the scene if it was the active one).
        let moving: Placed = if index == active {
            let placed: Placed = self
                .space
                .elements()
                .filter_map(|w| Some((w.clone(), self.space.element_location(w)?)))
                .collect();
            for (window, _) in &placed {
                self.space.unmap_elem(window);
            }
            placed
        } else {
            self.workspaces
                .stored
                .get_mut(index)
                .map(std::mem::take)
                .unwrap_or_default()
        };
        self.workspaces.stored.remove(index);
        if index < self.workspaces.owners.len() {
            self.workspaces.owners.remove(index);
        }
        // The space receiving the windows, with the indices already shifted after the removal.
        let receiver = index.saturating_sub(1);
        let new_active = match active.cmp(&index) {
            std::cmp::Ordering::Equal => receiver,
            std::cmp::Ordering::Greater => active - 1,
            std::cmp::Ordering::Less => active,
        };
        if receiver == new_active {
            // The receiver is (or becomes) the active one: everything in the scene.
            let mut placed = if index == active {
                self.workspaces
                    .stored
                    .get_mut(receiver)
                    .map(std::mem::take)
                    .unwrap_or_default()
            } else {
                Placed::new()
            };
            placed.extend(moving);
            for (window, location) in placed {
                self.space.map_element(window, location, false);
            }
        } else if let Some(slot) = self.workspaces.stored.get_mut(receiver) {
            slot.extend(moving);
        }
        self.workspaces.active = new_active;
        let top = self.space.elements().last().cloned();
        self.focus_window(top);
        self.panorama_space_removed(index, Instant::now());
        Ok(())
    }

    /// Moves a window to another space, at the same position.
    pub fn move_window_to_space(&mut self, window: &Window, to: usize) -> Result<(), String> {
        self.settle_transition();
        if to >= self.workspaces.count() {
            return Err(format!("space {to} absent"));
        }
        let from = self
            .window_space(window)
            .ok_or("window not in a space (minimized?)")?;
        if from == to {
            return Ok(());
        }
        let active = self.workspaces.active;
        let location = if from == active {
            let location = self
                .space
                .element_location(window)
                .ok_or("window not on screen")?;
            self.space.unmap_elem(window);
            location
        } else {
            let slot = self.workspaces.stored.get_mut(from).ok_or("space absent")?;
            let index = slot
                .iter()
                .position(|(w, _)| w == window)
                .ok_or("window absent from the space")?;
            slot.remove(index).1
        };
        if to == active {
            self.space.map_element(window.clone(), location, true);
            self.focus_window(Some(window.clone()));
        } else {
            if let Some(slot) = self.workspaces.stored.get_mut(to) {
                slot.push((window.clone(), location));
            }
            if from == active {
                let top = self.space.elements().last().cloned();
                self.focus_window(top);
            }
        }
        Ok(())
    }

    /// The desktop offset for the frame, in pt.
    pub fn space_shift(&self) -> Option<SpaceShift> {
        let width = self
            .space
            .output_geometry(&self.output)
            .map_or(0.0, |geo| f64::from(geo.size.w));
        self.workspaces.shift(width)
    }

    /// Closed windows leave the non-active spaces too.
    pub fn forget_dead_windows(&mut self) {
        for slot in &mut self.workspaces.stored {
            slot.retain(|(window, _)| window.alive());
        }
        // A full-screen window closed: its space goes too, back to the space before it.
        let dead = self
            .workspaces
            .owners
            .iter()
            .position(|owner| owner.as_ref().is_some_and(|w| !w.alive()));
        if let Some(index) = dead {
            if index == self.workspaces.active {
                if let Some(owner) = self.workspaces.owners.get(index).cloned().flatten() {
                    self.workspaces.pending_removal = Some(owner);
                }
                let before = index.saturating_sub(1);
                if !self.switch_space(before, Instant::now()) {
                    self.settle_transition();
                }
            } else {
                if let Some(slot) = self.workspaces.owners.get_mut(index) {
                    *slot = None;
                }
                if let Err(err) = self.remove_space(index) {
                    tracing::warn!("full-screen space: {err}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spring_follows_the_measured_curve() {
        // Measurements (spaces.md): 50 % at ~115–127 ms, 90 % at ~284–296 ms, 99 % at ~503–508 ms.
        let at = |p: f64| {
            let mut ms = 0.0;
            while spring_progress(ms) < p {
                ms += 1.0;
            }
            ms
        };
        assert!((115.0..=130.0).contains(&at(0.5)), "{}", at(0.5));
        assert!((280.0..=300.0).contains(&at(0.9)), "{}", at(0.9));
        assert!((495.0..=515.0).contains(&at(0.99)), "{}", at(0.99));
        // No overshoot.
        assert!((0..3000).all(|ms| spring_progress(f64::from(ms)) <= 1.0));
    }

    #[test]
    fn the_gap_sits_between_the_two_desktops() {
        let mut spaces = Workspaces::default();
        assert_eq!(spaces.shift(2560.0), None);
        spaces.transition = Some(Transition {
            from: 0,
            to: 1,
            start: Instant::now(),
            direction: 1,
            travel: 2624,
            outgoing: Vec::new(),
            incoming: Vec::new(),
            progress: 0.25,
        });
        let shift = spaces.shift(2560.0).unwrap();
        assert_eq!(shift.outgoing, -656.0);
        assert_eq!(shift.incoming, 2624.0 - 656.0);
        // The band starts where the leaving desktop ends, 64 wide.
        assert_eq!(shift.gap, -656.0 + 2560.0);
        assert_eq!(shift.incoming - shift.gap, 64.0);
    }
}
