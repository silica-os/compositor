//! Trackpad gestures (M5.6): pinching with thumb and three fingers opens the launcher. The
//! gestures of spaces and Mission Control belong to the Dock, as on macOS.
//!
//! Provisional logic: the threshold waits for the PA-20…PA-22 measurements. Here the gesture is
//! decided at the end, like a shortcut. They come from libinput events or from `input.gesture`.

use silica_control::commands::LAUNCHER_TOGGLE;

use crate::state::State;

/// Scale beyond which a pinch counts (pinching below the inverse).
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

/// The command of a finished gesture, if any. Swipes and spreading belong to the Dock (spaces,
/// Mission Control, Show Desktop), as on macOS: the compositor keeps only the pinch that opens
/// the launcher.
pub fn command_for(gesture: &Gesture) -> Option<&'static str> {
    match gesture.kind {
        GestureKind::Pinch
            if gesture.fingers >= 4
                && gesture.scale > 0.0
                && gesture.scale <= 1.0 / PINCH_THRESHOLD =>
        {
            Some(LAUNCHER_TOGGLE)
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
        let id = command_for(&gesture)?;
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
    fn only_the_launcher_pinch_picks_a_command() {
        assert_eq!(command_for(&swipe(3, -200.0, 10.0)), None);
        assert_eq!(command_for(&swipe(3, 0.0, -150.0)), None);
        let pinch = |fingers, scale| Gesture {
            kind: GestureKind::Pinch,
            fingers,
            dx: 0.0,
            dy: 0.0,
            scale,
        };
        assert_eq!(command_for(&pinch(4, 0.5)), Some(LAUNCHER_TOGGLE));
        assert_eq!(command_for(&pinch(4, 1.6)), None);
        assert_eq!(command_for(&pinch(4, 1.1)), None);
        assert_eq!(command_for(&pinch(2, 0.5)), None);
    }
}
