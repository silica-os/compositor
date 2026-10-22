//! Window animation curves, from `plan/reference/windows.md` ("Animations"):
//! recorded at 75 Hz, ±13 ms per instant.

use std::time::Duration;

/// Opening: the window appears already opaque and grows from 0.98 to 1 around the center in
/// ~93–100 ms, slowing down (0.980 → 0.985 → 0.990 → 0.9925 → 0.995 → 1.000).
pub const OPEN: Duration = Duration::from_millis(100);
pub const OPEN_FROM_SCALE: f64 = 0.98;

/// Closing: fade out in ~120 ms (opacity 0.76 → 0.51 → 0.22 → 0.14 → 0.05 → 0.02 → 0)
/// with a scale reduction ≲ 0.5 %.
pub const CLOSE: Duration = Duration::from_millis(125);
pub const CLOSE_TO_SCALE: f64 = 0.995;

/// Progress 0–1 after `elapsed`, with the durations multiplied by `speed` (0: no
/// animations, "Reduce motion").
pub fn progress(elapsed: Duration, duration: Duration, speed: f64) -> f64 {
    if speed <= 0.0 || !speed.is_finite() {
        return 1.0;
    }
    let total = duration.as_secs_f64() * speed;
    if total <= 0.0 {
        return 1.0;
    }
    (elapsed.as_secs_f64() / total).clamp(0.0, 1.0)
}

/// Scale of the opening window: quadratic ease-out.
pub fn open_scale(progress: f64) -> f64 {
    let eased = 1.0 - (1.0 - progress).powi(2);
    OPEN_FROM_SCALE + (1.0 - OPEN_FROM_SCALE) * eased
}

/// Opacity of the closing window: (1 − p)³.
pub fn close_alpha(progress: f64) -> f32 {
    (1.0 - progress).powi(3) as f32
}

/// Scale of the closing window.
pub fn close_scale(progress: f64) -> f64 {
    1.0 + (CLOSE_TO_SCALE - 1.0) * progress
}

/// Smoothstep: 3x² − 2x³ on [0, 1].
pub fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Minimizing into the Dock ("genie"), measured frame by frame on a
/// reference recording (silica-reference, windows.md; 75 Hz, screen 2560 × 1440, window 900 × 600 at
/// (700, 300), Dock cell at x ~1644–1698, Dock top at y 1356). Times from the first bent
/// frame (960 ms into the recording); the thumbnail is still at 1440 ms: **480 ms** in all
/// (the notes count 390 ms from the first movement of the top edge, at 1053 ms).
pub const MINIMIZE: Duration = Duration::from_millis(480);

/// Restore: 4868 → 5308/5335 ms into the recording, ~440–470 ms.
pub const RESTORE: Duration = Duration::from_millis(470);

/// The scale minimize effect (dock.md, SH-DK-09): ~213 ms to minimize and to restore.
pub const SCALE_EFFECT: Duration = Duration::from_millis(213);

/// The state of the genie at an instant: how bent the funnel is (0–1) and how far the top and
/// bottom edges have come down towards the thumbnail (0 = window, 1 = thumbnail).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GenieShape {
    pub bend: f64,
    pub top: f64,
    pub bottom: f64,
}

/// The shape of the genie after `ms` milliseconds of minimizing.
///
/// - Bend: the profile of the edges, once bent, is exactly a smoothstep between the top
///   of the window and the top of the Dock (left edge at 1213 ms: fraction 0.389 / 0.553 /
///   0.712 / 0.850 / 0.950 against 0.399 / 0.557 / 0.710 / 0.844 / 0.939). The bend grows as
///   smoothstep(t / 240): 0.04 at 13 ms, 0.22 at 67, 0.41 at 107, 0.62 at 133, 0.80 at 173, 0.97 at 227.
/// - Top edge: smoothstep((t − 75) / 380): y 308 at 93 ms, 384 at 133, 532 at 187, 731 at
///   253, 947 at 293, 1145 at 347, 1291 at 400, 1349 at 440 (measured fraction 0.22 / 0.41 / 0.61 /
///   0.80 / 0.94 against 0.21 / 0.45 / 0.62 / 0.80 / 0.94).
/// - Bottom edge: enters the Dock earlier, smoothstep((t − 95) / 185): y ~1060 at 173 ms, 1131 at
///   187, 1260 at 227, 1330 at 253, in the Dock at 280.
pub fn genie_minimize(ms: f64) -> GenieShape {
    GenieShape {
        bend: smoothstep(ms / 240.0),
        top: smoothstep((ms - 75.0) / 380.0),
        bottom: smoothstep((ms - 95.0) / 185.0),
    }
}

/// The shape of the genie after `ms` milliseconds of restoring (470 in all).
///
/// - Top edge (`windows.md`, restore): it comes out right away and fast, then slows down:
///   fraction 0.944 on the first frame, 0.81 at ~40 ms, 0.124 at 267, 0.045 at 307, 0.019 at 320,
///   0 at 360: (1 − t / 370)^1.7.
/// - Bend: the left edge goes back to x 700 between 267 and 320 ms (734, 711, 701, 697, 700): the
///   bend unwinds between 130 and 330 ms.
/// - Bottom edge: the inverse of the minimize (it is the last one to leave the Dock), over the
///   duration of the restore.
pub fn genie_restore(ms: f64) -> GenieShape {
    let total = RESTORE.as_secs_f64() * 1000.0;
    let minimize = MINIMIZE.as_secs_f64() * 1000.0;
    let mirrored = genie_minimize(minimize - ms.clamp(0.0, total) * minimize / total);
    GenieShape {
        bend: 1.0 - smoothstep((ms - 130.0) / 200.0),
        top: (1.0 - ms / 370.0).clamp(0.0, 1.0).powf(1.7),
        bottom: mirrored.bottom,
    }
}

/// Full screen, enter (`windows.md`, "Full screen", 1000 → 1520 ms of the file): the
/// window rectangle goes from the frame to the screen. Fraction measured on the right edge
/// (the left one coincides until ~200 ms, then touches the screen edge): 0.06 at 53 ms, 0.12 at
/// 67, 0.22 at 80, 0.417 at 107, 0.56 at 133, 0.66 at 160, 0.80 at 213, 0.91 at 293, 0.95 at 347, 0.98
/// at 413, 1 at 520: it starts accelerating (power 1.5 from 40 to 107 ms), then slows down as
/// 1 − e^(−t/100).
pub const FULLSCREEN_ENTER: Duration = Duration::from_millis(520);

/// Full screen, exit (4988 → 5441 ms of the file): height 1300 → 600 pt, fast at the start and
/// slow at the end; fraction 0.24 at 67 ms, 0.43 at 94, 0.56 at 120, 0.74 at 174, 0.84 at 227, 0.92
/// at 280, 0.96 at 334, 0.99 at 400: 1 − e^(−(t − 40)/90), normalized to 1 at 453 ms.
pub const FULLSCREEN_EXIT: Duration = Duration::from_millis(453);

/// Fraction of the rectangle after `ms` of entering full screen.
pub fn fullscreen_enter(ms: f64) -> f64 {
    const KNEE: f64 = 107.0;
    const AT_KNEE: f64 = 0.417;
    if ms <= 40.0 {
        return 0.0;
    }
    if ms < KNEE {
        return AT_KNEE * ((ms - 40.0) / (KNEE - 40.0)).powf(1.5);
    }
    let end = FULLSCREEN_ENTER.as_secs_f64() * 1000.0;
    let tail = |t: f64| (-(t - KNEE) / 100.0).exp();
    let norm = 1.0 - tail(end);
    (AT_KNEE + (1.0 - AT_KNEE) * (1.0 - tail(ms)) / norm).min(1.0)
}

/// Fraction of the rectangle after `ms` of leaving full screen.
pub fn fullscreen_exit(ms: f64) -> f64 {
    if ms <= 40.0 {
        return 0.0;
    }
    let end = FULLSCREEN_EXIT.as_secs_f64() * 1000.0;
    let curve = |t: f64| 1.0 - (-(t - 40.0) / 90.0).exp();
    (curve(ms) / curve(end)).min(1.0)
}

/// Tiling from the arrangement popover (Half, Fill; BACKLOG-1 / W-08): the frame reaches its
/// exact place ~380 ms after the click, through a ~340 ms ease-out that starts ~40 ms in.
pub const TILE: Duration = Duration::from_millis(380);
/// When the movement starts, inside [`TILE`].
pub const TILE_START_MS: f64 = 40.0;

/// Fraction of the way after `ms` of tiling: nothing until [`TILE_START_MS`], then a cubic
/// ease-out ending at [`TILE`] with no overshoot.
pub fn tile(ms: f64) -> f64 {
    let end = TILE.as_secs_f64() * 1000.0;
    ease_out(((ms - TILE_START_MS) / (end - TILE_START_MS)).clamp(0.0, 1.0))
}

/// A window dropped back where it was in Panorama (BACKLOG-1 / #74): ~500 ms, ease-out, no
/// overshoot.
pub const PANORAMA_RETURN: Duration = Duration::from_millis(500);

/// Cubic ease-out on [0, 1]: 1 − (1 − x)³. Monotonic, ends at exactly 1.
pub fn ease_out(x: f64) -> f64 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiling_is_a_late_start_ease_out_without_overshoot() {
        assert_eq!(tile(0.0), 0.0);
        assert_eq!(tile(40.0), 0.0);
        assert_eq!(tile(380.0), 1.0);
        assert_eq!(tile(900.0), 1.0);
        // Fast at the start, slow at the end.
        assert!(tile(40.0 + 85.0) > 0.5);
        let mut last = 0.0;
        for ms in 0..=400 {
            let f = tile(f64::from(ms));
            assert!((0.0..=1.0).contains(&f) && f >= last, "{ms} ms: {f}");
            last = f;
        }
    }

    #[test]
    fn ease_out_has_no_overshoot() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.5) > 0.5);
    }

    #[test]
    fn genie_follows_the_measured_frames() {
        // Top edge: (ms, y) from 300 to 1356.
        for (ms, y) in [
            (133.0, 384.0),
            (187.0, 532.0),
            (293.0, 947.0),
            (347.0, 1145.0),
            (440.0, 1349.0),
        ] {
            let measured = (y - 300.0) / 1056.0;
            let model = genie_minimize(ms).top;
            assert!(
                (model - measured).abs() <= 0.06,
                "{ms} ms: {model} against {measured}"
            );
        }
        // Bend.
        for (ms, measured) in [(67.0, 0.22), (133.0, 0.62), (173.0, 0.80), (227.0, 0.97)] {
            let model = genie_minimize(ms).bend;
            assert!(
                (model - measured).abs() <= 0.07,
                "{ms} ms: {model} against {measured}"
            );
        }
        assert_eq!(genie_minimize(0.0).top, 0.0);
        let end = genie_minimize(480.0);
        assert!(end.top >= 0.999 && end.bottom >= 0.999 && end.bend >= 0.999);
    }

    #[test]
    fn restore_is_a_genie_coming_out() {
        let start = genie_restore(0.0);
        assert!(start.top > 0.99 && start.bottom > 0.99 && start.bend > 0.99);
        let end = genie_restore(470.0);
        assert!(end.top < 0.001 && end.bottom < 0.001 && end.bend < 0.001);
        // Top edge: 0.124 at 267 ms, 0.045 at 307.
        assert!((genie_restore(267.0).top - 0.124).abs() < 0.03);
        assert!((genie_restore(307.0).top - 0.045).abs() < 0.03);
    }

    #[test]
    fn fullscreen_follows_the_measured_frames() {
        for (ms, measured) in [
            (67.0, 0.123),
            (80.0, 0.217),
            (107.0, 0.417),
            (133.0, 0.561),
            (160.0, 0.663),
            (213.0, 0.797),
            (293.0, 0.909),
            (413.0, 0.984),
        ] {
            let model = fullscreen_enter(ms);
            assert!(
                (model - measured).abs() <= 0.05,
                "enter {ms} ms: {model} against {measured}"
            );
        }
        assert_eq!(fullscreen_enter(520.0), 1.0);
        for (ms, measured) in [
            (67.0, 0.24),
            (120.0, 0.563),
            (200.0, 0.799),
            (307.0, 0.94),
            (400.0, 0.991),
        ] {
            let model = fullscreen_exit(ms);
            assert!(
                (model - measured).abs() <= 0.05,
                "exit {ms} ms: {model} against {measured}"
            );
        }
        assert_eq!(fullscreen_exit(453.0), 1.0);
    }

    #[test]
    fn open_follows_the_measured_frames() {
        // (ms from the first frame, measured scale): ±0.0025 uncertainty, ±13 ms.
        for (ms, measured) in [
            (0, 0.980),
            (13, 0.985),
            (40, 0.990),
            (53, 0.9925),
            (93, 1.0),
        ] {
            let p = progress(Duration::from_millis(ms), OPEN, 1.0);
            assert!(
                (open_scale(p) - measured).abs() <= 0.004,
                "{ms} ms: {}",
                open_scale(p)
            );
        }
    }

    #[test]
    fn close_follows_the_measured_frames() {
        for (ms, measured) in [
            (13, 0.76),
            (26, 0.51),
            (53, 0.22),
            (66, 0.14),
            (93, 0.05),
            (125, 0.0),
        ] {
            let p = progress(Duration::from_millis(ms), CLOSE, 1.0);
            assert!(
                (f64::from(close_alpha(p)) - measured).abs() <= 0.08,
                "{ms} ms: {}",
                close_alpha(p)
            );
        }
    }

    #[test]
    fn speed_zero_disables_animations() {
        assert_eq!(progress(Duration::ZERO, OPEN, 0.0), 1.0);
        assert_eq!(progress(Duration::from_millis(50), OPEN, 2.0), 0.25);
    }
}
