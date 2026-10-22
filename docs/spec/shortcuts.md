# Keyboard shortcuts

Silica's system shortcuts and the actions they run. Every action is a command ("everything is a
command"). The key combinations follow what was observed on macOS 27 (ref: K-01), adapted to PC
keyboards: **⌘ is Super** (the Logo key), ⌥ is Alt, ⌃ is Ctrl. The reference uses an fn key that
PC keyboards often lack for window tiling; Silica binds those with Super instead (marked
"provisional"). "Off" means the binding exists but is disabled by default.

## Spaces, Panorama, Dock

| Keys | Silica action | Default |
|---|---|---|
| F9 (Super+Up also) | open or close Panorama | on |
| F10 (Super+Down also) | App Windows for the current app | on |
| F11 | Show Desktop | on |
| Ctrl+← / Ctrl+→ | previous / next space | on |
| Ctrl+1 … Ctrl+9 | go to space N | off |
| — | show Notification Center | off |
| — | toggle Do Not Disturb | on (no key) |
| — | toggle stage view | on (no key) |
| Super+Alt+D | toggle Dock auto-hide | on |
| Fn+Q (provisional: Super+Shift+N) | new quick note | on |

## Windows

| Keys | Silica action |
|---|---|
| Super+M | minimize |
| Super+W / Super+Q / Super+N | close window / quit app / new window (app menus) |
| Super+H / Super+Alt+H | hide app / hide others |
| Super+\` | next window of the same app |
| Super+Ctrl+F | full screen |
| Ctrl+Super+F (reference: ⌃fn F; provisional) | fill the screen |
| Ctrl+Super+C (reference: ⌃fn C; provisional) | centre |
| Ctrl+Super+R (reference: ⌃fn R; provisional) | back to the previous size |
| Ctrl+Super+←/→/↑/↓ (reference: ⌃fn arrows; provisional) | left / right / top / bottom half |
| Ctrl+Shift+Super+←/→/↑/↓ (provisional) | arrange with the other window: left and right, right and left, top and bottom, bottom and top |
| — | the four quarters, arrange in quarters, full-screen left / right (no default key) |
| Super+Alt+Esc | Force Quit window |

## Faro, switcher, input

| Keys | Silica action |
|---|---|
| Super+Space | Faro |
| Super+Alt+Space | Mustache search window |
| Super+Tab / Super+Shift+Tab | app switcher forward / backward ([app-switcher.md](app-switcher.md)) |
| Ctrl+Space | previous input source |
| Ctrl+Alt+Space | next input source |
| Ctrl+Super+Space | characters and emoji (a small floating bar next to the insertion point; the full panel is reachable from it) |
| Ctrl+Super+Q | lock screen (not observed) |

## Screenshots

| Keys | Silica action |
|---|---|
| Super+Shift+3 | whole screen to a file |
| Ctrl+Super+Shift+3 | whole screen to the clipboard |
| Super+Shift+4 | selected area to a file (Space switches to window capture) |
| Ctrl+Super+Shift+4 | selected area to the clipboard |
| Super+Shift+5 | screenshot and recording options |

## Keyboard navigation (focus)

| Keys | Silica action |
|---|---|
| Ctrl+F1 | toggle full keyboard access |
| Ctrl+F2 | focus the menu bar |
| Ctrl+F3 | focus the Dock |
| Ctrl+F4 | focus the active or next window |
| Ctrl+F5 | focus the window toolbar |
| Ctrl+F6 | focus the floating window |
| Ctrl+F7 | change how Tab moves focus |
| Ctrl+F8 | focus the status menus |
| Super+< | focus the next window |
| Ctrl+Return | show the context menu |
| Super+Shift+/ | show the Help menu |

## Display and sound

| Keys | Silica action |
|---|---|
| brightness keys (or F14 / F15) | brightness down / up, with the HUD |
| volume keys | volume, with the HUD; Alt+Shift with the key for quarter steps |

## Accessibility

| Keys | Silica action | Default |
|---|---|---|
| Super+F5 | toggle the screen reader | on |
| Super+Alt+F5 | accessibility controls panel | on |
| Super+Alt+8 | toggle zoom | — |
| Super+Alt+= / Super+Alt+- | zoom in / out | — |
| Super+Alt+\\ | toggle zoom smoothing | — |
| Ctrl+Super+Alt+8 | invert colours | off |
| Ctrl+Super+Alt+. / , | increase / decrease contrast | — |

Other accessibility actions (live captions, type to speak, speak selection, pointer locator…) exist
without default keys.

## Settings defaults (ref: K-01, K-03)

- Key repeat: rate 6 and delay 4 on the settings scale (values in ms not measured).
- The fn/globe key alone does nothing by default.

## File manager (Mustache)

Mustache's menu shortcuts are specified in the Mustache repository. In short: File (Super+N,
Super+Shift+N new folder, Super+T, Super+O, Super+W, Super+I, Super+D, Super+Y preview,
Super+Backspace to Trash, Super+E eject), Edit (undo/redo, cut/copy/paste, Super+Alt+V move,
Super+A, Super+F), View (Super+1…4 views, Ctrl+Alt+Super+1…7 sort, Super+Alt+1…7 clean up by,
Super+J view options), Go (Super+[ / ], Super+↑, Super+Shift+letters for places, Super+K
connect), Window (minimize, tiling as above, Ctrl+Tab between tabs).

## Not observed

- Lock screen, the full characters panel, the globe-key language switcher, key repeat in ms.
