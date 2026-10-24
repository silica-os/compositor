//! Execution of the catalog commands (`silica_control::commands`).

use std::process::Command;

use base64::Engine;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use silica_control::commands::MENU_BAR;
use silica_control::commands::{
    self, APP_LAUNCH, AnimationScale, AppLaunch, Click, ENGINE_COMMANDS, ENGINE_EXECUTE,
    ExecuteParams, FARO_TOGGLE, GLASS_LIST, GlassQualityParams, INPUT_CLICK, INPUT_KEY,
    INPUT_POINTER_BUTTON, INPUT_POINTER_MOVE, INPUT_SCROLL, INPUT_TYPE, Key, LAYER_LIST,
    NOTIFICATIONS_TOGGLE, PERF_FRAMES, PERF_PROCESS, PERF_RENDER, PerfFramesParams,
    PerfProcessParams, PerfRenderParams, PointerButton, PointerMove, SESSION_LOCK, SESSION_QUIT,
    Scroll, TypeText, UI_ANIMATION_SCALE, UI_GLASS_QUALITY, UI_SCREENSHOT, WINDOW_ARRANGE_2,
    WINDOW_ARRANGE_3, WINDOW_ARRANGE_4, WINDOW_CLOSE, WINDOW_FOCUS, WINDOW_FULLSCREEN, WINDOW_LIST,
    WINDOW_MINIMIZE, WINDOW_MOVE, WINDOW_TILE_BOTTOM, WINDOW_TILE_FILL, WINDOW_TILE_LEFT,
    WINDOW_TILE_RIGHT, WINDOW_TILE_TOP, WindowInfo, WindowMove, WindowTarget,
};
use silica_control::commands::{
    APP_FORCE_QUIT, APP_FORCE_QUIT_PANEL, SCREENSHOT_REGION, SCREENSHOT_SCREEN,
    SCREENSHOT_SCREEN_CLIPBOARD, SCREENSHOT_SELECTION, SCREENSHOT_SELECTION_CLIPBOARD,
    SCREENSHOT_SETTINGS, SCREENSHOT_TOOLBAR, SCREENSHOT_WINDOW_AT, SCREENSHOT_WINDOW_VEIL,
};
use silica_control::commands::{APP_HIDE, APP_HIDE_OTHERS, APP_SHOW_ALL};
use silica_control::commands::{APP_QUIT, GestureParams, INPUT_GESTURE};
use silica_control::commands::{
    CONTROLCENTER_TOGGLE, EnabledParams, SYSTEM_BLUETOOTH, SYSTEM_BRIGHTNESS,
    SYSTEM_BRIGHTNESS_DOWN, SYSTEM_BRIGHTNESS_DOWN_FINE, SYSTEM_BRIGHTNESS_UP,
    SYSTEM_BRIGHTNESS_UP_FINE, SYSTEM_DARK, SYSTEM_DARK_TOGGLE, SYSTEM_DND,
    SYSTEM_KEYBOARD_BRIGHTNESS, SYSTEM_KEYBOARD_BRIGHTNESS_DOWN, SYSTEM_KEYBOARD_BRIGHTNESS_UP,
    SYSTEM_MUTE, SYSTEM_MUTE_TOGGLE, SYSTEM_STATUS, SYSTEM_VOLUME, SYSTEM_VOLUME_DOWN,
    SYSTEM_VOLUME_DOWN_FINE, SYSTEM_VOLUME_UP, SYSTEM_VOLUME_UP_FINE, SYSTEM_WIFI, ValueParams,
};
use silica_control::commands::{
    DESKTOP_SHOW, LAUNCHER_TOGGLE, PANORAMA_APP, PANORAMA_TOGGLE, SPACE_GO, SPACE_NEXT,
    SPACE_PREVIOUS, SWITCHER_CANCEL, SWITCHER_COMMIT, SWITCHER_EXPOSE, SWITCHER_NEXT,
    SWITCHER_PREVIOUS,
};
use silica_control::commands::{
    DOCK_ICONS, DockIconsParams, MinimizeEffectParams, WINDOW_MINIMIZE_EFFECT,
};
use silica_control::commands::{
    IDLE_STATE, INPUT_STATE, KEYBOARD_NEXT_SOURCE, KEYBOARD_SELECT_SOURCE, KEYBOARD_STATE,
};
use silica_control::commands::{OUTPUT_COLOR_TEMPERATURE, OUTPUT_CONFIGURE, OUTPUT_LIST};
use silica_control::{CaptureAnnouncement, CaptureOptions, ScreenshotSettings};
use silica_control::{Request, Response, Screenshot, ScreenshotParams, Shortcut};
use smithay::backend::input::{ButtonState, KeyState};
use smithay::desktop::Window;
use smithay::input::keyboard::Keycode;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Point, SERIAL_COUNTER};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
use tracing::{info, warn};

use crate::state::State;
use crate::system::Call;

/// Stable id of a window, saved in its user data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowId(pub u64);

/// The catalog shortcuts, already parsed.
pub fn shortcuts() -> Vec<(Shortcut, &'static str)> {
    shortcuts_with(&crate::config::ShortcutOverrides::default())
}

/// The catalog shortcuts with the user's changes (`shortcuts.json`): a command may get a
/// shortcut of its own (even one with no default), or none at all.
pub fn shortcuts_with(
    overrides: &crate::config::ShortcutOverrides,
) -> Vec<(Shortcut, &'static str)> {
    commands::CATALOG
        .iter()
        .filter_map(|spec| {
            let text = match overrides.get(spec.id) {
                Some(Some(text)) => text,
                Some(None) => return None,
                None => spec.shortcut?,
            };
            match Shortcut::parse(text) {
                Ok(shortcut) => Some((shortcut, spec.id)),
                Err(err) => {
                    warn!("shortcut of {} ignored: {err}", spec.id);
                    // A bad user value falls back to the default.
                    spec.shortcut
                        .and_then(|text| Shortcut::parse(text).ok())
                        .map(|shortcut| (shortcut, spec.id))
                }
            }
        })
        .collect()
}

fn parse<T: DeserializeOwned + Default>(params: Value) -> Result<T, String> {
    if params.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(params).map_err(|e| format!("parameters: {e}"))
}

fn parse_required<T: DeserializeOwned>(params: Value) -> Result<T, String> {
    serde_json::from_value(params).map_err(|e| format!("parameters: {e}"))
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

impl State {
    /// Runs a request of the control channel.
    pub fn execute(&mut self, request: Request) -> Response {
        match self.execute_command(&request.command, request.params) {
            Ok(value) => Response::ok(request.id, value),
            Err(err) => Response::err(request.id, err),
        }
    }

    /// Runs a catalog command: the same entry point for the channel, shortcuts and, later,
    /// UI and MCP.
    pub fn execute_command(&mut self, id: &str, params: Value) -> Result<Value, String> {
        // Synthetic input is activity too (the display wakes, the idle count restarts).
        if id.starts_with("input.") && id != INPUT_STATE {
            self.user_activity();
        }
        match id {
            UI_SCREENSHOT => self.command_screenshot(parse(params)?),
            KEYBOARD_STATE => Ok(self.keyboard_state()),
            INPUT_STATE => Ok(self.input_state()),
            IDLE_STATE => Ok(self.idle_state()),
            OUTPUT_LIST => Ok(self.output_list()),
            OUTPUT_CONFIGURE => self.output_configure(&params),
            OUTPUT_COLOR_TEMPERATURE => {
                let kelvin = params
                    .get("kelvin")
                    .and_then(Value::as_u64)
                    .and_then(|k| u32::try_from(k).ok())
                    .filter(|k| (1000..=6500).contains(k))
                    .ok_or("kelvin: 1000–6500")?;
                let applied = self.set_color_temperature(kelvin);
                Ok(json!({ "kelvin": kelvin, "applied": applied }))
            }
            KEYBOARD_NEXT_SOURCE => {
                let switched = self.next_input_source();
                Ok(json!({ "switched": switched, "active": self.active_layout() }))
            }
            KEYBOARD_SELECT_SOURCE => {
                let index = params
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|i| u32::try_from(i).ok())
                    .ok_or("index: 0–3")?;
                let switched = self.select_input_source(index);
                Ok(json!({ "switched": switched, "active": self.active_layout() }))
            }
            SCREENSHOT_SCREEN => {
                let options: CaptureOptions = parse(params)?;
                let capture = self.screenshot()?;
                self.publish_screenshot(capture, options, (0.0, 0.0))
            }
            SCREENSHOT_SCREEN_CLIPBOARD => {
                let capture = self.screenshot()?;
                let options = CaptureOptions {
                    pointer: None,
                    clipboard: true,
                };
                self.publish_screenshot(capture, options, (0.0, 0.0))
            }
            SCREENSHOT_SETTINGS => {
                let settings: ScreenshotSettings = parse_required(params)?;
                self.screenshot_settings = settings;
                to_value(settings)
            }
            SCREENSHOT_REGION => {
                let region: silica_control::RegionParams = parse_required(params)?;
                let capture = self.screenshot()?;
                let scale = self.output.current_scale().fractional_scale();
                let origin = self
                    .space
                    .output_geometry(&self.output)
                    .map(|g| g.loc.to_f64())
                    .unwrap_or_default();
                let (x, y) = ((region.x - origin.x) * scale, (region.y - origin.y) * scale);
                let crop = crate::capture::crop(
                    &capture,
                    x,
                    y,
                    region.width * scale,
                    region.height * scale,
                )
                .ok_or("empty region or off screen")?;
                // The crop starts at the rounded, clamped corner (see `crop`).
                let corner = (
                    x.round().clamp(0.0, f64::from(capture.width)),
                    y.round().clamp(0.0, f64::from(capture.height)),
                );
                self.publish_screenshot(crop, region.options(), corner)
            }
            SCREENSHOT_WINDOW_AT => {
                let shot: silica_control::WindowShotParams = parse_required(params)?;
                let window = match self.window_hit(Point::from((shot.x, shot.y))) {
                    Some(crate::decorations::Hit::Titlebar(window, _, _))
                    | Some(crate::decorations::Hit::Content(window)) => window,
                    None if shot.or_screen => {
                        let capture = self.screenshot()?;
                        return self.publish_screenshot(capture, shot.options(), (0.0, 0.0));
                    }
                    None => return Err("no window at that point".to_owned()),
                };
                let (capture, corner) =
                    self.window_capture(&window, shot.shadow.unwrap_or(true))?;
                self.publish_screenshot(capture, shot.options(), corner)
            }
            SCREENSHOT_WINDOW_VEIL => {
                let EnabledParams { enabled } = parse_required(params)?;
                self.window_veil = enabled;
                Ok(json!({ "enabled": enabled }))
            }
            APP_FORCE_QUIT => {
                let silica_control::AppParams { app_id } = parse_required(params)?;
                let killed = self.force_quit(&app_id)?;
                Ok(json!({ "killed": killed }))
            }
            SCREENSHOT_SELECTION
            | SCREENSHOT_SELECTION_CLIPBOARD
            | SCREENSHOT_TOOLBAR
            | APP_FORCE_QUIT_PANEL => Ok(json!({ "forwarded": self.forward_to_shell(id) })),
            WINDOW_MINIMIZE_EFFECT => {
                let MinimizeEffectParams { effect } = parse_required(params)?;
                self.minimize_effect = crate::render::MinimizeEffect::parse(&effect)
                    .ok_or_else(|| format!("unknown minimize effect (genie, scale): {effect}"))?;
                Ok(json!({ "effect": effect }))
            }
            DOCK_ICONS => {
                let DockIconsParams { icons } = parse_required(params)?;
                let count = icons.len();
                self.dock_icons = icons
                    .into_iter()
                    .filter(|icon| icon.x.is_finite() && icon.y.is_finite())
                    .map(|icon| (icon.app_id, (icon.x, icon.y)))
                    .collect();
                Ok(json!({ "icons": count }))
            }
            UI_GLASS_QUALITY => {
                let GlassQualityParams { quality } = parse_required(params)?;
                self.glass_setting =
                    crate::state::GlassSetting::parse(&quality).ok_or_else(|| {
                        format!("unknown glass quality (auto, full, frosted, opaque): {quality}")
                    })?;
                Ok(json!({ "quality": self.glass_quality().name() }))
            }
            PERF_RENDER => {
                let PerfRenderParams { reset } = parse(params)?;
                to_value(self.perf.render_times(reset))
            }
            UI_ANIMATION_SCALE => {
                let AnimationScale { scale } = parse_required(params)?;
                if !scale.is_finite() || !(0.0..=1000.0).contains(&scale) {
                    return Err(format!("invalid animation scale (0–1000): {scale}"));
                }
                self.animation_speed = scale;
                Ok(Value::Null)
            }
            ENGINE_COMMANDS => Ok(commands::catalog_json()),
            ENGINE_EXECUTE => {
                let ExecuteParams { id, params } = parse_required(params)?;
                if id == ENGINE_EXECUTE {
                    return Err("engine.execute does not nest".to_owned());
                }
                self.execute_command(&id, params)
            }
            WINDOW_LIST => to_value(self.window_list()),
            LAYER_LIST => to_value(self.layer_list()),
            GLASS_LIST => to_value(self.glass_list()),
            // Commands of the shell components: The compositor forwards them.
            SYSTEM_STATUS => self
                .system
                .status()
                .ok_or_else(|| "silica-daemon unreachable".to_owned()),
            SYSTEM_DARK_TOGGLE => Ok(json!({ "queued": self.system.dark_toggle() })),
            SYSTEM_WIFI | SYSTEM_BLUETOOTH | SYSTEM_MUTE | SYSTEM_DND | SYSTEM_DARK => {
                let EnabledParams { enabled } = parse_required(params)?;
                let call = match id {
                    SYSTEM_WIFI => Call::Wifi(enabled),
                    SYSTEM_BLUETOOTH => Call::Bluetooth(enabled),
                    SYSTEM_MUTE => Call::Muted(enabled),
                    SYSTEM_DARK => Call::Dark(enabled),
                    _ => Call::DoNotDisturb(enabled),
                };
                Ok(json!({ "queued": self.system.queue(call) }))
            }
            SYSTEM_VOLUME | SYSTEM_BRIGHTNESS | SYSTEM_KEYBOARD_BRIGHTNESS => {
                let ValueParams { value } = parse_required(params)?;
                if !value.is_finite() {
                    return Err("non-finite value".to_owned());
                }
                let value = value.clamp(0.0, 1.0);
                let call = match id {
                    SYSTEM_VOLUME => Call::Volume(value),
                    SYSTEM_BRIGHTNESS => Call::Brightness(value),
                    _ => Call::KeyboardBrightness(value),
                };
                Ok(json!({ "queued": self.system.queue(call) }))
            }
            SYSTEM_VOLUME_UP
            | SYSTEM_VOLUME_DOWN
            | SYSTEM_VOLUME_UP_FINE
            | SYSTEM_VOLUME_DOWN_FINE => Ok(json!({
                "queued": self.system.volume_step(
                    matches!(id, SYSTEM_VOLUME_UP | SYSTEM_VOLUME_UP_FINE),
                    matches!(id, SYSTEM_VOLUME_UP_FINE | SYSTEM_VOLUME_DOWN_FINE),
                )
            })),
            SYSTEM_MUTE_TOGGLE => Ok(json!({ "queued": self.system.mute_toggle() })),
            SYSTEM_BRIGHTNESS_UP
            | SYSTEM_BRIGHTNESS_DOWN
            | SYSTEM_BRIGHTNESS_UP_FINE
            | SYSTEM_BRIGHTNESS_DOWN_FINE => Ok(json!({
                "queued": self.system.brightness_step(
                    matches!(id, SYSTEM_BRIGHTNESS_UP | SYSTEM_BRIGHTNESS_UP_FINE),
                    matches!(id, SYSTEM_BRIGHTNESS_UP_FINE | SYSTEM_BRIGHTNESS_DOWN_FINE),
                )
            })),
            SYSTEM_KEYBOARD_BRIGHTNESS_UP | SYSTEM_KEYBOARD_BRIGHTNESS_DOWN => Ok(json!({
                "queued": self.system.keyboard_step(id == SYSTEM_KEYBOARD_BRIGHTNESS_UP)
            })),
            SWITCHER_NEXT | SWITCHER_PREVIOUS => {
                self.dismiss_transients();
                self.switcher_active = true;
                Ok(json!({ "forwarded": self.forward_to_shell(id) }))
            }
            SWITCHER_COMMIT | SWITCHER_CANCEL | SWITCHER_EXPOSE => {
                self.switcher_active = false;
                Ok(json!({ "forwarded": self.forward_to_shell(id) }))
            }
            FARO_TOGGLE | NOTIFICATIONS_TOGGLE | CONTROLCENTER_TOGGLE | LAUNCHER_TOGGLE
            | SPACE_NEXT | SPACE_PREVIOUS | PANORAMA_TOGGLE | PANORAMA_APP | DESKTOP_SHOW => {
                // Faro, the Launcher and the panels open over every menu (#237).
                self.dismiss_transients();
                let sent = self.forward_to_shell(id);
                Ok(json!({ "forwarded": sent }))
            }
            id if SPACE_GO.contains(&id) => {
                self.dismiss_transients();
                Ok(json!({ "forwarded": self.forward_to_shell(id) }))
            }
            WINDOW_FOCUS => {
                let target: WindowTarget = parse(params)?;
                // A window of a hidden app: the app comes back.
                let hidden = target.id.and_then(|id| {
                    self.hidden_apps
                        .windows()
                        .find(|w| Self::window_id(w) == Some(id))
                        .cloned()
                });
                if let Some(window) = hidden {
                    self.unhide_for(&window);
                }
                let window = match target.id.and_then(|id| self.minimized_window(id)) {
                    Some(window) => {
                        self.restore_window(&window);
                        window
                    }
                    None => self.target_window(target.id)?,
                };
                self.focus_window(Some(window));
                Ok(Value::Null)
            }
            WINDOW_MINIMIZE => {
                let target: WindowTarget = parse(params)?;
                let window = self.target_window(target.id)?;
                self.minimize_window(&window)?;
                Ok(Value::Null)
            }
            WINDOW_FULLSCREEN => {
                let target: WindowTarget = parse(params)?;
                let window = self.target_window(target.id)?;
                self.toggle_fullscreen(&window)?;
                Ok(Value::Null)
            }
            WINDOW_TILE_LEFT | WINDOW_TILE_RIGHT | WINDOW_TILE_TOP | WINDOW_TILE_BOTTOM
            | WINDOW_TILE_FILL => {
                let target: WindowTarget = parse(params)?;
                let window = self.target_window(target.id)?;
                let tile = match id {
                    WINDOW_TILE_LEFT => crate::windows::Tile::Left,
                    WINDOW_TILE_RIGHT => crate::windows::Tile::Right,
                    WINDOW_TILE_TOP => crate::windows::Tile::Top,
                    WINDOW_TILE_BOTTOM => crate::windows::Tile::Bottom,
                    _ => crate::windows::Tile::Fill,
                };
                self.tile_window(&window, tile)?;
                Ok(Value::Null)
            }
            WINDOW_ARRANGE_2 | WINDOW_ARRANGE_3 | WINDOW_ARRANGE_4 => {
                let target: WindowTarget = parse(params)?;
                let window = self.target_window(target.id)?;
                let count = match id {
                    WINDOW_ARRANGE_2 => 2,
                    WINDOW_ARRANGE_3 => 3,
                    _ => 4,
                };
                self.arrange_windows(&window, count)?;
                Ok(Value::Null)
            }
            WINDOW_CLOSE => {
                let target: WindowTarget = parse(params)?;
                let window = self.target_window(target.id)?;
                let toplevel = window.toplevel().ok_or("window without a toplevel")?;
                toplevel.send_close();
                Ok(Value::Null)
            }
            MENU_BAR => Ok(self.menu_bar_state()),
            APP_HIDE => {
                let silica_control::QuitParams { app_id } = parse(params)?;
                let app = app_id
                    .or_else(|| self.front_app_id())
                    .ok_or("no app in front")?;
                let hidden = self.hide_app(&app);
                Ok(json!({ "app_id": app, "hidden": hidden }))
            }
            APP_HIDE_OTHERS => Ok(json!({ "hidden": self.hide_other_apps() })),
            APP_SHOW_ALL => Ok(json!({ "shown": self.show_all_apps() })),
            APP_QUIT => {
                let silica_control::QuitParams { app_id } = parse(params)?;
                // No window focused: the frontmost windowless app (#142), if any.
                let app_id = app_id.or_else(|| {
                    self.focused_window()
                        .is_none()
                        .then(|| self.windowless_front.clone())
                        .flatten()
                });
                // A windowless app (Quit in its Dock menu) has no window to start from.
                if let Some(app) = app_id.filter(|app| registered_app(app)) {
                    let quitting = app.clone();
                    std::thread::spawn(move || silica_control::apps::quit_all(&quitting));
                    return Ok(json!({ "closed": 0, "quit": app }));
                }
                let window = self.target_window(None)?;
                let app = crate::windows::window_app(&window);
                let mut windows: Vec<Window> = vec![window];
                if let Some(app) = &app {
                    windows = self
                        .space
                        .elements()
                        .chain(self.minimized.iter().map(|(w, _)| w))
                        .chain(self.hidden_apps.windows())
                        .chain(self.dock_hidden.windows())
                        .filter(|w| crate::windows::window_app(w).as_deref() == Some(app.as_str()))
                        .cloned()
                        .collect();
                }
                // A Silica app keeps running when its windows close (#142): it is asked to
                // quit over its registry socket, on a thread (an app that does not answer must
                // not stall the compositor). Other apps end when their windows close.
                if let Some(app) = app.filter(|app| registered_app(app)) {
                    let quitting = app.clone();
                    std::thread::spawn(move || silica_control::apps::quit_all(&quitting));
                    return Ok(json!({ "closed": 0, "quit": app }));
                }
                for window in &windows {
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.send_close();
                    }
                }
                Ok(json!({ "closed": windows.len() }))
            }
            WINDOW_MOVE => {
                let WindowMove { id, x, y } = parse_required(params)?;
                let window = self.target_window(id)?;
                self.space.map_element(window, (x, y), false);
                self.follow_sheets();
                Ok(Value::Null)
            }
            APP_LAUNCH => {
                let AppLaunch {
                    command,
                    app,
                    shell,
                } = parse_required(params)?;
                // An app is run through its desktop entry, as the Dock does (its Exec keeps
                // the `--app-id`); a command runs as given.
                let command = match (command, app) {
                    (Some(command), None) => command,
                    (None, Some(app)) => {
                        crate::desktop_entry::command_for(&app, &|k| std::env::var(k).ok())?
                    }
                    _ => return Err("app.launch takes either command or app".to_owned()),
                };
                let pid = self.launch_with(&command, shell)?;
                Ok(json!({ "pid": pid }))
            }
            INPUT_POINTER_MOVE => {
                let PointerMove { x, y } = parse_required(params)?;
                self.pointer_move_to((x, y).into(), self.now_msec());
                Ok(Value::Null)
            }
            INPUT_POINTER_BUTTON => {
                let PointerButton { button, pressed } = parse_required(params)?;
                let state = if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                self.pointer_button(button.code(), state, self.now_msec());
                Ok(Value::Null)
            }
            INPUT_CLICK => {
                let Click { x, y, button } = parse_required(params)?;
                let time = self.now_msec();
                self.pointer_move_to((x, y).into(), time);
                self.pointer_button(button.code(), ButtonState::Pressed, time);
                self.pointer_button(button.code(), ButtonState::Released, time);
                Ok(Value::Null)
            }
            INPUT_SCROLL => {
                let Scroll { x, y, dx, dy } = parse_required(params)?;
                let time = self.now_msec();
                self.pointer_move_to((x, y).into(), time);
                let mut frame = smithay::input::pointer::AxisFrame::new(time)
                    .source(smithay::backend::input::AxisSource::Wheel);
                for (axis, amount) in [
                    (smithay::backend::input::Axis::Horizontal, dx),
                    (smithay::backend::input::Axis::Vertical, dy),
                ] {
                    if amount != 0.0 && amount.is_finite() {
                        frame = frame.value(axis, amount);
                    }
                }
                self.pointer_axis(frame);
                Ok(Value::Null)
            }
            INPUT_KEY => {
                let Key { key, pressed } = parse_required(params)?;
                self.refresh_keys();
                let stroke = self
                    .keys
                    .find(&key)
                    .ok_or_else(|| format!("unknown key: {key}"))?;
                let states: &[KeyState] = match pressed {
                    Some(true) => &[KeyState::Pressed],
                    Some(false) => &[KeyState::Released],
                    None => &[KeyState::Pressed, KeyState::Released],
                };
                for state in states {
                    self.synthetic_key(stroke, *state);
                }
                Ok(Value::Null)
            }
            INPUT_GESTURE => {
                let p: GestureParams = parse_required(params)?;
                let kind = match p.kind.as_str() {
                    "swipe" => crate::gestures::GestureKind::Swipe,
                    "pinch" => crate::gestures::GestureKind::Pinch,
                    other => return Err(format!("unknown gesture: {other}")),
                };
                let phase = p.phase.as_deref();
                if matches!(phase, None | Some("begin")) {
                    self.gesture_begin(kind, p.fingers);
                }
                match phase {
                    None | Some("update") => self.gesture_update(p.dx, p.dy, p.scale),
                    Some("begin" | "end") => {}
                    Some(other) => return Err(format!("unknown phase: {other}")),
                }
                if matches!(phase, None | Some("end")) {
                    let command = self.gesture_end(p.cancelled);
                    return Ok(json!({ "command": command }));
                }
                Ok(json!({ "command": null }))
            }
            INPUT_TYPE => {
                let TypeText { text } = parse_required(params)?;
                self.refresh_keys();
                // Everything is checked before pressing anything: no half-typed text. A
                // character the active layout lacks goes on a temporary key.
                let mut plan = Vec::new();
                for ch in text.chars() {
                    match self.keys.find(&ch.to_string()) {
                        Some(stroke) => plan.push((ch, Some(stroke))),
                        None if !ch.is_control() => plan.push((ch, None)),
                        None => return Err(format!("character cannot be typed: {ch:?}")),
                    }
                }
                let mut temporary = false;
                let mut failed = None;
                for (ch, stroke) in plan {
                    match stroke {
                        Some(stroke) => {
                            self.synthetic_key(stroke, KeyState::Pressed);
                            self.synthetic_key(stroke, KeyState::Released);
                        }
                        None => match self.type_on_temporary_key(ch) {
                            Ok(()) => temporary = true,
                            Err(err) => {
                                failed = Some(err);
                                break;
                            }
                        },
                    }
                }
                if temporary {
                    self.restore_keymap();
                }
                if let Some(err) = failed {
                    return Err(err);
                }
                Ok(Value::Null)
            }
            PERF_FRAMES => {
                let PerfFramesParams { reset } = parse(params)?;
                let report = self.perf_frames();
                if reset {
                    self.perf.reset();
                }
                to_value(report)
            }
            PERF_PROCESS => {
                let PerfProcessParams { pid } = parse_required(params)?;
                to_value(crate::perf::process_info(pid)?)
            }
            SESSION_LOCK => {
                self.lock_session()?;
                Ok(json!({ "locked": true }))
            }
            SESSION_QUIT => {
                info!("quit requested");
                self.loop_signal.stop();
                Ok(Value::Null)
            }
            other => Err(format!("unknown command: {other}")),
        }
    }

    /// A synthetic key to the focused window, with Shift and AltGr around it if needed.
    /// It goes through the same filter as real keys, so shortcuts work too.
    fn synthetic_key(&mut self, stroke: crate::keys::KeyStroke, state: KeyState) {
        let time = self.now_msec();
        // Like a real key: catalog shortcuts too.
        let press = |state: &mut Self, code: u32, key_state: KeyState| {
            state.handle_key(Keycode::new(code), key_state, time);
        };
        let modifiers: Vec<u32> = [
            stroke.shift.then_some(crate::keys::SHIFT_LEFT),
            stroke.level3,
        ]
        .into_iter()
        .flatten()
        .collect();
        if state == KeyState::Pressed {
            for code in &modifiers {
                press(self, *code, KeyState::Pressed);
            }
        }
        press(self, stroke.code, state);
        if state == KeyState::Released {
            for code in modifiers.iter().rev() {
                press(self, *code, KeyState::Released);
            }
        }
    }

    /// The xkb names of the keyboard in use: layouts, variants, model, options.
    fn keyboard_names(&self) -> (String, String, String, Option<String>) {
        let config = &self.keyboard_config;
        let (layout, variant) = config.xkb_names();
        let options = (!config.options.is_empty()).then(|| config.options.clone());
        (layout, variant, config.model.clone(), options)
    }

    /// Builds the synthetic keys again if the keyboard settings or the active input source
    /// changed: `input.type` and `input.key` follow the keymap the apps have (#195).
    fn refresh_keys(&mut self) {
        let active = self.active_layout();
        let (layout, variant, model, options) = self.keyboard_names();
        let key = (
            layout.clone(),
            variant.clone(),
            model.clone(),
            options.clone().unwrap_or_default(),
            active,
        );
        if self.keys_for.as_ref() == Some(&key) {
            return;
        }
        match crate::keys::KeyLookup::for_names(&layout, &variant, &model, options, active) {
            Ok(keys) => {
                self.keys = keys;
                self.keys_for = Some(key);
            }
            Err(err) => warn!("synthetic keys: {err}"),
        }
    }

    /// Types `ch` on a temporary key: a keymap with one more key that has the character is sent
    /// to the apps, the key is pressed, and the caller restores the keymap afterwards.
    fn type_on_temporary_key(&mut self, ch: char) -> Result<(), String> {
        let (layout, variant, model, options) = self.keyboard_names();
        let (text, code) = crate::keys::keymap_with_key_for(&layout, &variant, &model, options, ch)
            .ok_or_else(|| format!("character cannot be typed: {ch:?}"))?;
        let keyboard = self.seat.get_keyboard().ok_or("no keyboard")?;
        let active = self.active_layout();
        keyboard
            .set_keymap_from_string(self, text)
            .map_err(|e| format!("character cannot be typed: {ch:?} ({e:?})"))?;
        keyboard.with_xkb_state(self, |mut context| {
            context.set_layout(smithay::input::keyboard::Layout(active));
        });
        let stroke = crate::keys::KeyStroke {
            code,
            shift: false,
            level3: None,
        };
        self.synthetic_key(stroke, KeyState::Pressed);
        self.synthetic_key(stroke, KeyState::Released);
        Ok(())
    }

    /// Puts the keymap of the keyboard settings back after the temporary keys.
    fn restore_keymap(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let active = self.active_layout();
        let (layout, variant, model, options) = self.keyboard_names();
        let xkb = smithay::input::keyboard::XkbConfig {
            rules: "",
            model: &model,
            layout: &layout,
            variant: &variant,
            options,
        };
        if let Err(err) = keyboard.set_xkb_config(self, xkb) {
            warn!("restoring the keymap: {err:?}");
            return;
        }
        keyboard.with_xkb_state(self, |mut context| {
            context.set_layout(smithay::input::keyboard::Layout(active));
        });
    }

    /// Time for synthetic input events, in milliseconds since start.
    pub(crate) fn now_msec(&self) -> u32 {
        u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX)
    }

    /// Starts a shell command as a client of this session.
    pub fn launch(&mut self, command: &str) -> Result<u32, String> {
        self.launch_with(command, false)
    }

    /// Like `launch`; with `shell` the process is a shell component and sees its
    /// protocols (`shell.rs`).
    pub fn launch_with(&mut self, command: &str, shell: bool) -> Result<u32, String> {
        self.spawn_child(command, shell).map(|child| {
            let pid = child.id();
            crate::reap::reap(child, || {});
            pid
        })
    }

    /// Starts the process of `launch_with` and hands it over: the caller reaps it (`reap.rs`).
    pub fn spawn_child(
        &mut self,
        command: &str,
        shell: bool,
    ) -> Result<std::process::Child, String> {
        let mut process = Command::new("sh");
        process
            .arg("-c")
            .arg(command)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env("XDG_ACTIVATION_TOKEN", self.launch_activation_token());
        if let Some(address) = &self.session_bus {
            process.env("DBUS_SESSION_BUS_ADDRESS", address);
        }
        if shell {
            process.env(silica_protocols::shell::SHELL_ENV, "1");
        } else {
            process.env_remove(silica_protocols::shell::SHELL_ENV);
        }
        let child = process
            .spawn()
            .map_err(|e| format!("starting {command}: {e}"))?;
        info!("started {command} (pid {})", child.id());
        self.perf.launched(child.id(), std::time::Instant::now());
        Ok(child)
    }

    /// The frame measurements, with the surfaces named as in `window.list` and `layer.list`.
    fn perf_frames(&mut self) -> Vec<silica_control::commands::SurfaceFrames> {
        let mut names = std::collections::HashMap::new();
        for window in self.space.elements() {
            if let (Some(toplevel), Some(id)) = (window.toplevel(), Self::window_id(window)) {
                names.insert(toplevel.wl_surface().id(), format!("window {id}"));
            }
        }
        for layer in smithay::desktop::layer_map_for_output(&self.output).layers() {
            names.insert(
                layer.wl_surface().id(),
                format!("layer {}", layer.namespace()),
            );
        }
        let refresh = self
            .output
            .current_mode()
            .map_or(0, |mode| mode.refresh)
            .max(1);
        // refresh is in mHz; without a sensible value 60 Hz is assumed.
        let period = if refresh < 1000 {
            std::time::Duration::from_micros(16_667)
        } else {
            std::time::Duration::from_secs_f64(1000.0 / f64::from(refresh))
        };
        self.perf.report(period, |id| {
            names
                .get(id)
                .cloned()
                .unwrap_or_else(|| "surface".to_owned())
        })
    }

    pub fn assign_window_id(&mut self, window: &Window) -> u64 {
        self.next_window_id = self.next_window_id.wrapping_add(1);
        let id = self.next_window_id;
        window.user_data().insert_if_missing(|| WindowId(id));
        id
    }

    pub fn window_id(window: &Window) -> Option<u64> {
        window.user_data().get::<WindowId>().map(|id| id.0)
    }

    /// The window with that id, or the focused one.
    fn target_window(&self, id: Option<u64>) -> Result<Window, String> {
        match id {
            Some(id) => self
                .space
                .elements()
                .find(|w| Self::window_id(w) == Some(id))
                .cloned()
                .ok_or_else(|| format!("no window with id {id}")),
            None => self
                .focused_window()
                .ok_or_else(|| "no window has the focus".to_owned()),
        }
    }

    /// The surface a keyboard focus belongs to: for a popup (an app's menu or popover, a
    /// shell layer's menu) the root of its chain, the window or layer it hangs from; otherwise
    /// the surface itself.
    pub(crate) fn focus_root(
        &self,
        focus: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        self.popups
            .find_popup(focus)
            .and_then(|popup| smithay::desktop::find_popup_root_surface(&popup).ok())
            .unwrap_or_else(|| focus.clone())
    }

    pub(crate) fn focused_window(&self) -> Option<Window> {
        let focus = self.seat.get_keyboard()?.current_focus()?;
        if let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &focus))
        {
            return Some(window.clone());
        }
        // With the keyboard on an app's popup (its open menu) the window is the popup's own:
        // a press inside the menu must not look like leaving the window, which would end the
        // menu before the click reaches its item.
        let popup_root = self
            .popups
            .find_popup(&focus)
            .and_then(|popup| smithay::desktop::find_popup_root_surface(&popup).ok());
        if let Some(window) = popup_root.as_ref().and_then(|root| self.window_for(root)) {
            return Some(window);
        }
        // The same for a menu of a shell layer (silica-os/silica-desktop#222): the layer is the root.
        let focus = popup_root.unwrap_or(focus);
        // With the keyboard on a shell component (an open menu of the bar) the active app
        // stays the one on top: the menu commands act on it.
        let on_layer = smithay::desktop::layer_map_for_output(&self.output)
            .layers()
            .any(|layer| layer.wl_surface() == &focus);
        if on_layer {
            return self.space.elements().last().cloned();
        }
        None
    }

    /// Gives the focus to a window (or to none) and brings it to the top.
    pub fn focus_window(&mut self, window: Option<Window>) {
        // A window with a modal dialog (a sheet) gives the focus to the dialog.
        let window = window.map(|w| self.modal_child(&w).unwrap_or(w));
        // Locked: nothing takes the focus from the lock surface; the window is remembered.
        if self.locked {
            if window.is_some() {
                self.locked_focus = window;
            }
            return;
        }
        // Activation leaving the window (or app) an open menu belongs to closes the menu.
        if !self.transients.is_empty() && self.focused_window() != window {
            self.dismiss_transients();
        }
        // A window taking the focus ends the frontmost windowless app (#142).
        if window.is_some() {
            self.set_windowless_front(None);
        }
        for other in self.space.elements() {
            other.set_activated(false);
        }
        let focus = window
            .as_ref()
            .and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
        if let Some(window) = &window {
            self.space.raise_element(window, true);
            self.raise_children(window);
        }
        // A layer with exclusive keyboard above the windows keeps the focus (wlr-layer-shell).
        let focus = self.exclusive_layer_surface().or(focus);
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, focus, SERIAL_COUNTER.next_serial());
        }
        // The keyboard may have stayed on a shell layer: the shell still learns the window.
        self.reannounce_focus();
        for window in self.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_pending_configure();
            }
        }
    }

    fn window_list(&self) -> Vec<WindowInfo> {
        let focused = self.focused_window();
        // Space lists from bottom to top: it is reversed to have the top first.
        let mut windows: Vec<(&Window, bool)> =
            self.space.elements().rev().map(|w| (w, false)).collect();
        windows.extend(self.minimized.iter().map(|(w, _)| (w, true)));
        windows
            .into_iter()
            .filter_map(|(window, minimized)| {
                let id = Self::window_id(window)?;
                let location = if minimized {
                    self.minimized
                        .iter()
                        .find(|(w, _)| w == window)
                        .map(|(_, location)| *location)
                        .unwrap_or_default()
                } else {
                    self.space.element_location(window).unwrap_or_default()
                };
                let size = window.geometry().size;
                let (title, app_id) = window
                    .toplevel()
                    .map(|toplevel| {
                        with_states(toplevel.wl_surface(), |states| {
                            states
                                .data_map
                                .get::<XdgToplevelSurfaceData>()
                                .and_then(|data| data.lock().ok())
                                .map(|data| (data.title.clone(), data.app_id.clone()))
                                .unwrap_or_default()
                        })
                    })
                    .unwrap_or_default();
                Some(WindowInfo {
                    id,
                    title,
                    app_id,
                    x: location.x,
                    y: location.y,
                    width: size.w,
                    height: size.h,
                    focused: focused.as_ref() == Some(window),
                    minimized,
                    fullscreen: crate::windows::is_fullscreen(window),
                })
            })
            .collect()
    }

    /// Kills (SIGKILL) the processes of an app's windows: all those with that app_id, on
    /// screen, minimized or hidden. Never a shell component nor the compositor itself.
    fn force_quit(&mut self, app_id: &str) -> Result<usize, String> {
        let pids: std::collections::BTreeSet<i32> = self
            .space
            .elements()
            .chain(self.minimized.iter().map(|(w, _)| w))
            .chain(self.hidden_apps.windows())
            .chain(self.dock_hidden.windows())
            .filter(|w| crate::windows::window_app(w).as_deref() == Some(app_id))
            .filter_map(|w| {
                let surface = w.toplevel()?.wl_surface().clone();
                let client = self.display_handle.get_client(surface.id()).ok()?;
                let data = client.get_data::<crate::state::ClientState>()?;
                (!data.shell).then_some(data.pid?)
            })
            .filter(|pid| *pid != std::process::id() as i32)
            .collect();
        if pids.is_empty() {
            return Err(format!("no process for {app_id}"));
        }
        for pid in &pids {
            let Some(pid) = rustix::process::Pid::from_raw(*pid) else {
                continue;
            };
            if let Err(err) = rustix::process::kill_process(pid, rustix::process::Signal::KILL) {
                warn!("force quit of {app_id} ({}): {err}", pid.as_raw_nonzero());
            }
        }
        Ok(pids.len())
    }

    /// Writes the screenshot to a temporary file and announces it to the shell (`broadcast`
    /// "screenshot" = [`CaptureAnnouncement`] as JSON): the thumbnail, then its place.
    /// `corner` is where the image's top-left pixel sits on the output (physical pixels),
    /// to draw the pointer in when asked.
    fn publish_screenshot(
        &mut self,
        mut capture: crate::capture::Capture,
        options: CaptureOptions,
        corner: (f64, f64),
    ) -> Result<Value, String> {
        if options.pointer.unwrap_or(self.screenshot_settings.pointer)
            && let Some(pointer) = self.seat.get_pointer()
        {
            let scale = self.output.current_scale().fractional_scale();
            let origin = self
                .space
                .output_geometry(&self.output)
                .map(|g| g.loc.to_f64())
                .unwrap_or_default();
            let at = pointer.current_location() - origin;
            let (w, h, arrow) = crate::cursor::arrow();
            crate::capture::draw_over(
                &mut capture,
                (at.x * scale - corner.0, at.y * scale - corner.1),
                (w, h, &arrow),
                scale,
            );
        }
        let capture = &capture;
        let png = capture.to_png()?;
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        self.screenshot_count = self.screenshot_count.wrapping_add(1);
        let path = dir.join(format!(
            "silica-screenshot-{}-{}.png",
            std::process::id(),
            self.screenshot_count
        ));
        std::fs::write(&path, &png).map_err(|e| format!("writing {}: {e}", path.display()))?;
        let path = path.to_string_lossy().into_owned();
        let announcement = CaptureAnnouncement {
            path: path.clone(),
            width: capture.width,
            height: capture.height,
            clipboard: options.clipboard,
            seq: self.screenshot_count,
        };
        self.broadcast_to_shell(
            "screenshot".to_owned(),
            serde_json::to_string(&announcement).map_err(|e| e.to_string())?,
        );
        Ok(json!({
            "path": path,
            "width": capture.width,
            "height": capture.height,
            "clipboard": options.clipboard,
        }))
    }

    fn command_screenshot(&mut self, params: ScreenshotParams) -> Result<Value, String> {
        let capture = self.screenshot()?;
        let png = capture.to_png()?;
        let mut shot = Screenshot {
            width: capture.width,
            height: capture.height,
            path: None,
            png_base64: None,
        };
        match params.path {
            Some(path) => {
                std::fs::write(&path, &png).map_err(|e| format!("writing {path}: {e}"))?;
                shot.path = Some(path);
            }
            None => shot.png_base64 = Some(base64::engine::general_purpose::STANDARD.encode(&png)),
        }
        to_value(shot)
    }
}

/// Whether `app_id` has processes in the app registry (silica-control `apps`).
fn registered_app(app_id: &str) -> bool {
    silica_control::apps::valid_app_id(app_id)
        && silica_control::apps::dir()
            .is_some_and(|dir| !silica_control::apps::instances(&dir, Some(app_id)).is_empty())
}
