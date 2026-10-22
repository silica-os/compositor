# Stage view

Silica's stage view keeps the active window in place and moves the other app groups into a strip
of tilted thumbnails at the left edge. Measured on macOS 27 (1 pt = 1 px, 2560 × 1440, neutral
gray background). Reference: the stage view notes and SH-SM-02.

![Stage view strip](diagrams/stage-view-strip.svg)

## Layout (ref: stage view › observations; SH-SM-02)

| Property | Value |
|---|---|
| Active window | stays where it was, same size |
| Inactive groups | thumbnails in a vertical strip at the left edge, ~15–16 pt from the edge |
| Thumbnail box | about 84–89 × 97 pt for an ~800 × 560 window (perspective and shadow dominate; the aspect ratio is only roughly kept) |
| Strip | vertically centred on the screen (centre ~15–20 pt below the screen centre); group pitch 125 pt |
| Tilt | rotation about a vertical axis with the near edge on the left: the right edge is ~0.88 the height of the left edge; top edge slopes down ~4 pt, bottom edge up ~9 pt |
| App icon | ~32 pt, overlapping the thumbnail's lower left corner (partly off screen) |
| Desktop widgets | stay visible; the strip overlaps their column |
| Wallpaper | visible, not blurred |

## Switching groups (ref: SH-SM-02)

- Clicking a thumbnail starts the movement ~104 ms after the press (~40 ms after release).
- The clicked group grows from the strip to its own frame while the current group shrinks into
  the strip; both finish at ~560–600 ms.
- The outgoing group takes the slot of the clicked one; other thumbnails do not move.
- Turning stage view off returns every window to its frame.

## Not measured

- The activation animation; more than two groups; dark-specific differences.
