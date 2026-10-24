# Silica protocols

The XMLs are in `crates/protocols/xml/`; the Rust bindings in `silica-protocols` (feature
`server` for the compositor, `client` for the embedder and the tests).

## silica-glass-v1

A client declares regions of one of its surfaces to render as glass: The compositor samples what lies
behind the surface and shows it blurred, with the material's color and refraction. The
client leaves transparent the pixels where the glass must show and draws text and icons on
top.

- `silica_glass_manager_v1.get_glass(id, surface)`: one glass object per surface
  (a second one is the `already_has_glass` error).
- `silica_glass_v1.clear()`, `add_region(x, y, width, height, radius, material)`: pending
  state, current at the next `wl_surface.commit`. Surface coordinates in logical pixels;
  circular-arc corners of radius `radius` (Dock, menus, notifications: windows instead have
  superellipse corners).
- Materials: `clear` (the Dock), `thick` (menus and search) and `regular` (the Control Center
  modules); since version 2 also `notification` (banners and Notification Center cards; the
  regions do not merge). A client bound to version 1 that asks for it gets an
  `invalid_region` error; the embedder falls back to `thick` there. Since Amalfi the
  declared material only picks the kind of surface of a region without a profile (below):
  every kind is drawn with Glass/Regular, the one Amalfi glass of the shell
  (`docs/design/amalfi/materials.md`, [`m2-glass.md`](m2-glass.md)).
- Errors: `invalid_region` (non-positive size, negative or non-finite radius, unknown
  material), `too_many_regions` (over 64). The client is disconnected; the compositor goes on.

- Since version 3: `add_translucent_region(…, opacity)`, the same region composited with an
  opacity 0–1. A panel that fades in or out carries the glass with it; the opacity does not
  redo the blur (only the drawing changes). Outside 0–1 it is `invalid_region`. `silica_ui`
  derives it from the widget's `Opacity` ancestors.

- Since version 4: `separate()` closes the current group. The following regions do not merge
  with the previous ones, even when overlapping, and are drawn on top (a submenu on its menu:
  in `menubar.md` they are two panels, each with its own edge and the submenu's shadow on the
  menu). `clear` goes back to the first group. `add_tip(x, y, width, height, material,
  opacity)`: a tip pointing down, with the base on the top side of the rectangle and the
  rounded apex at the center of the bottom side, which merges with the body of its group into
  a single shape: the edge runs continuously from the body to the tip (the tip of the Dock's
  menus, `misc.md`). `silica_ui` sends them with `Glass.group` and `Glass.tip`; an embedder
  bound to an earlier version ignores groups and tips. The embedder sorts the regions by
  `Glass.group`, a signed integer, and separates them where it changes: the menus of
  `silica_ui` take 0 and up, so the shell puts what must stay below them in negative groups
  (the Dock's are in `shell/lib/dock/glass_groups.dart`).
- Since version 5: `add_upward_tip(...)`, the same tip upside down (base on the bottom side,
  apex at the top): the arrangement popover below the green button. `GlassTip.up` in
  `silica_ui`.
- Since version 6: `add_side_tip(x, y, width, height, side, material, opacity)`, the tip
  turned sideways: `left` has the apex at the middle of the left side and the base on the
  right one, `right` the other way round; the base is the rectangle's height, the depth its
  width. `silica_ui` sends `tip-left` and `tip-right`.
- Since version 7: `set_profile(profile, shadow)`: the following regions belong to a kind of
  surface, until the next `set_profile` or `clear`. Profiles: `menu`, `panel` (Notification
  Center, search, launcher), `module` (Control Center modules, HUDs), `control` (small
  controls on glass: toolbar buttons, pills), `dock`, `tooltip`, `sheet`, `banner`, `popover`; `default`
  derives the kind from the material (clear: dock, thick: menu, regular: module,
  notification: banner). The kind picks the Amalfi material (blur, saturation, tint, inside
  rim, rim light, shadow), the sharp share and the resolution of the blur
  (`crates/compositor-render/src/glass_profile.rs`, table in [`m2-glass.md`](m2-glass.md)). With
  `shadow` = `compositor` the compositor draws the profile's shadow outside the shape (the client then
  draws none); with `client` it draws none. Unknown values are `invalid_profile`. On every
  shape the lens narrows so it never samples past the far edge. `silica_ui` sends the
  `profile` and `shadow` keys on the `silica/glass` channel (`Glass.profile`,
  `Glass.profileShadow`); the embedder ignores them below version 7.
- Since version 8: `set_rotation(degrees)`: the following regions and tips are turned by this
  angle (degrees, clockwise on screen, `fixed`) about the centre of their own rectangle, until
  the next `set_rotation` or `clear` (back to 0). Their `x`, `y`, `width`, `height` are the
  rectangle before the turn. The compositor draws the whole shape in the region's frame: the corners,
  the lens, the edge light on the region's own top edge (and its sides and bottom), the outline
  and the profile's shadow (its offset turns too); only the blurred background is sampled on
  screen. A label tilted with its object (the Dock stack's fan) is drawn tilted instead of as
  its upright bounding box. Without `set_rotation` (an earlier client) nothing changes.
  On the `silica/glass` channel a region turned by its ancestors carries a `frame`:
  `{"transform": [a, b, c, d, tx, ty], "width", "height", "radius"}`, the affine part of the
  widget's `getTransformTo(null)` applied to its own rectangle (0, 0)–(width, height)
  (x' = a·x + c·y + tx, y' = b·x + d·y + ty), its size and radius before it. The embedder
  takes the centre where the local centre lands, the angle of the x axis (`atan2(b, a)`), the
  sizes scaled along each turned axis (a shear is dropped) and the radius by the smaller
  scale, and sends `set_rotation` with the upright rectangle; below version 8, or for an
  upright frame (under 0.01°), it sends the region's `x`, `y`, `width`, `height`, which stay
  the bounding box. The shell's `ProfiledGlass` sends `frame` (`shell/lib/system/
  glass_profile.dart`) until `silica_ui`'s `Glass` does.
- Since version 9: `add_continuous_region(x, y, width, height, radius, smoothing, material,
  opacity)`: a region whose corners are a continuous curve, not a circular arc (the body of an
  app icon, silica-os/silica-desktop#232). `radius` is the radius of the equivalent circular corner,
  `smoothing` (0 to 1, outside it is `invalid_region`) how far the curve spreads along the
  edges: the corner starts `radius × (1 + smoothing)` from the corner point along each edge.
  The compositor draws a superellipse over that extent whose exponent runs from 2 (smoothing 0, the
  circle) to 3.53 (smoothing 0.6), the least-squares fit of the Sketch body path
  (`tools/body.svg` of silica-icons: radius 214, smoothing 0.6 on 824): the edge stays within
  0.5 px of it at 60 and 128 pt (`compositor-render`, `a_continuous_region_follows_the_body_outline`).
  It merges, rotates and takes profiles like any region. On the `silica/glass` channel the key
  `smoothing` (0–1) goes with `radius`; below version 9 the embedder sends `add_region` with
  the circular radius `radius × (1 + 0.26 × smoothing)` (0.30 of the side for the body).

The tip is a rounded isosceles triangle: the rectangle's base side is the triangle's base, its
depth is where the rounded apex (radius ~2.5 pt) ends, and the sides join the body with a
~2 pt fillet. The Dock menu's measured tip is 24 × 10 pt (`misc.md`; widths per row 23, 18,
16, 15, 14, 12, 10, 8, 6, 2).

The current regions are read with `glass.list` (`docs/control.md`).

## silica-window-v1

How an app presents its window to the compositor. Everything is per toplevel (the surface with the
`xdg_toplevel` role) and double-buffered: it becomes current at the next `wl_surface.commit`.

- `silica_window_manager_v1.get_window(id, surface)`: one window object per surface (a second
  one is the `already_has_window` error). Version 4 is the current one; a client may bind any
  version, and the requests of the later ones are simply not available to it.
- `silica_window_v1.set_full_size_content(toolbar_height)`: the app draws its own toolbar
  (unified with the title bar); the compositor only draws the window buttons over it, centred in
  `toolbar_height` (0: The compositor's own bar; clamped to 200).
- `silica_window_v1.set_hidden(hidden)` (version 2): takes the window off the screen without
  closing it (the app keeps running with no window, #142). Applied at once.
- `silica_window_v1.set_size_increment(base_width, base_height, step_width, step_height)`
  (version 4): interactive resizing snaps the content size to `base + n * step` per axis (the
  nearest `n` within the minimum and maximum size); a step of 0 leaves the axis free. A
  terminal asks for its cell size (terminal.md TE-01, #18). Dart: `silica/window` ›
  `setSizeIncrement {baseWidth, baseHeight, stepWidth, stepHeight}`.
- `silica_window_v1.set_appearance(appearance)` (version 3): `system` (0, the default), `light`
  (1) or `dark` (2). The window overrides the system appearance, as `NSWindow.appearance`
  does on macOS, for everything the compositor draws for it: the traffic lights (TL-GLASS dark or
  light), the plain title bar and the title colour, the shadow and the 1 pt inner border,
  the glass regions the window declares with `silica-glass-v1` (the dark or light tones and
  profiles of their kinds and materials), and the arrangement popover opened from its
  buttons (below). An unknown value is the `invalid_appearance` error. `destroy` goes back
  to `system`. A window that is not overridden follows the system, which also follows a
  change of it; the override never changes the system appearance or other windows.
  Dart: `SilicaWindow.setAppearance` (`silica/window` › `setAppearance {appearance}`), which
  `SilicaAppearanceScope` and `SilicaApp.appearance` of silica_ui call.

## silica-shell-v1 and the window list

Reserved for the shell components: processes with `SILICA_SHELL=1` in the environment, which
the compositor sets when it launches them (`--spawn-shell`, `app.launch` with `shell: true`). The compositor
reads it on connection (pid from `SO_PEERCRED`, environment from `/proc`). It is not a barrier
against other processes of the same user: that will come with the app sandbox.

- `ext-foreign-toplevel-list-v1` (standard): the open windows, with title, app id and an
  identifier.
- `silica_shell_manager_v1.activate(identifier)`: focus to the window (and brings it back if
  it is minimized).
- `silica_shell_manager_v1.launch(command)`: starts an app as a child of the compositor.
- `silica_shell_manager_v1.execute(id)`: runs a catalog command without parameters
  (the bar's magnifier opens Faro with `faro.toggle`).
- event `silica_shell_manager_v1.command(id)`: a catalog command that belongs to the
  shell (`faro.toggle`, Logo+Space), forwarded to all the components.
- event `silica_shell_manager_v1.focused(identifier)`: the focused window (empty if none),
  on connection and at every change.
- event `silica_shell_manager_v1.window_menu(identifier, x, y)` (version 3): long press on
  the green button, or the pointer resting on it, for 0.8 s; the menu bar opens the arrangement popover with
  the tip at x, y, and the entries run `window.tile_*`, `window.arrange_*` and
  `window.fullscreen` on the focused window.
- event `silica_shell_manager_v1.window_menu_appearance(identifier, appearance)` (version 8):
  sent right before `window_menu`, for the same window: 0 follows the system, 1 light, 2 dark
  (the window's `set_appearance`). The shell draws the popover in that appearance
  (`silica/shell` › `windowMenuAppearance {id, appearance}` before `windowMenu`).
- `silica_shell_manager_v1.broadcast(topic, value)` and the event of the same name
  (version 4): a component's state for the others (`controlcenter.open`,
  `notifications.open`: the capsules in the bar). The compositor sends it back to everyone, sender
  included, and sends the last value of every topic to whoever connects later.
- `silica_shell_manager_v1.execute_with(id, params)` (version 5): like `execute`, with the
  parameters in a JSON object (screenshots: `screenshot.region`, `screenshot.window_at`).
  The compositor announces every screenshot with the broadcast `screenshot` = "<path> <w> <h>".
- `silica_shell_manager_v1.set_transient(surface, region, press, scroll)`,
  `clear_transient(surface)` and the event `dismissed(surface)` (version 6, #149): a menu or
  panel of a shell surface is open. The surface keeps an input region as small as the menu,
  so the windows below keep the pointer (hover, window button glyphs). A press outside every
  open transient (not on the owner surface or its popups, not inside `region`) makes the compositor
  send `dismissed`; then the press is consumed (`consume`) or also delivered
  (`pass_through`). Scrolling outside is dropped or delivered, and never ends it. Keyboard
  routing stays with the layer surface's keyboard interactivity.
- `set_owned_transient(surface, region, owner, owner_rule, other_rule)` (version 7): the same
  transient with the outside split in two targets, the surfaces of the owner app and every
  other surface, each with a `rule`:
  - `modal`: no hover (the pointer leaves those surfaces: no tooltips, no text cursor), no
    wheel; a press of any button ends the transient and stops there;
  - `pass`: hover and wheel work; a press ends the transient and is delivered as with nothing
    open (activation and the click's own effect, a right click opens that app's menu, a drag
    selects);
  - `activate`: hover and wheel work; a left press ends the transient and only activates the
    window under the pointer (focus and raise, or the desktop's owner on the desktop): the
    client sees neither the press nor its drag. Other buttons pass.

  `owner` is an app id (its windows with their popups and subsurfaces, and its desktop
  surface `desktop-icons:<app id>`), `desktop-icons` for whichever app owns the desktop
  surface, `desktop-icons:<app id>` for that app, or `""` for the system (every surface follows
  `other_rule`). With several transients open, the strictest outcome of those a press ends
  wins (modal, then activate, then pass). The wheel never ends a transient, and while one is
  open the compositor shows the arrow over every surface outside it: the window below does not have
  the keyboard, so no text cursor.

  The rules reproduce the reference's measured model (INPUT-01): menus are modal only for
  their owning app, system panels never block other windows, the wheel never closes
  anything. Keys always go to the open surface (menus type-select, Faro and the launcher type
  into their field, Control Center, status menus and Notification Center swallow them),
  through the layer's keyboard interactivity.

  | Shell surface | Owner | Owner rule | Other rule |
  | --- | --- | --- | --- |
  | Menu-bar app menus | the frontmost app | `modal` | `pass` |
  | Logo menu | system (`""`) | — | `pass` |
  | Desktop menu | `desktop-icons` | `modal` | `pass` |
  | Dock icon menu and stacks | the app that was active | `activate` | `pass` |
  | Faro, launcher, Control Center, status menus (Wi-Fi, battery…), Notification Center | system | — | `pass` |
  | Window arrangement popover | the frontmost app | `pass` | `pass` |

  The Dock menu's `activate` stands for the reference's "a click outside is delivered as to an
  inactive window": text views there only activate (the caret does not move), while views
  that accept the first click act on it. The compositor cannot tell the two apart, and in Silica a
  click on an inactive window always reaches it, so only the app that was active (the one
  whose caret would move) gets the focus-only activation; other apps get the click.

  App popups with `xdg_popup.grab` (third-party toolkits' context menus and popovers) are
  menus of their client: `modal` for every surface of that client except the popup and its
  sibling popups on the same window, `pass` for everything else; the popup has the keyboard
  while open and gets `popup_done` when a press ends it.

  The shell keeps the policies in one place (`TransientPolicy` in
  `shell/lib/system/layer_control.dart`: `menu`, `dockMenu`, `panel`, `popover`) and the
  embedder exposes them on `silica/layer` (`setTransient {rects, owner, ownerRule,
  otherRule, press, scroll}`, where `press` and `scroll` are the version 6 fallback;
  `clearTransient`) and `silica/shell` (`transientDismissed`).

Version 11 drops the menu messages of versions 8 and 9 (`app_menu`, `activate_menu_item`,
`popup_menu*`): the apps draw their menus themselves (`silica-menu-bar-v1` below).

## silica-menu-bar-v1: the menu bar strip (#12)

The strip belongs to the compositor, as WindowServer's menu bar on macOS: it is 32 points tall
on the main display, transparent over the tone that softens the top of the wallpaper, hidden
while a window is full screen in front, and out of the windows' way (`usable_area`). What sits
in it comes from clients, each drawing its own part on a surface without an xdg role:

- `silica_menu_bar_manager_v1.get_menu_titles(surface)`: the client's menu titles (the system
  menu, the app's name, its menus), at the start of the strip while the client is the
  frontmost app (the focused window's, else the windowless frontmost app or the desktop's
  owner). `configure(serial, width, height)` gives the room before the status items and the
  strip's height; `shown(1|0)` tells whether the titles are on screen. The last committed
  buffer stays on screen when the app hangs. A menu or popover of the strip taking the
  keyboard leaves the app in front as it is.
- `get_status_item(surface, placement, order)`: a status item, as wide as its buffer.
  The compositor lines the visible items up from 8 points before the right edge: the
  `system` items (Control Center) by `order`, 0 rightmost, then the apps' items, the newest
  leftmost. `set_visible(0)` hides one without leaving a gap.
- `get_popup(xdg_popup)` on either attaches a popup made with a null parent: the owner opens
  its menus and popovers as its own popups, placed against the screen like a layer's
  (silica-os/compositor#13 removed the shell-drawn popup menus of `silica-menu-v1`). Its
  grab is the owner's menu as for any app popup, except that the titles or item it hangs
  from stay live, so the pointer can track across the titles; when it closes the keyboard
  goes back where it was before.

Control channel: `menu_bar.state` (`docs/control.md`).

## Drag between apps (`wl_data_device`)

Standard protocol, no extension. The compositor draws the drag icon (`start_drag`) with its corner on
the pointer, moved by the offsets of its commits (`wl_surface.attach` dx/dy or
`wl_surface.offset`, added up), above everything, until the drop.

The embedder offers it to the apps on the `silica/drag` channel (JSONMethodCodec, `SilicaDrag`
in silica_ui ≥ 0.16):

- `start {uris, icon?: {width, height, rgba, x?, y?}}` from the app: a `text/uri-list` source
  (copy and move actions) with the serial of the last press. The icon's point (`x`, `y`) stays
  under the pointer (the corner without them): an image of the dragged items keeps where the
  pointer grabbed them. At most 1024 pixels per side. When the compositor takes the pointer,
  the Flutter gesture in progress receives a cancellation.
- Towards the app: `enter`/`motion {x, y}` and `leave` for drags with `text/uri-list`
  over the window (accepted, copy preferred), `drop {x, y, uris}` with the data already read
  (on a thread: the source may be the window itself), `finished {action}` for those that left
  the window.

## Pseudo-terminals (`silica/pty`)

The embedder runs processes on pseudo-terminals for apps (the Terminal app), so apps need no
native code (`crates/embedder/src/pty.rs`; the one unsafe step, making the slave the
controlling terminal between fork and exec, is in `ffi/pty.rs`). JSONMethodCodec:

- From the app: `spawn {command?: [argv…], cwd?, env?: {k: v}, rows, cols}` → `{id}` (without
  `command`, the user's login shell from `$SHELL` or the passwd entry, as a login shell;
  `TERM=xterm-256color`, `COLORTERM=truecolor`, then `env`; `cwd` defaults to `$HOME`);
  `write {id, data: base64}`, `resize {id, rows, cols, pixelWidth?, pixelHeight?}`,
  `signal {id, signal: "INT"|"TERM"|"HUP"|"KILL"}`, `close {id}` (SIGHUP, SIGKILL after 2 s,
  reaped; no `exit` is sent) → null; `busy {id}` → bool: true when the pty's foreground
  process group (`tcgetpgrp` on the master) is not the shell's, that is a command is running
  in it (the Terminal confirms closing a busy tab); false otherwise or on error. Errors come back as a PlatformException with code `pty`.
- To the app: `output {id, data: base64}`, coalesced, at most 64 KiB and one message per frame
  per pty; `exit {id, code?, signal?}` after the last output.
- **Grid sessions** (`spawn {grid: true, scrollback?: 0–1000000 (default 10000),
  clipboardWrite?: true, clipboardRead?: false, cursor?: block|underline|beam, cursorBlink?:
  false, theme?: {foreground, background, cursor, ansi: [16]}}`): the embedder emulates the terminal
  (alacritty_terminal on a thread of its own, `termgrid.rs`) and the app receives no `output`.
  Once per frame, if anything changed, it gets `grid {id, cols, rows, full, lines: [{y, runs:
  [[text, fg, bg, flags, cells, link?], …]}], cursor: {x, y, visible, shape}, modes, …}`: only
  the visible rows that really changed, as runs of cells with the same attributes (`cells` is
  the width, wide characters count two; `link` is the OSC 8 target). Colours are `0xRRGGBB`,
  -1 for the default, `0x1000000 + i` for entry `i` of the app's 256-colour palette.
  `flags`: 1 bold, 2 italic, 4 underline, 8 inverse, 16 dim, 32 strikeout, 64 curly underline,
  128 hidden, 256 wide. `modes` bits: 1 application cursor keys, 2 bracketed paste, 4 mouse
  reporting on, 8 SGR (1006), 16 focus events (1004), 32 alternate screen, 64 application
  keypad, 128 report drags (1002), 256 report all motion (1003), 512 UTF-8 mouse (1005), 1024
  X10 (9), 2048 alternate scroll (1007). Optional keys, present when they changed: `title`;
  `scroll {offset, history}` (lines scrolled back, lines in the scrollback); `selection
  [[y, x0, x1], …]` (the selected cells of the visible rows, empty when none); `marks [y, …]`
  (visible rows that start a prompt: OSC 133 A, or Mark); `cwd` (OSC 7); `command {running,
  exit?}` (OSC 133 C/D); `bell: true`; `clipboard [{set: text, selection} | {get: true,
  selection}, …]` (OSC 52; a `get` is answered with `clipboard`).
- Calls on a grid session (an error on other ptys; rows and columns are of the visible
  screen): `scroll {id, lines|page|to}` (`lines` > 0 scrolls towards older text; `page` ±1;
  `to`: `top`, `bottom`, `previousPrompt`, `nextPrompt`, or `row` with `row` counted from the
  top of the buffer, which centres a row that is out of view); `select {id, op: start|update|
  clear|all|marks, kind?: simple|semantic|lines|block, x, y, right?}` → bool (`marks` selects
  the output of the last command; false if there is none); `selectionText {id}` → string or
  null (soft-wrapped lines joined); `text {id}` → the whole buffer; `search {id, query,
  caseSensitive?}` → matches in order, each a list of `[row, x0, x1]` pieces (rows from the top
  of the buffer, more than one piece when the match crosses a soft wrap), at most 10000;
  `mark {id, op?: add|remove}` → bool; `clear {id, what: scrollback|toStart|toMark|screen}` →
  bool; `reset {id, hard?}`; `configure {id, scrollback?, clipboardWrite?, clipboardRead?, cursor?, cursorBlink?, theme?}`
  (`theme` colours, 0xRRGGBB, are what programs hear when they ask with OSC 4, 10, 11 and 12);
  `clipboard {id, text}` (answers an OSC 52 read; one answer per question); `cwd {id}` → the
  directory of OSC 7, else the shell's from `/proc`, on any pty.
- **Split view**: `split {id, op: open|resize|close, rows}` adds a second view of the same
  emulator with `rows` rows (the same columns), which ends at the bottom of the screen and
  scrolls on its own. Its frames are `grid {id, view: 1, cols, rows, full, lines, cursor,
  modes, scroll {offset, history}, selection?}` where `history` is how far it can scroll back;
  `scroll` and `select` take `view: 1` for it (cells are of that view). The selection and the
  program's keyboard input are the session's.
- A grid session stays after its process ends, so that the text can still be scrolled,
  selected and copied: `exit` is sent once, then only `close` (or the window) removes it.
- The grid also reads OSC 7 and OSC 133 itself (A prompt start, B prompt end, C output start,
  D finished): prompts are marked on their rows, so marks follow the text through scrolling,
  reflow and the scrollback; DEC mode 9 (X10 mouse) is read too. A synchronized update (mode
  2026) that a program never ends is cut after 150 ms.
- Limits: 64 ptys per window, rows and columns 1–1000, 256 arguments and environment
  variables of at most 4 KiB each. More than 1 MiB of output waiting for the app pauses
  reading from the pty (backpressure); more than 4 MiB of input waiting refuses `write`.
  Closing the window hangs up every pty.
