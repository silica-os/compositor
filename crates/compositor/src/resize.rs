//! Resizing from the frame edges (mustache-spec 1.1, "Drag zones and edges", MU-08; windows.md,
//! WIN-RESIZE): the left and top edges take the pointer from 2 pt outside to 3 pt inside the
//! frame, the right and bottom ones from 4 pt outside to 3 inside; near a corner, for 16 pt
//! along each edge, the resize is diagonal. At the rounded corner itself the zone reaches only
//! 1–2 pt outside, and on the diagonal it goes 8 pt (top left) to 10 pt (bottom right) inside.
//! It applies to full-size content windows too (the frame includes the app's bar) and to the
//! clients' `xdg_toplevel.resize` requests.

use std::time::{Duration, Instant};

use smithay::desktop::Window;
use smithay::input::pointer::CursorIcon;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Serial, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData};

use crate::state::State;

/// Outside the frame on the left and at the top.
pub const OUTSIDE_LEADING: f64 = 2.0;
/// Outside the frame on the right and at the bottom.
pub const OUTSIDE_TRAILING: f64 = 4.0;
/// Inside the frame, on every side.
pub const INSIDE: f64 = 3.0;
/// Along an edge, from the corner: the diagonal zone (~15–17 pt measured).
pub const CORNER: f64 = 16.0;
/// Outside the frame on both axes (past a corner): the zone stops this close to it.
pub const CORNER_OUTSIDE: f64 = 2.0;
/// Inside the frame, the square at the left or top corners that resizes diagonally.
pub const CORNER_INSIDE_LEADING: f64 = 9.0;
/// The same at the right or bottom corners.
pub const CORNER_INSIDE_TRAILING: f64 = 10.0;
/// The content does not go below this side if the client does not ask otherwise.
const MIN_CONTENT: i32 = 1;

/// The sides the pointer moves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Edges {
    pub fn any(self) -> bool {
        self.left || self.right || self.top || self.bottom
    }

    /// The sides of `xdg_toplevel.resize`.
    pub fn from_xdg(edge: xdg_toplevel::ResizeEdge) -> Self {
        use xdg_toplevel::ResizeEdge as E;
        Self {
            left: matches!(edge, E::Left | E::TopLeft | E::BottomLeft),
            right: matches!(edge, E::Right | E::TopRight | E::BottomRight),
            top: matches!(edge, E::Top | E::TopLeft | E::TopRight),
            bottom: matches!(edge, E::Bottom | E::BottomLeft | E::BottomRight),
        }
    }
}

/// The sides under point `p` for frame `frame`; None outside the zones.
pub fn edges_at(frame: Rectangle<i32, Logical>, p: Point<f64, Logical>) -> Option<Edges> {
    let left = f64::from(frame.loc.x);
    let top = f64::from(frame.loc.y);
    let right = left + f64::from(frame.size.w);
    let bottom = top + f64::from(frame.size.h);
    // Inside the enlarged rectangle of the zones.
    if p.x < left - OUTSIDE_LEADING
        || p.x >= right + OUTSIDE_TRAILING
        || p.y < top - OUTSIDE_LEADING
        || p.y >= bottom + OUTSIDE_TRAILING
    {
        return None;
    }
    let out_x = p.x < left || p.x >= right;
    let out_y = p.y < top || p.y >= bottom;
    // Past a corner on both axes: only 1–2 pt around the rounded corner.
    if out_x
        && out_y
        && (p.x < left - CORNER_OUTSIDE
            || p.x >= right + CORNER_OUTSIDE
            || p.y < top - CORNER_OUTSIDE
            || p.y >= bottom + CORNER_OUTSIDE)
    {
        return None;
    }
    // Inside, the square at each corner resizes diagonally (it reaches past the 3 pt bands).
    if !out_x && !out_y {
        let near_left = p.x < left + CORNER_INSIDE_LEADING;
        let near_right = p.x >= right - CORNER_INSIDE_TRAILING;
        let near_top = p.y < top + CORNER_INSIDE_LEADING;
        let near_bottom = p.y >= bottom - CORNER_INSIDE_TRAILING;
        // Not on windows too small for the squares to be apart.
        let room = right - left > CORNER_INSIDE_LEADING + CORNER_INSIDE_TRAILING
            && bottom - top > CORNER_INSIDE_LEADING + CORNER_INSIDE_TRAILING;
        if room && (near_left || near_right) && (near_top || near_bottom) {
            return Some(Edges {
                left: near_left,
                right: near_right,
                top: near_top,
                bottom: near_bottom,
            });
        }
    }
    let mut edges = Edges {
        left: p.x < left + INSIDE,
        right: p.x >= right - INSIDE,
        top: p.y < top + INSIDE,
        bottom: p.y >= bottom - INSIDE,
    };
    // A window narrower than the two zones: the nearest side wins.
    if edges.left && edges.right {
        let to_left = (p.x - left).abs();
        let to_right = (right - p.x).abs();
        edges.left = to_left <= to_right;
        edges.right = !edges.left;
    }
    if edges.top && edges.bottom {
        let to_top = (p.y - top).abs();
        let to_bottom = (bottom - p.y).abs();
        edges.top = to_top <= to_bottom;
        edges.bottom = !edges.top;
    }
    // Near a corner the edge becomes diagonal.
    if (edges.left || edges.right) && !(edges.top || edges.bottom) {
        edges.top = p.y < top + CORNER;
        edges.bottom = !edges.top && p.y >= bottom - CORNER;
    } else if (edges.top || edges.bottom) && !(edges.left || edges.right) {
        edges.left = p.x < left + CORNER;
        edges.right = !edges.left && p.x >= right - CORNER;
    }
    edges.any().then_some(edges)
}

/// Whole steps for the content size (`silica_window_v1.set_size_increment`): the content is
/// `base + n * step` on each axis with a non-zero step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Increment {
    pub base: (i32, i32),
    pub step: (i32, i32),
}

/// `value` snapped to the nearest `base + n * step` that is within `min` (0: none) and
/// `max` (0: none); unchanged when `step` is 0 or no step fits.
fn snap_axis(value: i32, base: i32, step: i32, min: i32, max: i32) -> i32 {
    if step <= 0 {
        return value;
    }
    let steps = (f64::from(value.saturating_sub(base)) / f64::from(step)).round() as i32;
    let mut snapped = base.saturating_add(steps.saturating_mul(step));
    while snapped < min.max(MIN_CONTENT) {
        snapped = snapped.saturating_add(step);
    }
    while max > 0 && snapped > max && snapped.saturating_sub(step) >= min.max(MIN_CONTENT) {
        snapped = snapped.saturating_sub(step);
    }
    snapped
}

/// [`resized`]'s `frame` with the content snapped to `increment`; the side opposite to the
/// dragged edge stays where `from` (the frame before the resize) had it.
pub fn snapped(
    frame: Rectangle<i32, Logical>,
    from: Rectangle<i32, Logical>,
    edges: Edges,
    bar: i32,
    increment: Increment,
    min: Size<i32, Logical>,
    max: Size<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let w = if edges.left || edges.right {
        snap_axis(
            frame.size.w,
            increment.base.0,
            increment.step.0,
            min.w,
            max.w,
        )
    } else {
        frame.size.w
    };
    let content_h = frame.size.h.saturating_sub(bar);
    let h = if edges.top || edges.bottom {
        snap_axis(content_h, increment.base.1, increment.step.1, min.h, max.h).saturating_add(bar)
    } else {
        frame.size.h
    };
    let x = if edges.left {
        from.loc.x.saturating_add(from.size.w).saturating_sub(w)
    } else {
        frame.loc.x
    };
    let y = if edges.top {
        from.loc.y.saturating_add(from.size.h).saturating_sub(h)
    } else {
        frame.loc.y
    };
    Rectangle::new((x, y).into(), (w, h).into())
}

/// The new frame after a pointer movement `delta`, within the content limits (`min` and
/// `max`, 0 for no limit) and with the bar `bar` on top; the opposite side stays still.
pub fn resized(
    frame: Rectangle<i32, Logical>,
    edges: Edges,
    delta: Point<f64, Logical>,
    bar: i32,
    min: Size<i32, Logical>,
    max: Size<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let dx = delta.x.round() as i32;
    let dy = delta.y.round() as i32;
    let clamp = |value: i32, low: i32, high: i32| {
        let value = value.max(low);
        if high > 0 {
            value.min(high.max(low))
        } else {
            value
        }
    };
    let mut w = frame.size.w;
    let mut h = frame.size.h;
    if edges.left {
        w = w.saturating_sub(dx);
    } else if edges.right {
        w = w.saturating_add(dx);
    }
    if edges.top {
        h = h.saturating_sub(dy);
    } else if edges.bottom {
        h = h.saturating_add(dy);
    }
    let min_w = min.w.max(MIN_CONTENT);
    let min_h = min.h.max(MIN_CONTENT).saturating_add(bar);
    let max_h = if max.h > 0 {
        max.h.saturating_add(bar)
    } else {
        0
    };
    w = clamp(w, min_w, max.w);
    h = clamp(h, min_h, max_h);
    let right = frame.loc.x.saturating_add(frame.size.w);
    let bottom = frame.loc.y.saturating_add(frame.size.h);
    let x = if edges.left {
        right.saturating_sub(w)
    } else {
        frame.loc.x
    };
    let y = if edges.top {
        bottom.saturating_sub(h)
    } else {
        frame.loc.y
    };
    Rectangle::new((x, y).into(), (w, h).into())
}

/// With the top edge the frame does not grow above `top_limit` (the top of the usable area,
/// below the menu bar), unless that would take it under `min_h`: the bottom side stays still.
pub fn below_top(
    frame: Rectangle<i32, Logical>,
    edges: Edges,
    top_limit: i32,
    min_h: i32,
) -> Rectangle<i32, Logical> {
    if !edges.top || frame.loc.y >= top_limit {
        return frame;
    }
    let bottom = frame.loc.y.saturating_add(frame.size.h);
    let h = bottom
        .saturating_sub(top_limit)
        .max(min_h)
        .min(frame.size.h);
    Rectangle::new(
        (frame.loc.x, bottom.saturating_sub(h)).into(),
        (frame.size.w, h).into(),
    )
}

/// Which ways the content can still go between the client's limits (`max` 0: no limit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    pub grow_w: bool,
    pub shrink_w: bool,
    pub grow_h: bool,
    pub shrink_h: bool,
}

impl Room {
    pub fn of(
        content: Size<i32, Logical>,
        min: Size<i32, Logical>,
        max: Size<i32, Logical>,
    ) -> Self {
        Self {
            grow_w: max.w <= 0 || content.w < max.w,
            shrink_w: content.w > min.w.max(MIN_CONTENT),
            grow_h: max.h <= 0 || content.h < max.h,
            shrink_h: content.h > min.h.max(MIN_CONTENT),
        }
    }

    /// A side whose size is fixed (minimum equal to maximum) does not resize.
    pub fn resizable(edges: Edges, min: Size<i32, Logical>, max: Size<i32, Logical>) -> Edges {
        let fixed_w = max.w > 0 && max.w <= min.w;
        let fixed_h = max.h > 0 && max.h <= min.h;
        Edges {
            left: edges.left && !fixed_w,
            right: edges.right && !fixed_w,
            top: edges.top && !fixed_h,
            bottom: edges.bottom && !fixed_h,
        }
    }
}

/// A configure the client has not answered for this long is followed by the newest size
/// anyway (a client that acks without a new buffer, or one hidden meanwhile). A safety net
/// of the compositor's, not a macOS value.
const STALE_CONFIGURE: Duration = Duration::from_millis(100);
/// After the release the client's answer to the last configure is still anchored; past this
/// time a client that never answers is let go.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(1);

/// A resize in progress (or, after the release, waiting for the client's last answer).
///
/// Live resize (#160): every pointer movement asks the client for the new content size, one
/// configure at a time; the next one leaves when the client commits a buffer for the one
/// before (so the pace is the client's frame rate, itself paced by the frame callbacks). The
/// window is drawn only as the client committed it, never stretched; the left and top edges
/// move the origin when the client commits the new size, from the size it actually
/// committed, so the opposite side never moves. On macOS (WIN-RESIZE) the edge stays under
/// the pointer in every frame (0 ± 5 ms slow, at most one frame behind at ~1300 pt/s) and
/// the content is drawn at the new size in the same frame: here the lag is the client's
/// frame time, and the embedder starts that frame as soon as the configure arrives. The
/// minimum size is a hard stop: the size comes from the pointer's distance from where the
/// drag started, so the edge resumes as soon as the pointer comes back past the stop.
#[derive(Debug, Clone)]
pub struct Resize {
    pub window: Window,
    pub edges: Edges,
    pub start: Point<f64, Logical>,
    /// The frame at the start.
    pub frame: Rectangle<i32, Logical>,
    /// The content at the start: its right and bottom sides are the anchors.
    pub content: Rectangle<i32, Logical>,
    /// The content size the pointer asks for now.
    pub target: Size<i32, Logical>,
    /// The content size last sent to the client.
    sent: Size<i32, Logical>,
    /// The configure not answered yet, and when it left.
    in_flight: Option<(Serial, Instant)>,
    /// When the button was released (the resize is settling).
    released: Option<Instant>,
}

/// The serial of the configure the client's current state answers.
fn committed_serial(surface: &WlSurface) -> Option<Serial> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok().and_then(|data| data.current_serial))
    })
}

/// The client's minimum and maximum content size (0: none).
fn limits(window: &Window) -> (Size<i32, Logical>, Size<i32, Logical>) {
    window.toplevel().map_or_else(
        || (Size::default(), Size::default()),
        |toplevel| {
            with_states(toplevel.wl_surface(), |states| {
                let mut cached = states.cached_state.get::<SurfaceCachedState>();
                let current = cached.current();
                (current.min_size, current.max_size)
            })
        },
    )
}

impl State {
    /// The window whose edge is under the point, from the top: a window covering the point
    /// outside its zones stops the search (the one below is not resized). The sides of a
    /// fixed size do not count.
    pub fn resize_hit(&self, location: Point<f64, Logical>) -> Option<(Window, Edges)> {
        for window in self.space.elements().rev() {
            if crate::windows::is_fullscreen(window) {
                continue;
            }
            let Some(frame) = self.window_frame(window) else {
                continue;
            };
            if let Some(edges) = edges_at(frame, location) {
                let (min, max) = limits(window);
                let edges = Room::resizable(edges, min, max);
                return edges.any().then(|| (window.clone(), edges));
            }
            if frame.to_f64().contains(location) {
                return None;
            }
        }
        None
    }

    /// The pointer shape for the sides `edges` of `window`: one-way at a limit.
    pub fn resize_cursor(&self, window: &Window, edges: Edges) -> CursorIcon {
        let (min, max) = limits(window);
        let content = window.geometry().size;
        crate::cursor_theme::resize_icon_with_room(edges, Room::of(content, min, max))
    }

    /// Starts resizing `window` from the sides `edges`, from point `start`.
    pub fn start_resize(&mut self, window: Window, edges: Edges, start: Point<f64, Logical>) {
        let (Some(frame), Some(content)) = (
            self.window_frame(&window),
            self.space.element_geometry(&window),
        ) else {
            return;
        };
        // A resize still settling on this window ends here.
        self.resize_settling = None;
        let mut in_flight = None;
        if let Some(toplevel) = window.toplevel() {
            // The size too: a client that was never given one must not read 0 × 0 here.
            toplevel.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Resizing);
                state.size = Some(content.size);
            });
            in_flight = toplevel
                .send_pending_configure()
                .map(|serial| (serial, Instant::now()));
        }
        self.resizing = Some(Resize {
            window,
            edges,
            start,
            frame,
            content,
            target: content.size,
            sent: content.size,
            in_flight,
            released: None,
        });
    }

    /// The pointer moved during a resize: the new content size goes to the client as soon as
    /// it answered the configure before; the position follows its commits.
    pub fn resize_to(&mut self, location: Point<f64, Logical>) {
        let Some(mut resize) = self.resizing.take() else {
            return;
        };
        let (min, max) = limits(&resize.window);
        let bar = crate::decorations::titlebar_height(&resize.window);
        let frame = resized(
            resize.frame,
            resize.edges,
            location - resize.start,
            bar,
            min,
            max,
        );
        let frame = match resize
            .window
            .toplevel()
            .and_then(|toplevel| crate::window_appearance::increment_of(toplevel.wl_surface()))
        {
            Some(increment) => snapped(frame, resize.frame, resize.edges, bar, increment, min, max),
            None => frame,
        };
        let min_h = min.h.max(MIN_CONTENT).saturating_add(bar);
        let frame = below_top(frame, resize.edges, self.usable_area().loc.y, min_h);
        resize.target = Size::from((frame.size.w, frame.size.h.saturating_sub(bar).max(1)));
        flush(&mut resize);
        self.resizing = Some(resize);
    }

    /// End of the resize (button release): the last size without the resizing state; the
    /// client's answer is still anchored.
    pub fn end_resize(&mut self) {
        let Some(mut resize) = self.resizing.take() else {
            return;
        };
        let Some(toplevel) = resize.window.toplevel() else {
            return;
        };
        let target = resize.target;
        toplevel.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Resizing);
            state.size = Some(target);
        });
        let now = Instant::now();
        resize.in_flight = toplevel
            .send_pending_configure()
            .map(|serial| (serial, now));
        resize.sent = target;
        resize.released = Some(now);
        if resize.in_flight.is_some() {
            self.resize_settling = Some(resize);
        }
    }

    /// A commit of `surface` (with or without a new buffer): if it is the window being
    /// resized, the anchored sides stay still and the next size can leave.
    pub fn resize_commit(&mut self, surface: &WlSurface, new_buffer: bool) {
        let is_window = |resize: &Resize| {
            resize
                .window
                .toplevel()
                .is_some_and(|t| t.wl_surface() == surface)
        };
        if let Some(mut resize) = self.resizing.take() {
            if is_window(&resize) {
                self.anchor(&resize);
                answered(&mut resize, surface, new_buffer);
                flush(&mut resize);
            }
            self.resizing = Some(resize);
        }
        if let Some(mut resize) = self.resize_settling.take() {
            let moved = self
                .moving
                .as_ref()
                .is_some_and(|(window, _)| *window == resize.window);
            let expired = resize
                .released
                .is_some_and(|at| at.elapsed() > SETTLE_TIMEOUT);
            if moved || expired || !resize.window.toplevel().is_some_and(|t| t.alive()) {
                return;
            }
            if is_window(&resize) {
                self.anchor(&resize);
                answered(&mut resize, surface, new_buffer);
                if resize.in_flight.is_none() {
                    return;
                }
            }
            self.resize_settling = Some(resize);
        }
    }

    /// Places the window from the size it committed: the right side for the left edge and the
    /// bottom side for the top one stay where they were at the start.
    fn anchor(&mut self, resize: &Resize) {
        let Some(location) = self.space.element_location(&resize.window) else {
            return;
        };
        let size = resize.window.geometry().size;
        let placed = anchored(resize.content, resize.edges, location, size);
        if placed != location {
            self.space.map_element(resize.window.clone(), placed, false);
        }
    }
}

/// Where the content of `size` goes so that the sides opposite `edges` stay those of `start`.
pub fn anchored(
    start: Rectangle<i32, Logical>,
    edges: Edges,
    location: Point<i32, Logical>,
    size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let x = if edges.left {
        start
            .loc
            .x
            .saturating_add(start.size.w)
            .saturating_sub(size.w)
    } else {
        location.x
    };
    let y = if edges.top {
        start
            .loc
            .y
            .saturating_add(start.size.h)
            .saturating_sub(size.h)
    } else {
        location.y
    };
    Point::from((x, y))
}

/// The client committed a buffer for the configure in flight (or a later one).
fn answered(resize: &mut Resize, surface: &WlSurface, new_buffer: bool) {
    let Some((serial, _)) = resize.in_flight else {
        return;
    };
    if new_buffer && committed_serial(surface).is_some_and(|current| current >= serial) {
        resize.in_flight = None;
    }
}

/// Sends the size the pointer asks for, if it changed and nothing is waiting for an answer
/// (or the wait went stale).
fn flush(resize: &mut Resize) {
    if resize.target == resize.sent {
        return;
    }
    if resize
        .in_flight
        .is_some_and(|(_, at)| at.elapsed() < STALE_CONFIGURE)
    {
        return;
    }
    let Some(toplevel) = resize.window.toplevel() else {
        return;
    };
    let target = resize.target;
    toplevel.with_pending_state(|state| {
        state.size = Some(target);
    });
    if let Some(serial) = toplevel.send_pending_configure() {
        resize.in_flight = Some((serial, Instant::now()));
    }
    resize.sent = target;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Rectangle<i32, Logical> {
        Rectangle::new((200, 150).into(), (1000, 700).into())
    }

    fn at(x: f64, y: f64) -> Option<Edges> {
        edges_at(frame(), Point::from((x, y)))
    }

    const L: Edges = Edges {
        left: true,
        right: false,
        top: false,
        bottom: false,
    };

    #[test]
    fn measured_zones_on_the_four_sides() {
        // Left: x 198–202 (edge at 200).
        assert_eq!(at(197.9, 400.0), None);
        assert_eq!(at(198.0, 400.0), Some(L));
        assert_eq!(at(202.9, 400.0), Some(L));
        assert_eq!(at(203.0, 400.0), None);
        // Right: edge at 1200, 3 inside and 4 outside.
        assert_eq!(at(1196.9, 400.0), None);
        assert!(at(1197.0, 400.0).is_some_and(|e| e.right && !e.top && !e.bottom));
        assert!(at(1203.9, 400.0).is_some_and(|e| e.right));
        assert_eq!(at(1204.0, 400.0), None);
        // Top and bottom.
        assert!(at(600.0, 148.0).is_some_and(|e| e.top && !e.left && !e.right));
        assert_eq!(at(600.0, 147.9), None);
        assert_eq!(at(600.0, 153.0), None);
        assert!(at(600.0, 847.0).is_some_and(|e| e.bottom));
        assert!(at(600.0, 853.9).is_some_and(|e| e.bottom));
        assert_eq!(at(600.0, 854.0), None);
    }

    #[test]
    fn corners_for_16_pt() {
        let e = at(199.0, 165.0).unwrap();
        assert!(e.left && e.top);
        let e = at(199.0, 166.0).unwrap();
        assert!(e.left && !e.top);
        let e = at(215.0, 149.0).unwrap();
        assert!(e.left && e.top);
        let e = at(1201.0, 840.0).unwrap();
        assert!(e.right && e.bottom);
        let e = at(1190.0, 851.0).unwrap();
        assert!(e.right && e.bottom);
    }

    #[test]
    fn the_frame_follows_and_respects_the_limits() {
        let none = Size::from((0, 0));
        let both = Edges {
            left: true,
            top: true,
            ..Edges::default()
        };
        let r = resized(frame(), both, Point::from((50.0, 30.0)), 52, none, none);
        assert_eq!(r, Rectangle::new((250, 180).into(), (950, 670).into()));
        // The client's minimum (content 600 × 300): the right and bottom sides stay.
        let min = Size::from((600, 300));
        let r = resized(frame(), both, Point::from((900.0, 900.0)), 52, min, none);
        assert_eq!(r.size, Size::from((600, 352)));
        assert_eq!((r.loc.x + r.size.w, r.loc.y + r.size.h), (1200, 850));
        let right = Edges {
            right: true,
            ..Edges::default()
        };
        let max = Size::from((1100, 0));
        let r = resized(frame(), right, Point::from((500.0, 0.0)), 0, none, max);
        assert_eq!(r, Rectangle::new((200, 150).into(), (1100, 700).into()));
    }

    #[test]
    fn snapping_goes_to_the_nearest_whole_step_and_keeps_the_far_side() {
        let none = Size::from((0, 0));
        let inc = Increment {
            base: (10, 20),
            step: (7, 14),
        };
        let right = Edges {
            right: true,
            bottom: true,
            ..Edges::default()
        };
        // 1000x700 content + 52 bar, dragged by (+10, +20).
        let moved = resized(frame(), right, Point::from((10.0, 20.0)), 0, none, none);
        let r = snapped(moved, frame(), right, 0, inc, none, none);
        // 1010 -> 10 + 143 * 7 = 1011; 720 -> 20 + 50 * 14 = 720.
        assert_eq!(r.size, Size::from((1011, 720)));
        assert_eq!(r.loc, frame().loc);
        let left = Edges {
            left: true,
            ..Edges::default()
        };
        let moved = resized(frame(), left, Point::from((20.0, 0.0)), 0, none, none);
        let r = snapped(moved, frame(), left, 0, inc, none, none);
        assert_eq!(r.size.w, 983); // 980 -> 10 + 139 * 7 = 983
        assert_eq!(r.loc.x + r.size.w, 1200);
    }

    #[test]
    fn snapping_respects_the_limits_and_zero_steps() {
        assert_eq!(snap_axis(100, 0, 0, 0, 0), 100);
        // The minimum 95 wins over the nearest step (90).
        assert_eq!(snap_axis(92, 0, 10, 95, 0), 100);
        // The maximum 95 wins over the nearest step (100).
        assert_eq!(snap_axis(99, 0, 10, 0, 95), 90);
    }

    #[test]
    fn the_anchor_keeps_the_opposite_sides() {
        let start = Rectangle::new((200, 150).into(), (1000, 700).into());
        let left_top = Edges {
            left: true,
            top: true,
            ..Edges::default()
        };
        // Whatever size the client commits (even not the one asked), right and bottom stay.
        for (w, h) in [(1000, 700), (1050, 760), (600, 300), (1, 1)] {
            let p = anchored(start, left_top, start.loc, Size::from((w, h)));
            assert_eq!((p.x + w, p.y + h), (1200, 850));
        }
        // Right and bottom edges: the origin never moves.
        let right = Edges {
            right: true,
            bottom: true,
            ..Edges::default()
        };
        let p = anchored(start, right, Point::from((210, 160)), Size::from((10, 10)));
        assert_eq!(p, Point::from((210, 160)));
    }

    #[test]
    fn the_top_edge_stops_below_the_menu_bar() {
        let top = Edges {
            top: true,
            ..Edges::default()
        };
        let frame = Rectangle::new((100, 10).into(), (400, 340).into());
        let r = below_top(frame, top, 30, 60);
        assert_eq!(r, Rectangle::new((100, 30).into(), (400, 320).into()));
        // Already below: unchanged; other edges: unchanged.
        let low = Rectangle::new((100, 40).into(), (400, 310).into());
        assert_eq!(below_top(low, top, 30, 60), low);
        assert_eq!(below_top(frame, L, 30, 60), frame);
    }

    #[test]
    fn fixed_sides_do_not_resize_and_room_follows_the_limits() {
        let all = Edges {
            left: true,
            top: true,
            ..Edges::default()
        };
        let fixed_w = Room::resizable(all, Size::from((300, 100)), Size::from((300, 0)));
        assert!(!fixed_w.left && fixed_w.top);
        let fixed = Room::resizable(all, Size::from((300, 200)), Size::from((300, 200)));
        assert!(!fixed.any());
        let room = Room::of(
            Size::from((300, 250)),
            Size::from((300, 100)),
            Size::from((0, 250)),
        );
        assert_eq!(
            room,
            Room {
                grow_w: true,
                shrink_w: false,
                grow_h: false,
                shrink_h: true
            }
        );
    }

    #[test]
    fn measured_corner_zones() {
        // Frame 200..1200 × 150..850. Bottom-right diagonal from 10 pt inside to 1 pt past.
        let br = |x, y| at(x, y).is_some_and(|e| e.right && e.bottom);
        assert!(br(1190.0, 840.0));
        assert!(br(1201.0, 851.0));
        // Past the corner on both axes the zone is only ~2 pt deep.
        assert_eq!(at(1202.0, 851.0), None);
        assert_eq!(at(1201.0, 852.0), None);
        assert_eq!(at(1203.0, 853.0), None);
        // Top-left from 1 pt outside to 8 pt inside.
        let tl = |x, y| at(x, y).is_some_and(|e| e.left && e.top);
        assert!(tl(199.0, 149.0));
        assert!(tl(208.0, 158.0));
        assert!(!tl(209.0, 159.0));
        // Inside, past the corner square and the 3 pt bands: the content.
        assert_eq!(at(212.0, 400.0), None);
    }
}
