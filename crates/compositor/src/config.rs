//! The user's settings that the compositor applies. Most come from the Settings service
//! (`settings_service.rs`, silica-os/silica-desktop#240): the parsers here read the service's keys of a
//! group (`keyboard.*`, `input.*`, …) as one object, so a value out of range is ignored and the
//! others still apply. Two are still files of `$XDG_CONFIG_HOME/silica` (usually
//! `~/.config/silica`): `power.json` and `displays.json` (`docs/control.md` › Settings files),
//! watched with inotify: a file written in place or renamed over is read again and applied at
//! once; a missing, unreadable or malformed file means the defaults. The glass and the
//! accessibility of the display come with the look of silica-daemon (`look.*` of `System1`).

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use rustix::fs::inotify;
use serde_json::Value;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use tracing::{info, warn};

use smithay::input::keyboard::XkbConfig;

use crate::state::{Compositor, State};

/// The settings folder: `$XDG_CONFIG_HOME/silica`, or `~/.config/silica`.
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("silica"))
}

/// The JSON object in `dir/name`; `None` if missing or not an object.
pub fn read_json(dir: &Path, name: &str) -> Option<serde_json::Map<String, Value>> {
    let text = std::fs::read_to_string(dir.join(name)).ok()?;
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Some(map),
        Ok(_) => None,
        Err(err) => {
            warn!("{name}: {err}");
            None
        }
    }
}

/// An integer in `range`, or `None`.
pub fn int_in(map: &serde_json::Map<String, Value>, key: &str, range: (i64, i64)) -> Option<i64> {
    let value = map.get(key)?;
    let n = value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.is_finite())
            .map(|f| f.round() as i64)
    })?;
    (range.0..=range.1).contains(&n).then_some(n)
}

/// A number in `range`, or `None`.
pub fn number_in(
    map: &serde_json::Map<String, Value>,
    key: &str,
    range: (f64, f64),
) -> Option<f64> {
    let n = map.get(key)?.as_f64().filter(|f| f.is_finite())?;
    (range.0..=range.1).contains(&n).then_some(n)
}

pub fn bool_of(map: &serde_json::Map<String, Value>, key: &str) -> Option<bool> {
    map.get(key)?.as_bool()
}

/// A short string (an xkb name, an option list): no control characters, at most 256 bytes.
pub fn text_of(map: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    let text = map.get(key)?.as_str()?;
    (text.len() <= 256 && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

// --- keyboard.* -------------------------------------------------------------------------

/// One input source: an xkb layout with its variant (`us`, `it`, `de(nodeadkeys)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub layout: String,
    pub variant: String,
}

impl Layout {
    /// `layout` or `layout(variant)`: letters, digits, `_` and `-` only.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (layout, variant) = match text.split_once('(') {
            Some((layout, rest)) => (layout, rest.strip_suffix(')')?),
            None => (text, ""),
        };
        let name = |s: &str| {
            s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        (!layout.is_empty() && name(layout) && name(variant)).then(|| Layout {
            layout: layout.to_owned(),
            variant: variant.to_owned(),
        })
    }

    pub fn name(&self) -> String {
        if self.variant.is_empty() {
            self.layout.clone()
        } else {
            format!("{}({})", self.layout, self.variant)
        }
    }
}

/// The `keyboard.*` settings: key repeat and input sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardConfig {
    /// Before the first repeat, in ms.
    pub repeat_delay: i32,
    /// Repeats per second; 0: no repeat.
    pub repeat_rate: i32,
    /// The input sources, in order; empty: xkb's default (or `XKB_DEFAULT_LAYOUT`).
    pub layouts: Vec<Layout>,
    pub model: String,
    pub options: String,
}

/// At most this many input sources (xkb keeps 4 groups per keymap).
pub const MAX_LAYOUTS: usize = 4;

impl KeyboardConfig {
    /// The defaults are `defaults` (key repeat from `SILICA_COMPOSITOR_KEY_REPEAT` or the built-in
    /// values); the file's valid keys win.
    pub fn parse(map: Option<&serde_json::Map<String, Value>>, defaults: (i32, i32)) -> Self {
        let mut config = KeyboardConfig {
            repeat_delay: defaults.0,
            repeat_rate: defaults.1,
            layouts: Vec::new(),
            model: String::new(),
            options: String::new(),
        };
        let Some(map) = map else {
            return config;
        };
        if let Some(delay) = int_in(map, "repeatDelay", (100, 3000)) {
            config.repeat_delay = delay as i32;
        }
        if let Some(rate) = int_in(map, "repeatRate", (0, 100)) {
            config.repeat_rate = rate as i32;
        }
        if let Some(Value::Array(list)) = map.get("layouts") {
            config.layouts = list
                .iter()
                .filter_map(Value::as_str)
                .filter_map(Layout::parse)
                .take(MAX_LAYOUTS)
                .collect();
        }
        config.model = text_of(map, "model").unwrap_or_default();
        config.options = text_of(map, "options").unwrap_or_default();
        config
    }

    /// The xkb names: comma-separated layouts and variants.
    pub fn xkb_names(&self) -> (String, String) {
        let layouts: Vec<&str> = self.layouts.iter().map(|l| l.layout.as_str()).collect();
        let variants: Vec<&str> = self.layouts.iter().map(|l| l.variant.as_str()).collect();
        (layouts.join(","), variants.join(","))
    }
}

// --- input.* ----------------------------------------------------------------------------

/// How a touchpad makes a secondary click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickMethod {
    /// Two fingers anywhere (libinput's clickfinger).
    TwoFinger,
    /// The bottom right corner (libinput's button areas).
    BottomRight,
    /// The bottom left corner (button areas, left-handed).
    BottomLeft,
}

/// The settings of one kind of pointer device; `None` keeps libinput's default for the device.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerSettings {
    /// Pointer acceleration speed, -1–1.
    pub speed: Option<f64>,
    pub natural_scroll: Option<bool>,
    /// Multiplies scroll deltas, 0.1–10 (1: unchanged).
    pub scroll_speed: f64,
    /// Primary and secondary buttons swapped.
    pub left_handed: Option<bool>,
    /// Touchpads only.
    pub click_method: Option<ClickMethod>,
    pub tap_to_click: Option<bool>,
    pub disable_while_typing: Option<bool>,
}

impl Default for PointerSettings {
    fn default() -> Self {
        Self {
            speed: None,
            natural_scroll: None,
            scroll_speed: 1.0,
            left_handed: None,
            click_method: None,
            tap_to_click: None,
            disable_while_typing: None,
        }
    }
}

/// The `input.*` settings: mice and touchpads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputConfig {
    pub mouse: PointerSettings,
    pub touchpad: PointerSettings,
    /// The longest time between two clicks of a double click, in ms (`doubleClickMs`).
    pub double_click_ms: Option<u32>,
}

/// The double click time without the `input.*` settings.
pub const DEFAULT_DOUBLE_CLICK_MS: u32 = 400;

impl InputConfig {
    pub fn parse(map: Option<&serde_json::Map<String, Value>>) -> Self {
        let mut config = InputConfig::default();
        let Some(map) = map else {
            return config;
        };
        config.double_click_ms = int_in(map, "doubleClickMs", (100, 2000)).map(|ms| ms as u32);
        let common = |group: &serde_json::Map<String, Value>| PointerSettings {
            speed: number_in(group, "speed", (-1.0, 1.0)),
            natural_scroll: bool_of(group, "naturalScroll"),
            scroll_speed: number_in(group, "scrollSpeed", (0.1, 10.0)).unwrap_or(1.0),
            ..PointerSettings::default()
        };
        if let Some(Value::Object(mouse)) = map.get("mouse") {
            config.mouse = PointerSettings {
                left_handed: match mouse.get("secondaryClick").and_then(Value::as_str) {
                    Some("right") => Some(false),
                    Some("left") => Some(true),
                    _ => None,
                },
                ..common(mouse)
            };
        }
        if let Some(Value::Object(touchpad)) = map.get("touchpad") {
            let click = match touchpad.get("secondaryClick").and_then(Value::as_str) {
                Some("twoFinger") => Some(ClickMethod::TwoFinger),
                Some("bottomRight") => Some(ClickMethod::BottomRight),
                Some("bottomLeft") => Some(ClickMethod::BottomLeft),
                _ => None,
            };
            config.touchpad = PointerSettings {
                click_method: click,
                left_handed: click.map(|c| c == ClickMethod::BottomLeft),
                tap_to_click: bool_of(touchpad, "tapToClick"),
                disable_while_typing: bool_of(touchpad, "disableWhileTyping"),
                ..common(touchpad)
            };
        }
        config
    }
}

// --- hotcorners.* -----------------------------------------------------------------------

/// A corner of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// `hotcorners.json` (Settings › Desktop & Dock › Hot Corners): the catalog command each corner
/// runs when the pointer reaches it. A corner left out does nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HotCornersConfig {
    pub top_left: Option<&'static str>,
    pub top_right: Option<&'static str>,
    pub bottom_left: Option<&'static str>,
    pub bottom_right: Option<&'static str>,
}

/// The command of a hot corner action name; `None` for `none` and unknown names.
pub fn hot_corner_command(action: &str) -> Option<&'static str> {
    use silica_control::commands as c;
    Some(match action {
        "panorama" => c::PANORAMA_TOGGLE,
        "appWindows" => c::PANORAMA_APP,
        "desktop" => c::DESKTOP_SHOW,
        "notifications" => c::NOTIFICATIONS_TOGGLE,
        "launcher" => c::LAUNCHER_TOGGLE,
        "controlCenter" => c::CONTROLCENTER_TOGGLE,
        "search" => c::FARO_TOGGLE,
        _ => return None,
    })
}

impl HotCornersConfig {
    pub fn parse(map: Option<&serde_json::Map<String, Value>>) -> Self {
        let action = |key: &str| {
            map.and_then(|map| map.get(key))
                .and_then(Value::as_str)
                .and_then(hot_corner_command)
        };
        Self {
            top_left: action("topLeft"),
            top_right: action("topRight"),
            bottom_left: action("bottomLeft"),
            bottom_right: action("bottomRight"),
        }
    }

    pub fn command(&self, corner: Corner) -> Option<&'static str> {
        match corner {
            Corner::TopLeft => self.top_left,
            Corner::TopRight => self.top_right,
            Corner::BottomLeft => self.bottom_left,
            Corner::BottomRight => self.bottom_right,
        }
    }
}

// --- lockscreen.passwordDelay and screensaver.* ----------------------------------------------------

/// How long after the screen saver or the display off the password is asked for, in seconds
/// (`passwordDelay`; 0 at once); 15 minutes without the key, never when it is `null`.
pub const PASSWORD_DELAY_DEFAULT: u32 = 900;

/// `lockscreen.json` (Settings › Lock Screen): the part the compositor applies. The shell reads the rest
/// (`showUser`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockConfig {
    pub password_delay: Option<u32>,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            password_delay: Some(PASSWORD_DELAY_DEFAULT),
        }
    }
}

impl LockConfig {
    pub fn parse(map: Option<&serde_json::Map<String, Value>>) -> Self {
        let Some(value) = map.and_then(|map| map.get("passwordDelay")) else {
            return Self::default();
        };
        match value {
            Value::Null => Self {
                password_delay: None,
            },
            _ => Self {
                password_delay: map
                    .and_then(|map| int_in(map, "passwordDelay", (0, 86_400)))
                    .map(|s| s as u32)
                    .or(Some(PASSWORD_DELAY_DEFAULT)),
            },
        }
    }
}

/// `screensaver.json` (Settings › Screen Saver): start after `minutes` without input (absent or
/// 0: never) with the saver `saver` (one of [`crate::lock::SAVERS`]; the shell draws it).
#[derive(Debug, Clone, PartialEq)]
pub struct SaverConfig {
    pub minutes: Option<f64>,
    pub saver: String,
}

impl Default for SaverConfig {
    fn default() -> Self {
        Self {
            minutes: None,
            saver: crate::lock::SAVER_DEFAULT.to_owned(),
        }
    }
}

impl SaverConfig {
    pub fn parse(map: Option<&serde_json::Map<String, Value>>) -> Self {
        let Some(map) = map else {
            return Self::default();
        };
        Self {
            minutes: number_in(map, "minutes", (0.0, 100_000.0)).filter(|m| *m > 0.0),
            saver: map
                .get("saver")
                .and_then(Value::as_str)
                .filter(|s| crate::lock::SAVERS.contains(s))
                .unwrap_or(crate::lock::SAVER_DEFAULT)
                .to_owned(),
        }
    }
}

// --- dock.* -----------------------------------------------------------------------------

/// What a double click on a window's title bar does (`titleBarDoubleClick`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TitleBarDoubleClick {
    #[default]
    Zoom,
    Minimize,
    Nothing,
}

/// `dock.json`: the part the compositor applies (the Dock itself is the shell's).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DockConfig {
    pub title_bar_double_click: TitleBarDoubleClick,
    /// Windows minimize into their app's icon, not into the Dock's right section.
    pub minimize_to_app: bool,
}

impl DockConfig {
    pub fn parse(map: Option<&serde_json::Map<String, Value>>) -> Self {
        let title_bar_double_click = match map
            .and_then(|map| map.get("titleBarDoubleClick"))
            .and_then(Value::as_str)
        {
            Some("minimize") => TitleBarDoubleClick::Minimize,
            Some("none") => TitleBarDoubleClick::Nothing,
            _ => TitleBarDoubleClick::Zoom,
        };
        Self {
            title_bar_double_click,
            minimize_to_app: map
                .and_then(|map| bool_of(map, "minimizeToApp"))
                .unwrap_or(false),
        }
    }
}

// --- shortcuts.* ------------------------------------------------------------------------

/// `shortcuts.json` (Settings › Keyboard › Keyboard Shortcuts): per catalog command id, the
/// shortcut in the `Logo+Shift+Q` format, or `null` for none. A command not listed keeps its
/// default; an id that is not in the catalog is ignored.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShortcutOverrides(Vec<(&'static str, Option<String>)>);

/// The app that records a new shortcut (Settings): while `shortcuts.json` says `"capture":
/// true` and this app is in front, the catalog shortcuts do not run and the keys reach it.
pub const SHORTCUT_CAPTURE_APP: &str = "dev.frascella.Settings";

impl ShortcutOverrides {
    /// `None`: not changed; `Some(None)`: switched off; `Some(Some(text))`: the user's keys.
    pub fn get(&self, id: &str) -> Option<Option<&str>> {
        self.0
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, text)| text.as_deref())
    }

    /// The `shortcuts.<command id>` keys of the Settings service: a shortcut, or `none` for
    /// none; empty (the default) keeps the catalog's.
    pub fn from_settings(settings: &crate::settings_service::ServiceSettings) -> Self {
        let mut map = serde_json::Map::new();
        for (id, value) in settings.group("shortcuts") {
            if let crate::settings_service::Setting::Text(text) = value {
                match text.as_str() {
                    "" => {}
                    "none" => {
                        map.insert(id.to_owned(), Value::Null);
                    }
                    keys => {
                        map.insert(id.to_owned(), Value::from(keys));
                    }
                }
            }
        }
        Self::parse(Some(&map))
    }

    pub fn parse(map: Option<&serde_json::Map<String, Value>>) -> Self {
        let Some(map) = map else {
            return Self::default();
        };
        let mut found = Vec::new();
        for spec in silica_control::commands::CATALOG {
            match map.get(spec.id) {
                Some(Value::Null) => found.push((spec.id, None)),
                Some(Value::String(text))
                    if silica_control::commands::Shortcut::parse(text).is_ok() =>
                {
                    found.push((spec.id, Some(text.clone())));
                }
                _ => {}
            }
        }
        Self(found)
    }
}

// --- look.* and accessibility.cursorSize ----------------------------------------------------------------------------

/// `theme.json` (Settings › Appearance): the part the compositor applies, the glass level. The rest of
/// the file is the look the apps follow (silica-daemon `SetLook`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeConfig {
    /// 0 clear to 1 tinted; 0.5, the measured material, by default.
    pub glass: f32,
    /// Accessibility › Display › Reduce Transparency: the glass is drawn opaque.
    pub reduce_transparency: bool,
    /// Accessibility › Display › Reduce Motion: The compositor's own animations are skipped.
    pub reduce_motion: bool,
    /// Accessibility › Pointer › Pointer size: the cursor's nominal size in logical pixels
    /// (`cursorSize`, 16–128); the environment's (24 by default) without it.
    pub cursor_size: Option<u32>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            glass: compositor_render::glass_profile::GLASS_LEVEL_DEFAULT,
            reduce_transparency: false,
            reduce_motion: false,
            cursor_size: None,
        }
    }
}

impl ThemeConfig {
    pub fn parse(map: Option<&serde_json::Map<String, Value>>) -> Self {
        let glass = map
            .and_then(|map| number_in(map, "glass", (0.0, 1.0)))
            .map_or(Self::default().glass, |g| g as f32);
        Self {
            glass,
            reduce_transparency: map
                .and_then(|map| bool_of(map, "reduceTransparency"))
                .unwrap_or(false),
            reduce_motion: map
                .and_then(|map| bool_of(map, "reduceMotion"))
                .unwrap_or(false),
            cursor_size: map
                .and_then(|map| int_in(map, "cursorSize", (16, 128)))
                .map(|size| size as u32),
        }
    }
}

// --- power.json ----------------------------------------------------------------------------

pub const POWER_FILE: &str = "power.json";

/// `power.json`: idle times, in minutes (decimals allowed); absent or 0: never.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PowerConfig {
    pub display_off_minutes: Option<f64>,
    pub sleep_minutes: Option<f64>,
    /// Never sleep on idle (silica-daemon also holds logind's idle inhibitor).
    pub prevent_sleep: bool,
}

impl PowerConfig {
    pub fn parse(map: Option<&serde_json::Map<String, Value>>) -> Self {
        let Some(map) = map else {
            return Self::default();
        };
        let minutes = |key| number_in(map, key, (0.0, 100_000.0)).filter(|m| *m > 0.0);
        Self {
            display_off_minutes: minutes("displayOffMinutes"),
            sleep_minutes: minutes("sleepMinutes"),
            prevent_sleep: bool_of(map, "preventSleep").unwrap_or(false),
        }
    }
}

impl State {
    /// A file of the settings folder changed.
    pub fn config_changed(&mut self, name: &str) {
        match name {
            POWER_FILE => self.apply_power_config(),
            crate::displays::DISPLAYS_FILE => self.apply_displays_config(),
            _ => {}
        }
    }

    /// The Settings service told something (every key, or one that changed): what those keys
    /// drive is applied at once.
    pub fn settings_event(&mut self, event: crate::settings_service::Event) {
        let changed = self.service_settings.take(event);
        let touched = |prefix: &str| changed.iter().any(|key| key.starts_with(prefix));
        if touched("keyboard.") {
            self.apply_keyboard_config();
        }
        if touched("input.") {
            self.apply_input_config();
        }
        if touched("hotcorners.") {
            self.apply_hot_corners_config();
        }
        if touched("shortcuts.") {
            self.apply_shortcuts_config();
        }
        if touched("dock.") {
            self.apply_dock_config();
        }
        if touched("lockscreen.passwordDelay") || touched("screensaver.") {
            self.apply_lock_config();
        }
        if touched("accessibility.cursorSize") {
            self.apply_theme_config();
        }
        if touched("accessibility.") {
            self.apply_accessibility_config();
        }
        if !changed.is_empty() {
            self.wake();
        }
    }

    /// `input.*`: to every mouse and touchpad now, and to each one that appears later.
    /// The double click time is the look's (`look.doubleClickMs` of silica-daemon), the one the
    /// apps use.
    pub fn apply_input_config(&mut self) {
        let mut map = self.service_settings.object("input");
        if let Some(Value::Object(state)) = self.system.status()
            && let Some(ms) = state.get("look.doubleClickMs")
        {
            map.get_or_insert_with(serde_json::Map::new)
                .insert("doubleClickMs".to_owned(), ms.clone());
        }
        let config = InputConfig::parse(map.as_ref());
        if config != self.input_config {
            self.input_config = config;
            #[cfg(feature = "drm")]
            self.configure_input_devices();
        }
    }

    /// `dock.*`: the title bar's double click, minimizing into the app.
    pub fn apply_dock_config(&mut self) {
        self.dock_config = DockConfig::parse(self.service_settings.object("dock").as_ref());
    }

    /// `shortcuts.*`: the catalog's shortcuts with the user's changes, at once.
    pub fn apply_shortcuts_config(&mut self) {
        self.shortcuts_capture = matches!(
            self.service_settings.get("shortcuts.capture"),
            Some(crate::settings_service::Setting::Bool(true))
        );
        let overrides = ShortcutOverrides::from_settings(&self.service_settings);
        self.shortcuts = crate::commands::shortcuts_with(&overrides);
    }

    /// `hotcorners.*`: the actions of the four corners.
    pub fn apply_hot_corners_config(&mut self) {
        self.hot_corners =
            HotCornersConfig::parse(self.service_settings.object("hotcorners").as_ref());
    }

    /// The pointer moved to `location`: when it arrives in a corner with an action, runs it
    /// (once; the corner is armed again after the pointer leaves it).
    pub fn hot_corner_check(
        &mut self,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        if self.locked {
            return;
        }
        let Some(geometry) = self.space.output_geometry(&self.output) else {
            return;
        };
        let (left, top) = (f64::from(geometry.loc.x), f64::from(geometry.loc.y));
        let (right, bottom) = (
            left + f64::from(geometry.size.w),
            top + f64::from(geometry.size.h),
        );
        // The pointer stops at the edge: the last point inside is a pixel from it.
        const REACH: f64 = 1.5;
        let near_left = location.x <= left + REACH;
        let near_right = location.x >= right - REACH;
        let near_top = location.y <= top + REACH;
        let near_bottom = location.y >= bottom - REACH;
        let corner = match (near_left, near_right, near_top, near_bottom) {
            (true, _, true, _) => Some(Corner::TopLeft),
            (_, true, true, _) => Some(Corner::TopRight),
            (true, _, _, true) => Some(Corner::BottomLeft),
            (_, true, _, true) => Some(Corner::BottomRight),
            _ => None,
        };
        // Dragging or resizing a window into a corner is not a hot corner.
        if self.moving.is_some() || self.resizing.is_some() {
            self.hot_corner_inside = corner;
            return;
        }
        if corner == self.hot_corner_inside {
            return;
        }
        self.hot_corner_inside = corner;
        if let Some(command) = corner.and_then(|c| self.hot_corners.command(c)) {
            info!("hot corner: {command}");
            if let Err(err) = self.execute_command(command, Value::Object(serde_json::Map::new())) {
                warn!("hot corner {command}: {err}");
            }
        }
    }

    /// The glass level and the accessibility of the display (the look of silica-daemon,
    /// `look.*`) and the pointer size (`accessibility.cursorSize`), drawn from the next frame.
    pub fn apply_theme_config(&mut self) {
        let mut map = serde_json::Map::new();
        if let Some(Value::Object(state)) = self.system.status() {
            for (look, key) in [
                ("look.glass", "glass"),
                ("look.reduceTransparency", "reduceTransparency"),
                ("look.reduceMotion", "reduceMotion"),
            ] {
                if let Some(value) = state.get(look) {
                    map.insert(key.to_owned(), value.clone());
                }
            }
        }
        if let Some(crate::settings_service::Setting::Int(size)) =
            self.service_settings.get("accessibility.cursorSize")
        {
            map.insert("cursorSize".to_owned(), Value::from(*size));
        }
        let config = ThemeConfig::parse(Some(&map));
        if config != self.theme_config {
            let was = self.theme_config.reduce_motion;
            let cursor_was = self.theme_config.cursor_size;
            self.theme_config = config;
            if config.cursor_size != cursor_was {
                self.cursor_theme.set_size(config.cursor_size);
            }
            // Reduce Motion: no animation of the compositor's own, and back to the chosen speed after.
            if config.reduce_motion && !was {
                self.saved_animation_speed = self.animation_speed;
                self.animation_speed = 0.0;
            } else if !config.reduce_motion && was {
                self.animation_speed = self.saved_animation_speed;
            }
            self.wake();
        }
    }

    /// Reads and applies every settings file (at start).
    pub fn apply_all_config(&mut self) {
        self.apply_keyboard_config();
        self.apply_input_config();
        self.apply_power_config();
        self.apply_theme_config();
        self.apply_hot_corners_config();
        self.apply_shortcuts_config();
        self.apply_dock_config();
        self.apply_lock_config();
        self.apply_displays_config();
        self.apply_accessibility_config();
    }

    /// `keyboard.*`: the repeat goes to every bound keyboard (`wl_keyboard.repeat_info`), a
    /// new keymap to the clients when the sources change. Until the Settings service answers
    /// (and without it) the repeat is `SILICA_COMPOSITOR_KEY_REPEAT`'s or the built-in one.
    pub fn apply_keyboard_config(&mut self) {
        let map = self.service_settings.object("keyboard");
        let config = KeyboardConfig::parse(map.as_ref(), crate::state::key_repeat());
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let old = std::mem::replace(&mut self.keyboard_config, config.clone());
        if (old.repeat_delay, old.repeat_rate) != (config.repeat_delay, config.repeat_rate) {
            keyboard.change_repeat_info(config.repeat_rate, config.repeat_delay);
            info!(
                "key repeat: {} ms, {}/s",
                config.repeat_delay, config.repeat_rate
            );
        }
        if (&old.layouts, &old.model, &old.options)
            != (&config.layouts, &config.model, &config.options)
        {
            let (layout, variant) = config.xkb_names();
            let xkb = XkbConfig {
                rules: "",
                model: &config.model,
                layout: &layout,
                variant: &variant,
                options: (!config.options.is_empty()).then(|| config.options.clone()),
            };
            match keyboard.set_xkb_config(self, xkb) {
                Ok(()) => info!("keyboard layouts: {layout} ({variant})"),
                Err(err) => {
                    // A layout xkb does not know: keep the keymap in use.
                    warn!("keyboard.layouts {layout} ({variant}): {err:?}");
                    self.keyboard_config.layouts = old.layouts;
                    self.keyboard_config.model = old.model;
                    self.keyboard_config.options = old.options;
                }
            }
        }
        self.announce_input_sources();
    }

    /// The index of the active input source.
    pub fn active_layout(&mut self) -> u32 {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return 0;
        };
        keyboard.with_xkb_state(self, |context| {
            context
                .xkb()
                .lock()
                .map(|xkb| xkb.active_layout().0)
                .unwrap_or(0)
        })
    }

    /// Switches to the next input source; false with only one.
    pub fn next_input_source(&mut self) -> bool {
        if self.keyboard_config.layouts.len() < 2 {
            return false;
        }
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        keyboard.with_xkb_state(self, |mut context| context.cycle_next_layout());
        self.announce_input_sources();
        true
    }

    /// Makes the input source at `index` the active one; false without two or more sources or
    /// past the last one.
    pub fn select_input_source(&mut self, index: u32) -> bool {
        let count = self.keyboard_config.layouts.len();
        if count < 2 || usize::try_from(index).map_or(true, |i| i >= count) {
            return false;
        }
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        keyboard.with_xkb_state(self, |mut context| {
            context.set_layout(smithay::input::keyboard::Layout(index));
        });
        self.announce_input_sources();
        true
    }

    /// Tells the shell the input sources when they or the active one changed: `keyboard.sources`
    /// holds their xkb names, comma-separated (empty with fewer than two: there is nothing to
    /// choose), `keyboard.source` the index of the active one.
    pub fn announce_input_sources(&mut self) {
        let layouts = &self.keyboard_config.layouts;
        let sources = if layouts.len() < 2 {
            String::new()
        } else {
            layouts
                .iter()
                .map(Layout::name)
                .collect::<Vec<_>>()
                .join(",")
        };
        let active = if sources.is_empty() {
            0
        } else {
            self.active_layout()
        };
        let announced = (sources, active);
        if self.announced_sources.as_ref() == Some(&announced) {
            return;
        }
        self.broadcast_to_shell("keyboard.sources".to_owned(), announced.0.clone());
        self.broadcast_to_shell("keyboard.source".to_owned(), active.to_string());
        self.announced_sources = Some(announced);
    }

    /// `input.state`: the settings in use, by kind of device, and the kinds connected.
    pub fn input_state(&self) -> Value {
        let settings = |s: &PointerSettings| {
            serde_json::json!({
                "speed": s.speed,
                "naturalScroll": s.natural_scroll,
                "scrollSpeed": s.scroll_speed,
                "leftHanded": s.left_handed,
                "secondaryClick": s.click_method.map(|m| match m {
                    ClickMethod::TwoFinger => "twoFinger",
                    ClickMethod::BottomRight => "bottomRight",
                    ClickMethod::BottomLeft => "bottomLeft",
                }),
                "tapToClick": s.tap_to_click,
                "disableWhileTyping": s.disable_while_typing,
            })
        };
        let report = self.system.reported();
        serde_json::json!({
            "mouse": settings(&self.input_config.mouse),
            "touchpad": settings(&self.input_config.touchpad),
            "devices": {
                "mouse": report.get("input.mouse").and_then(Value::as_bool).unwrap_or(false),
                "touchpad": report.get("input.touchpad").and_then(Value::as_bool).unwrap_or(false),
            },
        })
    }

    /// `keyboard.state`.
    pub fn keyboard_state(&mut self) -> Value {
        let active = self.active_layout();
        let config = &self.keyboard_config;
        serde_json::json!({
            "repeatDelay": config.repeat_delay,
            "repeatRate": config.repeat_rate,
            "layouts": config.layouts.iter().map(Layout::name).collect::<Vec<_>>(),
            "model": config.model,
            "options": config.options,
            "active": active,
        })
    }
}

// --- watching ------------------------------------------------------------------------------

/// Watches `dir` (created if missing) and calls `State::config_changed` with the name of each
/// file written, renamed or removed there.
pub fn watch(handle: &LoopHandle<'static, Compositor>, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let fd: OwnedFd = inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC)
        .map_err(|e| format!("inotify: {e}"))?;
    inotify::add_watch(
        &fd,
        dir,
        inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::DELETE
            | inotify::WatchFlags::MOVED_FROM,
    )
    .map_err(|e| format!("inotify {}: {e}", dir.display()))?;
    handle
        .insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, data| {
                data.state.wake();
                let mut names: Vec<String> = Vec::new();
                let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
                let mut reader = inotify::Reader::new(&*fd, &mut buffer);
                while let Ok(event) = reader.next() {
                    if let Some(name) = event.file_name().and_then(|n| n.to_str().ok())
                        && !names.iter().any(|n| n == name)
                    {
                        names.push(name.to_owned());
                    }
                }
                for name in names {
                    data.state.config_changed(&name);
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| format!("inotify source: {e}"))?;
    info!("settings folder: {}", dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn hot_corners_map_to_commands_and_ignore_the_rest() {
        let parse = |json: &str| match serde_json::from_str::<Value>(json) {
            Ok(Value::Object(map)) => HotCornersConfig::parse(Some(&map)),
            _ => HotCornersConfig::parse(None),
        };
        let config = parse(
            r#"{"topLeft": "panorama", "bottomRight": "desktop", "topRight": "none",
                "bottomLeft": "rm -rf"}"#,
        );
        assert_eq!(config.command(Corner::TopLeft), Some("panorama.toggle"));
        assert_eq!(config.command(Corner::BottomRight), Some("desktop.show"));
        assert_eq!(config.command(Corner::TopRight), None);
        assert_eq!(config.command(Corner::BottomLeft), None);
        assert_eq!(HotCornersConfig::parse(None), HotCornersConfig::default());
    }

    #[test]
    fn theme_accessibility_flags_are_read() {
        let parse = |json: &str| match serde_json::from_str::<Value>(json) {
            Ok(Value::Object(map)) => ThemeConfig::parse(Some(&map)),
            _ => ThemeConfig::parse(None),
        };
        let both = parse(r#"{"reduceMotion": true, "reduceTransparency": true}"#);
        assert!(both.reduce_motion && both.reduce_transparency);
        let junk = parse(r#"{"reduceMotion": "yes"}"#);
        assert!(!junk.reduce_motion && !junk.reduce_transparency);
        assert!(!ThemeConfig::parse(None).reduce_motion);
    }

    #[test]
    fn theme_glass_level_is_read_and_checked() {
        let parse = |json: &str| match serde_json::from_str::<Value>(json) {
            Ok(Value::Object(map)) => ThemeConfig::parse(Some(&map)),
            _ => ThemeConfig::parse(None),
        };
        assert_eq!(parse(r#"{"glass": 0.8}"#).glass, 0.8);
        assert_eq!(parse(r#"{"glass": 3}"#), ThemeConfig::default());
        assert_eq!(
            parse(r#"{"glass": "x", "accent": "red"}"#),
            ThemeConfig::default()
        );
        assert_eq!(ThemeConfig::parse(None).glass, 0.5);
    }

    use super::*;

    fn map(text: &str) -> serde_json::Map<String, Value> {
        match serde_json::from_str(text).unwrap() {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn input_file_by_device_kind() {
        assert_eq!(InputConfig::parse(None), InputConfig::default());
        let config = InputConfig::parse(Some(&map(
            r#"{"mouse": {"speed": -0.5, "naturalScroll": false, "secondaryClick": "left",
                          "scrollSpeed": 2},
                "touchpad": {"speed": 3, "tapToClick": true, "secondaryClick": "bottomLeft",
                             "disableWhileTyping": false, "scrollSpeed": 0},
                "doubleClickMs": 400}"#,
        )));
        assert_eq!(config.mouse.speed, Some(-0.5));
        assert_eq!(config.mouse.natural_scroll, Some(false));
        assert_eq!(config.mouse.left_handed, Some(true));
        assert_eq!(config.mouse.scroll_speed, 2.0);
        assert_eq!(config.mouse.click_method, None);
        // Out of range: libinput's default speed, the normal scroll.
        assert_eq!(config.touchpad.speed, None);
        assert_eq!(config.touchpad.scroll_speed, 1.0);
        assert_eq!(config.touchpad.tap_to_click, Some(true));
        assert_eq!(config.touchpad.click_method, Some(ClickMethod::BottomLeft));
        assert_eq!(config.touchpad.left_handed, Some(true));
        assert_eq!(config.touchpad.disable_while_typing, Some(false));
        let two = InputConfig::parse(Some(&map(
            r#"{"touchpad": {"secondaryClick": "twoFinger"}}"#,
        )));
        assert_eq!(two.touchpad.left_handed, Some(false));
    }

    #[test]
    fn layouts_take_an_optional_variant() {
        assert_eq!(
            Layout::parse("de(nodeadkeys)"),
            Some(Layout {
                layout: "de".into(),
                variant: "nodeadkeys".into()
            })
        );
        assert_eq!(Layout::parse(" it ").unwrap().name(), "it");
        for bad in ["", "(x)", "us(", "us,it", "us(x;y)", "a b"] {
            assert_eq!(Layout::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn keyboard_file_over_the_defaults() {
        let defaults = (375, 11);
        let none = KeyboardConfig::parse(None, defaults);
        assert_eq!((none.repeat_delay, none.repeat_rate), defaults);
        assert!(none.layouts.is_empty());

        let config = KeyboardConfig::parse(
            Some(&map(r#"{"repeatDelay": 250, "repeatRate": 30.4,
                    "layouts": ["us", "it", 3, "bad layout", "de(nodeadkeys)", "fr", "es"],
                    "options": "caps:escape"}"#)),
            defaults,
        );
        assert_eq!((config.repeat_delay, config.repeat_rate), (250, 30));
        assert_eq!(
            config.xkb_names(),
            ("us,it,de,fr".to_owned(), ",,nodeadkeys,".to_owned())
        );
        assert_eq!(config.options, "caps:escape");

        // Out of range: that key keeps its default, rate 0 turns repeat off.
        let config = KeyboardConfig::parse(
            Some(&map(r#"{"repeatDelay": 5, "repeatRate": 0}"#)),
            defaults,
        );
        assert_eq!((config.repeat_delay, config.repeat_rate), (375, 0));
    }

    #[test]
    fn shortcuts_file_changes_adds_and_removes() {
        use silica_control::commands as c;
        let id_of = |list: &[(silica_control::commands::Shortcut, &'static str)], id| {
            list.iter()
                .find(|(_, known)| *known == id)
                .map(|(s, _)| s.clone())
        };
        let defaults = crate::commands::shortcuts_with(&ShortcutOverrides::default());
        assert!(id_of(&defaults, c::WINDOW_MINIMIZE).is_some());
        assert!(id_of(&defaults, c::WINDOW_TILE_FILL).is_none());

        let overrides = ShortcutOverrides::parse(Some(&map(
            r#"{"window.minimize": "Logo+N", "window.tile_fill": "Logo+Ctrl+F5",
                "faro.toggle": null, "app.hide": "Hyper+H", "nonsense": "Logo+X"}"#,
        )));
        let list = crate::commands::shortcuts_with(&overrides);
        let minimize = id_of(&list, c::WINDOW_MINIMIZE);
        assert!(minimize.is_some_and(|s| s.matches(true, false, false, false, "n")));
        assert!(id_of(&list, c::WINDOW_TILE_FILL).is_some());
        // Off.
        assert!(id_of(&list, c::FARO_TOGGLE).is_none());
        // A bad value keeps the default.
        assert!(
            id_of(&list, c::APP_HIDE).is_some_and(|s| s.matches(true, false, false, false, "h"))
        );
    }

    #[test]
    fn dock_file_title_bar_double_click_and_double_click_time() {
        assert_eq!(
            DockConfig::parse(None).title_bar_double_click,
            TitleBarDoubleClick::Zoom
        );
        let parse = |text| DockConfig::parse(Some(&map(text))).title_bar_double_click;
        assert_eq!(
            parse(r#"{"titleBarDoubleClick": "minimize"}"#),
            TitleBarDoubleClick::Minimize
        );
        assert_eq!(
            parse(r#"{"titleBarDoubleClick": "none"}"#),
            TitleBarDoubleClick::Nothing
        );
        assert!(DockConfig::parse(Some(&map(r#"{"minimizeToApp": true}"#))).minimize_to_app);
        assert!(!DockConfig::parse(None).minimize_to_app);
        assert_eq!(
            parse(r#"{"titleBarDoubleClick": 3}"#),
            TitleBarDoubleClick::Zoom
        );
        let time = |text| InputConfig::parse(Some(&map(text))).double_click_ms;
        assert_eq!(time(r#"{"doubleClickMs": 650}"#), Some(650));
        assert_eq!(time(r#"{"doubleClickMs": 5}"#), None);
        assert_eq!(time("{}"), None);
    }

    #[test]
    fn theme_file_cursor_size() {
        let size = |text| ThemeConfig::parse(Some(&map(text))).cursor_size;
        assert_eq!(ThemeConfig::parse(None).cursor_size, None);
        assert_eq!(size(r#"{"cursorSize": 48}"#), Some(48));
        assert_eq!(size(r#"{"cursorSize": 8}"#), None);
        assert_eq!(size(r#"{"cursorSize": "big"}"#), None);
    }

    #[test]
    fn lock_and_saver_files() {
        let delay = |text| LockConfig::parse(Some(&map(text))).password_delay;
        assert_eq!(LockConfig::parse(None).password_delay, Some(900));
        assert_eq!(delay(r#"{"passwordDelay": 0}"#), Some(0));
        assert_eq!(delay(r#"{"passwordDelay": 60}"#), Some(60));
        assert_eq!(delay(r#"{"passwordDelay": null}"#), None);
        assert_eq!(delay(r#"{"passwordDelay": -3}"#), Some(900));
        assert_eq!(delay(r#"{"showUser": false}"#), Some(900));
        let saver = SaverConfig::parse(Some(&map(r#"{"minutes": 5, "saver": "clock"}"#)));
        assert_eq!((saver.minutes, saver.saver.as_str()), (Some(5.0), "clock"));
        let odd = SaverConfig::parse(Some(&map(r#"{"minutes": 0, "saver": "fire"}"#)));
        assert_eq!((odd.minutes, odd.saver.as_str()), (None, "drift"));
        assert_eq!(SaverConfig::parse(None).minutes, None);
    }

    #[test]
    fn shortcuts_come_from_the_settings_service() {
        use crate::settings_service::{Event, ServiceSettings, Setting};
        let ids: Vec<&str> = silica_control::commands::CATALOG
            .iter()
            .take(3)
            .map(|spec| spec.id)
            .collect();
        let mut settings = ServiceSettings::default();
        settings.take(Event::All(
            [
                (
                    format!("shortcuts.{}", ids[0]),
                    Setting::Text("Logo+Shift+K".into()),
                ),
                (
                    format!("shortcuts.{}", ids[1]),
                    Setting::Text("none".into()),
                ),
                (
                    format!("shortcuts.{}", ids[2]),
                    Setting::Text(String::new()),
                ),
                ("shortcuts.capture".to_owned(), Setting::Bool(true)),
            ]
            .into_iter()
            .collect(),
        ));
        let overrides = ShortcutOverrides::from_settings(&settings);
        assert_eq!(overrides.get(ids[0]), Some(Some("Logo+Shift+K")));
        assert_eq!(overrides.get(ids[1]), Some(None));
        // Empty: the catalog's own.
        assert_eq!(overrides.get(ids[2]), None);
    }
}
