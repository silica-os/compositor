//! Trackpad gestures (M5.6): three- or four-finger swipes between spaces, up for
//! Panorama, down for the app windows (or to close the open view); pinch with thumb and
//! three fingers: spreading Show Desktop, pinching the launcher.
//!
//! Provisional logic: the thresholds and the gesture that follows the fingers (the space
//! scrolling with the fingers, Panorama opening halfway) wait for the PA-20…PA-22
//! measurements. Here the gesture is decided at the end, like a shortcut. They come from
//! libinput events or from `input.gesture`.

use silica_control::commands::{
    DESKTOP_SHOW, LAUNCHER_TOGGLE, PANORAMA_APP, PANORAMA_TOGGLE, SPACE_NEXT, SPACE_PREVIOUS,
};

use crate::panorama::Mode;
use crate::state::State;

/// Minimum movement (in points, as libinput reports it) for a swipe.
pub const SWIPE_THRESHOLD: f64 = 80.0;
/// Scale beyond which a pinch counts (spreading, or pinching below the inverse).
pub const PINCH_THRESHOLD: f64 = 1.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureKind {
    Swipe,
    Pinch,
}

/// A gesture in progress: fingers and accumulated movement.
#[derive(Clone, Copy, Debug)]
pub struct Gesture {
    pub kind: GestureKind,
    pub fingers: u32,
    pub dx: f64,
    pub dy: f64,
    pub scale: f64,
}

/// The command of a finished gesture, if any; `view` is the open view.
pub fn command_for(gesture: &Gesture, view: Option<Mode>) -> Option<&'static str> {
    match gesture.kind {
        GestureKind::Swipe if gesture.fingers >= 3 => {
            let (dx, dy) = (gesture.dx, gesture.dy);
            if dx.abs() >= dy.abs() {
                if dx.abs() < SWIPE_THRESHOLD || view.is_some() {
                    return None;
                }
                // Fingers moving left bring the space on the right.
                Some(if dx < 0.0 { SPACE_NEXT } else { SPACE_PREVIOUS })
            } else if dy.abs() < SWIPE_THRESHOLD {
                None
            } else if dy < 0.0 {
                // Up: Panorama (already open, nothing).
                (view != Some(Mode::Panorama)).then_some(PANORAMA_TOGGLE)
            } else {
                // Down: closes the open view, otherwise the app windows.
                Some(match view {
                    Some(Mode::Panorama) => PANORAMA_TOGGLE,
                    Some(Mode::Desktop) => DESKTOP_SHOW,
                    // With the app windows open, the same command closes them.
                    Some(Mode::App) | None => PANORAMA_APP,
                })
            }
        }
        GestureKind::Pinch if gesture.fingers >= 4 => {
            if gesture.scale >= PINCH_THRESHOLD {
                Some(DESKTOP_SHOW)
            } else if gesture.scale > 0.0 && gesture.scale <= 1.0 / PINCH_THRESHOLD {
                Some(LAUNCHER_TOGGLE)
            } else {
                None
            }
        }
        _ => None,
    }
}

impl State {
    pub fn gesture_begin(&mut self, kind: GestureKind, fingers: u32) {
        if self.locked {
            return;
        }
        self.gesture = Some(Gesture {
            kind,
            fingers,
            dx: 0.0,
            dy: 0.0,
            scale: 1.0,
        });
    }

    /// Movement (swipe) or absolute scale (pinch) since the start of the gesture.
    pub fn gesture_update(&mut self, dx: f64, dy: f64, scale: Option<f64>) {
        if let Some(gesture) = self.gesture.as_mut() {
            if dx.is_finite() && dy.is_finite() {
                gesture.dx += dx;
                gesture.dy += dy;
            }
            if let Some(scale) = scale.filter(|s| s.is_finite()) {
                gesture.scale = scale;
            }
        }
    }

    /// End of the gesture: runs its command (unless cancelled) and returns it.
    pub fn gesture_end(&mut self, cancelled: bool) -> Option<&'static str> {
        let gesture = self.gesture.take()?;
        if cancelled {
            return None;
        }
        let id = command_for(&gesture, self.panorama_mode())?;
        if let Err(err) = self.execute_command(id, serde_json::Value::Null) {
            tracing::warn!("gesture {id}: {err}");
        }
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swipe(fingers: u32, dx: f64, dy: f64) -> Gesture {
        Gesture {
            kind: GestureKind::Swipe,
            fingers,
            dx,
            dy,
            scale: 1.0,
        }
    }

    #[test]
    fn swipes_and_pinches_pick_their_command() {
        assert_eq!(command_for(&swipe(3, -200.0, 10.0), None), Some(SPACE_NEXT));
        assert_eq!(
            command_for(&swipe(4, 200.0, 10.0), None),
            Some(SPACE_PREVIOUS)
        );
        assert_eq!(command_for(&swipe(3, 30.0, 0.0), None), None);
        assert_eq!(command_for(&swipe(2, -200.0, 0.0), None), None);
        assert_eq!(
            command_for(&swipe(3, 0.0, -150.0), None),
            Some(PANORAMA_TOGGLE)
        );
        assert_eq!(
            command_for(&swipe(3, 0.0, -150.0), Some(Mode::Panorama)),
            None
        );
        assert_eq!(command_for(&swipe(3, 0.0, 150.0), None), Some(PANORAMA_APP));
        assert_eq!(
            command_for(&swipe(3, 0.0, 150.0), Some(Mode::Panorama)),
            Some(PANORAMA_TOGGLE)
        );
        assert_eq!(
            command_for(&swipe(3, 0.0, 150.0), Some(Mode::App)),
            Some(PANORAMA_APP)
        );
        let pinch = |scale| Gesture {
            kind: GestureKind::Pinch,
            fingers: 4,
            dx: 0.0,
            dy: 0.0,
            scale,
        };
        assert_eq!(command_for(&pinch(1.6), None), Some(DESKTOP_SHOW));
        assert_eq!(command_for(&pinch(0.5), None), Some(LAUNCHER_TOGGLE));
        assert_eq!(command_for(&pinch(1.1), None), None);
    }
}
