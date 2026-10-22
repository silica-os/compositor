# Multiple displays

Measured on macOS 27 with two identical 2560 × 1440 displays at 1× (primary with menu bar and
Dock, secondary on its left), with separate spaces per display. Reference: PA-50, L-02.

## Menu bar on each display (ref: displays › menu bar)

- Every display has its own full menu bar (the active app's menus and the status area).
- The bar on the display **without** the active window is dimmed: text and status icons at
  ~35 % of their contrast (white text blends to ~35 % between the bar background and white). The
  bar background does not change.
- The dimming follows the active window, not the pointer.

## A window across two displays (ref: displays › straddling)

- With separate spaces, a window is drawn only on the display holding the larger part of it; the
  part on the other display is not shown.
- Moving it past the halfway point hands it to the other display.

## Panorama with two displays (ref: displays › overview)

- Panorama opens on both displays at once, each with its own spaces bar and its own windows.
- Dragging a window thumbnail across: it appears at full size under the pointer on the other
  display; both spaces bars expand to 170 pt; the destination display's wallpaper darkens to 50 %
  (the source display does not change). On release the window joins that display's space and is
  laid out with the others. A display left with no windows shows a centred "No windows available"
  capsule. The darkening lasts while Panorama is open.
- Switching space on one display never changes the other display's space (no slide); each display
  keeps its own current space.

## Moving a window from the menu (ref: displays › menu)

- The window menu offers "Move to <display name>" and "Move all to <display name>".
- The window keeps its size and its position relative to the display.

## Dock

- The Dock lives on the primary display. Placed on the left, it goes to the left edge of the whole
  desktop, which may be on another display.

## Not measured

- Displays sharing one set of spaces.
- Which display Faro opens on.
- The animation of "Move to"; dragging windows between displays outside Panorama.
- Several spaces on the secondary display.
