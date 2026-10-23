//! Panorama (M5.2): all the windows of the space, side by side, and the spaces bar at the
//! top (plan/reference/missioncontrol.md).
//!
//! - Windows move (and shrink only if they do not fit) until they no longer overlap: with
//!   two windows and enough room the scale stays 1, they move apart along the line through
//!   their centers and there are 16 pt between them. The rule with many windows is not
//!   measured yet (RICHIESTE-panorama.md, PA-10): if they do not fit at full size, balanced
//!   scaled rows, centered group.
//! - The wallpaper behind the windows stays exactly as at rest (no blur, dim or tint); the
//!   menu bar and the desktop icons fade out, the spaces bar comes down in the menu bar's
//!   place, 40 pt tall: Glass/Regular over the wallpaper under it (and only that; design decision,
//!   not the measured grey tone), ~60 pt shadow; the Dock stays.
//! - Opening and closing: progressive slowdown with a long tail, 50 % in ~40–54 ms, 90 % in
//!   ~130–140 ms, done in ~300 ms (tables measured frame by frame).
//! - A click on a window brings it to the front and closes Panorama; Esc or a click on the
//!   empty area close it; a click on a space of the bar switches to it; "+" adds a space.

use std::time::Instant;

use compositor_render::sprite::{Canvas, Sprite, Weight, text_width};
use silica_control::commands::{PanoramaRect, PanoramaState, PanoramaWindow};
use smithay::desktop::Window;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::state::State;

/// Compact spaces bar.
pub const BAR_HEIGHT: f64 = 40.0;
/// Shadow under the bar: ~60 pt compact, ~45 pt (and lighter) expanded (MC-BLUR).
pub const BAR_SHADOW: f64 = 60.0;
/// The bar is Glass/Regular over the wallpaper under it (`bar_region`).
/// The shadow below the bar darkens the wallpaper at its top edge: from `#626367` (compact,
/// ~20 %) and from `#6a6b6f` (expanded, ~13 %) back to `#7a7b80`. Dark: not measured.
const BAR_SHADOW_ALPHA: f64 = 0.2;
const EXPANDED_SHADOW_ALPHA: f64 = 0.13;
/// `App/Mission Control/Bar Capsule` of the Amalfi tokens (light `#87888E`, dark `#78797E`),
/// drawn at 55 % opacity under the current space's name (kit `Apps/Mission Control/Panorama`).
const CAPSULE_FILL: [u8; 4] = [0x87, 0x88, 0x8e, 0xff];
const CAPSULE_FILL_DARK: [u8; 4] = [0x78, 0x79, 0x7e, 0xff];
const CAPSULE_OPACITY: f64 = 0.55;
/// `Text/Primary` (names) and `Text/Secondary` (the "+") of the Amalfi tokens.
const LABEL_COLOR: [u8; 4] = [0x1d, 0x1e, 0x21, 0xff];
const LABEL_COLOR_DARK: [u8; 4] = [0xf4, 0xf5, 0xf7, 0xff];
const PLUS_COLOR: [u8; 4] = [0x5f, 0x63, 0x6b, 0xff];
const PLUS_COLOR_DARK: [u8; 4] = [0xb0, 0xb4, 0xbc, 0xff];
/// Labels (Inter 13 regular): the capsule of each space is 112 × 24, radius 8, 8 pt from the top
/// and 8 pt from the next one, the name centred in it; a longer name widens its capsule (12 pt
/// of margin each side).
const LABEL_SIZE: f32 = 13.0;
const CAPSULE_TOP: f64 = 8.0;
const CAPSULE_HEIGHT: f64 = 24.0;
const CAPSULE_RADIUS: f32 = 8.0;
const CAPSULE_WIDTH: f64 = 112.0;
const LABEL_PADDING: f64 = 12.0;
const LABEL_GAP: f64 = 8.0;
/// The "+" (`Regular / plus`, 20 × 20, `Text/Secondary`): centre 26 pt from the right edge (the
/// glyph box is 16 from it) and 20 from the top.
const PLUS_SIZE: f64 = 20.0;
const PLUS_RIGHT: f64 = 26.0;
const PLUS_STROKE: f32 = 1.6;

/// Gap between the arranged windows.
pub const WINDOW_GAP: f64 = 16.0;
/// The window area of Panorama (panorama.md › Window layout, PA-10): x 20…(width − 20), from
/// y 80. It is the same with the compact and the expanded bar (expanding does not re-lay out
/// the windows): the expanded bar (170 pt) covers the top of the ones laid out under it, as in
/// the reference.
const PANORAMA_SIDE: f64 = 20.0;
const PANORAMA_TOP: f64 = 80.0;
/// Side margin of the App Exposé area (provisional).
const MARGIN_SIDE: f64 = 48.0;
const MARGIN_BOTTOM: f64 = 24.0;

/// Expanded bar (pointer at the top): 168 pt tall (kit) with ~40 pt of shadow;
/// the fill goes from `#9e9fa5` to `#a4a5ab` (y 114 and below).
pub const EXPANDED_HEIGHT: f64 = 168.0;
const EXPANDED_SHADOW: f64 = 44.0;
/// Ring of the current space: 3 pt, 1 pt of bar between ring and thumbnail, sRGB `#3b99fd`.
const RING_WIDTH: f64 = 3.0;
const RING_GAP: f64 = 1.0;
const RING_COLOR: [u8; 4] = [0x3b, 0x99, 0xfd, 0xff];
/// Labels below the thumbnails: size ~12 pt, baseline 18 pt below the thumbnail.
const THUMB_LABEL_SIZE: f32 = 12.0;
/// Close button of a space (spaces.md › close button; missioncontrol.md › PA-14): white disc
/// ~22 pt centered on the thumbnail's top-left corner, gray `#878787` cross of two 2 pt
/// strokes ~8–9 pt across, soft shadow ~4 pt a little below; `#e3e3e3` while pressed.
/// It appears after 0.5 s on the thumbnail (user decision; ~1.03 s measured), at once on a
/// neighbor while one is showing, on all of them with ⌥.
const CLOSE_RADIUS: f64 = 11.0;
const CLOSE_CROSS: f64 = 4.25;
const CLOSE_FILL: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
const CLOSE_PRESSED: [u8; 4] = [0xe3, 0xe3, 0xe3, 0xff];
const CLOSE_CROSS_COLOR: [u8; 4] = [0x87, 0x87, 0x87, 0xff];
/// User decision (issue #246): 0.5 s, diverging from the measured ~1.03 s on purpose.
const CLOSE_DELAY_MS: f64 = 500.0;
/// Crossing the gap to a neighbor keeps the button showing for this long (not measured).
const CLOSE_REARM_MS: f64 = 300.0;
const THUMB_LABEL_BASELINE: f64 = 18.0;
/// The "+" comes down with the bar: center at y 20 compact, 98 expanded (kit: y 88 + 10).
const PLUS_Y: f64 = 20.0;
const PLUS_Y_EXPANDED: f64 = 98.0;
/// The thumbnails appear halfway through the expansion (in the video between 63 % and 80 %).
const THUMBS_FROM: f64 = 0.6;
const THUMBS_FULL: f64 = 0.8;
/// Beyond this distance from the press, a click on a window becomes a drag.
const DRAG_THRESHOLD: f64 = 4.0;
/// Climbing toward the bar the dragged window shrinks linearly with height: ~1.65 pt of width
/// per pt climbed (spaces.md, PA-13; −0.0027 of scale per pt for a 600 pt window), and grows
/// back the same way going down.
const DRAG_SHRINK_PER_PT: f64 = 1.65;
/// Over the bar it is a miniature of about 0.066–0.073 of the window's real size, following the
/// pointer about 55 % down it.
const DRAG_OVER_BAR_FACTOR: f64 = 0.07;
const DRAG_OVER_BAR_GRAB_Y: f64 = 0.55;

/// Bar expansion (ms, 0–1): height from 40 to 169 pt in the video, frame by frame. Closing is
/// not measured: same curve.
pub const EXPAND_CURVE: &[(f64, f64)] = &[
    (0.0, 0.0),
    (13.0, 0.29),
    (26.0, 0.37),
    (53.0, 0.52),
    (66.0, 0.59),
    (80.0, 0.64),
    (106.0, 0.74),
    (120.0, 0.78),
    (133.0, 0.81),
    (146.0, 0.87),
    (173.0, 0.91),
    (200.0, 0.92),
    (226.0, 0.95),
    (280.0, 0.98),
    (346.0, 0.99),
    (440.0, 1.0),
];

/// Opening progress (ms, 0–1): movement of the Terminal in the video.
pub const OPEN_CURVE: &[(f64, f64)] = &[
    (0.0, 0.0),
    (13.0, 0.26),
    (27.0, 0.38),
    (67.0, 0.59),
    (93.0, 0.74),
    (120.0, 0.84),
    (133.0, 0.91),
    (173.0, 0.95),
    (227.0, 0.98),
    (320.0, 1.0),
];

/// Closing progress (ms, 0–1): edge of the file manager window in the burst after Esc.
pub const CLOSE_CURVE: &[(f64, f64)] = &[
    (0.0, 0.0),
    (13.0, 0.054),
    (31.0, 0.269),
    (52.0, 0.389),
    (67.0, 0.497),
    (79.0, 0.599),
    (93.0, 0.683),
    (106.0, 0.749),
    (118.0, 0.802),
    (132.0, 0.850),
    (145.0, 0.880),
    (157.0, 0.910),
    (171.0, 0.934),
    (184.0, 0.946),
    (199.0, 0.958),
    (211.0, 0.964),
    (224.0, 0.976),
    (236.0, 0.988),
    (291.0, 0.994),
    (319.0, 1.0),
];

/// App Exposé, opening (ms, 0–1): top edge of the window from y 300 to 388 in the video.
pub const APP_OPEN_CURVE: &[(f64, f64)] = &[
    (0.0, 0.0),
    (6.0, 0.05),
    (20.0, 0.27),
    (33.0, 0.39),
    (60.0, 0.50),
    (73.0, 0.68),
    (86.0, 0.75),
    (113.0, 0.81),
    (126.0, 0.85),
    (140.0, 0.91),
    (166.0, 0.93),
    (180.0, 0.95),
    (193.0, 0.98),
    (233.0, 0.99),
    (273.0, 1.0),
];

/// App Exposé, closing (ms, 0–1): from y 388 to 300 after Esc.
pub const APP_CLOSE_CURVE: &[(f64, f64)] = &[
    (0.0, 0.0),
    (13.0, 0.045),
    (26.0, 0.26),
    (40.0, 0.39),
    (53.0, 0.50),
    (80.0, 0.67),
    (93.0, 0.74),
    (120.0, 0.84),
    (133.0, 0.875),
    (160.0, 0.93),
    (173.0, 0.94),
    (200.0, 0.955),
    (213.0, 0.966),
    (253.0, 0.977),
    (266.0, 0.989),
    (320.0, 1.0),
];

/// App Exposé: the app windows in the middle, each with its title below. With one 900 × 600
/// window on the reference screen the scale is 0.947 and the center is at y 671.5: area from
/// 48 pt from the top to 61 pt above the Dock (room for the label). The rule with more windows
/// is not measured.
const APP_MARGIN_TOP: f64 = 48.0;
const APP_MARGIN_BOTTOM: f64 = 61.0;
const APP_MAX_SCALE: f64 = 0.947;
/// Label: size ~12 pt, white, capitals 14 pt below the window.
pub const APP_LABEL_SIZE: f32 = 12.0;
pub const APP_LABEL_GAP: f64 = 14.0;
/// At the top the wallpaper darkens: from `#6f7075` at y 0 to `#7a7b80` at y 102.
pub const APP_SHADE_HEIGHT: f64 = 102.0;
pub const APP_SHADE_ALPHA: f32 = 0.09;
/// Show Desktop (not measured, PA-31): the windows go out towards the nearest edge and a
/// strip of them stays visible.
const DESKTOP_PEEK: f64 = 24.0;

/// Value of a (time, progress) table at instant `ms`, interpolated.
pub fn curve_at(table: &[(f64, f64)], ms: f64) -> f64 {
    let mut previous = (0.0, 0.0);
    for &(t, p) in table {
        if ms <= t {
            let span = t - previous.0;
            if span <= 0.0 {
                return p;
            }
            return previous.1 + (p - previous.1) * (ms - previous.0) / span;
        }
        previous = (t, p);
    }
    1.0
}

/// Arranges the frames in `area` without overlapping them. If there is room at full size,
/// the windows move apart in pairs along the line through their centers ([`spread`]), as in
/// the reference; otherwise scaled rows ([`grid`]).
pub fn layout(
    frames: &[Rectangle<f64, Logical>],
    area: Rectangle<f64, Logical>,
) -> Vec<Rectangle<f64, Logical>> {
    if frames.is_empty() || area.size.w <= 0.0 || area.size.h <= 0.0 {
        return Vec::new();
    }
    if let Some(spread) = spread(frames, area) {
        return spread;
    }
    grid(frames, area)
}

fn center(r: &Rectangle<f64, Logical>) -> (f64, f64) {
    (r.loc.x + r.size.w / 2.0, r.loc.y + r.size.h / 2.0)
}

/// Overlapping windows (counting the 16 pt gap) move apart in equal parts along the line
/// through their centers, by as much as needed to separate them on one of the two axes. In
/// the reference: a file manager window (700,300 900 × 600) and a terminal (1283,308 597 × 422) move by
/// (−167, +32) and (+166, −32). Then the group comes back into the area, if it goes out. `None`
/// if it does not fit at full size.
fn spread(
    frames: &[Rectangle<f64, Logical>],
    area: Rectangle<f64, Logical>,
) -> Option<Vec<Rectangle<f64, Logical>>> {
    let mut out = frames.to_vec();
    let n = out.len();
    let mut settled = false;
    for _ in 0..64 {
        settled = true;
        for i in 0..n {
            for j in (i + 1)..n {
                let (Some(&a), Some(&b)) = (out.get(i), out.get(j)) else {
                    continue;
                };
                let (ca, cb) = (center(&a), center(&b));
                let (mut dx, mut dy) = (cb.0 - ca.0, cb.1 - ca.1);
                let need_x = (a.size.w + b.size.w) / 2.0 + WINDOW_GAP - dx.abs();
                let need_y = (a.size.h + b.size.h) / 2.0 + WINDOW_GAP - dy.abs();
                if need_x <= 1e-6 || need_y <= 1e-6 {
                    continue;
                }
                settled = false;
                // Coinciding centers: the second one to the right.
                if dx.abs() + dy.abs() < 1e-6 {
                    (dx, dy) = (1.0, 0.0);
                }
                let length = dx.hypot(dy);
                let (ux, uy) = (dx / length, dy / length);
                let along_x = if ux.abs() > 1e-6 {
                    need_x / ux.abs()
                } else {
                    f64::INFINITY
                };
                let along_y = if uy.abs() > 1e-6 {
                    need_y / uy.abs()
                } else {
                    f64::INFINITY
                };
                let half = along_x.min(along_y) / 2.0;
                if let Some(r) = out.get_mut(i) {
                    r.loc -= Point::from((ux * half, uy * half));
                }
                if let Some(r) = out.get_mut(j) {
                    r.loc += Point::from((ux * half, uy * half));
                }
            }
        }
        if settled {
            break;
        }
    }
    if !settled {
        return None;
    }
    let bounds = out.iter().copied().reduce(|a, b| a.merge(b))?;
    if bounds.size.w > area.size.w || bounds.size.h > area.size.h {
        return None;
    }
    let shift_x = (area.loc.x - bounds.loc.x).max(0.0)
        + (area.loc.x + area.size.w - (bounds.loc.x + bounds.size.w)).min(0.0);
    let shift_y = (area.loc.y - bounds.loc.y).max(0.0)
        + (area.loc.y + area.size.h - (bounds.loc.y + bounds.size.h)).min(0.0);
    for r in &mut out {
        r.loc += Point::from((shift_x, shift_y));
    }
    Some(out)
}

/// Rows balanced in vertical order, every row in horizontal order, a common scale at most 1
/// (the largest among the row splits), rows and group centered.
fn grid(
    frames: &[Rectangle<f64, Logical>],
    area: Rectangle<f64, Logical>,
) -> Vec<Rectangle<f64, Logical>> {
    grid_scaled(frames, area, 1.0)
}

/// Like [`grid`], with the scale at most `max_scale`.
fn grid_scaled(
    frames: &[Rectangle<f64, Logical>],
    area: Rectangle<f64, Logical>,
    max_scale: f64,
) -> Vec<Rectangle<f64, Logical>> {
    let n = frames.len();
    let at = |i: usize| frames.get(i).copied().unwrap_or_default();
    let mut by_y: Vec<usize> = (0..n).collect();
    by_y.sort_by(|&a, &b| {
        let ca = at(a).loc.y + at(a).size.h / 2.0;
        let cb = at(b).loc.y + at(b).size.h / 2.0;
        ca.total_cmp(&cb)
    });
    let mut best: Option<(f64, Vec<Vec<usize>>)> = None;
    for rows in 1..=n {
        let mut grouped: Vec<Vec<usize>> = Vec::with_capacity(rows);
        for r in 0..rows {
            let from = r * n / rows;
            let to = (r + 1) * n / rows;
            let mut row: Vec<usize> = by_y.get(from..to).unwrap_or_default().to_vec();
            row.sort_by(|&a, &b| at(a).loc.x.total_cmp(&at(b).loc.x));
            grouped.push(row);
        }
        // The gaps between windows are not scaled: they stay 16 pt.
        let width_scale = grouped
            .iter()
            .map(|row| {
                let gaps = WINDOW_GAP * (row.len().saturating_sub(1)) as f64;
                let widths = row.iter().map(|&i| at(i).size.w).sum::<f64>();
                (area.size.w - gaps) / widths.max(1.0)
            })
            .fold(f64::INFINITY, f64::min);
        let heights = grouped
            .iter()
            .map(|row| row.iter().map(|&i| at(i).size.h).fold(0.0, f64::max))
            .sum::<f64>();
        let height_scale = (area.size.h - WINDOW_GAP * (rows - 1) as f64) / heights.max(1.0);
        let scale = max_scale.min(width_scale).min(height_scale).max(0.05);
        if best.as_ref().is_none_or(|(s, _)| scale > *s + 1e-6) {
            best = Some((scale, grouped));
        }
    }
    let Some((scale, grouped)) = best else {
        return Vec::new();
    };
    let mut out = frames.to_vec();
    let heights: Vec<f64> = grouped
        .iter()
        .map(|row| {
            row.iter()
                .map(|&i| at(i).size.h * scale)
                .fold(0.0, f64::max)
        })
        .collect();
    let total = heights.iter().sum::<f64>() + WINDOW_GAP * (grouped.len() - 1) as f64;
    let mut y = area.loc.y + (area.size.h - total) / 2.0;
    for (row, height) in grouped.iter().zip(&heights) {
        let width = row.iter().map(|&i| at(i).size.w * scale).sum::<f64>()
            + WINDOW_GAP * (row.len().saturating_sub(1)) as f64;
        let mut x = area.loc.x + (area.size.w - width) / 2.0;
        for &i in row {
            let size = at(i).size.upscale(scale);
            if let Some(slot) = out.get_mut(i) {
                *slot = Rectangle::new((x, y + (height - size.h) / 2.0).into(), size);
            }
            x += size.w + WINDOW_GAP;
        }
        y += height + WINDOW_GAP;
    }
    out
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

/// Show Desktop: the frame pushed past the screen edge nearest to its center, with a strip of
/// `DESKTOP_PEEK` points still inside.
fn out_of_screen(
    frame: Rectangle<f64, Logical>,
    output: Rectangle<f64, Logical>,
) -> Rectangle<f64, Logical> {
    let (cx, cy) = center(&frame);
    let left = cx - output.loc.x;
    let right = output.loc.x + output.size.w - cx;
    let top = cy - output.loc.y;
    let bottom = output.loc.y + output.size.h - cy;
    let nearest = left.min(right).min(top).min(bottom);
    let mut out = frame;
    if nearest == left {
        out.loc.x = output.loc.x + DESKTOP_PEEK - frame.size.w;
    } else if nearest == right {
        out.loc.x = output.loc.x + output.size.w - DESKTOP_PEEK;
    } else if nearest == top {
        out.loc.y = output.loc.y + DESKTOP_PEEK - frame.size.h;
    } else {
        out.loc.y = output.loc.y + output.size.h - DESKTOP_PEEK;
    }
    out
}

/// A window in Panorama: the real frame and the arranged one.
#[derive(Clone)]
pub struct Placement {
    pub window: Window,
    pub from: Rectangle<f64, Logical>,
    pub to: Rectangle<f64, Logical>,
}

/// A window grabbed with the pointer: a click if it does not move, a drag otherwise.
#[derive(Clone)]
pub struct Drag {
    pub window: Window,
    /// Grab point inside the arranged frame, as a proportion (0–1).
    pub grab: (f64, f64),
    pub press: Point<f64, Logical>,
    pub pointer: Point<f64, Logical>,
    pub moved: bool,
}

/// The three views that move the windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// All the windows of the space, with the spaces bar.
    Panorama,
    /// The windows of an app (App Exposé).
    App,
    /// The windows off screen: the desktop shows.
    Desktop,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Panorama => "panorama",
            Mode::App => "app",
            Mode::Desktop => "desktop",
        }
    }
}

/// A dragged window dropped away from a thumbnail: it goes back to its place (BACKLOG-1 /
/// #74: ~500 ms, ease-out, no overshoot).
#[derive(Clone)]
pub struct Returning {
    pub window: Window,
    /// Where it was let go.
    pub from: Rectangle<f64, Logical>,
    pub start: Instant,
    /// Fraction of the way, eased (0–1); updated on every frame.
    pub fraction: f64,
}

/// The rectangle `fraction` of the way from `from` to `to`.
pub fn lerp_rect(
    from: Rectangle<f64, Logical>,
    to: Rectangle<f64, Logical>,
    fraction: f64,
) -> Rectangle<f64, Logical> {
    let lerp = |a: f64, b: f64| a + (b - a) * fraction;
    Rectangle::new(
        (lerp(from.loc.x, to.loc.x), lerp(from.loc.y, to.loc.y)).into(),
        (lerp(from.size.w, to.size.w), lerp(from.size.h, to.size.h)).into(),
    )
}

/// The eased fraction of the way back after `elapsed`, durations scaled by `speed`.
pub fn return_fraction(elapsed: std::time::Duration, speed: f64) -> f64 {
    compositor_render::animation::ease_out(compositor_render::animation::progress(
        elapsed,
        compositor_render::animation::PANORAMA_RETURN,
        speed,
    ))
}

pub struct Panorama {
    pub mode: Mode,
    pub opening: bool,
    pub start: Instant,
    /// Progress the phase restarts from (an Esc halfway through opening does not jump).
    pub from: f64,
    pub progress: f64,
    pub placements: Vec<Placement>,
    /// Spaces bar expanded (or expanding), with its animation.
    pub expanded: bool,
    pub expand_start: Instant,
    pub expand_from: f64,
    pub expansion: f64,
    pub drag: Option<Drag>,
    /// The window going back to its place after a drop that changed nothing.
    pub returning: Option<Returning>,
    /// The space thumbnails, laid out when the bar expands.
    pub row: Option<crate::thumbnail_row::Row>,
    /// The thumbnail under the pointer, for its close button.
    pub hover: Option<Hover>,
    /// Where the pointer is (for the ring and the title of the window under it).
    pub pointer: Option<Point<f64, Logical>>,
    /// A close button just left with its button showing: a neighbor shows its own at once.
    pub rearm_until: Option<Instant>,
    /// The close button pressed (the space goes on release over it).
    pub close_press: Option<usize>,
    /// The "+" pressed (the space is added on release over it).
    pub plus_press: bool,
    /// The ghost thumbnail sliding in while the pointer is on the "+" (spaces.md › Adding a
    /// space); `None` before the first hover.
    pub ghost: Option<Ghost>,
    /// The space being magnified out of its thumbnail as Panorama closes, and the thumbnail.
    pub zoom: Option<(usize, Rectangle<f64, Logical>)>,
    /// The time of the last frame.
    pub clock: Instant,
}

/// The ghost thumbnail of the "+": its target (in or out), since when, and where it started.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ghost {
    pub shown: bool,
    pub since: Instant,
    pub from: f64,
}

/// The ghost's slide: a critically damped spring (ω ≈ 16.4 s⁻¹, spaces.md › Ghost entry).
const GHOST_OMEGA: f64 = 0.0164;
/// Its left edge, this far from the screen's right edge once in.
const GHOST_LEFT: f64 = 80.0;
/// Pressed, it darkens to about 68 % brightness.
const GHOST_PRESSED: f64 = 0.68;

impl Ghost {
    /// How far in, 0 (out of the screen) to 1, at `now`; `speed` is the animation scale (0:
    /// at once).
    pub fn value(&self, now: Instant, speed: f64) -> f64 {
        let target = if self.shown { 1.0 } else { 0.0 };
        if speed <= 0.0 {
            return target;
        }
        let ms = now.saturating_duration_since(self.since).as_secs_f64() * 1000.0 / speed;
        let t = crate::thumbnail_row::spring(ms, GHOST_OMEGA);
        self.from + (target - self.from) * t
    }
}

/// The ghost's rectangle in bar coordinates for `value` (0–1): the size of a thumbnail, cut by
/// the screen's right edge, its left edge sliding in to [`GHOST_LEFT`] from it, centred on the
/// "+" (so the cross sits at its vertical centre).
pub fn ghost_rect(width: f64, height: f64, expansion: f64, value: f64) -> Rectangle<f64, Logical> {
    let plus = plus_rect(width, expansion);
    let center = plus.loc.y + plus.size.h / 2.0;
    let thumb = crate::thumbnail_row::THUMB_HEIGHT;
    let left = width - GHOST_LEFT * value.clamp(0.0, 1.0);
    let thumb_width = if height > 0.0 {
        thumb * width / height
    } else {
        thumb * 16.0 / 9.0
    };
    Rectangle::new(
        (left, center - thumb / 2.0).into(),
        ((width - left).min(thumb_width), thumb).into(),
    )
}

/// The thumbnail under the pointer and since when.
#[derive(Clone, Copy, Debug)]
pub struct Hover {
    pub index: usize,
    pub since: Instant,
    /// Shown at once (coming from a neighbor that showed its button).
    pub at_once: bool,
}

/// A space close button in the frame: its center and whether it is pressed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloseButton {
    pub center: Point<f64, Logical>,
    pub pressed: bool,
}

/// A space thumbnail in the frame: where it is and the windows of the space (from bottom to
/// top, at their positions on the desktop).
#[derive(Clone)]
pub struct Thumbnail {
    pub rect: Rectangle<f64, Logical>,
    pub windows: Vec<(Window, Point<i32, Logical>)>,
}

/// What the frame needs to know about Panorama.
#[derive(Clone)]
pub struct PanoramaView {
    pub mode: Mode,
    pub progress: f64,
    pub placements: Vec<Placement>,
    pub expansion: f64,
    /// The thumbnails, only when visible.
    pub thumbnails: Vec<Thumbnail>,
    /// Where the thumbnails are in the bar (bar coordinates, also when not visible).
    pub thumbnail_rects: Vec<Rectangle<f64, Logical>>,
    /// Whether the "+" shows (not at the maximum of spaces).
    pub plus: bool,
    /// How far in the ghost thumbnail of the "+" is, 0–1, and whether the "+" is pressed.
    pub ghost: f64,
    pub ghost_pressed: bool,
    /// The close buttons showing.
    pub close_buttons: Vec<CloseButton>,
    /// The space magnified out of its thumbnail (closing after a click on it): its index and
    /// where its desktop is now.
    pub zoom: Option<(usize, Rectangle<f64, Logical>)>,
    /// The dragged window and its frame at this moment.
    pub drag: Option<(Window, Rectangle<f64, Logical>)>,
    /// The label of each space.
    pub labels: Vec<String>,
    /// The window under the pointer: where it is now and its title (Panorama only).
    pub hover_window: Option<HoverWindow>,
}

/// The ring and the title capsule of the window under the pointer (kit: `Hover ring`, `Hover
/// title`).
#[derive(Clone, Debug, PartialEq)]
pub struct HoverWindow {
    pub rect: Rectangle<f64, Logical>,
    /// The title in the capsule; none in App Windows (the title is under each window there).
    pub title: Option<String>,
    pub dark: bool,
}

/// An entry of the spaces bar, in points in the output.
struct BarItem {
    index: usize,
    label: String,
    rect: Rectangle<f64, Logical>,
}

/// The label of a space in the spaces bar, in the system language (English source, Italian
/// translation; `LANGUAGE`, then `LC_ALL`, `LC_MESSAGES`, `LANG`).
fn space_label(index: usize) -> String {
    space_label_in(index, &system_language(|key| std::env::var(key).ok()))
}

/// The label of the `index`-th desktop (counting desktops only).
pub fn desktop_label(index: usize) -> String {
    space_label(index)
}

/// The label of a full-screen space: the window title, or its app id without the reverse
/// domain when there is no title.
pub fn window_label(window: &Window) -> String {
    use smithay::wayland::compositor::with_states;
    use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
    let (title, app_id) = window
        .toplevel()
        .map(|toplevel| {
            with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok())
                    .map(|data| (data.title.clone(), data.app_id.clone()))
                    .unwrap_or_default()
            })
        })
        .unwrap_or_default();
    title
        .filter(|t| !t.is_empty())
        .or_else(|| app_id.and_then(|id| id.rsplit('.').next().map(str::to_owned)))
        .unwrap_or_default()
}

/// The label of the only desktop: without a number.
pub fn lone_desktop_label() -> String {
    lone_label_in(&system_language(|key| std::env::var(key).ok()))
}

fn lone_label_in(language: &str) -> String {
    match language {
        "it" => "Spazio".to_owned(),
        _ => "Space".to_owned(),
    }
}

fn space_label_in(index: usize, language: &str) -> String {
    let n = index.saturating_add(1);
    match language {
        "it" => format!("Spazio {n}"),
        _ => format!("Space {n}"),
    }
}

/// The first language code of the environment (`it_IT.UTF-8` → `it`); `en` if unknown.
fn system_language(get: impl Fn(&str) -> Option<String>) -> String {
    let single = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| get(key).filter(|v| !v.is_empty()));
    let c_locale = single.as_deref().is_some_and(|v| v == "C" || v == "POSIX");
    let first = (!c_locale)
        .then(|| get("LANGUAGE"))
        .flatten()
        .and_then(|list| list.split(':').find(|s| !s.is_empty()).map(str::to_owned))
        .or(single);
    first
        .as_deref()
        .and_then(|name| name.split(['_', '-', '.', '@']).next())
        .filter(|code| !code.is_empty() && code.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_ascii_lowercase)
        .filter(|code| code != "c" && code != "posix")
        .unwrap_or_else(|| "en".to_owned())
}

fn bar_items(labels: &[String], width: f64) -> Vec<BarItem> {
    let mut items: Vec<BarItem> = (0..labels.len())
        .map(|index| {
            let label = labels.get(index).cloned().unwrap_or_default();
            let w = (f64::from(text_width(&label, LABEL_SIZE, Weight::Regular))
                + 2.0 * LABEL_PADDING)
                .max(CAPSULE_WIDTH);
            BarItem {
                index,
                label,
                rect: Rectangle::new((0.0, CAPSULE_TOP).into(), (w, CAPSULE_HEIGHT).into()),
            }
        })
        .collect();
    let total = items.iter().map(|i| i.rect.size.w).sum::<f64>()
        + LABEL_GAP * labels.len().saturating_sub(1) as f64;
    let mut x = (width - total) / 2.0;
    for item in &mut items {
        item.rect.loc.x = x;
        x += item.rect.size.w + LABEL_GAP;
    }
    items
}

fn plus_rect(width: f64, expansion: f64) -> Rectangle<f64, Logical> {
    let y = PLUS_Y + (PLUS_Y_EXPANDED - PLUS_Y) * expansion;
    Rectangle::new(
        (width - PLUS_RIGHT - 16.0, y - 16.0).into(),
        (32.0, 32.0).into(),
    )
}

/// Bar height for an expansion 0–1.
pub fn bar_height(expansion: f64) -> f64 {
    BAR_HEIGHT + (EXPANDED_HEIGHT - BAR_HEIGHT) * expansion.clamp(0.0, 1.0)
}

/// The frame at this moment of the animation.
pub fn interpolate(p: &Placement, progress: f64) -> Rectangle<f64, Logical> {
    let lerp = |a: f64, b: f64| a + (b - a) * progress;
    Rectangle::new(
        (
            lerp(p.from.loc.x, p.to.loc.x),
            lerp(p.from.loc.y, p.to.loc.y),
        )
            .into(),
        (
            lerp(p.from.size.w, p.to.size.w),
            lerp(p.from.size.h, p.to.size.h),
        )
            .into(),
    )
}

/// The frame of the dragged window: it follows the pointer from the grab point; over the
/// bar it shrinks.
/// Where the dragged window is drawn: its arranged `frame` scaled down as it climbs from the
/// press toward the bar, a miniature of its `real` width over the bar (spaces.md, PA-13).
fn drag_rect(
    drag: &Drag,
    frame: Rectangle<f64, Logical>,
    real_width: f64,
    bar: f64,
) -> Rectangle<f64, Logical> {
    drag_geometry(drag.press, drag.pointer, drag.grab, frame, real_width, bar)
}

/// [`drag_rect`] from the press and the pointer, the grab point (proportions of the frame).
fn drag_geometry(
    press: Point<f64, Logical>,
    pointer: Point<f64, Logical>,
    grab: (f64, f64),
    frame: Rectangle<f64, Logical>,
    real_width: f64,
    bar: f64,
) -> Rectangle<f64, Logical> {
    let full = frame.size.w.max(1.0);
    let mini = (real_width * DRAG_OVER_BAR_FACTOR).clamp(1.0, full);
    let over_bar = pointer.y < bar;
    let width = if over_bar {
        mini
    } else {
        let climbed = (press.y - pointer.y).max(0.0);
        (full - DRAG_SHRINK_PER_PT * climbed).max(mini)
    };
    let size = frame.size.upscale(width / full);
    let grab_y = if over_bar {
        DRAG_OVER_BAR_GRAB_Y
    } else {
        grab.1
    };
    Rectangle::new(
        (pointer.x - grab.0 * size.w, pointer.y - grab_y * size.h).into(),
        size,
    )
}

impl State {
    pub fn panorama_open(&self) -> bool {
        self.panorama.as_ref().is_some_and(|p| p.opening)
    }

    fn output_size(&self) -> Size<f64, Logical> {
        self.space
            .output_geometry(&self.output)
            .map_or_else(Size::default, |geo| geo.size.to_f64())
    }

    /// The open view (not closing).
    pub fn panorama_mode(&self) -> Option<Mode> {
        self.panorama.as_ref().filter(|p| p.opening).map(|p| p.mode)
    }

    /// Opens Panorama, or closes it if it is open.
    pub fn toggle_panorama(&mut self, now: Instant) {
        self.toggle_view(Mode::Panorama, now);
    }

    /// Opens a view; if it is already open it closes it, if another one is open it switches to this.
    pub fn toggle_view(&mut self, mode: Mode, now: Instant) {
        self.dismiss_transients();
        if self.panorama_mode() == Some(mode) {
            self.close_panorama(now, None);
        } else {
            self.open_view(mode, now);
        }
    }

    /// The windows of the space (for App Exposé only those of the frontmost app) with the
    /// real frame and the arranged one.
    fn panorama_placements(&self, mode: Mode) -> Vec<Placement> {
        let mut windows: Vec<Window> = self.space.elements().cloned().collect();
        if mode == Mode::App {
            let Some(focused) = self.focused_window() else {
                return Vec::new();
            };
            let app = window_app(&focused);
            windows.retain(|w| *w == focused || (app.is_some() && window_app(w) == app));
        }
        let frames: Vec<Rectangle<f64, Logical>> = windows
            .iter()
            .filter_map(|w| {
                let geometry = self.space.element_geometry(w)?;
                Some(crate::decorations::frame_of(w, geometry).to_f64())
            })
            .collect();
        if frames.len() != windows.len() {
            return Vec::new();
        }
        let output = self
            .space
            .output_geometry(&self.output)
            .unwrap_or_default()
            .to_f64();
        let usable = self.usable_area().to_f64();
        let targets = match mode {
            Mode::Panorama => {
                let top = output.loc.y + PANORAMA_TOP;
                let bottom = usable.loc.y + usable.size.h - MARGIN_BOTTOM;
                let area = Rectangle::new(
                    (output.loc.x + PANORAMA_SIDE, top).into(),
                    (output.size.w - 2.0 * PANORAMA_SIDE, (bottom - top).max(1.0)).into(),
                );
                layout(&frames, area)
            }
            Mode::App => {
                let top = output.loc.y + APP_MARGIN_TOP;
                let bottom = usable.loc.y + usable.size.h - APP_MARGIN_BOTTOM;
                let area = Rectangle::new(
                    (output.loc.x + MARGIN_SIDE, top).into(),
                    (output.size.w - 2.0 * MARGIN_SIDE, (bottom - top).max(1.0)).into(),
                );
                if frames.is_empty() {
                    Vec::new()
                } else {
                    grid_scaled(&frames, area, APP_MAX_SCALE)
                }
            }
            Mode::Desktop => frames.iter().map(|f| out_of_screen(*f, output)).collect(),
        };
        windows
            .into_iter()
            .zip(frames)
            .zip(targets)
            .map(|((window, from), to)| Placement { window, from, to })
            .collect()
    }

    fn open_view(&mut self, mode: Mode, now: Instant) {
        self.settle_transition();
        // From another view it restarts from zero (the windows go back to their place at once).
        let from = self
            .panorama
            .as_ref()
            .filter(|p| p.mode == mode)
            .map_or(0.0, |p| p.progress);
        let placements = self.panorama_placements(mode);
        self.titlebar_hover = None;
        self.panorama = Some(Panorama {
            mode,
            opening: true,
            start: now,
            from,
            progress: from,
            placements,
            expanded: false,
            expand_start: now,
            expand_from: 0.0,
            expansion: 0.0,
            drag: None,
            returning: None,
            row: None,
            hover: None,
            pointer: None,
            rearm_until: None,
            close_press: None,
            plus_press: false,
            ghost: None,
            zoom: None,
            clock: now,
        });
        if self.animation_speed <= 0.0 {
            self.advance_panorama(now);
        }
    }

    /// A click on a space thumbnail: that space becomes the current one at once (no slide) and
    /// Panorama closes magnifying its thumbnail to the whole screen: its windows grow from
    /// their place in the thumbnail to their place on the desktop. The curve is Panorama's
    /// closing one (the magnification is not measured).
    pub fn zoom_into_space(&mut self, index: usize, now: Instant) {
        let Some(rect) = self.thumbnail_rects_now().get(index).copied() else {
            return;
        };
        if index != self.workspaces.active {
            self.switch_space(index, now);
            self.settle_transition();
        }
        let output = self
            .space
            .output_geometry(&self.output)
            .unwrap_or_default()
            .to_f64();
        let s = rect.size.w / output.size.w.max(1.0);
        let placements: Vec<Placement> = self
            .space
            .elements()
            .filter_map(|window| {
                let geometry = self.space.element_geometry(window)?;
                let from = crate::decorations::frame_of(window, geometry).to_f64();
                let to = Rectangle::new(
                    rect.loc + (from.loc - output.loc).upscale(s),
                    from.size.upscale(s),
                );
                Some(Placement {
                    window: window.clone(),
                    from,
                    to,
                })
            })
            .collect();
        if let Some(panorama) = self.panorama.as_mut() {
            panorama.placements = placements;
            panorama.zoom = Some((index, rect));
            panorama.hover = None;
        }
        self.close_panorama(now, None);
    }

    /// Closes Panorama; with `focus` it brings that window to the front.
    pub fn close_panorama(&mut self, now: Instant, focus: Option<Window>) {
        let Some(panorama) = self.panorama.as_mut() else {
            return;
        };
        if panorama.opening {
            panorama.opening = false;
            panorama.from = panorama.progress;
            panorama.start = now;
            panorama.drag = None;
            panorama.returning = None;
        }
        if let Some(window) = focus {
            self.focus_window(Some(window));
        }
        if self.animation_speed <= 0.0 {
            self.advance_panorama(now);
        }
    }

    /// Expands or collapses the spaces bar.
    pub fn expand_spaces_bar(&mut self, expanded: bool, now: Instant) {
        let output = self.output_size();
        let Some(panorama) = self.panorama.as_mut() else {
            return;
        };
        if panorama.expanded == expanded || !panorama.opening || panorama.mode != Mode::Panorama {
            return;
        }
        panorama.expanded = expanded;
        panorama.expand_from = panorama.expansion;
        panorama.expand_start = now;
        // The row is recentered when the bar expands (spaces.md › Thumbnail layout).
        if expanded && panorama.expansion < THUMBS_FROM {
            panorama.row = Some(crate::thumbnail_row::Row::centered(
                self.workspaces.count(),
                output,
                now,
            ));
        }
        if self.animation_speed <= 0.0 {
            self.advance_panorama(now);
        }
    }

    /// Advances the animations (on every frame); once fully closed, Panorama disappears.
    pub fn advance_panorama(&mut self, now: Instant) {
        let speed = self.animation_speed;
        let count = self.workspaces.count();
        let output = self.output_size();
        let Some(panorama) = self.panorama.as_mut() else {
            return;
        };
        panorama.clock = now;
        // Spaces added or removed elsewhere (commands, full screen): added ones slide in,
        // removed ones (index unknown) leave from the end.
        if let Some(row) = panorama.row.as_mut() {
            while row.len() < count {
                row.add(output, now, speed);
            }
            while row.len() > count {
                row.remove(row.len() - 1, output, now, speed);
            }
        }
        let elapsed = |start: Instant| {
            let ms = now.saturating_duration_since(start).as_secs_f64() * 1000.0;
            if speed <= 0.0 { f64::MAX } else { ms / speed }
        };
        let ms = elapsed(panorama.start);
        let (open, close) = match panorama.mode {
            Mode::App => (APP_OPEN_CURVE, APP_CLOSE_CURVE),
            Mode::Panorama | Mode::Desktop => (OPEN_CURVE, CLOSE_CURVE),
        };
        panorama.progress = if panorama.opening {
            panorama.from + (1.0 - panorama.from) * curve_at(open, ms)
        } else {
            panorama.from * (1.0 - curve_at(close, ms))
        };
        let step = curve_at(EXPAND_CURVE, elapsed(panorama.expand_start));
        panorama.expansion = if panorama.expanded {
            panorama.expand_from + (1.0 - panorama.expand_from) * step
        } else {
            panorama.expand_from * (1.0 - step)
        };
        if let Some(back) = panorama.returning.as_mut() {
            back.fraction = return_fraction(now.saturating_duration_since(back.start), speed);
            if back.fraction >= 1.0 {
                panorama.returning = None;
            }
        }
        if !panorama.opening && panorama.progress <= 0.0 {
            self.panorama = None;
            return;
        }
        self.refresh_placements();
    }

    /// A window that maps, closes or changes size while a view is open is arranged again with
    /// the others: arranged with no size (a window still drawing its first frame when the view
    /// opened) or left out, it would be drawn at its real place, under the spaces bar
    /// (silica-os/silica-desktop#246). Not while a window is dragged, returning or a space magnifying.
    fn refresh_placements(&mut self) {
        let Some(panorama) = self.panorama.as_ref() else {
            return;
        };
        if !panorama.opening
            || panorama.drag.is_some()
            || panorama.returning.is_some()
            || panorama.zoom.is_some()
        {
            return;
        }
        let mode = panorama.mode;
        let current: Vec<(Window, Option<Rectangle<f64, Logical>>)> = self
            .space
            .elements()
            .map(|w| {
                let frame = self
                    .space
                    .element_geometry(w)
                    .map(|g| crate::decorations::frame_of(w, g).to_f64());
                (w.clone(), frame)
            })
            .collect();
        let known = |window: &Window| panorama.placements.iter().find(|p| &p.window == window);
        let changed = match mode {
            // App Exposé arranges only some windows: a placement whose window changed or went.
            Mode::App => panorama.placements.iter().any(|p| {
                current
                    .iter()
                    .find(|(w, _)| *w == p.window)
                    .is_none_or(|(_, frame)| *frame != Some(p.from))
            }),
            Mode::Panorama | Mode::Desktop => {
                current.len() != panorama.placements.len()
                    || current
                        .iter()
                        .any(|(window, frame)| known(window).is_none_or(|p| *frame != Some(p.from)))
            }
        };
        if !changed {
            return;
        }
        let fresh = self.panorama_placements(mode);
        if let Some(panorama) = self.panorama.as_mut() {
            panorama.placements = fresh;
        }
    }

    /// The dragged window is let go away from any thumbnail: it goes back to its place.
    fn start_return(&mut self, drag: &Drag, now: Instant) {
        if self.animation_speed <= 0.0 {
            return;
        }
        let Some(panorama) = self.panorama.as_mut() else {
            return;
        };
        let Some(placement) = panorama.placements.iter().find(|p| p.window == drag.window) else {
            return;
        };
        let from = drag_rect(
            drag,
            placement.to,
            placement.from.size.w,
            bar_height(panorama.expansion),
        );
        panorama.returning = Some(Returning {
            window: drag.window.clone(),
            from,
            start: now,
            fraction: 0.0,
        });
    }

    /// The windows of every space at their positions (those of the active space from the Space).
    fn space_contents(&self) -> Vec<Vec<(Window, Point<i32, Logical>)>> {
        (0..self.workspaces.count())
            .map(|index| {
                if index == self.workspaces.active {
                    self.space
                        .elements()
                        .filter_map(|w| Some((w.clone(), self.space.element_location(w)?)))
                        .collect()
                } else {
                    self.workspaces
                        .stored
                        .get(index)
                        .cloned()
                        .unwrap_or_default()
                }
            })
            .collect()
    }

    pub fn panorama_view(&self) -> Option<PanoramaView> {
        let panorama = self.panorama.as_ref()?;
        // The thumbnails go up with the bar when Panorama closes.
        let lift = bar_top(panorama.expansion, panorama.progress);
        let thumbnails = if panorama.expansion >= THUMBS_FROM {
            self.thumbnail_rects_now()
                .into_iter()
                .zip(self.space_contents())
                .enumerate()
                .filter(|(index, _)| panorama.zoom.is_none_or(|(zoomed, _)| zoomed != *index))
                .map(|(_, (rect, windows))| Thumbnail {
                    rect: Rectangle::new(rect.loc + Point::from((0.0, lift)), rect.size),
                    windows,
                })
                .collect()
        } else {
            Vec::new()
        };
        let dragging = panorama.drag.as_ref().filter(|d| d.moved).and_then(|drag| {
            let placement = panorama
                .placements
                .iter()
                .find(|p| p.window == drag.window)?;
            Some((
                drag.window.clone(),
                drag_rect(
                    drag,
                    placement.to,
                    placement.from.size.w,
                    bar_height(panorama.expansion),
                ),
            ))
        });
        let drag = dragging.or_else(|| {
            let back = panorama.returning.as_ref()?;
            let placement = panorama
                .placements
                .iter()
                .find(|p| p.window == back.window)?;
            Some((
                back.window.clone(),
                lerp_rect(back.from, placement.to, back.fraction),
            ))
        });
        Some(PanoramaView {
            mode: panorama.mode,
            progress: panorama.progress,
            placements: panorama.placements.clone(),
            expansion: panorama.expansion,
            thumbnails,
            thumbnail_rects: self.thumbnail_rects_now(),
            drag,
            labels: self.space_labels(),
            hover_window: self.hover_window(),
            plus: self.workspaces.count() < crate::workspaces::MAX_SPACES,
            ghost: panorama
                .ghost
                .map_or(0.0, |g| g.value(panorama.clock, self.animation_speed)),
            ghost_pressed: panorama.plus_press,
            zoom: panorama.zoom.map(|(index, rect)| {
                let output = self.output_size();
                let full = Rectangle::from_size(output);
                (index, lerp_rect(full, rect, panorama.progress))
            }),
            close_buttons: self
                .close_buttons()
                .into_iter()
                .map(|(index, mut center)| {
                    center.y += lift;
                    CloseButton {
                        center,
                        pressed: panorama.close_press == Some(index),
                    }
                })
                .collect(),
        })
    }

    /// The close buttons showing (space index, center at rest).
    fn close_buttons(&self) -> Vec<(usize, Point<f64, Logical>)> {
        let Some(panorama) = self.panorama.as_ref() else {
            return Vec::new();
        };
        if panorama.expansion < THUMBS_FULL || !panorama.opening || self.workspaces.count() <= 1 {
            return Vec::new();
        }
        let rects = self.thumbnail_rects_now();
        let corner = |index: usize| rects.get(index).map(|r| (index, r.loc));
        if self.alt_held() {
            return (0..rects.len()).filter_map(corner).collect();
        }
        let delay = CLOSE_DELAY_MS * self.animation_speed.max(0.0);
        panorama
            .hover
            .filter(|hover| {
                hover.at_once
                    || panorama
                        .clock
                        .saturating_duration_since(hover.since)
                        .as_secs_f64()
                        * 1000.0
                        >= delay
            })
            .and_then(|hover| corner(hover.index))
            .into_iter()
            .collect()
    }

    /// The close button under the point.
    fn close_button_at(&self, location: Point<f64, Logical>) -> Option<usize> {
        self.close_buttons()
            .into_iter()
            .find(|(_, center)| {
                (location.x - center.x).hypot(location.y - center.y) <= CLOSE_RADIUS
            })
            .map(|(index, _)| index)
    }

    /// The window under the pointer, once the windows are in place: the topmost arranged one
    /// that contains it, below the spaces bar and with no window grabbed.
    fn hover_window(&self) -> Option<HoverWindow> {
        let panorama = self.panorama.as_ref()?;
        if !panorama.opening
            || panorama.progress < 1.0
            || panorama.drag.is_some()
            || panorama.returning.is_some()
            || !matches!(panorama.mode, Mode::Panorama | Mode::App)
        {
            return None;
        }
        let pointer = panorama.pointer?;
        if panorama.mode == Mode::Panorama && pointer.y < bar_height(panorama.expansion) {
            return None;
        }
        let placement = panorama
            .placements
            .iter()
            .rev()
            .find(|p| p.to.contains(pointer))?;
        Some(HoverWindow {
            rect: placement.to,
            title: (panorama.mode == Mode::Panorama)
                .then(|| window_title_of(&placement.window))
                .filter(|title| !title.is_empty()),
            dark: self.dark(),
        })
    }

    /// The pointer over the thumbnails: which one shows its close button.
    fn track_hover(&mut self, location: Point<f64, Logical>, now: Instant) {
        let showing = self.close_buttons();
        let on_button = self.close_button_at(location);
        let over = self.thumbnail_at(location).or(on_button);
        let speed = self.animation_speed.max(0.0);
        let Some(panorama) = self.panorama.as_mut() else {
            return;
        };
        match (over, panorama.hover) {
            (Some(index), Some(hover)) if hover.index == index => {}
            (Some(index), _) => {
                let rearmed =
                    !showing.is_empty() || panorama.rearm_until.is_some_and(|until| now <= until);
                panorama.hover = Some(Hover {
                    index,
                    since: now,
                    at_once: rearmed,
                });
                panorama.rearm_until = None;
            }
            (None, Some(_)) => {
                if !showing.is_empty() {
                    panorama.rearm_until = Some(
                        now + std::time::Duration::from_secs_f64(CLOSE_REARM_MS * speed / 1000.0),
                    );
                }
                panorama.hover = None;
            }
            (None, None) => {}
        }
    }

    /// A space was added at the end: with the bar expanded its thumbnail slides in.
    pub fn panorama_space_added(&mut self, now: Instant) {
        let (output, speed) = (self.output_size(), self.animation_speed);
        if let Some(row) = self.panorama.as_mut().and_then(|p| p.row.as_mut()) {
            row.add(output, now, speed);
        }
    }

    /// The space at `index` was removed: its thumbnail goes, the others close the gap.
    pub fn panorama_space_removed(&mut self, index: usize, now: Instant) {
        let (output, speed) = (self.output_size(), self.animation_speed);
        if let Some(row) = self.panorama.as_mut().and_then(|p| p.row.as_mut()) {
            row.remove(index, output, now, speed);
        }
        // The windows of the active space may have changed.
        if self
            .panorama
            .as_ref()
            .is_some_and(|p| p.mode == Mode::Panorama && p.opening)
        {
            let placements = self.panorama_placements(Mode::Panorama);
            if let Some(panorama) = self.panorama.as_mut() {
                panorama.placements = placements;
            }
        }
    }

    /// Where the space thumbnails are now (at rest, without the bar's own movement).
    fn thumbnail_rects_now(&self) -> Vec<Rectangle<f64, Logical>> {
        let output = self.output_size();
        match self
            .panorama
            .as_ref()
            .and_then(|p| Some((p.row.as_ref()?, p.clock)))
        {
            Some((row, now)) => row.rects(now, self.animation_speed),
            None => crate::thumbnail_row::centered(self.workspaces.count(), output),
        }
    }

    /// The pointer moves with Panorama open: at the top the bar expands, below the expanded
    /// bar (outside the bar) it collapses; a grabbed window follows the pointer.
    pub fn panorama_pointer(&mut self, location: Point<f64, Logical>, now: Instant) {
        let Some(panorama) = self.panorama.as_mut() else {
            return;
        };
        if !panorama.opening {
            return;
        }
        panorama.pointer = Some(location);
        if panorama.mode != Mode::Panorama {
            return;
        }
        if let Some(drag) = panorama.drag.as_mut() {
            drag.pointer = location;
            let (dx, dy) = (location.x - drag.press.x, location.y - drag.press.y);
            if dx.hypot(dy) > DRAG_THRESHOLD {
                drag.moved = true;
            }
        }
        // Once expanded the bar stays so while Panorama is open (missioncontrol.md › PA-14).
        if !panorama.expanded && location.y < BAR_HEIGHT {
            self.expand_spaces_bar(true, now);
        }
        self.track_hover(location, now);
        self.track_ghost(location, now);
    }

    /// The ghost thumbnail slides in while the pointer is on the "+" of the expanded bar, and
    /// out when it leaves (the reverse of the entry: not measured).
    fn track_ghost(&mut self, location: Point<f64, Logical>, now: Instant) {
        let width = self.output_size().w;
        let speed = self.animation_speed;
        let plus = self.workspaces.count() < crate::workspaces::MAX_SPACES;
        let Some(panorama) = self.panorama.as_mut() else {
            return;
        };
        let on = plus
            && panorama.expansion >= THUMBS_FULL
            && panorama.drag.is_none()
            && plus_rect(width, panorama.expansion).contains(location);
        let current = panorama.ghost.map_or(0.0, |g| g.value(now, speed));
        if panorama.ghost.map(|g| g.shown) != Some(on) && (on || panorama.ghost.is_some()) {
            panorama.ghost = Some(Ghost {
                shown: on,
                since: now,
                from: current,
            });
        }
    }

    /// The space of the thumbnail under the point, with the bar expanded.
    fn thumbnail_at(&self, location: Point<f64, Logical>) -> Option<usize> {
        let panorama = self.panorama.as_ref()?;
        if panorama.expansion < THUMBS_FROM {
            return None;
        }
        self.thumbnail_rects_now()
            .iter()
            .position(|rect| rect.contains(location))
    }

    /// Press with Panorama open: an entry of the bar, the "+", a thumbnail, a window (which
    /// can be dragged) or the empty area.
    pub fn panorama_press_at(&mut self, location: Point<f64, Logical>, now: Instant) {
        let Some(panorama) = self.panorama.as_ref() else {
            return;
        };
        if !panorama.opening {
            return;
        }
        let expansion = panorama.expansion;
        let progress = panorama.progress;
        let width = self.output_size().w;
        // With the desktop in view a click puts the windows back in their place.
        if panorama.mode == Mode::Desktop {
            self.close_panorama(now, None);
            return;
        }
        if panorama.mode == Mode::Panorama
            && let Some(index) = self.close_button_at(location)
        {
            if let Some(panorama) = self.panorama.as_mut() {
                panorama.close_press = Some(index);
            }
            return;
        }
        if panorama.mode == Mode::Panorama && location.y < bar_height(expansion) {
            if self.workspaces.count() < crate::workspaces::MAX_SPACES
                && plus_rect(width, expansion).contains(location)
            {
                // Added on release (spaces.md › add animation).
                if let Some(panorama) = self.panorama.as_mut() {
                    panorama.plus_press = true;
                }
                return;
            }
            if expansion >= THUMBS_FROM {
                if let Some(index) = self.thumbnail_at(location) {
                    self.zoom_into_space(index, now);
                }
                return;
            }
            let index = bar_items(&self.space_labels(), width)
                .iter()
                .find(|item| item.rect.contains(location))
                .map(|item| item.index);
            if let Some(index) = index {
                self.close_panorama(now, None);
                self.switch_space(index, now);
            }
            return;
        }
        let hit = panorama.placements.iter().rev().find_map(|placement| {
            let rect = interpolate(placement, progress);
            rect.contains(location).then(|| {
                let grab = (
                    (location.x - rect.loc.x) / rect.size.w.max(1.0),
                    (location.y - rect.loc.y) / rect.size.h.max(1.0),
                );
                (placement.window.clone(), grab)
            })
        });
        match hit {
            Some((window, grab)) => {
                if let Some(panorama) = self.panorama.as_mut() {
                    panorama.drag = Some(Drag {
                        window,
                        grab,
                        press: location,
                        pointer: location,
                        moved: false,
                    });
                }
            }
            None => self.close_panorama(now, None),
        }
    }

    /// Release: a window not moved goes to the front and Panorama closes; one dragged onto a
    /// thumbnail moves to that space, elsewhere it goes back to its place.
    pub fn panorama_release(&mut self, location: Point<f64, Logical>, now: Instant) {
        // A close button: the space goes if the pointer is still on it.
        if let Some(index) = self.panorama.as_mut().and_then(|p| p.close_press.take()) {
            if self.close_button_at(location) == Some(index) {
                if let Err(err) = self.remove_space(index) {
                    tracing::info!("panorama: {err}");
                }
                if let Some(panorama) = self.panorama.as_mut() {
                    panorama.hover = None;
                }
            }
            return;
        }
        if self
            .panorama
            .as_mut()
            .is_some_and(|p| std::mem::take(&mut p.plus_press))
        {
            // The ghost vanishes in one frame; the new thumbnail comes in from the edge.
            if let Some(panorama) = self.panorama.as_mut() {
                panorama.ghost = None;
            }
            let expansion = self.panorama.as_ref().map_or(0.0, |p| p.expansion);
            if plus_rect(self.output_size().w, expansion).contains(location)
                && let Err(err) = self.add_space()
            {
                tracing::info!("panorama: {err}");
            }
            return;
        }
        let Some(drag) = self.panorama.as_mut().and_then(|p| p.drag.take()) else {
            return;
        };
        if !drag.moved {
            self.close_panorama(now, Some(drag.window));
            return;
        }
        if self
            .panorama
            .as_ref()
            .is_none_or(|p| p.mode != Mode::Panorama)
        {
            return;
        }
        let Some(index) = self
            .thumbnail_at(location)
            .filter(|index| *index != self.workspaces.active)
        else {
            self.start_return(&drag, now);
            return;
        };
        if let Err(err) = self.move_window_to_space(&drag.window, index) {
            tracing::info!("panorama: {err}");
            self.start_return(&drag, now);
            return;
        }
        // The remaining windows are rearranged right away.
        let placements = self.panorama_placements(Mode::Panorama);
        if let Some(panorama) = self.panorama.as_mut() {
            panorama.placements = placements;
        }
    }

    /// For `panorama.state`.
    pub fn panorama_state(&self) -> PanoramaState {
        let Some(panorama) = self.panorama.as_ref() else {
            return PanoramaState {
                mode: None,
                open: false,
                progress: 0.0,
                windows: Vec::new(),
                expansion: 0.0,
                thumbnails: Vec::new(),
            };
        };
        let rect = |r: Rectangle<f64, Logical>| PanoramaRect {
            x: r.loc.x,
            y: r.loc.y,
            width: r.size.w,
            height: r.size.h,
        };
        PanoramaState {
            mode: Some(panorama.mode.name().to_owned()),
            open: panorama.opening,
            progress: panorama.progress,
            windows: panorama
                .placements
                .iter()
                .filter_map(|p| {
                    Some(PanoramaWindow {
                        id: State::window_id(&p.window)?,
                        x: p.to.loc.x,
                        y: p.to.loc.y,
                        width: p.to.size.w,
                        height: p.to.size.h,
                    })
                })
                .collect(),
            expansion: panorama.expansion,
            thumbnails: if panorama.expansion >= THUMBS_FROM {
                self.thumbnail_rects_now().into_iter().map(rect).collect()
            } else {
                Vec::new()
            },
        }
    }
}

fn faded(color: [u8; 4], alpha: f64) -> [u8; 4] {
    let [r, g, b, a] = color;
    [
        r,
        g,
        b,
        (f64::from(a) * alpha.clamp(0.0, 1.0)).round() as u8,
    ]
}

/// What is needed to draw the bar.
pub struct BarLook {
    pub width: f64,
    /// The thumbnails (bar coordinates).
    pub thumbnails: Vec<Rectangle<f64, Logical>>,
    pub active: usize,
    /// Whether the "+" shows.
    pub plus: bool,
    /// How far in the ghost thumbnail of the "+" is (0–1), and whether the "+" is pressed.
    pub ghost: f64,
    pub ghost_pressed: bool,
    /// The output's height (the ghost has a thumbnail's aspect).
    pub height: f64,
    /// A thumbnail not drawn (the one magnifying out of the bar).
    pub hidden: Option<usize>,
    pub expansion: f64,
    /// Color of the thumbnails behind the wallpaper (the desktop's).
    pub backdrop: [u8; 4],
    /// The label of each space (desktops and full-screen apps).
    pub labels: Vec<String>,
    /// Dark appearance.
    pub dark: bool,
}

impl BarLook {
    /// Height of the sprite: bar and shadow.
    fn sprite_height(&self) -> f64 {
        let shadow = BAR_SHADOW + (EXPANDED_SHADOW - BAR_SHADOW) * self.expansion;
        bar_height(self.expansion) + shadow
    }

    /// Cache key: it changes with what is visible (height at the whole point).
    fn key(&self) -> String {
        let thumbs: Vec<String> = self
            .thumbnails
            .iter()
            .map(|r| format!("{:.1},{:.1},{:.1}", r.loc.x, r.loc.y, r.size.w))
            .collect();
        format!(
            "{}:{:?}:{}:{}:{}:{}:{:?}:{}:{}:{}",
            self.dark,
            self.hidden,
            self.plus,
            thumbs.join(";"),
            self.active,
            (self.expansion * 1000.0).round(),
            self.backdrop,
            (self.ghost * 1000.0).round(),
            self.ghost_pressed,
            self.labels.join("\u{1f}")
        )
    }
}

/// Draws the spaces bar (compact, expanded or halfway) with the shadow below.
pub fn draw_bar(canvas: &mut Canvas, look: &BarLook) {
    let e = look.expansion.clamp(0.0, 1.0);
    let w = look.width as f32;
    let height = bar_height(e);
    // The fill is the material drawn behind this sprite (Glass/Regular, with its own rims).
    let bar = height as f32;
    // Shadow: black fading to zero.
    let shadow = BAR_SHADOW + (EXPANDED_SHADOW - BAR_SHADOW) * e;
    let strength = BAR_SHADOW_ALPHA + (EXPANDED_SHADOW_ALPHA - BAR_SHADOW_ALPHA) * e;
    let rows = shadow as usize;
    for row in 0..rows {
        let t = 1.0 - row as f64 / rows as f64;
        let alpha = (strength * t * t * 255.0).round() as u8;
        canvas.fill_rect([0.0, bar + row as f32, w, 1.0], [0, 0, 0, alpha]);
    }
    let (capsule_fill, label_color, plus_color) = if look.dark {
        (CAPSULE_FILL_DARK, LABEL_COLOR_DARK, PLUS_COLOR_DARK)
    } else {
        (CAPSULE_FILL, LABEL_COLOR, PLUS_COLOR)
    };
    // The compact entries vanish at the start of the expansion.
    let compact = 1.0 - e / 0.3;
    if compact > 0.0 {
        for item in bar_items(&look.labels, look.width) {
            let r = item.rect;
            if item.index == look.active {
                let rect = [
                    r.loc.x as f32,
                    r.loc.y as f32,
                    r.size.w as f32,
                    r.size.h as f32,
                ];
                canvas.fill_rounded(
                    rect,
                    CAPSULE_RADIUS,
                    faded(capsule_fill, compact * CAPSULE_OPACITY),
                );
            }
            // Baseline: the 13 pt line of 18 sits at y 11 of the bar (cap height 9.4, centred
            // on the 24 pt capsule).
            let baseline = (CAPSULE_TOP + CAPSULE_HEIGHT / 2.0 + 4.7) as f32;
            let text = f64::from(text_width(&item.label, LABEL_SIZE, Weight::Regular));
            canvas.text(
                [(r.loc.x + (r.size.w - text) / 2.0) as f32, baseline],
                &item.label,
                LABEL_SIZE,
                Weight::Regular,
                faded(label_color, compact),
            );
        }
    }
    // The thumbnails: the background (the desktop wallpaper is drawn over it), the ring and
    // the label.
    let shown = (e - THUMBS_FROM) / (THUMBS_FULL - THUMBS_FROM);
    if shown > 0.0 {
        for (index, r) in look.thumbnails.iter().copied().enumerate() {
            if look.hidden == Some(index) {
                continue;
            }
            let rect = [
                r.loc.x as f32,
                r.loc.y as f32,
                r.size.w as f32,
                r.size.h as f32,
            ];
            canvas.fill_rect(rect, faded(look.backdrop, shown));
            if index == look.active {
                let inset = (RING_GAP + RING_WIDTH / 2.0) as f32;
                canvas.stroke_rounded(
                    [
                        rect[0] - inset,
                        rect[1] - inset,
                        rect[2] + 2.0 * inset,
                        rect[3] + 2.0 * inset,
                    ],
                    inset + 1.0,
                    RING_WIDTH as f32,
                    faded(RING_COLOR, shown),
                );
            }
            // Shrunk thumbnails (16 spaces) have no label.
            if !crate::thumbnail_row::full_size(r) {
                continue;
            }
            let label = look.labels.get(index).cloned().unwrap_or_default();
            let text = f64::from(text_width(&label, THUMB_LABEL_SIZE, Weight::Regular));
            canvas.text(
                [
                    (r.loc.x + (r.size.w - text) / 2.0) as f32,
                    (r.loc.y + r.size.h + THUMB_LABEL_BASELINE) as f32,
                ],
                &label,
                THUMB_LABEL_SIZE,
                Weight::Regular,
                faded(label_color, shown),
            );
        }
    }
    // The ghost thumbnail of the "+" (under the cross), in the empty thumbnails' colour.
    if look.plus && look.ghost > 0.0 && look.expansion >= THUMBS_FULL {
        let r = ghost_rect(look.width, look.height, look.expansion, look.ghost);
        let [red, green, blue, alpha] = look.backdrop;
        let shade = |c: u8| {
            if look.ghost_pressed {
                (f64::from(c) * GHOST_PRESSED).round() as u8
            } else {
                c
            }
        };
        canvas.fill_rect(
            [
                r.loc.x as f32,
                r.loc.y as f32,
                r.size.w as f32,
                r.size.h as f32,
            ],
            [shade(red), shade(green), shade(blue), alpha],
        );
    }
    if !look.plus {
        return;
    }
    let plus = plus_rect(look.width, e);
    let cx = (plus.loc.x + plus.size.w / 2.0) as f32;
    let cy = (plus.loc.y + plus.size.h / 2.0) as f32;
    let half = (PLUS_SIZE / 2.0) as f32 - 1.0;
    canvas.line([cx - half, cy], [cx + half, cy], PLUS_STROKE, plus_color);
    canvas.line([cx, cy - half], [cx, cy + half], PLUS_STROKE, plus_color);
}

/// Draws the close buttons (`origin`: where the canvas starts in the output).
pub fn draw_close_buttons(canvas: &mut Canvas, buttons: &[CloseButton]) {
    for button in buttons {
        let (x, y) = (button.center.x as f32, button.center.y as f32);
        let r = CLOSE_RADIUS as f32;
        // Soft shadow, a little below.
        for k in 1..=4 {
            let grow = k as f32;
            let alpha = (12 - 2 * k) as u8;
            canvas.fill_rounded(
                [
                    x - r - grow,
                    y - r - grow + 1.5,
                    2.0 * (r + grow),
                    2.0 * (r + grow),
                ],
                r + grow,
                [0, 0, 0, alpha],
            );
        }
        canvas.fill_rounded(
            [x - r, y - r, 2.0 * r, 2.0 * r],
            r,
            if button.pressed {
                CLOSE_PRESSED
            } else {
                CLOSE_FILL
            },
        );
        let c = CLOSE_CROSS as f32;
        canvas.line([x - c, y - c], [x + c, y + c], 2.0, CLOSE_CROSS_COLOR);
        canvas.line([x - c, y + c], [x + c, y - c], 2.0, CLOSE_CROSS_COLOR);
    }
}

/// Ring of the window under the pointer: `App/Mission Control/Ring`, 4 pt inside the edge, radius
/// 20 (kit `Hover ring`).
const WINDOW_RING_WIDTH: f32 = 4.0;
const WINDOW_RING_RADIUS: f32 = 20.0;
/// Title capsule (kit `Hover title`): 32 tall, radius 16, at least 112 wide, Inter 12 regular,
/// translucent `Surface` fill and 1 pt border (the kit's blur 6 and soft shadow are not drawn).
const TITLE_HEIGHT: f32 = 32.0;
const TITLE_MIN_WIDTH: f32 = 112.0;
const TITLE_PADDING: f32 = 20.0;
const TITLE_SIZE: f32 = 12.0;
const TITLE_FILL: [u8; 4] = [0xff, 0xff, 0xff, 0x99];
const TITLE_FILL_DARK: [u8; 4] = [0x2a, 0x2b, 0x30, 0x99];
const TITLE_BORDER: [u8; 4] = [0x00, 0x00, 0x00, 0x1f];
const TITLE_BORDER_DARK: [u8; 4] = [0x00, 0x00, 0x00, 0x66];

/// The window title of a placement (the label shown in the capsule).
fn window_title_of(window: &Window) -> String {
    window_label(window)
}

/// Draws the ring and the capsule of a hovered window (canvas = the window's rectangle).
pub fn draw_hover_window(canvas: &mut Canvas, hover: &HoverWindow) {
    let (w, h) = (hover.rect.size.w as f32, hover.rect.size.h as f32);
    let inset = WINDOW_RING_WIDTH / 2.0;
    canvas.stroke_rounded(
        [inset, inset, w - 2.0 * inset, h - 2.0 * inset],
        WINDOW_RING_RADIUS - inset,
        WINDOW_RING_WIDTH,
        RING_COLOR,
    );
    let Some(title) = &hover.title else {
        return;
    };
    let text = text_width(title, TITLE_SIZE, Weight::Regular);
    let width = (text + 2.0 * TITLE_PADDING)
        .max(TITLE_MIN_WIDTH)
        .min((w - 2.0 * WINDOW_RING_WIDTH).max(0.0));
    let (fill, border, ink) = if hover.dark {
        (TITLE_FILL_DARK, TITLE_BORDER_DARK, LABEL_COLOR_DARK)
    } else {
        (TITLE_FILL, TITLE_BORDER, LABEL_COLOR)
    };
    let rect = [
        (w - width) / 2.0,
        (h - TITLE_HEIGHT) / 2.0,
        width,
        TITLE_HEIGHT,
    ];
    canvas.fill_rounded(rect, TITLE_HEIGHT / 2.0, fill);
    canvas.stroke_rounded(
        [rect[0] + 0.5, rect[1] + 0.5, rect[2] - 1.0, rect[3] - 1.0],
        TITLE_HEIGHT / 2.0 - 0.5,
        1.0,
        border,
    );
    // The 16 pt line of the text sits 8 pt below the capsule's top: baseline ~ 11.6 under it.
    canvas.text(
        [(w - text.min(width)) / 2.0, rect[1] + 8.0 + 11.6],
        title,
        TITLE_SIZE,
        Weight::Regular,
        ink,
    );
}

/// The hovered window's ring and title as an element at the window.
pub fn hover_window_element(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    sprite: &HoverSprite,
    hover: &HoverWindow,
    scale: f64,
) -> Option<
    smithay::backend::renderer::element::texture::TextureRenderElement<
        smithay::backend::renderer::gles::GlesTexture,
    >,
> {
    sprite.0.element(
        renderer,
        &format!("{}:{:?}", hover.dark, hover.title),
        hover.rect.size,
        hover.rect.loc,
        scale,
        1.0,
        |canvas| draw_hover_window(canvas, hover),
    )
}

/// The cached drawing of the hovered window (in the output's data).
#[derive(Default)]
pub struct HoverSprite(Sprite);

/// The close buttons as an element over the whole bar.
pub fn close_buttons_element(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    sprite: &CloseSprite,
    buttons: &[CloseButton],
    width: f64,
    scale: f64,
) -> Option<
    smithay::backend::renderer::element::texture::TextureRenderElement<
        smithay::backend::renderer::gles::GlesTexture,
    >,
> {
    if buttons.is_empty() {
        return None;
    }
    let key: Vec<String> = buttons
        .iter()
        .map(|b| format!("{:.1},{:.1},{}", b.center.x, b.center.y, b.pressed))
        .collect();
    sprite.0.element(
        renderer,
        &key.join(";"),
        Size::from((width, EXPANDED_HEIGHT)),
        Point::from((0.0, 0.0)),
        scale,
        1.0,
        |canvas| draw_close_buttons(canvas, buttons),
    )
}

/// The cached drawing of the close buttons (in the output's data).
#[derive(Default)]
pub struct CloseSprite(Sprite);

/// The spaces bar as an element, already moved up as much as the opening needs.
pub fn bar_element(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    sprite: &Sprite,
    look: &BarLook,
    progress: f64,
    scale: f64,
) -> Option<
    smithay::backend::renderer::element::texture::TextureRenderElement<
        smithay::backend::renderer::gles::GlesTexture,
    >,
> {
    sprite.element(
        renderer,
        &look.key(),
        Size::from((look.width, look.sprite_height())),
        Point::from((0.0, bar_top(look.expansion, progress))),
        scale,
        bar_alpha(progress),
        |canvas| draw_bar(canvas, look),
    )
}

/// The top edge of the bar: its bottom edge comes down from 0 to 40 pt on the window curve.
fn bar_top(expansion: f64, progress: f64) -> f64 {
    -(1.0 - progress) * bar_height(expansion)
}

/// The opacity of the bar while it comes down: the shadow, which at rest is below, appears
/// on the first frame.
pub fn bar_alpha(progress: f64) -> f32 {
    (progress * 4.0).clamp(0.0, 1.0) as f32
}

/// Where the bar material (Glass/Regular over what is under the bar) shows: the bar fill,
/// without the shadow.
pub fn bar_material_rect(width: f64, expansion: f64, progress: f64) -> Rectangle<f64, Logical> {
    Rectangle::new(
        (0.0, bar_top(expansion, progress)).into(),
        (width, bar_height(expansion)).into(),
    )
}

/// The glass region of the spaces bar (physical pixels): Glass/Regular exactly like every Amalfi
/// kind (σ 8, saturation 1.1, tint, refraction, inside rim, rim light, and the blur scale, which
/// the equivalence test covers), without the kind's drop shadow (the bar's sprite draws its own).
pub fn bar_region(
    rect: Rectangle<i32, smithay::utils::Physical>,
    dark: bool,
    opacity: f32,
    scale: f64,
) -> compositor_render::glass::PhysicalRegion {
    use compositor_render::glass_profile::GlassKind;
    // Panel is a kind with no sharp share.
    let kind = GlassKind::Panel;
    compositor_render::glass::PhysicalRegion {
        rect,
        radius: 0.0,
        smoothing: 0.0,
        material: kind.material(dark),
        opacity,
        scale,
        tip: None,
        group: 0,
        profile: kind.profile(dark).without_shadow(),
        rotation: 0.0,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_close_button_shows_after_half_a_second_by_user_decision() {
        assert_eq!(super::CLOSE_DELAY_MS, 500.0);
    }

    use super::{lerp_rect, return_fraction};
    use smithay::utils::{Logical, Rectangle};
    use std::time::Duration;

    #[test]
    fn a_dropped_window_goes_back_in_half_a_second_without_overshoot() {
        let ms = Duration::from_millis;
        assert_eq!(return_fraction(ms(0), 1.0), 0.0);
        assert_eq!(return_fraction(ms(500), 1.0), 1.0);
        assert_eq!(return_fraction(ms(900), 1.0), 1.0);
        // Ease-out: more than half of the way at half the time, never past the end.
        assert!(return_fraction(ms(250), 1.0) > 0.5);
        let mut last = 0.0;
        for t in 0..=500 {
            let f = return_fraction(ms(t), 1.0);
            assert!((0.0..=1.0).contains(&f) && f >= last, "{t} ms: {f}");
            last = f;
        }
        // Slowed down x2 it takes twice as long; with animations off it is immediate.
        assert!(return_fraction(ms(500), 2.0) < 1.0);
        assert_eq!(return_fraction(ms(0), 0.0), 1.0);
    }

    #[test]
    fn the_ghost_of_the_plus_slides_in_from_the_edge_and_out_again() {
        let start = Instant::now();
        let ms = |n: u64| start + Duration::from_millis(n);
        let ghost = Ghost {
            shown: true,
            since: start,
            from: 0.0,
        };
        // The spring of the entry (ω ≈ 16.4 s⁻¹): about half way at ~102 ms, in by ~410 ms.
        assert_eq!(ghost.value(start, 1.0), 0.0);
        assert!((ghost.value(ms(102), 1.0) - 0.5).abs() < 0.02);
        assert!(ghost.value(ms(410), 1.0) > 0.98);
        // Slowed down x2 it takes twice as long; with animations off it is in at once.
        assert!((ghost.value(ms(204), 2.0) - 0.5).abs() < 0.02);
        assert_eq!(ghost.value(start, 0.0), 1.0);
        // Leaving from half way it goes back out from there.
        let out = Ghost {
            shown: false,
            since: start,
            from: 0.5,
        };
        assert_eq!(out.value(start, 1.0), 0.5);
        assert!(out.value(ms(600), 1.0) < 0.01);
        // In, its left edge is 80 pt from the screen's right edge, a thumbnail tall, cut by the
        // edge, centred on the "+"; out, it is past the edge.
        let (w, h) = (1920.0, 1200.0);
        let plus = plus_rect(w, 1.0);
        let r = ghost_rect(w, h, 1.0, 1.0);
        assert_eq!(r.loc.x, w - 80.0);
        assert_eq!(r.size, (80.0, 90.0).into());
        assert!((r.loc.y + 45.0 - (plus.loc.y + plus.size.h / 2.0)).abs() < 1e-9);
        assert_eq!(ghost_rect(w, h, 1.0, 0.0).size.w, 0.0);
    }

    #[test]
    fn a_dragged_window_shrinks_as_it_climbs_and_is_a_miniature_over_the_bar() {
        // A 600 × 400 window arranged at 300 × 200, pressed at y 700, grabbed in its middle.
        let frame = Rectangle::<f64, Logical>::new((200.0, 600.0).into(), (300.0, 200.0).into());
        let press = Point::from((350.0, 700.0));
        let at = |y: f64| drag_geometry(press, (350.0, y).into(), (0.5, 0.5), frame, 600.0, 168.0);
        // At the press, or below it: full size.
        assert_eq!(at(700.0).size, frame.size);
        assert_eq!(at(760.0).size, frame.size);
        // 100 pt up: 165 pt narrower (1.65 pt of width per pt), the height in proportion.
        let up = at(600.0);
        assert!((up.size.w - 135.0).abs() < 1e-9, "{:?}", up.size);
        assert!((up.size.h - 90.0).abs() < 1e-9);
        // The pointer keeps its place in it.
        assert!((up.loc.x + up.size.w / 2.0 - 350.0).abs() < 1e-9);
        // Never below the miniature: 0.07 of the real width (42 pt), and that over the bar,
        // with the pointer 55 % down it.
        assert!((at(200.0).size.w - 42.0).abs() < 1e-9);
        let over = at(100.0);
        assert!((over.size.w - 42.0).abs() < 1e-9);
        assert!((over.loc.y + 0.55 * over.size.h - 100.0).abs() < 1e-9);
        // Going back down it grows back the same way.
        assert_eq!(at(600.0), up);
    }

    #[test]
    fn the_return_rectangle_interpolates_position_and_size() {
        let from = Rectangle::<f64, Logical>::new((10.0, 20.0).into(), (100.0, 50.0).into());
        let to = Rectangle::<f64, Logical>::new((110.0, 120.0).into(), (200.0, 150.0).into());
        assert_eq!(lerp_rect(from, to, 0.0), from);
        assert_eq!(lerp_rect(from, to, 1.0), to);
        let half = lerp_rect(from, to, 0.5);
        assert_eq!(half.loc, (60.0, 70.0).into());
        assert_eq!(half.size, (150.0, 100.0).into());
    }

    #[test]
    fn space_labels_follow_the_system_language() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            super::system_language(env(&[("LANG", "it_IT.UTF-8")])),
            "it"
        );
        assert_eq!(
            super::system_language(env(&[("LANGUAGE", "en:it"), ("LANG", "it_IT.UTF-8")])),
            "en"
        );
        assert_eq!(
            super::system_language(env(&[("LANG", "C"), ("LANGUAGE", "it")])),
            "en"
        );
        assert_eq!(super::system_language(env(&[])), "en");
        assert_eq!(super::space_label_in(0, "it"), "Spazio 1");
        assert_eq!(super::space_label_in(1, "en"), "Space 2");
        assert_eq!(super::space_label_in(2, "de"), "Space 3");
        assert_eq!(super::lone_label_in("it"), "Spazio");
        assert_eq!(super::lone_label_in("en"), "Space");
    }

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rectangle<f64, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn two_windows_that_fit_keep_their_size() {
        // The reference: file manager 900 × 600 and terminal 597 × 422 go to (533,332) and
        // (1449,276), scale 1, 16 pt between them (measurements at ±1 pt).
        let area = rect(48.0, 88.0, 2464.0, 1220.0);
        let out = layout(
            &[
                rect(700.0, 300.0, 900.0, 600.0),
                rect(1283.0, 308.0, 597.0, 422.0),
            ],
            area,
        );
        assert_eq!(out[0].size, Size::from((900.0, 600.0)));
        assert_eq!(out[1].size, Size::from((597.0, 422.0)));
        assert!((out[1].loc.x - (out[0].loc.x + 900.0) - WINDOW_GAP).abs() < 1e-6);
        for (got, want) in [(out[0].loc, (533.0, 332.0)), (out[1].loc, (1449.0, 276.0))] {
            assert!(
                (got.x - want.0).abs() <= 1.5 && (got.y - want.1).abs() <= 1.5,
                "{got:?} instead of {want:?}"
            );
        }
        // Already separate: they stay where they are.
        let apart = [
            rect(100.0, 100.0, 300.0, 200.0),
            rect(900.0, 500.0, 300.0, 200.0),
        ];
        assert_eq!(layout(&apart, area), apart);
    }

    #[test]
    fn many_windows_shrink_into_rows_without_overlap() {
        let area = rect(48.0, 88.0, 2464.0, 1220.0);
        let frames: Vec<_> = (0..12)
            .map(|i| {
                rect(
                    100.0 + 40.0 * f64::from(i),
                    100.0 + 30.0 * f64::from(i),
                    700.0,
                    500.0,
                )
            })
            .collect();
        let out = layout(&frames, area);
        assert_eq!(out.len(), 12);
        for (i, a) in out.iter().enumerate() {
            assert!(a.size.w <= 700.0 && a.size.w > 100.0);
            assert!(area.contains_rect(*a), "{a:?} outside {area:?}");
            for b in out.iter().skip(i + 1) {
                assert!(!a.overlaps(*b), "{a:?} {b:?}");
            }
        }
        assert!(layout(&[], area).is_empty());
    }

    #[test]
    fn the_bar_material_follows_the_bar_down() {
        for expansion in [0.0, 0.5, 1.0] {
            for progress in [0.0, 0.3, 1.0] {
                let r = bar_material_rect(1920.0, expansion, progress);
                assert_eq!(r.size.h, bar_height(expansion));
                assert!(r.loc.y <= 0.0 && r.loc.y + r.size.h <= EXPANDED_HEIGHT);
            }
        }
        assert_eq!(bar_material_rect(1920.0, 0.0, 1.0).loc.y, 0.0);
        assert_eq!(bar_alpha(0.0), 0.0);
        assert_eq!(bar_alpha(1.0), 1.0);
    }

    #[test]
    fn the_bar_is_glass_regular_in_light_and_dark() {
        use compositor_render::glass_profile::Amalfi;
        let rect = Rectangle::new((0, 0).into(), (1920, 40).into());
        for dark in [false, true] {
            let region = bar_region(rect, dark, 1.0, 1.0);
            let regular = Amalfi::Regular.material(dark);
            // Exactly the Amalfi Regular material: σ 8, saturation 1.1, brightness, tint, rims.
            assert_eq!(region.sigma(), 8.0);
            assert_eq!(region.profile.saturation, 1.1);
            assert_eq!(region.profile.brightness, regular.brightness);
            assert_eq!(region.profile.tint, regular.tint);
            assert_eq!(region.profile.inside_rim, regular.inside_rim);
            assert_eq!(region.profile.rim_light, regular.rim_light);
            let tint: [u8; 3] = if dark { [42, 43, 48] } else { [255, 255, 255] };
            for (got, want) in region.profile.tint.iter().zip(tint) {
                assert!((got * 255.0 - f32::from(want)).abs() < 0.5);
            }
            assert!((region.profile.tint[3] - regular.tint[3]).abs() < 0.005);
            assert_eq!(region.profile.inside_rim.width, 1.0);
            let rim = if dark { 0.40 } else { 0.12 };
            assert!((region.profile.inside_rim.color[3] - rim).abs() < 0.005);
            assert_eq!(region.profile.inside_rim.color[..3], [0.0; 3]);
            // The bar draws its own shadow; a square rectangle, no lens, no tip.
            assert_eq!(
                region.profile.shadow,
                compositor_render::glass_profile::GlassShadow::NONE
            );
            assert_eq!((region.radius, region.tip, region.rect), (0.0, None, rect));
            // The reduced-resolution blur scale is the thick kinds' (the equivalence test's).
            assert_eq!(
                region.profile.blur_scale,
                compositor_render::glass_profile::GlassKind::Panel
                    .profile(dark)
                    .blur_scale
            );
        }
    }

    #[test]
    fn curves_follow_the_measurements() {
        assert_eq!(curve_at(OPEN_CURVE, 0.0), 0.0);
        // Halfway between 40 and 54 ms.
        assert!(curve_at(OPEN_CURVE, 40.0) < 0.5 && curve_at(OPEN_CURVE, 54.0) > 0.5);
        assert!((curve_at(CLOSE_CURVE, 67.0) - 0.497).abs() < 1e-9);
        assert_eq!(curve_at(CLOSE_CURVE, 1000.0), 1.0);
    }
}
