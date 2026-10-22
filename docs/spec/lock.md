# Lock screen

Measured on macOS 27 (1 pt = 1 px, 2560 × 1440, scale 1×) over a photographic wallpaper, in the **dark appearance
only**. Colours over a photo are given together with the wallpaper colour behind them. Type sizes are estimates from
cap height and x-height. Reference: L-01 in silica-reference.

Not measured yet: the light appearance, increase contrast and reduce transparency, the login window (several users),
and the shutdown and restart confirmation.

## Layout (L-01)

Everything is centred on the screen's vertical axis except the help button and the status items. Coordinates are
screen coordinates in pt on a 2560 × 1440 display.

| Element | Geometry | Type |
|---|---|---|
| Date | baseline y 168 | about 40 pt, semibold |
| Clock | digit tops y 200, baseline y 341 (digit height 142 pt) | about 200 pt, bold, tabular digits |
| Avatar | circle 72 pt, y 1186–1257 | — |
| User name | baseline y 1287, 19 pt below the avatar | about 15 pt, bold, white |
| Password field | 160 × 28 pt capsule, y 1306–1333, 19 pt below the name baseline | — |
| Help button | 28 pt circle, 6 pt right of the field, same vertical centre | — |
| Bottom margin | 106 pt from the field's bottom edge to the screen edge | — |

- **Date to clock:** 32 pt from the date baseline to the top of the digits.
- **Clock spacing:** tight. The gaps between digit shapes are 13–22 pt; the colon is about 30 pt wide.
- **Centring:** the password field alone is centred. The help button hangs to its right and does not shift it.
- **Digits:** tabular. When a minute changes, the other digits do not move.

## Clock and date material (L-01)

- **Clock fill:** a pale glass fill that lets the wallpaper's colour through. It is about 65–85 % white over what is
  behind. For example, `#ffefed` over `#a99c9a` and `#d6ddfb` over `#82879b`.
  - Inside a stroke, the fill brightens towards the edges: `#ffffff` at the edge, `#d6ddfb` in the middle.
- **Clock edges:**
  - A 1 pt bright line just inside the outline.
  - A 1 pt dark rim just outside it, about 20–30 % darker than the wallpaper (`#807473` over `#a99c9a`).
  - No other shadow.
- **Date:** the same tinted white, about 83 % over the wallpaper (`#ebecff` over `#8a8b9e`), with no rim and no
  highlight.

## Password field and buttons (L-01)

| Part | Value |
|---|---|
| Field fill | the wallpaper behind it, blurred so strongly that no detail survives across 28 pt, plus about 30–40 % white |
| Field edge | a faint 1 pt lighter line; no dark border |
| Caret | 2 × 16 pt, white, 13 pt from the left edge, vertically centred; it blinks |
| Placeholder | about 13 pt, translucent white (about 60 %), left-aligned right after the caret |
| Dots | about 6.5 pt white discs, 8.4 pt pitch, first one 15 pt from the left edge; the caret follows the last dot |
| Submit button | inside the field's right end, shown only when the field is not empty: a 22 pt outlined circle (about 1.5 pt stroke) with a right arrow, about 40 % white, 3 pt from the field's right edge |
| Help button | a 28 pt disc of the field material holding a 22 pt outlined circle (about 1.5 pt stroke) with a question mark, about 55 % white |

## After failed attempts (L-01)

- **Shake:** every wrong password shakes the field and its help button. See "Wrong password shake" below.
- **Hint state:** after a repeated failure, the avatar and the name are **replaced in one step**, with no fade. They
  are replaced by:
  - **A hint paragraph.** Centred, about 13 pt semibold, white, line pitch 16 pt, wrap width about 256 pt. It
    explains that the password can be reset through the account recovery and that the computer will restart to do
    it.
  - **A link row, 10 pt below the paragraph.** A 16 pt white disc with an arrow cut out of it, a 6 pt gap, then about
    13 pt regular white text. The row starts the account recovery and is centred as a whole.
  - The field stays where it was, 29 pt below the link row.
  - **A help address line.** A single line, about 12 pt, about 60–70 % white, centred 12 pt below the field.
- **Typed text:** after a failure, the typed dots stay in the field and are shown as selected.

## Status items (L-01)

- **Top right, no menu bar and no background strip:**
  - the input source name, about 13 pt, white, y 8–18;
  - a keyboard glyph 16 × 11 pt, 7 pt after the name;
  - the network icon 14 × 11 pt, 20 pt after the glyph, dimmed to about 35 % white.
- **Right edge:** the group ends about 25 pt from the right edge. When a privacy indicator is shown, it takes the last
  ~12 pt.
- **Input source menu:** the input source item opens an ordinary menu with the shared panel material. The menu lists
  the input sources (the current one checked), then a separator, a submenu for more sources and an item that removes
  the current one. It opens over the status item.

## Notifications while locked (L-01)

- **Placement:** banners appear at the same place and size as on the desktop: 344 pt wide, 16 pt from the right edge,
  top at 46 pt, corners about 20 pt, the 32 pt icon 14 pt in, text from 58 pt. See the notifications spec.
- **Entrance:** they slide in from the right edge.
- **Content:** it is hidden. The banner shows the app name and a notification count instead of the body, and a
  two-line banner is about 58 pt tall.

## Wallpaper (L-01)

- **Picture:** the desktop wallpaper, at the same framing (no shift, no zoom) and **not blurred**.
- **Dimming:** about 5–7 % darker, as a dark overlay of roughly 6 %. Highlights lose slightly more than shadows: a level
  of 200 becomes about 185, and 20 stays 20.
- **Nothing else covers it:** no visible vignette and no extra shading behind the clock (the darkening ratio stays
  within 0.93–0.98 across the screen).
- **Static wallpaper:** a static wallpaper stays still while locked.

## Animation timings (L-01)

| Event | Duration | Shape |
|---|---|---|
| Lock: the lock screen appears | about 250 ms | almost linear, soft end (progress about 0.6 at half time) |
| Unlock: the lock screen disappears | about 220–250 ms to 95 %, then a 100–150 ms tail (about 350 ms in total) | decelerating |
| Minute change | about 900 ms | decelerating |
| Hint state replacing the avatar and name | one frame | cut |
| Display off with no input | about 9–10 s after locking | cut to black |

- **Lock:**
  - The desktop windows fade out a little faster than the wallpaper changes.
  - The clock, the date and the user block fade in. The user block runs slightly behind the clock.
  - Nothing slides or scales.
- **Unlock:** the user block first returns to its plain state in one frame: the hint goes away, the avatar comes back,
  the field is empty. Then the whole lock screen fades out to the desktop.
- **Minute change:** only the digits that change animate.
  - Each one rolls upwards: the old digit moves up and fades out, and the new one comes up from below and fades in,
    through a soft vertical fade mask.
  - The ones digit starts slightly after the tens digit.
- **Display off:** the delay is a setting. The value above was observed with the settings in use.
- **Wake:**
  - The wallpaper and the status items come back first.
  - About 150 ms later the date, the clock, the avatar and the name appear without animation. At this point the
    avatar and name sit 36 pt higher than usual, with no field.
  - About 20 ms later the field and the help button appear, and the avatar and name are in their normal place.

### Wrong password shake

- **What moves:** the field and the help button, horizontally only. The avatar and the name stay still.
- **Motion:** a damped oscillation, first to the left.
- **Amplitude:** the peaks are about 14 pt and decay to about 5 pt.
- **Rhythm:** about 3 cycles at about 8 Hz (period about 125 ms).
- **Total:** about 400 ms.

Peak offsets from one shake, in pt (about 20–45 ms sampling): −10, +14, −11, +10, −7, +6, then rest.

## What is not shown

- **With the settings in use** the screen showed no sleep, restart or shut down buttons, no user switcher and no
  message.
- **Those are optional settings** (see the lock screen settings in [misc.md](misc.md)). Their appearance is not
  measured.
