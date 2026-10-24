# Control channel

The compositor opens the channel with `--control PATH`, a Unix socket (its folder 0700, the
socket 0600, so only the compositor's user reaches it), and announces it on stdout with a line
`SILICA_COMPOSITOR_CONTROL=PATH`. The usual path is `$XDG_RUNTIME_DIR/silica/control.sock`,
where `silica-cli` looks by default. The channel is for agents and tests; without `--control`
it is closed.

## Protocol

One JSON line per message. The client sends a request, the compositor answers with the same `id`.
Requests on the same connection are served in order.

```
→ {"id":1,"command":"window.move","params":{"x":10,"y":20}}
← {"id":1,"ok":true,"result":null}
← {"id":2,"ok":false,"error":"no window with id 9"}
```

A request over 1 MiB closes the connection. Unknown parameters are an error.

## Commands

The catalog is in `crates/control/src/commands.rs`; `engine.commands` returns it with the
parameter schemas. Every catalog id is run by the compositor (test
`every_catalog_command_is_implemented`).

| Command | Parameters | Result |
| --- | --- | --- |
| `ui.screenshot` | `path?` | `width`, `height`, `path` or `png_base64` |
| `ui.animation_scale` | `scale` (0–1000) | — (multiplies the animation durations; 0 turns them off) |
| `ui.glass_quality` | `quality` (`auto`, `full`, `frosted`, `opaque`) | `quality` in use |
| `dock.icons` | `icons` (`app_id`, `x`, `y`: the center of each app icon in the Dock surface) | `icons` kept; the shell sends it, windows then minimize into their app's icon (`dock.minimizeToApp` of the Settings service) |
| `session.lock` | — | locks the session (`Logo+Ctrl+Q`) and asks the shell's locker to take the lock; refused when no shell is running. There is no command to unlock |
| `engine.commands` | — | the catalog |
| `engine.execute` | `id`, `params?` | the command's result |
| `window.list` | — | windows from top to bottom, then the minimized ones: `id`, `title`, `app_id`, `x`, `y`, `width`, `height` (of the content, without the bar), `focused`, `minimized`, `fullscreen` |
| `input.gesture` | `kind` (`swipe`, `pinch`), `fingers`, `phase?` (`begin`, `update`, `end`; without: the whole gesture), `dx`, `dy`, `scale`, `cancelled` | `command` run at the end (null if none). Swipes go to the app; thumb and three fingers pinching open the launcher. The gestures of spaces and Mission Control belong to the Dock |
| `switcher.next`, `switcher.previous` | — | `forwarded`: opens the app switcher or moves the selection (Logo+Tab, Logo+Shift+Tab); while it is open, releasing the Logo key runs `switcher.commit` and Esc `switcher.cancel` |
| `switcher.commit`, `switcher.cancel` | — | `forwarded`: closes the switcher activating the chosen app, or without changing app |
| `switcher.expose` | — | `forwarded`: closes the switcher and opens App Exposé of the chosen app (Up or Down while the switcher is open); releasing the Logo key afterwards changes nothing |
| `launcher.toggle` | — | `forwarded`: opens or closes the app launcher (Faro in grid mode) |
| `app.quit` | — | `closed`: asks all the windows of the frontmost app to close (Logo+Q) |
| `app.hide` | `app_id?` | `app_id`, `hidden`: hides the frontmost app (Logo+H): its windows leave the screen and stay in the shell's list; activating one brings the app back |
| `app.hide_others` | — | `hidden`: hides every app but the frontmost one (Logo+Alt+H) |
| `app.show_all` | — | `shown`: every hidden app comes back below the one in front |
| `menu_bar.state` | — | `visible`, `room` (the titles' room before the status items) and `surfaces` as laid out, left to right: `kind` (`titles` or `item`), `system`, `order`, `x`, `width`, the owner's `pid` (silica-menu-bar-v1, `docs/protocols.md`) |
| `faro.toggle` | — | `forwarded`: shell components that received it (Logo+Space) |
| `notifications.toggle` | — | `forwarded`: opens or closes the Notification Center (click on the clock) |
| `controlcenter.toggle` | — | `forwarded`: opens or closes the Control Center (icon in the bar) |
| `system.status` | — | state from silica-daemon (`docs/daemon.md`, System1): `wifi.*`, `bluetooth.*`, `battery.*`, `brightness`, `audio.*`, `dnd`; error if the daemon does not answer |
| `system.wifi`, `system.bluetooth`, `system.mute`, `system.dnd` | `enabled` | `queued`: the call runs on a thread, without waiting for the daemon |
| `system.volume`, `system.brightness` | `value` (0–1) | `queued` |
| `system.volume_up`, `system.volume_down`, `system.mute_toggle` | — | `queued`; steps of 1/16, raising unmutes (volume and mute keys) |
| `system.brightness_up`, `system.brightness_down` | — | `queued`; steps of 1/16 (brightness keys) |
| `system.volume_up_fine`, `system.volume_down_fine`, `system.brightness_up_fine`, `system.brightness_down_fine` | — | `queued`; a quarter step, 1/64 (⌥⇧ with the keys) |
| `system.keyboard_brightness` | `value` (0–1) | `queued`; the keyboard backlight (UPower), `false` without one |
| `system.keyboard_brightness_up`, `system.keyboard_brightness_down` | — | `queued`; steps of 1/16 (keyboard backlight keys) |
| `glass.list` | — | surfaces with glass (`silica-glass-v1`): `surface`, `x`, `y`, `regions` (`x`, `y`, `width`, `height`, `radius`, `material`, `opacity`, `shape`, `group`, `profile` (null: from the material), `shadow`, `rotation` (degrees clockwise about the centre; `x` … `height` are then the upright rectangle)) |
| `layer.list` | — | shell layers: `namespace`, `layer`, `x`, `y`, `width`, `height`, `exclusive_zone`, `focused` |
| `window.focus` | `id?` | — (also brings back a minimized window) |
| `window.close` | `id?` | — |
| `window.move` | `id?`, `x`, `y` | — |
| `window.minimize` | `id?` | — (Logo+M) |
| `window.fullscreen` | `id?` | — (Logo+Ctrl+F; again to go back) |
| `app.launch` | `command` or `app` (a desktop entry id, run like the Dock does), `shell?` | `pid` (with `shell` the process sees the shell protocols) |
| `session.quit` | — | — |
| `input.pointer_move` | `x`, `y` | — |
| `input.pointer_button` | `button?`, `pressed` | — |
| `input.click` | `x`, `y`, `button?` | — |
| `input.scroll` | `x`, `y`, `dx?`, `dy?` | — ; the wheel at that point (logical pixels, 15 = one notch), like a real one: an open menu may drop it (#149) |
| `input.key` | `key` (xkb name or character), `pressed?` | — ; like a real key, catalog shortcuts included |
| `input.type` | `text` | — |
| `perf.frames` | `reset?` | per surface: `surface`, `pid`, `frames`, `mean_ms`, `p50_ms`, `p95_ms`, `p99_ms`, `max_ms`, `slow`, `period_ms`, `startup_ms` |
| `perf.process` | `pid` | `pid`, `rss_kib`, `pss_kib` |
| `keyboard.state` | — | the keyboard settings in use: `repeatDelay`, `repeatRate`, `layouts`, `model`, `options`, `active` (index of the active source) |
| `input.state` | — | the pointer settings in use (`mouse`, `touchpad`: `speed`, `naturalScroll`, `scrollSpeed`, `leftHanded`, `secondaryClick`, `tapToClick`, `disableWhileTyping`) and `devices` (`mouse`, `touchpad`: connected) |
| `idle.state` | — | `idleSeconds` since the last input, `displayOff`, and `displayOffMinutes`, `sleepMinutes`, `preventSleep` from `power.json` |
| `output.list` | — | the connected displays: `connector`, `identity` (EDID make, model and serial; the connector without EDID), `make`, `model`, `serial`, `modes` and `current` (`width`, `height`, `refresh` in Hz, `preferred`), `scale`, `transform`, `enabled`, `primary`, `widthMm`, `heightMm` |
| `output.configure` | `connector?`, `mode?` (`width`, `height`, `refresh?`), `scale?` (1–4, quarter steps), `transform?` (`normal`, `90`, `180`, `270`, `flipped`, `flipped-90`…) | the display after the change; applied at once and saved in `displays.json` for that display. Without `connector`: the display in use. The real session drives one display: the others are listed but not configurable yet; nested and headless cannot change mode |
| `output.color_temperature` | `kelvin` (1000–6500) | `kelvin`, `applied`: the night light on every display through the CRTC gamma ramp (6500 is neutral; `applied` false where there is no gamma: nested, headless, some virtual GPUs). silica-daemon drives it with the schedule |
| `keyboard.next_source` | — | `switched`, `active`: the next input source (⌃Space; with a single source the shortcut goes to the app) |
| `keyboard.select_source` | `index` (0–3) | `switched`, `active`: makes that input source the active one (the input menu of the menu bar and the lock screen); `false` with fewer than two sources or past the last |
| `perf.render` | `reset?` | The compositor's rendering per drawn frame (drm backend, headless with `SILICA_COMPOSITOR_HEADLESS_FRAMES=1`): `frames`, `mean_ms`, `p50_ms`, `p95_ms`, `p99_ms`, `max_ms`, and `stages` (`build`, `draw`, `submit`, `gpu`: `frames` and percentiles) |

The `perf.frames` measurements are the compositor's, the same for every toolkit: a frame is
a commit with a new buffer, the interval is the time between two frames of the same
surface, slow if it exceeds 1.5 refresh periods of the output. `startup_ms` goes from the
launch with `app.launch` (or `--spawn`) to the first buffer of the process or of one of its
descendants.

Without `id`, the window commands act on the focused one.

## Settings

the compositor applies the user's settings (`crates/compositor/src/config.rs`). Most are keys of the
Settings service of silica-daemon (`dev.frascella.Silica.Settings1`, `docs/daemon.md`,
`crates/compositor/src/settings_service.rs`, silica-os/silica-desktop#240): The compositor reads them all when the
service appears and follows `Changed`, applying each at once; the service refuses a value that
does not fit its schema. Until the service answers (and without it, in tests) the defaults
apply. The glass and the accessibility of the display come with the look of silica-daemon
(`look.glass`, `look.reduceTransparency`, `look.reduceMotion` of `System1`). Two settings are
still files of `$XDG_CONFIG_HOME/silica` (usually `~/.config/silica`), watched with inotify:
`displays.json` and `power.json`.

### keyboard.*

| Key | Type | Meaning |
| --- | --- | --- |
| `keyboard.repeatDelay` | integer, 100–3000 | ms before a held key repeats (default 375; without the service, `SILICA_COMPOSITOR_KEY_REPEAT` or 375) |
| `keyboard.repeatRate` | integer, 0–100 | repeats per second; 0 turns repeat off (default 11) |
| `keyboard.layouts` | list of strings, at most 4 used | the input sources: xkb layouts, a variant in parentheses (`de(nodeadkeys)`); the first is active at start. Empty: xkb's default (`XKB_DEFAULT_*`) |
| `keyboard.model` | string | xkb model (empty: default) |
| `keyboard.options` | string | xkb options, comma-separated (`caps:escape,compose:ralt`) |

A change of repeat is sent to every bound keyboard (`wl_keyboard.repeat_info`); a change of
`layouts`, `model` or `options` compiles a new keymap and sends it to the clients. A layout xkb
does not know keeps the keymap in use. ⌃Space (`keyboard.next_source`) moves to the next
source when there are two or more. With two or more, the compositor announces them to the shell: `keyboard.sources` (the xkb names,
comma-separated; empty with fewer than two) and `keyboard.source` (the index of the active one).

### displays.json

```json
{
  "outputs": {
    "DEL DELL U2720Q ABC123": {
      "mode": {"width": 2560, "height": 1440, "refresh": 59.95},
      "scale": 1.5,
      "transform": "normal"
    }
  }
}
```

Written by the compositor on `output.configure` (keeping the other keys of the file), read at start,
when the file changes and when a display is plugged in. The key is the display's identity
(`output.list` › `identity`), so a display gets its choice back on any connector. A mode is
matched by size and the closest refresh; a mode the display does not have is ignored.

### power.json

```json
{"displayOffMinutes": 10, "sleepMinutes": 30, "preventSleep": false}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `displayOffMinutes` | number > 0 (decimals allowed) | after this idle time the display turns off; absent or 0: never |
| `sleepMinutes` | number > 0 | after this idle time the compositor asks silica-daemon to suspend (`System1.Suspend`, logind); absent or 0: never |
| `preventSleep` | boolean | never sleep on idle; silica-daemon also holds logind's `idle` inhibitor |

Any input (real or the `input.*` commands) is activity: it wakes the display and restarts the
count. In the real session the display goes off with DPMS (the CRTC is cleared and nothing is
drawn until the next input); headless only keeps the state (`idle.state`). There is
no `ext-idle-notify-v1` for clients yet (Smithay's implementation needs the event loop's data
to be the compositor state).

### lockscreen.passwordDelay and screensaver.*

*Lock screen and screen saver.* The lock screen is the shell's client of `ext_session_lock_v1`
(offered to shell clients only). While the session is locked only its lock surface is drawn and
only it gets input: pointer motion and hover included, the keyboard, the wheel; no shortcut, VT
switch, gesture or hot corner runs, and no window or layer takes the focus. If the locker dies
the session stays locked (a plain black screen, no input) until a new locker, the shell started
again, takes the lock over; only the client that holds the lock unlocks it
(`unlock_and_destroy`), after silica-daemon checked the password (`System1.Unlock`). A lock
asked for by `session.lock`, the Logo menu, the password delay or sleeping starts at once and is
announced to the shell as `session.locked` (`1` or `0`), so that its locker takes the lock. A
screenshot taken with the headless software renderer is not covered.

`lockscreen.passwordDelay`: integer seconds (0 to 86400) between the start of the screen saver
or the display turning off and the lock; default 900 (15 minutes); -1: never (the lock stays
manual). The session also locks before sleeping when a delay is set. `lockscreen.showUser`
belongs to the shell. `screensaver.minutes`: minutes without input before the screen saver
starts (0: never) and `screensaver.saver`, `drift`, `clock` or `colors`; the compositor announces
the saver to the shell as `screensaver.active` (its id, empty when none) and any input ends it.
`idle.state` has a `lock` object with the state.

### hotcorners.*

`hotcorners.topLeft`, `hotcorners.topRight`, `hotcorners.bottomLeft`, `hotcorners.bottomRight`
(and their `.modifiers`): read by the **Dock**, not the compositor. The compositor only reports
the pointer reaching or leaving a corner (`silica-dock-v1`, `corner`, protocols.md); the Dock
waits its own delay, runs the action and toggles it back on the next visit. Dragging or
resizing a window into a corner does not report it.

### dock.titleBarDoubleClick and dock.minimizeToApp

| Key | Type | Meaning |
| --- | --- | --- |
| `dock.titleBarDoubleClick` | `zoom` (default), `minimize`, `none` | what a double click on the title bar does; the apps that draw their own bar read the same key (silica_ui). A window that cannot be resized never zooms |
| `dock.minimizeToApp` | boolean, default false | a window minimizes into its app's icon (the target of the genie or scale effect) instead of the Dock's right section; the shell tells where the icons are with `dock.icons` |

### shortcuts.*

Settings › Keyboard › Keyboard Shortcuts. `shortcuts.<command id>` for a command of the catalog
(`engine.commands`): the shortcut in the catalog's format (`Logo`, `Ctrl`, `Alt`, `Shift` and an
xkb keysym name, joined by `+`), or `none` to switch the command's shortcut off; empty (the
default, `Reset`) keeps the catalog's. Any command can be given a shortcut, also one with no
default. Two commands with the same keys: the first of the catalog runs (Settings refuses the
duplicate). `shortcuts.capture` true (set by Settings while it records a new shortcut) makes
the compositor let every key reach Settings, and only while Settings is the frontmost app.

### The glass, the accessibility of the display, the pointer size

| Key | Type | Meaning |
| --- | --- | --- |
| `look.reduceTransparency` (System1) | boolean | Accessibility › Display: the glass is drawn opaque (the `opaque` quality, no blur) |
| `look.reduceMotion` (System1) | boolean | Accessibility › Display: The compositor's own animations are skipped (the window genie, closing) |
| `look.glass` (System1) | number 0–1 | how tinted the glass is: 0 clearest, 1 most tinted; 0.5 (default) is the measured material. Only the tint's strength changes (`GlassProfile::with_glass_level`) |
| `accessibility.cursorSize` | integer 16–128 | Accessibility › Pointer: the pointer's size in logical pixels (default 24) |

### input.*

| Key | Type | Meaning |
| --- | --- | --- |
| `input.{mouse,touchpad}.speed` | number, -1–1 | pointer acceleration speed (libinput; default 0) |
| `input.{mouse,touchpad}.naturalScroll` | boolean | content follows the fingers / the wheel is reversed (default false) |
| `input.{mouse,touchpad}.scrollSpeed` | number, 0.1–10 | multiplies the scroll deltas (wheel for the mouse, fingers for the touchpad; default 1) |
| `input.mouse.secondaryClick` | `right` (default), `left` | which button is the secondary click (`left`: left-handed) |
| `input.touchpad.secondaryClick` | `twoFinger` (default), `bottomRight`, `bottomLeft` | click with two fingers (clickfinger) or in a bottom corner (button areas; `bottomLeft` is left-handed) |
| `input.touchpad.tapToClick` | boolean | a tap is a click (default false) |
| `input.touchpad.disableWhileTyping` | boolean | the touchpad ignores the palm while typing (default true) |
| `look.doubleClickMs` (System1) | integer | the double click time, the apps' too: The compositor uses it on its own title bars (400 ms without the daemon) |

A device is a touchpad when libinput can tap with it, otherwise a mouse. Every key left out
gives the device libinput's default. The settings apply to every pointer device at once and
to each one plugged in later (real session only: headless has no libinput
devices). `input.state` shows the settings in use and whether a mouse or a touchpad is
connected; the same `input.mouse` and `input.touchpad` reach apps through silica-daemon's
`System1` state (`docs/daemon.md`).

## CLI and MCP

```
silica-compositor --control "$XDG_RUNTIME_DIR/silica/control.sock"
silica-cli window.list
silica-cli window.move x=100 y=80
silica-cli screenshot /tmp/silica.png
silica-cli mcp            # MCP server on stdio
```

`--logo-key rightctrl` makes the **right Ctrl act as the Logo key** (right Ctrl+Space opens
Faro, right Ctrl+Tab the app switcher), for keyboards or VM viewers that keep Super for
themselves. `--logo-key rightalt` uses the right Alt, `--logo-key none` leaves the keys as they
are (the default).

The appearance (light or dark) of windows, bars, shadows and glass is decided by
`silica-daemon` (`appearance.dark`, changed with `system.dark` / `system.dark_toggle`); the compositor
reads it again on every frame. Without the daemon (headless, tests) it is light:
`--appearance dark` (or `light`) forces it and wins over the daemon, for deterministic
screenshots in dark:

```
silica-compositor --backend headless --appearance dark --control /dev/shm/control.sock
```

Via MCP the commands are tools with `_` instead of `.` (`window_list`); `ui_screenshot`
without `path` returns the image as `image` content.

## Actions socket

A real session doesn't open the control channel, but a few user-facing actions must work from
anywhere: the Apps, Mission Control and Screenshot applications, launchers, scripts. The compositor always listens on
`$XDG_RUNTIME_DIR/silica/actions-<wayland display>.sock` (folder 0700, socket 0600). The
protocol is the same JSON lines as the control channel, and only the commands in
`silica_control::actions::ACTIONS` run there:

- `launcher.toggle`, `faro.toggle`;
- `screenshot.toolbar`, `screenshot.screen`, `screenshot.selection`,
  `screenshot.screen_clipboard`, `screenshot.selection_clipboard`;
- `notifications.toggle`, `controlcenter.toggle`;
- `output.list`, `output.configure`, `output.color_temperature`: for silica-daemon, which gives
  apps the displays and runs the night light (`System1`, `docs/daemon.md`). The compositor passes its
  `WAYLAND_DISPLAY` to the services it starts, so they find the socket.

Anything else is refused. The client is `silica-action <action>`, which finds the socket
through `WAYLAND_DISPLAY`. `shell/linux/*.desktop.in` are the shell's own applications:
`dev.frascella.Apps` runs `silica-action launcher.toggle` and `dev.frascella.Screenshot` runs
`silica-action screenshot.toolbar`. Spaces and Mission Control belong to the Dock, as on macOS.
