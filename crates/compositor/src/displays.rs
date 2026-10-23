//! Displays: `output.list`, `output.configure` and `output.color_temperature`, and the choices
//! saved in `$XDG_CONFIG_HOME/silica/displays.json`, keyed by the display's identity (EDID make,
//! model and serial; the connector name when there is no EDID). They are applied again at start,
//! when the file changes and when a display is plugged in.
//!
//! The real session drives one output (the first connected connector, `drm.rs`); the others are
//! listed but not enabled. Nested and headless sessions have a single virtual output: its scale
//! and rotation can change, its mode cannot.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use smithay::output::{Mode, Scale};
use smithay::utils::Transform;
use tracing::{info, warn};

use crate::config::{read_json, text_of};
use crate::state::State;

pub const DISPLAYS_FILE: &str = "displays.json";

/// Neutral colour temperature: no night light.
pub const NEUTRAL_KELVIN: u32 = 6500;
pub const MIN_KELVIN: u32 = 1000;

/// A display mode as the apps see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeInfo {
    pub width: i32,
    pub height: i32,
    /// Hz, with decimals (59.94).
    pub refresh: f64,
    pub preferred: bool,
}

impl ModeInfo {
    pub fn json(&self) -> Value {
        json!({
            "width": self.width,
            "height": self.height,
            "refresh": (self.refresh * 100.0).round() / 100.0,
            "preferred": self.preferred,
        })
    }
}

/// A display, connected, driven or not.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputInfo {
    pub connector: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub modes: Vec<ModeInfo>,
    pub current: Option<ModeInfo>,
    pub scale: f64,
    pub transform: Transform,
    pub enabled: bool,
    pub primary: bool,
    /// Physical size in millimetres.
    pub size_mm: (i32, i32),
}

impl OutputInfo {
    /// The key in `displays.json`.
    pub fn identity(&self) -> String {
        identity(&self.make, &self.model, &self.serial, &self.connector)
    }

    pub fn json(&self) -> Value {
        json!({
            "connector": self.connector,
            "identity": self.identity(),
            "make": self.make,
            "model": self.model,
            "serial": self.serial,
            "modes": self.modes.iter().map(ModeInfo::json).collect::<Vec<_>>(),
            "current": self.current.as_ref().map(ModeInfo::json),
            "scale": self.scale,
            "transform": transform_name(self.transform),
            "enabled": self.enabled,
            "primary": self.primary,
            "widthMm": self.size_mm.0,
            "heightMm": self.size_mm.1,
        })
    }
}

/// `make model serial`, or the connector without EDID.
pub fn identity(make: &str, model: &str, serial: &str, connector: &str) -> String {
    let parts: Vec<&str> = [make, model, serial]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        connector.to_owned()
    } else {
        parts.join(" ")
    }
}

pub fn transform_name(transform: Transform) -> &'static str {
    match transform {
        Transform::Normal => "normal",
        Transform::_90 => "90",
        Transform::_180 => "180",
        Transform::_270 => "270",
        Transform::Flipped => "flipped",
        Transform::Flipped90 => "flipped-90",
        Transform::Flipped180 => "flipped-180",
        Transform::Flipped270 => "flipped-270",
    }
}

pub fn parse_transform(name: &str) -> Option<Transform> {
    Some(match name {
        "normal" => Transform::Normal,
        "90" => Transform::_90,
        "180" => Transform::_180,
        "270" => Transform::_270,
        "flipped" => Transform::Flipped,
        "flipped-90" => Transform::Flipped90,
        "flipped-180" => Transform::Flipped180,
        "flipped-270" => Transform::Flipped270,
        _ => return None,
    })
}

/// The make, model and serial in an EDID block: the PNP id, the monitor name descriptor
/// (`0xFC`, else the product code) and the serial string (`0xFF`, else the serial number).
pub fn edid_identity(edid: &[u8]) -> Option<(String, String, String)> {
    const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
    if edid.get(..8)? != HEADER {
        return None;
    }
    let id = u16::from_be_bytes([*edid.get(8)?, *edid.get(9)?]);
    let letter = |shift: u16| char::from(b'@' + ((id >> shift) & 0x1f) as u8);
    let make: String = [letter(10), letter(5), letter(0)].iter().collect();
    let product = u16::from_le_bytes([*edid.get(10)?, *edid.get(11)?]);
    let number = u32::from_le_bytes([
        *edid.get(12)?,
        *edid.get(13)?,
        *edid.get(14)?,
        *edid.get(15)?,
    ]);
    let mut model = String::new();
    let mut serial = String::new();
    for start in [54usize, 72, 90, 108] {
        let Some(block) = edid.get(start..start + 18) else {
            continue;
        };
        if block.get(..3) != Some(&[0, 0, 0]) {
            continue;
        }
        let text = || {
            let bytes = block.get(5..18).unwrap_or_default();
            let end = bytes
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(bytes.len());
            String::from_utf8_lossy(bytes.get(..end).unwrap_or_default())
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        };
        match block.get(3) {
            Some(0xfc) => model = text(),
            Some(0xff) => serial = text(),
            _ => {}
        }
    }
    if model.is_empty() {
        model = format!("0x{product:04x}");
    }
    if serial.is_empty() && number != 0 {
        serial = number.to_string();
    }
    Some((make, model, serial))
}

/// One display's saved choice.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputPrefs {
    /// Width, height and refresh in Hz (none: the closest).
    pub mode: Option<(i32, i32, Option<f64>)>,
    pub scale: Option<f64>,
    pub transform: Option<Transform>,
}

impl OutputPrefs {
    pub fn parse(map: &Map<String, Value>) -> Self {
        let mode = map.get("mode").and_then(Value::as_object).and_then(|m| {
            let width = m
                .get("width")?
                .as_i64()
                .and_then(|w| i32::try_from(w).ok())?;
            let height = m
                .get("height")?
                .as_i64()
                .and_then(|h| i32::try_from(h).ok())?;
            let refresh = m
                .get("refresh")
                .and_then(Value::as_f64)
                .filter(|r| *r > 0.0);
            (width > 0 && height > 0).then_some((width, height, refresh))
        });
        let scale = map
            .get("scale")
            .and_then(Value::as_f64)
            .filter(|s| valid_scale(*s));
        let transform = text_of(map, "transform").and_then(|t| parse_transform(&t));
        Self {
            mode,
            scale,
            transform,
        }
    }

    pub fn json(&self) -> Value {
        let mut map = Map::new();
        if let Some((width, height, refresh)) = self.mode {
            map.insert(
                "mode".into(),
                json!({"width": width, "height": height, "refresh": refresh}),
            );
        }
        if let Some(scale) = self.scale {
            map.insert("scale".into(), json!(scale));
        }
        if let Some(transform) = self.transform {
            map.insert("transform".into(), json!(transform_name(transform)));
        }
        Value::Object(map)
    }
}

/// 1–4 in quarter steps.
pub fn valid_scale(scale: f64) -> bool {
    scale.is_finite() && (1.0..=4.0).contains(&scale) && (scale * 4.0).fract() == 0.0
}

/// The saved choices, by identity.
pub fn parse_displays(map: Option<&Map<String, Value>>) -> BTreeMap<String, OutputPrefs> {
    map.and_then(|m| m.get("outputs"))
        .and_then(Value::as_object)
        .map(|outputs| {
            outputs
                .iter()
                .filter_map(|(key, value)| {
                    Some((key.clone(), OutputPrefs::parse(value.as_object()?)))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The mode among `modes` that matches the wish: same size, the closest refresh (the highest
/// without one).
pub fn pick_mode(modes: &[ModeInfo], wish: (i32, i32, Option<f64>)) -> Option<usize> {
    let (width, height, refresh) = wish;
    modes
        .iter()
        .enumerate()
        .filter(|(_, m)| m.width == width && m.height == height)
        .min_by(|(_, a), (_, b)| {
            let distance = |m: &ModeInfo| match refresh {
                Some(r) => (m.refresh - r).abs(),
                None => -m.refresh,
            };
            distance(a).total_cmp(&distance(b))
        })
        .map(|(i, _)| i)
}

/// The red, green and blue factors of a colour temperature (an approximation of the blackbody
/// curve in sRGB, 1000–6500 K; 6500 is white).
pub fn kelvin_rgb(kelvin: u32) -> (f64, f64, f64) {
    if kelvin >= NEUTRAL_KELVIN {
        return (1.0, 1.0, 1.0);
    }
    let k = f64::from(kelvin.max(MIN_KELVIN)) / 100.0;
    let red = 1.0;
    let green = if k <= 66.0 {
        (99.470_802_586_1 * k.ln() - 161.119_568_166_1) / 255.0
    } else {
        1.0
    };
    let blue = if k >= 65.0 {
        1.0
    } else if k <= 19.0 {
        0.0
    } else {
        (138.517_731_223_1 * (k - 10.0).ln() - 305.044_792_730_7) / 255.0
    };
    (red, green.clamp(0.0, 1.0), blue.clamp(0.0, 1.0))
}

/// A gamma ramp of `size` entries for a colour temperature.
pub fn gamma_ramp(kelvin: u32, size: usize) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let (r, g, b) = kelvin_rgb(kelvin);
    let ramp = |factor: f64| {
        (0..size)
            .map(|i| {
                let x = i as f64 / (size.max(2) - 1) as f64;
                (x * factor * 65535.0).round().clamp(0.0, 65535.0) as u16
            })
            .collect::<Vec<u16>>()
    };
    (ramp(r), ramp(g), ramp(b))
}

/// The parameters of `output.configure`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Configure {
    pub connector: Option<String>,
    pub prefs: OutputPrefs,
}

impl Configure {
    pub fn parse(params: &Value) -> Result<Self, String> {
        let map = params
            .as_object()
            .ok_or("parameters: an object with connector, mode, scale, transform")?;
        for key in map.keys() {
            if !["connector", "mode", "scale", "transform"].contains(&key.as_str()) {
                return Err(format!("parameters: unknown field {key}"));
            }
        }
        let prefs = OutputPrefs::parse(map);
        if map.contains_key("mode") && prefs.mode.is_none() {
            return Err("mode: {width, height, refresh?}".into());
        }
        if map.contains_key("scale") && prefs.scale.is_none() {
            return Err("scale: 1–4 in steps of 0.25".into());
        }
        if map.contains_key("transform") && prefs.transform.is_none() {
            return Err("transform: normal, 90, 180, 270, flipped, flipped-90…".into());
        }
        Ok(Self {
            connector: text_of(map, "connector"),
            prefs,
        })
    }
}

impl State {
    /// Every display known to the backend.
    pub fn outputs(&mut self) -> Vec<OutputInfo> {
        #[cfg(feature = "drm")]
        if let Some(list) = self.drm_outputs() {
            return list;
        }
        let mode = self.output.current_mode();
        let current = mode.map(|m| ModeInfo {
            width: m.size.w,
            height: m.size.h,
            refresh: f64::from(m.refresh) / 1000.0,
            preferred: true,
        });
        let physical = self.output.physical_properties();
        vec![OutputInfo {
            connector: self.output.name(),
            make: String::new(),
            model: String::new(),
            serial: String::new(),
            modes: current.into_iter().collect(),
            current,
            scale: self.output.current_scale().fractional_scale(),
            transform: self.output.current_transform(),
            enabled: true,
            primary: true,
            size_mm: (physical.size.w, physical.size.h),
        }]
    }

    pub fn output_list(&mut self) -> Value {
        Value::Array(self.outputs().iter().map(OutputInfo::json).collect())
    }

    /// `output.configure`: applies at once and saves it for that display.
    pub fn output_configure(&mut self, params: &Value) -> Result<Value, String> {
        let configure = Configure::parse(params)?;
        let outputs = self.outputs();
        let target = match &configure.connector {
            Some(name) => outputs.iter().find(|o| &o.connector == name),
            None => outputs.iter().find(|o| o.enabled),
        }
        .ok_or("no such display")?
        .clone();
        if !target.enabled {
            return Err("only the display in use can be configured for now".into());
        }
        if let Some(wish) = configure.prefs.mode
            && pick_mode(&target.modes, wish).is_none()
        {
            return Err(format!(
                "{}: no {}x{} mode",
                target.connector, wish.0, wish.1
            ));
        }
        // Merge with what is saved for the display, then apply the whole choice.
        let identity = target.identity();
        let mut saved = self.saved_displays();
        let prefs = saved.entry(identity.clone()).or_default();
        if configure.prefs.mode.is_some() {
            prefs.mode = configure.prefs.mode;
        }
        if configure.prefs.scale.is_some() {
            prefs.scale = configure.prefs.scale;
        }
        if configure.prefs.transform.is_some() {
            prefs.transform = configure.prefs.transform;
        }
        let prefs = prefs.clone();
        self.apply_output_prefs(&target, &prefs)?;
        self.save_displays(&saved);
        self.report_outputs();
        Ok(self
            .outputs()
            .into_iter()
            .find(|o| o.connector == target.connector)
            .map(|o| o.json())
            .unwrap_or(Value::Null))
    }

    fn saved_displays(&self) -> BTreeMap<String, OutputPrefs> {
        match &self.config_dir {
            Some(dir) => parse_displays(read_json(dir, DISPLAYS_FILE).as_ref()),
            None => BTreeMap::new(),
        }
    }

    /// Writes `displays.json` keeping the keys other components put there.
    fn save_displays(&self, outputs: &BTreeMap<String, OutputPrefs>) {
        let Some(dir) = &self.config_dir else {
            return;
        };
        let mut map = read_json(dir, DISPLAYS_FILE).unwrap_or_default();
        map.insert(
            "outputs".into(),
            Value::Object(
                outputs
                    .iter()
                    .map(|(key, prefs)| (key.clone(), prefs.json()))
                    .collect(),
            ),
        );
        let text = serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_default();
        let temporary = dir.join(format!("{DISPLAYS_FILE}.tmp"));
        let written = std::fs::write(&temporary, text)
            .and_then(|()| std::fs::rename(&temporary, dir.join(DISPLAYS_FILE)));
        if let Err(err) = written {
            warn!("{DISPLAYS_FILE}: {err}");
        }
    }

    /// Mode, scale and rotation of a display.
    fn apply_output_prefs(
        &mut self,
        target: &OutputInfo,
        prefs: &OutputPrefs,
    ) -> Result<(), String> {
        if let Some(wish) = prefs.mode
            && let Some(index) = pick_mode(&target.modes, wish)
            && target.current.as_ref() != target.modes.get(index)
        {
            #[cfg(feature = "drm")]
            self.drm_set_mode(index)?;
            #[cfg(not(feature = "drm"))]
            let _ = index;
        }
        let scale = prefs.scale.map(|s| {
            if s.fract() == 0.0 {
                Scale::Integer(s as i32)
            } else {
                Scale::Fractional(s)
            }
        });
        if scale.is_some() || prefs.transform.is_some() {
            self.output
                .change_current_state(None, prefs.transform, scale, None);
            self.output_resized();
            info!(
                "display {}: scale {:?}, {:?}",
                target.connector, prefs.scale, prefs.transform
            );
        }
        Ok(())
    }

    /// `displays.json` changed, the compositor started or a display was plugged in: the saved choice of
    /// the display in use.
    pub fn apply_displays_config(&mut self) {
        let saved = self.saved_displays();
        let outputs = self.outputs();
        for output in outputs.iter().filter(|o| o.enabled) {
            if let Some(prefs) = saved.get(&output.identity())
                && let Err(err) = self.apply_output_prefs(output, prefs)
            {
                warn!("{DISPLAYS_FILE}: {}: {err}", output.connector);
            }
        }
        self.report_outputs();
    }

    /// Tells silica-daemon the displays (`displays` in `System1`, for Settings).
    pub fn report_outputs(&mut self) {
        let list = self.output_list().to_string();
        self.system.report("displays", Value::String(list));
    }

    /// `output.color_temperature`: the night light (1000–6500 K; 6500 is neutral) on every
    /// display, through the CRTC gamma ramp. False where the backend has no gamma.
    pub fn set_color_temperature(&mut self, kelvin: u32) -> bool {
        self.color_temperature = kelvin.clamp(MIN_KELVIN, NEUTRAL_KELVIN);
        #[cfg(feature = "drm")]
        if let Some(applied) = self.drm_apply_gamma() {
            return applied;
        }
        false
    }
}

/// Used by the backends: a `ModeInfo` from Smithay's mode.
pub fn mode_info(mode: Mode, preferred: bool) -> ModeInfo {
    ModeInfo {
        width: mode.size.w,
        height: mode.size.h,
        refresh: f64::from(mode.refresh) / 1000.0,
        preferred,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An EDID with PNP id "DEL", product 0x4321, serial number 7, a name and a serial string.
    fn edid() -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
        // D=4, E=5, L=12: 00100 00101 01100.
        let id: u16 = (4 << 10) | (5 << 5) | 12;
        e[8..10].copy_from_slice(&id.to_be_bytes());
        e[10..12].copy_from_slice(&0x4321u16.to_le_bytes());
        e[12..16].copy_from_slice(&7u32.to_le_bytes());
        let descriptor = |e: &mut Vec<u8>, at: usize, tag: u8, text: &str| {
            e[at..at + 5].copy_from_slice(&[0, 0, 0, tag, 0]);
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(b'\n');
            bytes.resize(13, b' ');
            e[at + 5..at + 18].copy_from_slice(&bytes);
        };
        descriptor(&mut e, 72, 0xfc, "DELL U2720Q");
        descriptor(&mut e, 90, 0xff, "ABC123");
        e
    }

    #[test]
    fn edid_names_the_display() {
        assert_eq!(
            edid_identity(&edid()),
            Some(("DEL".into(), "DELL U2720Q".into(), "ABC123".into()))
        );
        let mut bare = edid();
        bare[72..108].fill(1);
        assert_eq!(
            edid_identity(&bare),
            Some(("DEL".into(), "0x4321".into(), "7".into()))
        );
        assert_eq!(edid_identity(&[0; 20]), None);
        assert_eq!(edid_identity(&edid()[..9]), None);
        assert_eq!(identity("", "", "", "Virtual-1"), "Virtual-1");
        assert_eq!(identity("DEL", "U", "", "DP-1"), "DEL U");
    }

    #[test]
    fn modes_match_size_then_refresh() {
        let mode = |width, height, refresh| ModeInfo {
            width,
            height,
            refresh,
            preferred: false,
        };
        let modes = [
            mode(1920, 1080, 60.0),
            mode(1920, 1080, 144.0),
            mode(1920, 1080, 59.94),
            mode(1280, 720, 60.0),
        ];
        assert_eq!(pick_mode(&modes, (1920, 1080, Some(59.9))), Some(2));
        assert_eq!(pick_mode(&modes, (1920, 1080, None)), Some(1));
        assert_eq!(pick_mode(&modes, (800, 600, None)), None);
    }

    #[test]
    fn configure_parameters_are_checked() {
        let c = Configure::parse(
            &json!({"connector": "DP-1", "scale": 1.5, "transform": "90",
            "mode": {"width": 1920, "height": 1080, "refresh": 60}}),
        )
        .unwrap();
        assert_eq!(c.connector.as_deref(), Some("DP-1"));
        assert_eq!(c.prefs.scale, Some(1.5));
        assert_eq!(c.prefs.transform, Some(Transform::_90));
        assert_eq!(c.prefs.mode, Some((1920, 1080, Some(60.0))));
        for bad in [
            json!({"scale": 1.3}),
            json!({"scale": 9}),
            json!({"transform": "sideways"}),
            json!({"mode": {"width": 0, "height": 1}}),
            json!({"position": 1}),
            json!([1]),
        ] {
            assert!(Configure::parse(&bad).is_err(), "{bad}");
        }
        // Saved choices survive a round trip.
        let saved = parse_displays(Some(
            json!({"outputs": {"DEL U": c.prefs.json(), "bad": 3}})
                .as_object()
                .unwrap(),
        ));
        assert_eq!(saved.len(), 1);
        assert_eq!(saved["DEL U"], c.prefs);
    }

    #[test]
    fn night_light_warms_the_ramp() {
        assert_eq!(kelvin_rgb(6500), (1.0, 1.0, 1.0));
        let (r, g, b) = kelvin_rgb(3400);
        assert_eq!(r, 1.0);
        assert!(g < 1.0 && b < g, "{g} {b}");
        assert_eq!(kelvin_rgb(10), kelvin_rgb(1000));
        let (red, green, blue) = gamma_ramp(3400, 256);
        assert_eq!((red[0], red[255]), (0, 65535));
        assert!(green[255] < 65535 && blue[255] < green[255]);
        assert_eq!(gamma_ramp(6500, 4).2, [0, 21845, 43690, 65535]);
    }
}
