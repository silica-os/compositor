# Silica specification

This directory is the public specification that the shell and the compositor are built against. It
records what macOS 27 does, measured by **watching it from the outside**. Silica follows a
clean-room process: someone uses a real Mac, takes screenshots and screen recordings, and
measures sizes, colours and timings. This text restates those numbers and behaviours in our
own words, under Silica's names.

The raw material (screenshots, videos, measurement scripts and session notes) stays in the
private repository `silica-reference` and is never published here. Every section of this
spec carries the **measurement ID** it comes from, for example `MU-21`, `SH-CC-25` or
`TIP-01`. The same ID appears in issues and code comments, so a value can be traced back to
its measurement without the capture.

## Names

Silica's components have their own names. The spec uses them for the observed macOS
counterparts:

| Silica | What it is |
|---|---|
| The compositor | The compositor: windows, glass, animations |
| Faro | the search panel opened from the menu bar or with ⌘Space |
| Panorama | the overview of all windows and spaces |
| Launcher | the full-screen grid of apps |
| Control Center | the panel of system toggles and sliders |
| Notification Center | the panel of notifications and widgets |
| Mustache | the file manager |
| nearby sharing | sending files to nearby devices |
| logo menu | the first menu of the menu bar |

## Units and conventions

- **Lengths in pt.** Measurements were taken at 1× scale, so 1 pt equals 1 px on the
  reference display. Values below about 0.2 pt cannot be resolved at that scale.
- **Colours in sRGB hex** (`#rrggbb`). They were converted from the display profile of the
  captures. Alpha is written separately, as a fraction or a percentage.
- **Light and dark.** Values are given for both appearances. When a table has a single
  column, the value is the same in both.
- **Times in ms.** A recording of the display runs at 75 Hz, so one frame is 13.3 ms, and
  timings are accurate to about one frame. Times start at the input event: the press, the
  key, or the pointer arriving.
- **Curves.** "Decelerating" means fast at first and slow at the end, "accelerating" means
  the opposite, and "spring" means it overshoots and settles.
- **Provisional.** A value marked *provisional* was estimated rather than measured, or was
  measured only once. It will change when a new measurement arrives.
- **Coordinates** are relative to the element described, with the origin at its top left,
  unless a section says otherwise.

## How the measurements were taken

- **Machine.** A Mac mini (M1) running macOS 27.0.1, on a 2560 × 1440 monitor at 75 Hz,
  scale 1×.
- **Wallpaper.** Neutral grey `#7a7b80` unless stated otherwise. The glass materials were
  also measured over generated test wallpapers: solid black and white, stripes, gradients and
  a red patch.
- **Settings.** Default settings. The Dock is at the bottom and always visible, its size
  slider gives 63 pt cells, and magnification is on.
- **Pointer, keyboard and accessibility.** Synthetic pointer and keyboard events drove the
  interface, and a listen-only event log was synchronised with the screen recordings to
  time the animations. Geometry and actions were also read through the accessibility tree.
- **What was never used.** No system file, resource, icon, sound or text was read or
  copied.

## Diagrams

The spec has no screenshots. Where geometry matters, `diagrams/` holds SVG drawings made
from the numbers:

- dimensioned boxes;
- outlines drawn with the measured parameters;
- swatches of the measured colours.

The diagrams are drawn to scale in pt, with one SVG unit equal to 1 pt unless the drawing
says otherwise. Labels are in English.

## Files

| File | Area |
|---|---|
| [menubar.md](menubar.md) | Menu bar, menus, status items and status menus |
| [shell-config.md](shell-config.md) | Shell configuration: the Settings service keys and who applies them |
| [dock.md](dock.md) | Dock: geometry, glass, magnification, animations, behaviour |
| [dock-stacks.md](dock-stacks.md) | Folder stacks in the Dock: fan, grid, list |
| [controlcenter.md](controlcenter.md) | Control Center: grid, modules, sliders, details |
| [notifications.md](notifications.md) | Banners and Notification Center |
| [hud.md](hud.md) | Volume and brightness HUD |
| [screenshot-ui.md](screenshot-ui.md) | Screenshots (⇧⌘3, ⇧⌘4, ⇧⌘5), force quit, character viewer |
| [glass.md](glass.md) | Glass materials: blur, colour, refraction, edges |
| [tip.md](tip.md) | The tips of menus and popovers |
| [windows.md](windows.md) | Windows: corners, shadows, border, buttons, animations |
| [dialogs.md](dialogs.md) | System alerts and dialogs |
| [spaces.md](spaces.md) | Spaces: switching, adding, removing, full screen, dragging |
| [panorama.md](panorama.md) | Panorama, App Windows, Show Desktop, hot corners |
| [app-switcher.md](app-switcher.md) | The app switcher panel |
| [launcher.md](launcher.md) | Launcher: layout, categories, search |
| [faro.md](faro.md) | Faro, the search panel |
| [stage-view.md](stage-view.md) | Stage view: the strip of window groups |
| [displays.md](displays.md) | Several displays |
| [shortcuts.md](shortcuts.md) | System shortcuts and their Silica actions |
| [lock.md](lock.md) | Lock screen: layout, clock, password field, hint, transitions, shake |
| [misc.md](misc.md) | Shared panel material, alerts, context menus, launch bounce, system panels |
| [icon-geometry.md](icon-geometry.md) | Icon sizes, mask, shadow and edge light |
| [inventory-shell.md](inventory-shell.md) | Inventory of shell features with their status in Silica |
| [launch-services.md](launch-services.md) | The app registry (`Apps1`): sources, watching, associations |
| [launch.md](launch.md) | `silica-launch`: the trust check and the bubblewrap sandbox of app bundles |
| [trust.md](trust.md) | Gatekeeper (`Trust1`): signatures, quarantine, verdicts, Open Anyway |
| [volumes.md](volumes.md) | Disk images (`Volumes1`): verifying, mounting and ejecting `.sxv` files |
| [updates.md](updates.md) | Software update (`Updates1`): bundle feeds, Flatpak, bootc |
