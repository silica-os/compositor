# Spaces

What Silica's spaces do, as measured on macOS 27 (1 pt = 1 px, 2560 × 1440 at 75 Hz, light
appearance, neutral gray background). Timings read from screen recordings are good to about
±13 ms (one frame). The spaces bar and opening Panorama are in [panorama.md](panorama.md).

Reference IDs in parentheses point to the private measurement notes (silica-reference).

## Switching spaces (ref: spaces › switching; ⌃→ / ⌃←)

Both spaces move sideways as one rigid strip; the Dock stays put on top.

| Property | Value |
|---|---|
| What moves | the whole space: windows, wallpaper, desktop widgets and the menu bar |
| What stays | the Dock, drawn above the moving strip |
| Gap between the two spaces | solid black band, 64 pt wide, full screen height, sharp edges, no shadow |
| Total travel | screen width + 64 pt (2624 pt on a 2560 pt screen) |
| Parallax | none: windows and wallpaper move by the same amount |
| Latency, key to first moving frame | ~130 ms (includes synthetic input delay) |
| Curve | critically damped spring, ω ≈ 13.2 s⁻¹ (0.0132 rad/ms), no overshoot |
| Progress | 50 % at ~115–125 ms, 90 % at ~285–295 ms, 99 % at ~505 ms |
| Last pixel settles | ~780 ms after the first movement |
| Peak speed | ~14 pt/ms, reached ~45 ms in |

Model: remaining distance `D(t) = L · (1 + ω t) · e^(−ω t)` with `L` the total travel. A pure
exponential does not fit; a short (~30 ms) acceleration precedes the long tail.

![Space switch](diagrams/spaces-switch.svg)

- **At the first or last space** the shortcut does nothing: no rubber-band, no bounce.
- **Reduce motion**: the switch becomes a cross-fade of about 550 ms (see
  [panorama.md](panorama.md)).

## Adding a space (ref: spaces › adding)

In the expanded spaces bar a "+" sits at the right end.

| Element | Value |
|---|---|
| "+" glyph | 20 × 20 pt cross, 2 pt stroke, `#4c4c4c`, 16 pt from the right screen edge, vertically centred at y 101.5 of the expanded bar; no background |
| Hover on "+" | a ghost thumbnail slides in from the right: fill same as an empty thumbnail (`#7a7b80` on the test background), sharp corners, left edge 80 pt from the screen edge, 90 pt tall, 5 pt lower than the thumbnails so the cross sits at its vertical centre |
| Ghost entry | spring ω ≈ 16.4 s⁻¹: 50 % at ~60 ms, 90 % at ~200 ms, done at ~410 ms |
| Press on "+" | ghost darkens to about 68 % brightness; on release it vanishes in one frame |
| New thumbnail | appears ~27 ms after release at the right edge (right side aligned with the "+"), then slides left into the next free slot |
| Slide curve | spring ω ≈ 16 s⁻¹: 50 % at ~105 ms, 90 % at ~240 ms, 99 % at ~410 ms, no overshoot |
| Other thumbnails | do not move in the reference; **user decision (#246):** they slide to the recentred row |

Thumbnail layout: 160 × 90 pt, pitch 192 pt (gap 32), top at y 52. The row is centred when the
bar expands. The reference did not recentre it when a space was added or removed later;
**user decision (issue #246):** Silica recentres it at once, animated, after adding and after
removing a space. Labels "Desktop N" are centred under each thumbnail. The
current space has a 3 pt accent ring (sRGB `#3b99fd`); new thumbnails have none.

Do not reproduce: the new thumbnail shows black for one frame before its content is drawn.

## Removing a space (ref: spaces › close button)

| Element | Value |
|---|---|
| Close button | appears only on the thumbnail under the pointer (also on the current space); white disc ~21–22 pt, centred on the thumbnail's top-left corner |
| Glyph | dark × of two 2 pt strokes, ~8 × 9 pt, gray `#606060`–`#878787` |
| Button shadow | soft, ~4–5 pt blur, ~1–2 pt down (estimated) |
| Hover on the thumbnail | nothing else changes (no ring, no tint) |
| Show delay | ~1.03 s measured; **user decision (#246): 0.5 s** (see [panorama.md](panorama.md)) |
| Press | disc darkens to about `#e3e3e3` |
| Removal | thumbnail, label and button vanish in a single frame; the remaining thumbnails stay in place |
| Windows of the removed space | move to the previous space (not observed here) |

## A full-screen window is a space (ref: spaces › full screen)

- Making a window full screen adds a space right after the space it came from (for example
  Desktop 1, the app, Desktop 2).
- Its thumbnail is 160 × 90 pt and shows just the window content scaled by 160 / screen width
  (0.0625), with no title bar or menu bar.
- The label is the window title, or the app name (when that is used is not determined).
- The window leaves its original space.

## Dragging a window onto the spaces bar (ref: spaces › dragging; PA-13, PA-42)

Inside Panorama a window thumbnail can be dragged to the bar.

| Step | Behaviour |
|---|---|
| Start | the press must be held ~1 s before the thumbnail detaches; a 3 pt accent outline marks it |
| Compact bar | expands to 170 pt with the same spring as hover expansion (ω ≈ 16.2 s⁻¹, 90 % at ~210 ms) |
| Other windows | rearrange immediately, larger, as if the dragged one were gone |
| Ghost "+" | slides in during the drag (same motion as on hover), slides out on drop |
| Scale while climbing | shrinks linearly with height: ~1.65 pt of width per pt climbed (scale −0.0027 per pt for a 600 pt window) |
| Over the bar | stops at about 0.066–0.073 of the window size and follows the pointer (pointer ~55 % down the miniature) |
| Over an existing thumbnail | the thumbnail darkens to about 70 % (`#55565a` on the test background), no ring |
| Over a gap or past the last thumbnail | a placeholder thumbnail opens (160 × 90, ~11 % lighter than normal thumbnails, slight vertical gradient, labelled with the app name, showing the miniature) and pushes the others apart; settled pitch ~190 pt |
| Placeholder opening | spring ω ≈ 17.2 s⁻¹, ~350 ms; appeared ~1 s after the pointer stopped (dwell dependence unknown) |

Drop results:

- **On an existing space**: the window moves to that space (it shows up shrunken in that
  thumbnail, which returns to its normal colour). The remaining windows grow back into place
  starting ~515 ms after release, spring ω ≈ 16.9 s⁻¹, done ~900 ms after release.
- **On a gap**: a new full-screen space is created there, labelled with the app name; the
  row settles at pitch 192 within ~60 ms.
- **Outside the bar**: the window goes back to its place in the layout (timing not measured).
- **Dragging a window to the screen edge outside Panorama** never switches space: it tiles
  the window into that half (or, on an edge shared with another display, moves it there).
- There is no menu command to move a window to another space; the window menu only offers
  moving it to another display ([displays.md](displays.md)).

## Not measured

- Switching in dark appearance.
- The full width of the ghost "+" (cut by the screen edge).
- The frame-by-frame entry of a dropped window into a space thumbnail, and the return when
  dropped outside.
- Whether the placeholder depends on dwell time.
- When a full-screen space uses the title versus the app name.
