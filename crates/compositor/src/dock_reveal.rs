//! The Dock over a full-screen window (silica-os/compositor#18). The Dock steps aside while the
//! front window fills the screen. Pushing the pointer against the bottom edge slides it in over
//! the window, as on macOS, and it slides away once the pointer leaves it. The delay and the
//! slides are the Dock's auto-hide ones (silica-os/dock `geometry::AUTOHIDE_*`, SH-DK-auto). While
//! it shows, the full-screen client is composited instead of scanned out.

use std::time::{Duration, Instant};

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Logical, Point};
use tracing::warn;

use crate::state::State;

/// How long the pointer rests against the edge before the Dock comes.
const DELAY: Duration = Duration::from_millis(190);
/// The slide in (decelerating), in seconds.
const REVEAL: f64 = 0.19;
/// The slide away (accelerating), in seconds.
const HIDE: f64 = 0.175;
/// The pointer stops at the edge: the last point inside is a pixel from it.
const REACH: f64 = 1.5;

/// Where the Dock is over a full-screen window.
#[derive(Debug, Default)]
pub struct DockReveal {
    /// The pointer is against the edge: the arming its timer checks.
    edge: Option<u64>,
    armed: u64,
    /// The shown fraction: from, to, since, duration in seconds.
    slide: Option<(f32, f32, Instant, f64)>,
}

impl DockReveal {
    /// The shown fraction at `now`, 0 (away) to 1 (in).
    pub fn shown(&self, now: Instant) -> f32 {
        let Some((from, to, since, duration)) = self.slide else {
            return 0.0;
        };
        let t = ((now - since).as_secs_f64() / duration.max(0.001)).clamp(0.0, 1.0) as f32;
        // In decelerating, away accelerating, as the Dock's auto-hide.
        let e = if to > from {
            1.0 - (1.0 - t).powi(2)
        } else {
            t * t
        };
        from + (to - from) * e
    }

    /// Whether it is sliding at `now`.
    pub fn animating(&self, now: Instant) -> bool {
        self.slide
            .is_some_and(|(_, _, since, duration)| (now - since).as_secs_f64() < duration)
    }

    fn target(&self) -> f32 {
        self.slide.map_or(0.0, |(_, to, _, _)| to)
    }

    fn slide_to(&mut self, to: f32, now: Instant) {
        if self.target() == to {
            return;
        }
        let from = self.shown(now);
        let full = if to > from { REVEAL } else { HIDE };
        self.slide = Some((from, to, now, full * f64::from((to - from).abs())));
    }
}

impl State {
    /// How much of the Dock shows: all of it unless the front window is full screen.
    pub fn dock_shown(&self) -> f32 {
        if self.front_is_fullscreen() {
            self.dock_reveal.shown(Instant::now())
        } else {
            1.0
        }
    }

    /// The pointer moved to `location`: against the bottom edge over a full-screen window it
    /// arms the reveal; away from a revealed Dock it sends the Dock away.
    pub fn dock_reveal_check(&mut self, location: Point<f64, Logical>) {
        if !self.front_is_fullscreen() || self.locked {
            self.dock_reveal = DockReveal::default();
            return;
        }
        let Some(geometry) = self.space.output_geometry(&self.output) else {
            return;
        };
        let bottom = f64::from(geometry.loc.y + geometry.size.h);
        if location.y >= bottom - REACH {
            if self.dock_reveal.target() < 1.0 && self.dock_reveal.edge.is_none() {
                self.arm_dock_reveal();
            }
            return;
        }
        self.dock_reveal.edge = None;
        let over_dock = self
            .layer_under(&[smithay::wayland::shell::wlr_layer::Layer::Top], location)
            .is_some_and(|(layer, _)| crate::layers::is_dock(&layer));
        if self.dock_reveal.target() > 0.0 && !over_dock {
            self.dock_reveal.slide_to(0.0, Instant::now());
            self.wake();
        }
    }

    fn arm_dock_reveal(&mut self) {
        self.dock_reveal.armed = self.dock_reveal.armed.wrapping_add(1);
        let id = self.dock_reveal.armed;
        self.dock_reveal.edge = Some(id);
        let inserted =
            self.loop_handle
                .insert_source(Timer::from_duration(DELAY), move |_, _, data| {
                    let state = &mut data.state;
                    if state.dock_reveal.edge == Some(id) && state.front_is_fullscreen() {
                        state.dock_reveal.slide_to(1.0, Instant::now());
                        state.wake();
                    }
                    TimeoutAction::Drop
                });
        if let Err(err) = inserted {
            warn!("dock reveal timer: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_slides_in_decelerating_and_away_accelerating() {
        let mut reveal = DockReveal::default();
        let t0 = Instant::now();
        assert_eq!(reveal.shown(t0), 0.0);
        reveal.slide_to(1.0, t0);
        let half = t0 + Duration::from_secs_f64(REVEAL / 2.0);
        assert!((reveal.shown(half) - 0.75).abs() < 1e-3);
        assert!(reveal.animating(half));
        let t1 = t0 + Duration::from_secs_f64(REVEAL);
        assert_eq!(reveal.shown(t1), 1.0);
        assert!(!reveal.animating(t1));
        reveal.slide_to(0.0, t1);
        let half = t1 + Duration::from_secs_f64(HIDE / 2.0);
        assert!((reveal.shown(half) - 0.75).abs() < 1e-3);
        assert_eq!(reveal.shown(t1 + Duration::from_secs_f64(HIDE)), 0.0);
    }
}
