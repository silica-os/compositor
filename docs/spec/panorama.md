# Panorama, App Windows, Show Desktop, hot corners

Panorama is Silica's overview of all windows and spaces. Values measured on macOS 27
(1 pt = 1 px, 2560 × 1440, 75 Hz, neutral gray wallpaper `#7a7b80`). Timing is good to about
±13 ms. Saturated colours are given in sRGB. Spaces themselves are in [spaces.md](spaces.md).

## Overview scene (ref: missioncontrol › desktop)

- The wallpaper is **not dimmed or tinted**, in light or dark.
- The menu bar and the desktop widgets go away; the spaces bar takes the menu bar's place.
  The Dock stays exactly as it is.
- Windows keep their own look (live content, shadows); there is no title under them in
  Panorama (there is one in App Windows).
- Expanding the spaces bar does not re-lay out the windows.

## Spaces bar (ref: missioncontrol › compact / expanded bar; PA-14)

![Spaces bar](diagrams/panorama-bar.svg)

| | Compact | Expanded |
|---|---|---|
| Height | 40 pt, full width, at the top | 170 pt |
| Shadow below | soft, ~62 pt | soft, ~40 pt |
| Fill | Glass/Thick over the wallpaper under the bar (σ 16, saturation 1.5, tint `#FFFFFF` / `#2A2B30` at 76 %, 1 pt inside rim, rim light), light and dark | same |
| Content | space names side by side, centred on screen | thumbnails with names under them |
| "+" | 20 × 20 pt cross, 2 pt stroke, 16 pt from the right edge, centre y 20; light `#4c4c4c`, dark `#e5e5e5` | same cross, centre y 101.5 |

Compact bar entries:

- The current space's name sits in a capsule 24 pt tall, 7 pt from the top, ~10–11 pt
  horizontal padding, radius ~6–7 pt (estimated). Capsule fill: light `#87888e` with a 1 pt
  `#8d8e94` border; dark `#78797e` with a `#6e6f74` border.
- Text ~13–14 pt; black in light, white in dark. The other names have no capsule; text pitch ~91 pt.

Expanded bar:

- Thumbnails 160 × 90 pt (16:9), top at y 52, gap 32 pt (pitch 192), practically square
  corners. They show the space at 1/16 scale with its windows in their normal (not Panorama)
  layout, no menu bar or Dock.
- The current space has a 3 pt ring, 1 pt away from the thumbnail, sRGB `#3b99fd` in light and
  dark. Other thumbnails have none.
- Names under the thumbnails, centred, ~7 pt below, regular weight ~12–13 pt (cap height
  ~10 pt). Black in light, white in dark. With a single space the name has no number.
- Up to 12 spaces fit at pitch 192, centred. With 16 spaces (the maximum) the "+" disappears and
  thumbnails shrink to ~126 × 71 (pitch ~152, y 63–133) without names. With 15 the "+" returns
  and the row may overflow to the left.
- Reopening Panorama recentres the row. **User decision (issue #246):** the row also recentres
  at once, animated (same springs), after adding and after removing a space.

Behaviour:

- The bar expands when the pointer enters it (~30–50 ms after crossing y 40). It **does not
  collapse** again while Panorama is open.
- Expansion: ~440 ms, strong ease-out (25 % at the first frame, 80 % at ~105 ms, 90 % at
  ~170 ms); fits a critically damped spring ω ≈ 16.2 s⁻¹. Thumbnails appear during the expansion.
- Close button on a space thumbnail: shown only with the pointer on the thumbnail, after
  ~1.03 s of dwell (**user decision, issue #246: 0.5 s**, diverging from the measurement on
  purpose), in a single frame; moving to a neighbour moves it immediately; leaving into a
  gap hides it immediately. Holding ⌥ shows it on every thumbnail. Look: see
  [spaces.md](spaces.md) (white ~22 pt disc on the top-left corner, gray `#878787` ×, same in
  dark).
- Removing a space: the thumbnail vanishes in one frame (~45 ms after release), then the others
  close the gap and recentre (each side moves half a pitch) with a spring starting ~120 ms after
  the press, ω ≈ 16.5 s⁻¹, done ~500 ms after the press. When the count drops from 16 to 15 the
  thumbnails also grow back (ω ≈ 24.7 s⁻¹, ~350 ms) and the "+" slides back in ~100 ms later.

## Opening and closing (ref: missioncontrol › timing; PA-14)

| | Value |
|---|---|
| Curve | ease-out with a long tail, windows and bar together; fits a critically damped spring ω ≈ 22–24 s⁻¹ |
| Opening | 50 % at ~40 ms, 90 % at ~130 ms, 98 % at ~230 ms; ~0.3–0.35 s with many windows |
| Closing (Esc) | 50 % at ~55 ms, 90 % at ~140 ms, within 2 pt at ~210 ms, still at ~300 ms |
| Menu bar on open | stays in place and fades out (~220 ms) while the spaces bar slides down from under it (~240 ms) |
| Menu bar on close | spaces bar slides up in ~150 ms, menu text fades back in ~250 ms |

## Window layout (ref: PA-10)

Every window gets the **same scale**, never above 1. The stacking/activation order does not
change the result, and hiding the Dock does not either (its area stays reserved).

Usable area **A**: x 20…(width − 20), y 80…(top of the Dock glass − 15). Without a Dock on that
display, down to 20 pt from the bottom.

Rules observed:

1. Minimum gap between thumbnails: ~14 pt (13–16 vertically), in screen points (not scaled).
2. Overlapping windows are pushed apart along the line joining their centres until one axis has
   the gap. Windows already apart do not move.
3. Coincident centres: the window further back goes towards the top left, the other towards the
   bottom right along the diagonal.
4. If separating would leave A on one axis, windows slide along the other axis instead.
5. The scale drops below 1 only when the blocked windows touch **two adjacent** edges of A (one
   vertical and one horizontal). Against a single edge, or two opposite edges, they slide.
6. A single window that fits stays where it is if it lies within A (vertically ~98…~1324). If it
   sticks out, its position is mirrored back inside the edge it crossed (e.g. 10 pt past the left
   edge → 10 pt inside), then clamped to that vertical band. A window larger than A shrinks to fit
   and is centred horizontally.
7. From about five coincident windows on, they are arranged in a grid rather than a diagonal.

Measured scales for reference sets (cascades of 720 × 500 windows): 2 → 0.958, 3 → 0.897,
4 → 0.808, 5 → 0.760, 6 → 0.706, 8 → 0.647, 10 → 0.586, 12 → 0.532; 20 mixed windows → 0.432.
Halving the cascade step does not change the scale.

Silica's model (pairwise push along the centre vector, Jacobi steps, scale reduced in small
steps only while blocked windows touch two adjacent edges, then a second pass at fixed scale)
reproduces 1–3 windows and pairs within ~10 pt. **Long cascades and many coincident windows are
provisional**: the reference shrinks them more and keeps them near their original diagonal.

## Window thumbnails (ref: PA-11)

| | Value |
|---|---|
| Hover ring | 4–5 pt, sRGB `#3b99fd`, 1–2 pt outside the window edge; same in dark; the thumbnail does not grow |
| Hover title | capsule centred on the thumbnail (~29 pt tall, ~14 pt side margins, fully rounded), black text, translucent white background (~35–40 % white) |
| Timing | ring and title appear in the same frame the pointer enters, and jump straight to the next thumbnail |
| Close button | none on window thumbnails, even with ⌥ |
| Click | Panorama closes ~100 ms after the press; that window is activated; **all** windows return to place together, spring ω ≈ 22 s⁻¹ (50 % ~76 ms, 90 % ~175 ms, 99 % ~300 ms), scale back to 1 on the same curve |
| Click on empty wallpaper | closes, no order change |
| Click on a Dock icon | closes and activates that app |

## Keyboard inside Panorama (ref: PA-12)

| Key | Effect |
|---|---|
| Arrows, Tab, ⇧Tab, Space, Return, ⌘W | nothing (no keyboard selection) |
| ⌃← / ⌃→ | switch space while Panorama stays open: the destination scene slides in, first frame ~130 ms after the key, spring ω ≈ 12.7 s⁻¹, settled ~800 ms after the key; the old scene leaves fast (~170 ms) |
| Esc | closes |

## App Windows (ref: missioncontrol › App Windows; PA-30)

- Shows only the current app's open windows on this space; other apps' windows disappear; menu
  bar and widgets hidden; Dock unchanged. Minimized windows are not shown.
- One window: centred horizontally, scaled ~0.947, centre slightly above the screen centre.
- Several windows: one row of equal cells with evenly spaced centres (pitch 840 pt for three
  windows on 2560 pt), all **top-aligned**; each window is scaled on its own to fit its cell
  (never above 1).
- Title under each window, ~14 pt below, ~12 pt white text with a shadow.
- Wallpaper not dimmed; a gentle darkening ~102 pt tall at the top edge (`#6f7075` → wallpaper),
  same in light and dark.
- Window shadow below: ~50 pt.
- Hover: same blue ring as Panorama, no title capsule.
- Tab moves to the next app in switcher order; an app with no window here shows a centred capsule
  "No windows available".
- Opening: 50 % at ~60 ms, 90 % at ~140 ms, end ~270 ms; closing ~330 ms, same ease-out shape.

## Show Desktop (ref: PA-31)

- Windows slide off towards the nearest side edge; a strip of ~15 pt per side stays visible
  (window border and shadow, dark gray ~`#5d5d5d`). Desktop widgets show even if they are off.
- Leaving is very fast (~70 ms). Coming back takes ~350 ms, ease-out with a tail.
- Clicking either strip brings all windows back, order unchanged.

## Hot corners (ref: PA-32)

- The action fires 70–110 ms after the pointer reaches the corner pixel, whether it arrives
  slowly or fast. A ~90 ms touch is enough; leaving immediately does not cancel.
- Visiting the corner again toggles back (closes Panorama, brings windows back).
- Only the corner pixel is sensitive.

## Reduce motion (ref: PA-51)

- Panorama: no movement, a cross-fade between desktop and overview: opening ~550 ms (50 % at
  ~70 ms, 90 % at ~250 ms), closing ~520 ms starting ~370 ms after Esc.
- Space switch: a fade (~550 ms, ease-out) instead of the slide.
- Show Desktop: a ~560 ms fade; windows do not move.

## Dark appearance

Same layout and colours of rings and close buttons; bar fills as in the table above; names in
white; wallpaper unchanged.

## Two displays (ref: PA-50)

With separate spaces per display, Panorama opens on both displays at once, each with its own
spaces bar (tinted by its wallpaper) and its own windows. More in [displays.md](displays.md).

## Not measured

- The exact layout for long cascades and many windows (see above).
- Blur of the wallpaper (solid test backgrounds).
- Window shadow parameters in the overview.
- App Windows with Reduce motion; gestures (no trackpad).
