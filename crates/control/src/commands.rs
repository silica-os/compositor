//! The catalog of Silica commands: the same list for UI, CLI, control channel and MCP.
//!
//! Every command has an id, a label, a description, an optional shortcut and the JSON schema
//! of its parameters. Whoever runs the commands (the compositor) must handle every id in the catalog: a
//! test in `crates/compositor/tests/` checks it.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Captures the output as a PNG.
pub const UI_SCREENSHOT: &str = "ui.screenshot";
pub const SCREENSHOT_SCREEN: &str = "screenshot.screen";
pub const APP_FORCE_QUIT: &str = "app.force_quit";
pub const APP_FORCE_QUIT_PANEL: &str = "app.force_quit_panel";
pub const SCREENSHOT_SELECTION: &str = "screenshot.selection";
pub const SCREENSHOT_TOOLBAR: &str = "screenshot.toolbar";
pub const SCREENSHOT_REGION: &str = "screenshot.region";
pub const SCREENSHOT_WINDOW_AT: &str = "screenshot.window_at";
pub const SCREENSHOT_WINDOW_VEIL: &str = "screenshot.window_veil";
pub const SCREENSHOT_SCREEN_CLIPBOARD: &str = "screenshot.screen_clipboard";
pub const SCREENSHOT_SELECTION_CLIPBOARD: &str = "screenshot.selection_clipboard";
pub const SCREENSHOT_SETTINGS: &str = "screenshot.settings";
pub const UI_ANIMATION_SCALE: &str = "ui.animation_scale";
pub const UI_GLASS_QUALITY: &str = "ui.glass_quality";
pub const PERF_RENDER: &str = "perf.render";
/// Lists the command catalog.
pub const ENGINE_COMMANDS: &str = "engine.commands";
/// Runs a catalog command by id: `{"id": "...", "params": {...}}`.
pub const ENGINE_EXECUTE: &str = "engine.execute";
pub const WINDOW_LIST: &str = "window.list";
pub const WINDOW_FOCUS: &str = "window.focus";
pub const WINDOW_CLOSE: &str = "window.close";
pub const WINDOW_MOVE: &str = "window.move";
pub const WINDOW_MINIMIZE: &str = "window.minimize";
/// The minimize effect (Dock setting): genie or scale.
pub const WINDOW_MINIMIZE_EFFECT: &str = "window.minimize_effect";
/// Where the Dock's app icons are, for windows that minimize into their app's icon.
pub const DOCK_ICONS: &str = "dock.icons";
pub const WINDOW_FULLSCREEN: &str = "window.fullscreen";
pub const WINDOW_TILE_LEFT: &str = "window.tile_left";
pub const WINDOW_TILE_RIGHT: &str = "window.tile_right";
pub const WINDOW_TILE_TOP: &str = "window.tile_top";
pub const WINDOW_TILE_BOTTOM: &str = "window.tile_bottom";
pub const WINDOW_TILE_FILL: &str = "window.tile_fill";
pub const WINDOW_ARRANGE_2: &str = "window.arrange_2";
pub const WINDOW_ARRANGE_3: &str = "window.arrange_3";
pub const WINDOW_ARRANGE_4: &str = "window.arrange_4";
pub const APP_LAUNCH: &str = "app.launch";
pub const SESSION_QUIT: &str = "session.quit";
/// Locks the session (the lock screen asks for the password).
pub const SESSION_LOCK: &str = "session.lock";
pub const APP_QUIT: &str = "app.quit";
/// Hides the frontmost app (or `app_id`): its windows leave the screen until it is activated.
pub const APP_HIDE: &str = "app.hide";
/// Hides every app but the frontmost one.
pub const APP_HIDE_OTHERS: &str = "app.hide_others";
/// Brings back every hidden app.
pub const APP_SHOW_ALL: &str = "app.show_all";
/// The frontmost app's menus, as the menu bar shows them (silica-menu-v1, #25).
pub const APP_MENU: &str = "app.menu";
/// Chooses an item of the frontmost app's menus: `{"id": n}`.
pub const APP_MENU_ACTIVATE: &str = "app.menu_activate";
pub const INPUT_POINTER_MOVE: &str = "input.pointer_move";
pub const INPUT_POINTER_BUTTON: &str = "input.pointer_button";
pub const INPUT_CLICK: &str = "input.click";
pub const INPUT_SCROLL: &str = "input.scroll";
pub const INPUT_KEY: &str = "input.key";
pub const INPUT_TYPE: &str = "input.type";
pub const INPUT_GESTURE: &str = "input.gesture";
pub const LAYER_LIST: &str = "layer.list";
pub const PERF_FRAMES: &str = "perf.frames";
pub const PERF_PROCESS: &str = "perf.process";
pub const GLASS_LIST: &str = "glass.list";
pub const FARO_TOGGLE: &str = "faro.toggle";
pub const NOTIFICATIONS_TOGGLE: &str = "notifications.toggle";
pub const LAUNCHER_TOGGLE: &str = "launcher.toggle";
pub const SWITCHER_NEXT: &str = "switcher.next";
pub const SWITCHER_PREVIOUS: &str = "switcher.previous";
pub const SWITCHER_COMMIT: &str = "switcher.commit";
pub const SWITCHER_CANCEL: &str = "switcher.cancel";
pub const SWITCHER_EXPOSE: &str = "switcher.expose";
pub const CONTROLCENTER_TOGGLE: &str = "controlcenter.toggle";
pub const SYSTEM_STATUS: &str = "system.status";
pub const SPACE_LIST: &str = "space.list";
pub const SPACE_NEXT: &str = "space.next";
pub const SPACE_PREVIOUS: &str = "space.previous";
pub const SPACE_GOTO: &str = "space.goto";
pub const SPACE_ADD: &str = "space.add";
pub const SPACE_REMOVE: &str = "space.remove";
pub const WINDOW_MOVE_TO_SPACE: &str = "window.move_to_space";
pub const PANORAMA_TOGGLE: &str = "panorama.toggle";
pub const PANORAMA_STATE: &str = "panorama.state";
pub const PANORAMA_SPACES_BAR: &str = "panorama.spaces_bar";
pub const PANORAMA_APP: &str = "panorama.app";
pub const DESKTOP_SHOW: &str = "desktop.show";
pub const SYSTEM_WIFI: &str = "system.wifi";
pub const SYSTEM_BLUETOOTH: &str = "system.bluetooth";
pub const SYSTEM_VOLUME: &str = "system.volume";
pub const SYSTEM_VOLUME_UP: &str = "system.volume_up";
pub const SYSTEM_VOLUME_DOWN: &str = "system.volume_down";
pub const SYSTEM_VOLUME_UP_FINE: &str = "system.volume_up_fine";
pub const SYSTEM_VOLUME_DOWN_FINE: &str = "system.volume_down_fine";
pub const SYSTEM_BRIGHTNESS_UP_FINE: &str = "system.brightness_up_fine";
pub const SYSTEM_BRIGHTNESS_DOWN_FINE: &str = "system.brightness_down_fine";
pub const SYSTEM_MUTE: &str = "system.mute";
pub const SYSTEM_MUTE_TOGGLE: &str = "system.mute_toggle";
pub const SYSTEM_BRIGHTNESS: &str = "system.brightness";
pub const SYSTEM_BRIGHTNESS_UP: &str = "system.brightness_up";
pub const SYSTEM_BRIGHTNESS_DOWN: &str = "system.brightness_down";
pub const SYSTEM_KEYBOARD_BRIGHTNESS: &str = "system.keyboard_brightness";
pub const SYSTEM_KEYBOARD_BRIGHTNESS_UP: &str = "system.keyboard_brightness_up";
pub const SYSTEM_KEYBOARD_BRIGHTNESS_DOWN: &str = "system.keyboard_brightness_down";
pub const SYSTEM_DND: &str = "system.dnd";
pub const SYSTEM_DARK: &str = "system.dark";
pub const SYSTEM_DARK_TOGGLE: &str = "system.dark_toggle";
pub const KEYBOARD_STATE: &str = "keyboard.state";
pub const KEYBOARD_NEXT_SOURCE: &str = "keyboard.next_source";
pub const KEYBOARD_SELECT_SOURCE: &str = "keyboard.select_source";
pub const INPUT_STATE: &str = "input.state";
pub const IDLE_STATE: &str = "idle.state";
pub const OUTPUT_LIST: &str = "output.list";
pub const OUTPUT_CONFIGURE: &str = "output.configure";
pub const OUTPUT_COLOR_TEMPERATURE: &str = "output.color_temperature";

#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub id: &'static str,
    /// Label for the interface (menus, Faro).
    pub label: &'static str,
    pub description: &'static str,
    /// Shortcut in the [`Shortcut::parse`] format, for example `Logo+Shift+Q`.
    pub shortcut: Option<&'static str>,
    /// JSON schema of the parameters.
    pub params: fn() -> Value,
}

impl CommandSpec {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "description": self.description,
            "shortcut": self.shortcut,
            "params": (self.params)(),
        })
    }
}

fn no_params() -> Value {
    json!({"type": "object", "properties": {}, "additionalProperties": false})
}

fn index_param() -> Value {
    json!({
        "type": "object",
        "properties": {"index": {"type": "integer", "minimum": 0}},
        "required": ["index"],
        "additionalProperties": false
    })
}

fn enabled_param() -> Value {
    json!({
        "type": "object",
        "properties": {"enabled": {"type": "boolean"}},
        "required": ["enabled"],
        "additionalProperties": false
    })
}

fn value_param() -> Value {
    json!({
        "type": "object",
        "properties": {"value": {"type": "number", "minimum": 0, "maximum": 1}},
        "required": ["value"],
        "additionalProperties": false
    })
}

fn window_target() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "integer", "minimum": 0, "description": "Window; without it, the focused one"}
        },
        "additionalProperties": false
    })
}

fn point() -> Value {
    json!({
        "type": "object",
        "properties": {
            "x": {"type": "number", "description": "Logical pixels from the left edge of the output"},
            "y": {"type": "number", "description": "Logical pixels from the top edge of the output"},
            "button": {"type": "string", "enum": ["left", "right", "middle"]}
        },
        "required": ["x", "y"],
        "additionalProperties": false
    })
}

pub const CATALOG: &[CommandSpec] = &[
    CommandSpec {
        id: UI_SCREENSHOT,
        label: "Screenshot",
        description: "Captures the output as a PNG, into a file or as base64.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Where to write the PNG; without it, it comes back in png_base64"}
                },
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: SCREENSHOT_SCREEN,
        label: "Screenshot",
        description: "Captures the whole screen; the thumbnail appears at the bottom right and the file goes to the chosen place (the Desktop by default).",
        shortcut: Some("Logo+Shift+3"),
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "pointer": {"type": "boolean", "description": "Draw the pointer into the image (default: screenshot.settings)"},
                    "clipboard": {"type": "boolean", "description": "The capture goes to the clipboard"}
                },
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: SCREENSHOT_SCREEN_CLIPBOARD,
        label: "Screenshot to the clipboard",
        description: "Captures the whole screen into the clipboard, without a file.",
        shortcut: Some("Logo+Ctrl+Shift+3"),
        params: no_params,
    },
    CommandSpec {
        id: SCREENSHOT_SELECTION,
        label: "Screenshot of a part",
        description: "The crosshair to choose a region (Space: a window); shell component.",
        shortcut: Some("Logo+Shift+4"),
        params: no_params,
    },
    CommandSpec {
        id: SCREENSHOT_SELECTION_CLIPBOARD,
        label: "Screenshot of a part to the clipboard",
        description: "The crosshair to choose a region (Space: a window), copied into the clipboard; shell component.",
        shortcut: Some("Logo+Ctrl+Shift+4"),
        params: no_params,
    },
    CommandSpec {
        id: SCREENSHOT_TOOLBAR,
        label: "Screenshot tools",
        description: "The screenshot bar: screen, window or selection; shell component.",
        shortcut: Some("Logo+Shift+5"),
        params: no_params,
    },
    CommandSpec {
        id: SCREENSHOT_REGION,
        label: "Screenshot of a region",
        description: "Captures a rectangle of the screen, in points.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "x": {"type": "number"}, "y": {"type": "number"},
                    "width": {"type": "number"}, "height": {"type": "number"},
                    "pointer": {"type": "boolean", "description": "Draw the pointer into the image (default: screenshot.settings)"},
                    "clipboard": {"type": "boolean", "description": "The capture goes to the clipboard"}
                },
                "required": ["x", "y", "width", "height"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: SCREENSHOT_WINDOW_AT,
        label: "Screenshot of a window",
        description: "Captures the window under the given point, on its own, with its shadow on a transparent background.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "x": {"type": "number"}, "y": {"type": "number"},
                    "shadow": {"type": "boolean", "description": "With the shadow (default true)"},
                    "or_screen": {"type": "boolean", "description": "No window there: the whole screen"},
                    "pointer": {"type": "boolean", "description": "Draw the pointer into the image (default: screenshot.settings)"},
                    "clipboard": {"type": "boolean", "description": "The capture goes to the clipboard"}
                },
                "required": ["x", "y"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: SCREENSHOT_SETTINGS,
        label: "Screenshot settings",
        description: "The settings the compositor applies to its own screenshot shortcuts: whether the pointer is drawn into the image.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"pointer": {"type": "boolean"}},
                "required": ["pointer"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: SCREENSHOT_WINDOW_VEIL,
        label: "Window screenshot veil",
        description: "While on, the compositor veils the frame of the window under the pointer \
                      (window mode of the screenshot tools).",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"enabled": {"type": "boolean"}},
                "required": ["enabled"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: UI_ANIMATION_SCALE,
        label: "Animation speed",
        description: "Multiplies animation durations; 0 turns them off (reduce motion).",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"scale": {"type": "number", "minimum": 0, "maximum": 1000}},
                "required": ["scale"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: UI_GLASS_QUALITY,
        label: "Glass quality",
        description: "Full glass, static frosted (desktop background only) or opaque; auto chooses based on the GPU.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"quality": {"type": "string", "enum": ["auto", "full", "frosted", "opaque"]}},
                "required": ["quality"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: WINDOW_MINIMIZE_EFFECT,
        label: "Minimize effect",
        description: "How windows go into the Dock and come out of it: genie or scale (the Dock's setting).",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"effect": {"type": "string", "enum": ["genie", "scale"]}},
                "required": ["effect"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: DOCK_ICONS,
        label: "Dock icons",
        description: "The shell tells where the Dock's app icons are (centers, in the Dock surface), so that a window minimizes into its app's icon when the Dock setting minimizeToApp is on.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "icons": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "app_id": {"type": "string"},
                                "x": {"type": "number"},
                                "y": {"type": "number"}
                            },
                            "required": ["app_id", "x", "y"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["icons"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: PERF_RENDER,
        label: "Rendering times",
        description: "Duration of the compositor's rendering per drawn frame: mean, percentiles, maximum, and the same per stage (build, draw, submit, gpu).",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"reset": {"type": "boolean"}},
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: ENGINE_COMMANDS,
        label: "List commands",
        description: "Returns the command catalog with parameters and shortcuts.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: ENGINE_EXECUTE,
        label: "Run command",
        description: "Runs a catalog command by id.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "params": {"type": "object"}
                },
                "required": ["id"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: WINDOW_LIST,
        label: "List windows",
        description: "Open windows, from top to bottom, with title, app and geometry.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: LAYER_LIST,
        label: "List layers",
        description: "Shell surfaces (wlr-layer-shell): namespace, layer, geometry, exclusive zone.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: LAUNCHER_TOGGLE,
        label: "Apps",
        description: "Opens or closes the app launcher: all apps in a grid by category, with the recent ones on top (shell component).",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: SWITCHER_NEXT,
        label: "Next app",
        description: "Opens the app switcher on the previously used app, or moves to the next one; releasing the Logo key activates the chosen app (shell component).",
        shortcut: Some("Logo+Tab"),
        params: no_params,
    },
    CommandSpec {
        id: SWITCHER_PREVIOUS,
        label: "Previous app",
        description: "Like switcher.next, backwards.",
        shortcut: Some("Logo+Shift+Tab"),
        params: no_params,
    },
    CommandSpec {
        id: SWITCHER_COMMIT,
        label: "Activate the chosen app",
        description: "Closes the app switcher activating the chosen app (releasing the Logo key).",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: SWITCHER_CANCEL,
        label: "Cancel the switcher",
        description: "Closes the app switcher without changing app (Esc).",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: SWITCHER_EXPOSE,
        label: "App Exposé from the switcher",
        description: "Closes the app switcher and opens App Exposé of the chosen app (Up or Down while it is open); the Logo key released afterwards changes nothing.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: FARO_TOGGLE,
        label: "Faro",
        description: "Opens or closes Faro, the app search (shell component).",
        shortcut: Some("Logo+space"),
        params: no_params,
    },
    CommandSpec {
        id: NOTIFICATIONS_TOGGLE,
        label: "Notification Center",
        description: "Opens or closes the Notification Center (shell component; click on the clock).",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: CONTROLCENTER_TOGGLE,
        label: "Control Center",
        description: "Opens or closes the Control Center (shell component; icon in the bar).",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: SPACE_LIST,
        label: "List spaces",
        description: "The workspaces: which one is active and the window ids of each.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: SPACE_NEXT,
        label: "Next space",
        description: "Moves to the space on the right (does nothing past the last one).",
        shortcut: Some("Ctrl+Right"),
        params: no_params,
    },
    CommandSpec {
        id: SPACE_PREVIOUS,
        label: "Previous space",
        description: "Moves to the space on the left (does nothing before the first one).",
        shortcut: Some("Ctrl+Left"),
        params: no_params,
    },
    CommandSpec {
        id: SPACE_GOTO,
        label: "Go to space",
        description: "Moves to space `index` (from 0).",
        shortcut: None,
        params: index_param,
    },
    CommandSpec {
        id: SPACE_ADD,
        label: "New space",
        description: "Adds an empty space at the end; returns its index.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: SPACE_REMOVE,
        label: "Remove space",
        description: "Removes space `index`; its windows move to the previous one.",
        shortcut: None,
        params: index_param,
    },
    CommandSpec {
        id: WINDOW_MOVE_TO_SPACE,
        label: "Move to space",
        description: "Moves window `id` (or the focused one) to space `index`.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "minimum": 0},
                    "index": {"type": "integer", "minimum": 0}
                },
                "required": ["index"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: PANORAMA_TOGGLE,
        label: "Panorama",
        description: "Shows all the windows of the space side by side with the spaces bar at the top; if open, closes it.",
        shortcut: Some("Ctrl+Up"),
        params: no_params,
    },
    CommandSpec {
        id: PANORAMA_APP,
        label: "App windows",
        description: "Shows the windows of the frontmost app in the middle, with the title below; if already open, closes it.",
        shortcut: Some("Ctrl+Down"),
        params: no_params,
    },
    CommandSpec {
        id: DESKTOP_SHOW,
        label: "Show desktop",
        description: "Moves the windows off screen to show the desktop; again (or a click) puts them back.",
        shortcut: Some("F11"),
        params: no_params,
    },
    CommandSpec {
        id: PANORAMA_SPACES_BAR,
        label: "Spaces bar",
        description: "With Panorama open, expands (`enabled`) or collapses the spaces bar with the thumbnails, like the pointer at the top.",
        shortcut: None,
        params: enabled_param,
    },
    CommandSpec {
        id: PANORAMA_STATE,
        label: "Panorama state",
        description: "Whether Panorama is open, the animation progress (0–1) and the arranged window frames.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_STATUS,
        label: "System state",
        description: "Wi-Fi, Bluetooth, battery, brightness, volume and Do Not Disturb, from silica-daemon (keys of dev.frascella.Silica.System1).",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_WIFI,
        label: "Wi-Fi",
        description: "Turns Wi-Fi on or off.",
        shortcut: None,
        params: enabled_param,
    },
    CommandSpec {
        id: SYSTEM_BLUETOOTH,
        label: "Bluetooth",
        description: "Turns Bluetooth on or off.",
        shortcut: None,
        params: enabled_param,
    },
    CommandSpec {
        id: SYSTEM_VOLUME,
        label: "Volume",
        description: "Volume of the default output, from 0 to 1.",
        shortcut: None,
        params: value_param,
    },
    CommandSpec {
        id: SYSTEM_VOLUME_UP,
        label: "Volume up",
        description: "Raises the volume by one sixteenth (unmutes).",
        shortcut: Some("XF86AudioRaiseVolume"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_VOLUME_DOWN,
        label: "Volume down",
        description: "Lowers the volume by one sixteenth.",
        shortcut: Some("XF86AudioLowerVolume"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_VOLUME_UP_FINE,
        label: "Volume up slightly",
        description: "Raises the volume by a quarter step (1/64).",
        shortcut: Some("Alt+Shift+XF86AudioRaiseVolume"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_VOLUME_DOWN_FINE,
        label: "Volume down slightly",
        description: "Lowers the volume by a quarter step (1/64).",
        shortcut: Some("Alt+Shift+XF86AudioLowerVolume"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_MUTE,
        label: "Mute",
        description: "Mutes or unmutes the default output.",
        shortcut: None,
        params: enabled_param,
    },
    CommandSpec {
        id: SYSTEM_MUTE_TOGGLE,
        label: "Toggle mute",
        description: "Toggles mute on the default output.",
        shortcut: Some("XF86AudioMute"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_BRIGHTNESS,
        label: "Brightness",
        description: "Screen brightness, from 0 to 1 (never fully off).",
        shortcut: None,
        params: value_param,
    },
    CommandSpec {
        id: SYSTEM_BRIGHTNESS_UP,
        label: "Brightness up",
        description: "Raises the brightness by one sixteenth.",
        shortcut: Some("XF86MonBrightnessUp"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_BRIGHTNESS_DOWN,
        label: "Brightness down",
        description: "Lowers the brightness by one sixteenth.",
        shortcut: Some("XF86MonBrightnessDown"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_BRIGHTNESS_UP_FINE,
        label: "Brightness up slightly",
        description: "Raises the brightness by a quarter step (1/64).",
        shortcut: Some("Alt+Shift+XF86MonBrightnessUp"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_BRIGHTNESS_DOWN_FINE,
        label: "Brightness down slightly",
        description: "Lowers the brightness by a quarter step (1/64).",
        shortcut: Some("Alt+Shift+XF86MonBrightnessDown"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_KEYBOARD_BRIGHTNESS,
        label: "Keyboard brightness",
        description: "Keyboard backlight, from 0 to 1 (UPower).",
        shortcut: None,
        params: value_param,
    },
    CommandSpec {
        id: SYSTEM_KEYBOARD_BRIGHTNESS_UP,
        label: "Keyboard brightness up",
        description: "Raises the keyboard backlight by one sixteenth.",
        shortcut: Some("XF86KbdBrightnessUp"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_KEYBOARD_BRIGHTNESS_DOWN,
        label: "Keyboard brightness down",
        description: "Lowers the keyboard backlight by one sixteenth.",
        shortcut: Some("XF86KbdBrightnessDown"),
        params: no_params,
    },
    CommandSpec {
        id: SYSTEM_DND,
        label: "Do Not Disturb",
        description: "With Do Not Disturb on, notifications arrive in the Center without a banner.",
        shortcut: None,
        params: enabled_param,
    },
    CommandSpec {
        id: SYSTEM_DARK,
        label: "Dark appearance",
        description: "Dark or light appearance for the whole system: The compositor, the shell and the apps (Settings portal).",
        shortcut: None,
        params: enabled_param,
    },
    CommandSpec {
        id: SYSTEM_DARK_TOGGLE,
        label: "Toggle appearance",
        description: "Switches from the light appearance to the dark one and back.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: GLASS_LIST,
        label: "List glass",
        description: "Surfaces with glass regions (silica-glass-v1): surface, origin and regions.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: WINDOW_FOCUS,
        label: "Bring to front",
        description: "Gives the focus to a window and brings it above the others; if minimized, restores it.",
        shortcut: None,
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_CLOSE,
        label: "Close window",
        description: "Asks the window to close.",
        shortcut: Some("Logo+W"),
        params: window_target,
    },
    CommandSpec {
        id: APP_QUIT,
        label: "Quit app",
        description: "Quits the frontmost app, or the app `app_id`: a Silica app over its registry socket (it keeps running when its windows close), any other app by closing all its windows (same app_id).",
        shortcut: Some("Logo+Q"),
        params: || {
            json!({
                "type": "object",
                "properties": {"app_id": {"type": "string"}},
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: APP_HIDE,
        label: "Hide app",
        description: "Hides the frontmost app, or the app `app_id`: its windows leave the screen (they stay in the Dock's list) until the app is activated again.",
        shortcut: Some("Logo+H"),
        params: || {
            json!({
                "type": "object",
                "properties": {"app_id": {"type": "string"}},
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: APP_HIDE_OTHERS,
        label: "Hide others",
        description: "Hides every app but the frontmost one.",
        shortcut: Some("Logo+Alt+H"),
        params: no_params,
    },
    CommandSpec {
        id: APP_SHOW_ALL,
        label: "Show all",
        description: "Brings back every hidden app, below the app in front.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: APP_MENU,
        label: "App menus",
        description: "The menus the frontmost app published for the menu bar: `app_id`, `serial` (0 if it published none) and `model` (silica-menu-v1 JSON, null if none).",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: APP_MENU_ACTIVATE,
        label: "Choose an app menu item",
        description: "Chooses the item `id` of the frontmost app's menus, as a click in the menu bar does.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"id": {"type": "integer", "minimum": 0}},
                "required": ["id"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: APP_FORCE_QUIT_PANEL,
        label: "Force quit",
        description: "The panel to terminate an app that does not respond (shell component).",
        shortcut: Some("Logo+Alt+Escape"),
        params: no_params,
    },
    CommandSpec {
        id: APP_FORCE_QUIT,
        label: "Terminate an app",
        description: "Terminates the processes of an app's windows (app_id) right away, without asking.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"app_id": {"type": "string"}},
                "required": ["app_id"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: WINDOW_MINIMIZE,
        label: "Minimize window",
        description: "Hides the window; window.focus brings it back.",
        shortcut: Some("Logo+M"),
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_FULLSCREEN,
        label: "Full screen",
        description: "Makes the window full screen, or brings it back as it was.",
        shortcut: Some("Logo+Ctrl+F"),
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_TILE_LEFT,
        label: "Left half",
        description: "Moves the window to the left half of the screen.",
        shortcut: Some("Logo+Ctrl+Left"),
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_TILE_RIGHT,
        label: "Right half",
        description: "Moves the window to the right half of the screen.",
        shortcut: Some("Logo+Ctrl+Right"),
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_TILE_TOP,
        label: "Top half",
        description: "Moves the window to the top half of the screen.",
        shortcut: Some("Logo+Ctrl+Up"),
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_TILE_BOTTOM,
        label: "Bottom half",
        description: "Moves the window to the bottom half of the screen.",
        shortcut: Some("Logo+Ctrl+Down"),
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_TILE_FILL,
        label: "Fill",
        description: "The window fills the screen, without full screen.",
        shortcut: None,
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_ARRANGE_2,
        label: "Arrange two windows",
        description: "The window on the left and the last used one on the right.",
        shortcut: None,
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_ARRANGE_3,
        label: "Arrange three windows",
        description: "The window on the left and the two last used ones stacked on the right.",
        shortcut: None,
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_ARRANGE_4,
        label: "Arrange four windows",
        description: "The window and the three last used ones in the quarters of the screen.",
        shortcut: None,
        params: window_target,
    },
    CommandSpec {
        id: WINDOW_MOVE,
        label: "Move window",
        description: "Moves a window to the given position, in logical pixels.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "minimum": 0},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"}
                },
                "required": ["x", "y"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: APP_LAUNCH,
        label: "Open app",
        description: "Starts a command, or an app by its desktop entry id, as a client of this session.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "app": {"type": "string", "description": "Desktop entry id (org.mozilla.firefox): runs its Exec without field codes, as the Dock does"},
                    "shell": {"type": "boolean", "description": "Shell component (sees silica-shell-v1)"}
                },
                "oneOf": [{"required": ["command"]}, {"required": ["app"]}],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: SESSION_QUIT,
        label: "Quit the session",
        description: "Closes the compositor and every window.",
        shortcut: Some("Logo+Shift+Q"),
        params: no_params,
    },
    CommandSpec {
        id: SESSION_LOCK,
        label: "Lock the screen",
        description: "Locks the session: only the lock screen (the shell's ext-session-lock client) is drawn and gets input until the password is right. Refused when no shell is running.",
        shortcut: Some("Logo+Ctrl+Q"),
        params: no_params,
    },
    CommandSpec {
        id: INPUT_POINTER_MOVE,
        label: "Move the pointer",
        description: "Moves the pointer to a point of the output, like a real mouse.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"x": {"type": "number"}, "y": {"type": "number"}},
                "required": ["x", "y"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: INPUT_POINTER_BUTTON,
        label: "Press or release a button",
        description: "Presses or releases a mouse button where the pointer is.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "button": {"type": "string", "enum": ["left", "right", "middle"]},
                    "pressed": {"type": "boolean"}
                },
                "required": ["pressed"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: INPUT_CLICK,
        label: "Click",
        description: "Moves the pointer to a point and clicks (left button if not given).",
        shortcut: None,
        params: point,
    },
    CommandSpec {
        id: INPUT_SCROLL,
        label: "Scroll",
        description: "Moves the pointer to a point and turns the wheel: dx and dy in logical pixels, positive right and down (dy 15 is one notch).",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "x": {"type": "number"},
                    "y": {"type": "number"},
                    "dx": {"type": "number"},
                    "dy": {"type": "number"}
                },
                "required": ["x", "y"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: INPUT_KEY,
        label: "Press a key",
        description: "Presses and releases a key by xkb name (Return, BackSpace, Escape, Left...) or character; with pressed only the press or the release.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "pressed": {"type": "boolean"}
                },
                "required": ["key"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: INPUT_GESTURE,
        label: "Trackpad gesture",
        description: "A multi-finger gesture: `kind` swipe or pinch, `fingers`, `phase` begin, update or end (without phase the whole gesture in one call), `dx` and `dy` (scroll, points), `scale` (pinch, absolute), `cancelled`. At the end it returns `command`, the command it ran: three or four fingers left or right change space, up Panorama, down the app windows; thumb and three fingers spreading the desktop, pinching the launcher.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "kind": {"enum": ["swipe", "pinch"]},
                    "fingers": {"type": "integer", "minimum": 1, "maximum": 10},
                    "phase": {"enum": ["begin", "update", "end"]},
                    "dx": {"type": "number"},
                    "dy": {"type": "number"},
                    "scale": {"type": "number"},
                    "cancelled": {"type": "boolean"}
                },
                "required": ["kind", "fingers"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: INPUT_TYPE,
        label: "Type text",
        description: "Types a text key by key into the focused window.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: PERF_FRAMES,
        label: "Measure frames",
        description: "For every surface with a buffer: frames received, intervals between commits (mean, p50, p95, p99, maximum), slow frames (over 1.5 refresh periods) and time from start to the first buffer. With reset it clears the measurements after returning them.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"reset": {"type": "boolean"}},
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: PERF_PROCESS,
        label: "Measure a process",
        description: "Memory of a client process (RSS and PSS from /proc, in KiB).",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"pid": {"type": "integer", "minimum": 1}},
                "required": ["pid"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: KEYBOARD_STATE,
        label: "Keyboard settings",
        description: "The keyboard settings in use (keyboard.* of the Settings service): repeat delay and rate, input sources and the active one.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: KEYBOARD_NEXT_SOURCE,
        label: "Next input source",
        description: "Switches to the next input source of keyboard.layouts (the shortcut works only with two or more).",
        shortcut: Some("Ctrl+space"),
        params: no_params,
    },
    CommandSpec {
        id: KEYBOARD_SELECT_SOURCE,
        label: "Choose input source",
        description: "Makes the input source at an index of keyboard.json the active one (the input menu of the menu bar and of the lock screen).",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"index": {"type": "integer", "minimum": 0, "maximum": 3}},
                "required": ["index"],
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: INPUT_STATE,
        label: "Pointer settings",
        description: "The mouse and touchpad settings in use (the input.* settings) and the kinds of pointer connected.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: IDLE_STATE,
        label: "Idle state",
        description: "Seconds since the last input, whether the display is off, and the idle times of power.json.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: OUTPUT_LIST,
        label: "List displays",
        description: "The connected displays: connector, identity, make, model, serial, modes, current mode, scale, rotation, enabled.",
        shortcut: None,
        params: no_params,
    },
    CommandSpec {
        id: OUTPUT_CONFIGURE,
        label: "Configure a display",
        description: "Changes the mode, scale or rotation of a display at once and saves it in displays.json for that display.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "connector": {"type": "string"},
                    "mode": {
                        "type": "object",
                        "properties": {
                            "width": {"type": "integer"},
                            "height": {"type": "integer"},
                            "refresh": {"type": "number"}
                        },
                        "required": ["width", "height"]
                    },
                    "scale": {"type": "number", "minimum": 1, "maximum": 4},
                    "transform": {"type": "string", "enum": ["normal", "90", "180", "270", "flipped", "flipped-90", "flipped-180", "flipped-270"]}
                },
                "additionalProperties": false
            })
        },
    },
    CommandSpec {
        id: OUTPUT_COLOR_TEMPERATURE,
        label: "Night light temperature",
        description: "The colour temperature of every display in kelvin (1000–6500; 6500 is neutral), through the gamma ramp. silica-daemon drives it with the night light schedule.",
        shortcut: None,
        params: || {
            json!({
                "type": "object",
                "properties": {"kelvin": {"type": "integer", "minimum": 1000, "maximum": 6500}},
                "required": ["kelvin"],
                "additionalProperties": false
            })
        },
    },
];

pub fn find(id: &str) -> Option<&'static CommandSpec> {
    CATALOG.iter().find(|spec| spec.id == id)
}

pub fn catalog_json() -> Value {
    Value::Array(CATALOG.iter().map(CommandSpec::to_json).collect())
}

/// Parameters of [`ENGINE_EXECUTE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteParams {
    pub id: String,
    #[serde(default)]
    pub params: Value,
}

/// Parameters of the commands that act on a window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowMove {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppLaunch {
    /// A command for `sh -c`; or [`Self::app`].
    #[serde(default)]
    pub command: Option<String>,
    /// A desktop entry id (`org.mozilla.firefox`): its `Exec` runs with the field codes
    /// stripped, as the Dock and Faro run it.
    #[serde(default)]
    pub app: Option<String>,
    /// A shell component: sees the shell protocols.
    #[serde(default)]
    pub shell: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

impl MouseButton {
    /// evdev code of the button (linux/input-event-codes.h).
    pub fn code(self) -> u32 {
        match self {
            MouseButton::Left => 0x110,
            MouseButton::Right => 0x111,
            MouseButton::Middle => 0x112,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerMove {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerButton {
    #[serde(default)]
    pub button: MouseButton,
    pub pressed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Click {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub button: MouseButton,
}

/// Parameters of [`INPUT_SCROLL`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scroll {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub dx: f64,
    #[serde(default)]
    pub dy: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub key: String,
    /// Absent: press and release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeText {
    pub text: String,
}

/// Parameters of [`PERF_FRAMES`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfFramesParams {
    #[serde(default)]
    pub reset: bool,
}

/// The measurements of a surface in [`PERF_FRAMES`]. Times in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceFrames {
    /// `window <id>`, `layer <namespace>` or `surface` for the others.
    pub surface: String,
    pub pid: Option<u32>,
    /// Commits with a new buffer since the last reset.
    pub frames: u64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// Intervals over 1.5 refresh periods of the output.
    pub slow: u64,
    pub period_ms: f64,
    /// From the launch with `app.launch` (or `--spawn`) to the first buffer, if launched by the compositor.
    pub startup_ms: Option<f64>,
}

/// Parameters of [`PERF_PROCESS`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfProcessParams {
    pub pid: u32,
}

/// The result of [`PERF_PROCESS`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub rss_kib: u64,
    pub pss_kib: u64,
}

/// A layer in [`LAYER_LIST`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerInfo {
    pub namespace: String,
    /// `background`, `bottom`, `top` or `overlay`.
    pub layer: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub exclusive_zone: i32,
    pub focused: bool,
}

/// A window in [`WINDOW_LIST`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u64,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub focused: bool,
    /// Minimized: not on screen, window.focus brings it back.
    #[serde(default)]
    pub minimized: bool,
    /// Full screen.
    #[serde(default)]
    pub fullscreen: bool,
    /// The workspace of the window (from 0).
    #[serde(default)]
    pub space: usize,
}

/// A keyboard shortcut: modifiers plus the name of the xkb keysym.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shortcut {
    pub logo: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Keysym name without modifiers, compared case-insensitively.
    pub key: String,
}

impl Shortcut {
    /// Reads `Logo+Shift+Q`: modifiers (`Logo`, `Ctrl`, `Alt`, `Shift`) and a final key.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut shortcut = Shortcut::default();
        let mut parts = text.split('+').map(str::trim).peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                if part.is_empty() {
                    return Err(format!("shortcut without a key: {text}"));
                }
                shortcut.key = part.to_owned();
                return Ok(shortcut);
            }
            let flag = match part.to_ascii_lowercase().as_str() {
                "logo" | "super" => &mut shortcut.logo,
                "ctrl" | "control" => &mut shortcut.ctrl,
                "alt" => &mut shortcut.alt,
                "shift" => &mut shortcut.shift,
                other => return Err(format!("unknown modifier: {other}")),
            };
            *flag = true;
        }
        Err(format!("empty shortcut: {text:?}"))
    }

    pub fn matches(&self, logo: bool, ctrl: bool, alt: bool, shift: bool, key: &str) -> bool {
        self.logo == logo
            && self.ctrl == ctrl
            && self.alt == alt
            && self.shift == shift
            && self.key.eq_ignore_ascii_case(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_bound_to_the_backtick() {
        // Logo+` does nothing on the Italian layout (measured). Shortcuts are matched on the
        // layout's own symbol (the raw Latin symbol of the key, see `handle_key`), so a
        // binding to `grave` would exist only where the layout produces it.
        for spec in CATALOG {
            if let Some(shortcut) = spec.shortcut {
                let parsed = Shortcut::parse(shortcut).unwrap();
                assert!(
                    !parsed.matches(true, false, false, false, "grave")
                        && !parsed.matches(true, false, false, false, "quoteleft"),
                    "{}",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn catalog_ids_are_unique_and_documented() {
        let mut ids: Vec<_> = CATALOG.iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        for spec in CATALOG {
            assert!(
                !spec.label.is_empty() && !spec.description.is_empty(),
                "{}",
                spec.id
            );
            assert_eq!((spec.params)()["type"], "object", "{}", spec.id);
        }
    }

    #[test]
    fn every_shortcut_parses_and_none_collide() {
        let shortcuts: Vec<_> = CATALOG
            .iter()
            .filter_map(|spec| spec.shortcut)
            .map(|text| Shortcut::parse(text).unwrap())
            .collect();
        for (i, a) in shortcuts.iter().enumerate() {
            for b in shortcuts.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn shortcut_parse_and_match() {
        let shortcut = Shortcut::parse("Logo+Shift+Q").unwrap();
        assert!(shortcut.matches(true, false, false, true, "q"));
        assert!(!shortcut.matches(true, false, false, false, "q"));
        assert!(Shortcut::parse("Logo+").is_err());
        assert!(Shortcut::parse("Hyper+Q").is_err());
    }

    #[test]
    fn window_target_rejects_unknown_fields() {
        assert!(serde_json::from_value::<WindowTarget>(json!({"idd": 1})).is_err());
        assert_eq!(
            serde_json::from_value::<WindowTarget>(json!({})).unwrap(),
            WindowTarget::default()
        );
    }
}

/// Parameters of [`UI_ANIMATION_SCALE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnimationScale {
    pub scale: f64,
}

/// Parameters of [`SPACE_GOTO`] and [`SPACE_REMOVE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexParams {
    pub index: usize,
}

/// Parameters of [`WINDOW_MOVE_TO_SPACE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveToSpaceParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub index: usize,
}

/// A window arranged in [`PANORAMA_STATE`] (logical points).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PanoramaWindow {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A rectangle in logical points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PanoramaRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Response of [`PANORAMA_STATE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanoramaState {
    /// `panorama`, `app` (App Exposé) or `desktop` (Show Desktop); absent when closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub open: bool,
    pub progress: f64,
    pub windows: Vec<PanoramaWindow>,
    /// Expansion of the spaces bar (0 compact, 1 expanded).
    pub expansion: f64,
    /// The space thumbnails, when visible.
    pub thumbnails: Vec<PanoramaRect>,
}

/// Parameters of [`INPUT_GESTURE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GestureParams {
    pub kind: String,
    pub fingers: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default)]
    pub dx: f64,
    #[serde(default)]
    pub dy: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default)]
    pub cancelled: bool,
}

/// A space in [`SPACE_LIST`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub index: usize,
    pub active: bool,
    /// Window ids, from bottom to top.
    pub windows: Vec<u64>,
}

/// Parameters of [`SYSTEM_WIFI`], [`SYSTEM_BLUETOOTH`], [`SYSTEM_MUTE`], [`SYSTEM_DND`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnabledParams {
    pub enabled: bool,
}

/// Parameters of [`SYSTEM_VOLUME`] and [`SYSTEM_BRIGHTNESS`], from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueParams {
    pub value: f64,
}

/// Parameters of [`UI_GLASS_QUALITY`]: `auto`, `full`, `frosted`, `opaque`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlassQualityParams {
    pub quality: String,
}

/// One app icon of [`DOCK_ICONS`]: the app id of its windows and the center of the icon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockIcon {
    pub app_id: String,
    pub x: f64,
    pub y: f64,
}

/// Parameters of [`DOCK_ICONS`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockIconsParams {
    pub icons: Vec<DockIcon>,
}

/// Parameters of [`WINDOW_MINIMIZE_EFFECT`]: `genie` or `scale`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinimizeEffectParams {
    pub effect: String,
}

/// Result of [`PERF_RENDER`]: The compositor's rendering time per drawn frame, in ms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderTimes {
    pub frames: u64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// The same frames split by stage: `build` (the scene), `draw` (damage and drawing),
    /// `submit` (handing the frame to the display) and, when the renderer measures it, `gpu`.
    #[serde(default)]
    pub stages: Vec<StageTimes>,
    /// Frames shown without composing anything, not in the times above: only a plane changed
    /// (the cursor moved), or a client's buffer went straight to the display.
    #[serde(default)]
    pub planes_only: u64,
    /// Of `planes_only`, the frames whose primary plane scanned out a client's buffer.
    #[serde(default)]
    pub scanout: u64,
}

/// The times of one stage of the drawn frames, in ms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StageTimes {
    pub stage: String,
    pub frames: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Parameters of [`PERF_RENDER`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfRenderParams {
    #[serde(default)]
    pub reset: bool,
}
