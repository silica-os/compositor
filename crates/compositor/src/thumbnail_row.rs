//! The row of space thumbnails in the expanded spaces bar (`plan/reference/spaces.md` ›
//! Thumbnail layout, add animation, removal; `missioncontrol.md` › Expanded bar: interactions).
//!
//! - 90 pt tall thumbnails (160 × 90 on the 16:9 reference screen: the width follows the
//!   screen's aspect, as the wallpaper must fill it; the kit draws 160 × 92, which is no screen's
//!   aspect), 32 pt apart, centered around y 98 (the middle of the kit's 52–144, ring 48–148).
//! - A new space is born at the right edge of the screen (right edge 16 pt from it) and slides
//!   to its slot on a critically damped spring (ω 0.0160 rad/ms). User decision (issue #246,
//!   diverging from the measurement): the row recenters at once after an addition and after any
//!   removal (spring ω 0.0165 rad/ms, ω 0.0247 when the thumbnails also change size).
//! - When they do not fit (more than 12 on the reference screen) they all shrink together
//!   with ~76 pt left on each side (16 spaces: ~126 × 71, pitch ~152).

use std::time::Instant;

use smithay::utils::{Logical, Rectangle, Size};

/// Height of a thumbnail at full size.
pub const THUMB_HEIGHT: f64 = 90.0;
/// Gap between two thumbnails at full size.
pub const THUMB_GAP: f64 = 32.0;
/// Vertical center of the thumbnails (53–143).
pub const THUMB_CENTER_Y: f64 = 98.0;
/// Room left on each side when the thumbnails have to shrink (16 spaces on 2560 pt:
/// thumbnails ~126 pt, pitch ~152).
pub const ROW_MARGIN: f64 = 76.0;
/// A new thumbnail is born with its right edge this far from the screen's right edge.
pub const BORN_RIGHT: f64 = 16.0;
/// Springs (rad/ms): a new thumbnail sliding into place, the row closing a gap, the row
/// closing a gap while the thumbnails change size.
pub const ADD_OMEGA: f64 = 0.0160;
pub const REMOVE_OMEGA: f64 = 0.0165;
pub const RESIZE_OMEGA: f64 = 0.0247;
/// The others start moving after the removed one is gone: the slide starts ~122 ms after
/// the press, the thumbnail disappears ~65–109 ms after it (estimated difference).
pub const REMOVE_DELAY_MS: f64 = 35.0;

/// Progress of a critically damped spring `ms` after its start: `1 − (1 + ωt) e^(−ωt)`.
pub fn spring(ms: f64, omega: f64) -> f64 {
    if ms <= 0.0 {
        return 0.0;
    }
    let x = omega * ms;
    (1.0 - (1.0 + x) * (-x).exp()).clamp(0.0, 1.0)
}

/// The thumbnails of `count` spaces centered on the output, all the same size.
pub fn centered(count: usize, output: Size<f64, Logical>) -> Vec<Rectangle<f64, Logical>> {
    if output.w <= 0.0 || output.h <= 0.0 || count == 0 {
        return Vec::new();
    }
    let n = count as f64;
    let width = THUMB_HEIGHT * output.w / output.h;
    let natural = n * width + (n - 1.0) * THUMB_GAP;
    let room = output.w - 2.0 * ROW_MARGIN;
    let shrink = (room / natural).clamp(0.05, 1.0);
    let (w, h, gap) = (width * shrink, THUMB_HEIGHT * shrink, THUMB_GAP * shrink);
    let x0 = (output.w - natural * shrink) / 2.0;
    (0..count)
        .map(|i| {
            Rectangle::new(
                (x0 + i as f64 * (w + gap), THUMB_CENTER_Y - h / 2.0).into(),
                (w, h).into(),
            )
        })
        .collect()
}

/// Whether a thumbnail is at full size (the labels show only then: 16 spaces have none).
pub fn full_size(rect: Rectangle<f64, Logical>) -> bool {
    rect.size.h >= THUMB_HEIGHT - 0.5
}

#[derive(Clone, Debug)]
struct Slot {
    from: Rectangle<f64, Logical>,
    to: Rectangle<f64, Logical>,
    start: Instant,
    omega: f64,
}

impl Slot {
    fn still(rect: Rectangle<f64, Logical>, now: Instant) -> Self {
        Self {
            from: rect,
            to: rect,
            start: now,
            omega: ADD_OMEGA,
        }
    }

    fn at(&self, now: Instant, speed: f64) -> Rectangle<f64, Logical> {
        let p = if speed <= 0.0 {
            1.0
        } else if now < self.start {
            0.0
        } else {
            spring(
                now.duration_since(self.start).as_secs_f64() * 1000.0 / speed,
                self.omega,
            )
        };
        crate::panorama::lerp_rect(self.from, self.to, p)
    }
}

/// The thumbnails of the spaces with their slides in progress.
#[derive(Clone, Debug)]
pub struct Row {
    slots: Vec<Slot>,
}

impl Row {
    /// The row recentered (when the bar expands), still.
    pub fn centered(count: usize, output: Size<f64, Logical>, now: Instant) -> Self {
        Self {
            slots: centered(count, output)
                .into_iter()
                .map(|rect| Slot::still(rect, now))
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Where the thumbnails are at `now` (durations scaled by `speed`).
    pub fn rects(&self, now: Instant, speed: f64) -> Vec<Rectangle<f64, Logical>> {
        self.slots.iter().map(|slot| slot.at(now, speed)).collect()
    }

    /// Where the thumbnails are going.
    pub fn targets(&self) -> Vec<Rectangle<f64, Logical>> {
        self.slots.iter().map(|slot| slot.to).collect()
    }

    /// Whether something is still moving.
    pub fn moving(&self, now: Instant, speed: f64) -> bool {
        self.slots.iter().any(|slot| {
            let at = slot.at(now, speed);
            (at.loc.x - slot.to.loc.x).abs() > 0.25 || (at.size.w - slot.to.size.w).abs() > 0.25
        })
    }

    /// A space added at the end: it is born at the right edge and slides to its slot while
    /// the whole row recenters at once (user decision, issue #246: the measured behaviour left
    /// the row where it was until Panorama was reopened).
    pub fn add(&mut self, output: Size<f64, Logical>, now: Instant, speed: f64) {
        let current = self.rects(now, speed);
        let targets = centered(self.slots.len() + 1, output);
        let mut slots = Vec::with_capacity(targets.len());
        for (index, to) in targets.into_iter().enumerate() {
            let slot = match current.get(index) {
                Some(from) => Slot {
                    from: *from,
                    to,
                    start: now,
                    omega: if (from.size.w - to.size.w).abs() > 0.5 {
                        RESIZE_OMEGA
                    } else {
                        ADD_OMEGA
                    },
                },
                None => {
                    let born = Rectangle::new(
                        (output.w - BORN_RIGHT - to.size.w, to.loc.y).into(),
                        to.size,
                    );
                    Slot {
                        from: born,
                        to,
                        start: now,
                        omega: ADD_OMEGA,
                    }
                }
            };
            slots.push(slot);
        }
        self.slots = slots;
    }

    /// The space at `index` removed: it disappears at once; the others close the gap
    /// recentering (also when it was the last one: user decision, issue #246).
    pub fn remove(&mut self, index: usize, output: Size<f64, Logical>, now: Instant, speed: f64) {
        if index >= self.slots.len() {
            return;
        }
        let mut current = self.rects(now, speed);
        current.remove(index);
        self.slots.remove(index);
        if self.slots.is_empty() {
            return;
        }
        let targets = centered(self.slots.len(), output);
        let start =
            now + std::time::Duration::from_secs_f64(REMOVE_DELAY_MS * speed.max(0.0) / 1000.0);
        self.slots = current
            .into_iter()
            .zip(targets)
            .map(|(from, to)| Slot {
                from,
                to,
                start,
                omega: if (from.size.w - to.size.w).abs() > 0.5 {
                    RESIZE_OMEGA
                } else {
                    REMOVE_OMEGA
                },
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const REFERENCE: (f64, f64) = (2560.0, 1440.0);

    fn xs(rects: &[Rectangle<f64, Logical>]) -> Vec<f64> {
        rects.iter().map(|r| r.loc.x.round()).collect()
    }

    #[test]
    fn the_row_is_centered_as_measured() {
        let output = Size::from(REFERENCE);
        // 2 spaces with the bar just expanded: 1103/1296 (±1), 160 × 90, centred on y 98.
        let two = centered(2, output);
        assert_eq!(xs(&two), [1104.0, 1296.0]);
        assert_eq!(two[0].size, Size::from((160.0, 90.0)));
        assert_eq!(two[0].loc.y, 53.0);
        // 3 spaces: 1008, 1200, 1392; 12 spaces from x 144.
        assert_eq!(xs(&centered(3, output)), [1008.0, 1200.0, 1392.0]);
        assert_eq!(centered(12, output)[0].loc.x, 144.0);
        // 1 space: in the middle.
        let one = centered(1, output);
        assert_eq!(one[0].loc.x + one[0].size.w / 2.0, 1280.0);
        // 16 spaces: ~126 × 71, pitch ~152, at y ~63.
        let sixteen = centered(16, output);
        assert!((sixteen[0].size.w - 126.0).abs() < 1.5, "{:?}", sixteen[0]);
        assert!((sixteen[1].loc.x - sixteen[0].loc.x - 152.0).abs() < 1.5);
        assert!((sixteen[0].loc.y - 63.0).abs() < 2.0);
        assert!(!full_size(sixteen[0]) && full_size(two[0]));
        // On a 16:10 screen the thumbnails keep 90 pt of height (and stay in the bar).
        let wide = centered(1, Size::from((1920.0, 1200.0)));
        assert_eq!(wide[0].size, Size::from((144.0, 90.0)));
        assert_eq!(wide[0].loc.x + 72.0, 960.0);
    }

    #[test]
    fn a_new_space_slides_in_from_the_right_and_the_row_recenters() {
        let output = Size::from(REFERENCE);
        let start = Instant::now();
        let mut row = Row::centered(2, output, start);
        row.add(output, start, 1.0);
        // At the start nothing has moved; the new one is born at the right edge (2384–2543)...
        let now = row.rects(start, 1.0);
        assert_eq!(xs(&now), [1104.0, 1296.0, 2384.0]);
        // ...and the three end centered (1008, 1200, 1392), with the measured spring.
        assert_eq!(xs(&row.targets()), [1008.0, 1200.0, 1392.0]);
        let half = row.rects(start + Duration::from_millis(104), 1.0)[2].loc.x;
        assert!((half - (2384.0 + 1392.0) / 2.0).abs() < 40.0, "{half}");
        let end = row.rects(start + Duration::from_millis(800), 1.0);
        assert!((end[2].loc.x - 1392.0).abs() < 1.0);
        assert!((end[0].loc.x - 1008.0).abs() < 1.0);
        assert!(!row.moving(start + Duration::from_millis(900), 1.0));
    }

    #[test]
    fn removing_a_space_recenters_the_row_even_the_last() {
        let output = Size::from(REFERENCE);
        let start = Instant::now();
        let mut row = Row::centered(3, output, start);
        row.remove(2, output, start, 1.0);
        assert_eq!(xs(&row.rects(start, 1.0)), [1008.0, 1200.0]);
        assert_eq!(xs(&row.targets()), [1104.0, 1296.0]);
        let end = row.rects(start + Duration::from_secs(2), 1.0);
        assert!((end[0].loc.x - 1104.0).abs() < 0.5);

        // 8 → 7, the fifth removed: the others close the gap ±96 and recenter (624 + 192 i).
        let mut row = Row::centered(8, output, start);
        let before = row.rects(start, 1.0);
        row.remove(4, output, start, 1.0);
        let end = row.rects(start + Duration::from_secs(2), 1.0);
        for (i, rect) in end.iter().enumerate() {
            assert!(
                (rect.loc.x - (624.0 + 192.0 * i as f64)).abs() < 0.5,
                "{end:?}"
            );
        }
        assert!((end[0].loc.x - before[0].loc.x - 96.0).abs() < 0.01);
        assert!((end[4].loc.x - before[5].loc.x + 96.0).abs() < 0.01);
        // Still at the start (the slide begins after the removed one is gone).
        assert_eq!(row.rects(start, 1.0)[0].loc.x, before[0].loc.x);
    }

    #[test]
    fn the_spring_matches_the_measured_halfway_points() {
        // Add: 50 % at ~104 ms with ω 0.0160; removal: 50 % at ~103 ms from its start.
        assert!((spring(104.0, ADD_OMEGA) - 0.5).abs() < 0.03);
        assert!((spring(103.0, REMOVE_OMEGA) - 0.5).abs() < 0.04);
        assert_eq!(spring(0.0, ADD_OMEGA), 0.0);
        assert!(spring(1000.0, ADD_OMEGA) > 0.999);
    }
}
