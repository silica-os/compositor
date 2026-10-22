//! Accessibility of the display and the keyboard (`accessibility.*` of the Settings service):
//! zoom around the pointer, colour filters and contrast on the whole frame, sticky keys and
//! slow keys. Reduce Motion and Reduce Transparency live with the look (`config.rs`).

use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::input::KeyState;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::gles::element::TextureShaderElement;
use smithay::backend::renderer::gles::{GlesRenderer, Uniform};
use smithay::backend::renderer::{Bind, Offscreen, Renderer};
use smithay::input::keyboard::Keycode;
use smithay::output::Output;
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};
use tracing::warn;

use crate::render::OutputElement;
use crate::state::State;

/// The largest zoom factor.
const MAX_ZOOM: f64 = 20.0;

/// How the colours are filtered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ColorFilter {
    #[default]
    None,
    Invert,
    Grayscale,
    InvertGrayscale,
}

impl ColorFilter {
    fn parse(name: &str) -> Self {
        match name {
            "invert" => Self::Invert,
            "grayscale" => Self::Grayscale,
            "invertGrayscale" => Self::InvertGrayscale,
            _ => Self::None,
        }
    }

    fn mode(self) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Invert => 1.0,
            Self::Grayscale => 2.0,
            Self::InvertGrayscale => 3.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccessibilityConfig {
    /// 1 is no zoom.
    pub zoom: f64,
    pub color_filter: ColorFilter,
    pub increase_contrast: bool,
    pub sticky_keys: bool,
    /// A key counts only once held this long; zero is off.
    pub slow_keys: Duration,
}

impl Default for AccessibilityConfig {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            color_filter: ColorFilter::None,
            increase_contrast: false,
            sticky_keys: false,
            slow_keys: Duration::ZERO,
        }
    }
}

impl AccessibilityConfig {
    pub fn parse(map: Option<&serde_json::Map<String, serde_json::Value>>) -> Self {
        let mut config = Self::default();
        let Some(map) = map else {
            return config;
        };
        if let Some(zoom) = map.get("zoom").and_then(serde_json::Value::as_f64)
            && zoom.is_finite()
        {
            config.zoom = zoom.clamp(1.0, MAX_ZOOM);
        }
        if let Some(name) = map.get("colorFilter").and_then(serde_json::Value::as_str) {
            config.color_filter = ColorFilter::parse(name);
        }
        let flag = |key: &str| map.get(key).and_then(serde_json::Value::as_bool);
        config.increase_contrast = flag("increaseContrast").unwrap_or(false);
        config.sticky_keys = flag("stickyKeys").unwrap_or(false);
        if let Some(ms) = map.get("slowKeysDelay").and_then(serde_json::Value::as_u64) {
            config.slow_keys = Duration::from_millis(ms.min(2000));
        }
        config
    }

    /// The frame goes through the filter pass.
    pub fn filters_frame(&self) -> bool {
        self.zoom > 1.0 || self.color_filter != ColorFilter::None || self.increase_contrast
    }
}

/// The keyboard side: keys waiting for their slow-keys delay, and latched modifiers.
#[derive(Debug, Default)]
pub struct KeyAccess {
    /// Pressed, not yet accepted (slow keys).
    waiting: Vec<(Keycode, RegistrationToken)>,
    /// Accepted after the delay, still held.
    accepted: Vec<Keycode>,
    /// The modifier pressed with no other key since (sticky keys).
    lone_modifier: Option<Keycode>,
    /// Modifiers released by the hand but held for the next key (sticky keys).
    latched: Vec<Keycode>,
}

/// Shift, Control, Alt and the logo keys (xkb keycodes: evdev + 8).
fn is_modifier(code: Keycode) -> bool {
    matches!(code.raw(), 50 | 62 | 37 | 105 | 64 | 108 | 133 | 134)
}

/// The visible part of the frame at `zoom`, centred on the pointer and kept inside it.
pub fn zoom_viewport(
    size: Size<f64, Logical>,
    pointer: Point<f64, Logical>,
    zoom: f64,
) -> Rectangle<f64, Logical> {
    let zoom = zoom.clamp(1.0, MAX_ZOOM);
    let view = Size::from((size.w / zoom, size.h / zoom));
    let x = (pointer.x - view.w / 2.0).clamp(0.0, (size.w - view.w).max(0.0));
    let y = (pointer.y - view.h / 2.0).clamp(0.0, (size.h - view.h).max(0.0));
    Rectangle::new((x, y).into(), view)
}

/// The frame drawn into a texture, then on the output zoomed and filtered. Without a filter,
/// the elements are returned as they are; on an error too (the frame stays readable).
pub fn post_process(
    renderer: &mut GlesRenderer,
    output: &Output,
    elements: Vec<OutputElement>,
    pointer: Option<Point<f64, Logical>>,
    clear_color: [f32; 4],
    config: &AccessibilityConfig,
) -> Vec<OutputElement> {
    if !config.filters_frame() {
        return elements;
    }
    let Some(program) = compositor_render::shaders(renderer).filter else {
        return elements;
    };
    let Some(mode) = output.current_mode() else {
        return elements;
    };
    let size = mode.size;
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    let rendered = (|| -> Result<_, String> {
        let mut texture =
            Offscreen::<smithay::backend::renderer::gles::GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                buffer_size,
            )
            .map_err(|e| format!("buffer: {e:?}"))?;
        {
            let mut framebuffer = renderer
                .bind(&mut texture)
                .map_err(|e| format!("bind: {e:?}"))?;
            let scale = output.current_scale().fractional_scale();
            let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
            tracker
                .render_output(renderer, &mut framebuffer, 0, &elements, clear_color)
                .map_err(|e| format!("render: {e:?}"))?;
        }
        Ok(texture)
    })();
    let texture = match rendered {
        Ok(texture) => texture,
        Err(err) => {
            warn!("accessibility pass: {err}");
            return elements;
        }
    };
    let scale = output.current_scale().fractional_scale();
    let logical = size.to_f64().to_logical(scale);
    // The texture is in pixels; the viewport is computed in points, then scaled to them.
    let origin = pointer.unwrap_or_default();
    let view = zoom_viewport(logical, origin, config.zoom);
    let src = Rectangle::new(
        (view.loc.x * scale, view.loc.y * scale).into(),
        (view.size.w * scale, view.size.h * scale).into(),
    );
    let inner = TextureRenderElement::from_static_texture(
        smithay::backend::renderer::element::Id::new(),
        renderer.context_id(),
        (0.0, 0.0),
        texture,
        1,
        Transform::Normal,
        None,
        Some(src),
        Some(logical.to_i32_round::<i32>()),
        None,
        Kind::Unspecified,
    );
    let contrast = if config.increase_contrast { 1.35 } else { 1.0 };
    let element = TextureShaderElement::new(
        inner,
        program,
        vec![
            Uniform::new("mode", config.color_filter.mode()),
            Uniform::new("contrast", contrast),
        ],
    );
    vec![OutputElement::Filtered(element)]
}

impl State {
    /// `accessibility.*` changed.
    pub fn apply_accessibility_config(&mut self) {
        let config =
            AccessibilityConfig::parse(self.service_settings.object("accessibility").as_ref());
        if config != self.accessibility {
            self.accessibility = config;
            self.wake();
        }
    }

    /// A key from the hardware, through slow keys and sticky keys, then to `deliver_key`.
    pub fn handle_key(&mut self, code: Keycode, state: KeyState, time: u32) {
        let delay = self.accessibility.slow_keys;
        if delay.is_zero() {
            self.sticky_key(code, state, time);
            return;
        }
        match state {
            KeyState::Pressed => {
                let timer = Timer::from_duration(delay);
                let token = self.loop_handle.insert_source(timer, move |_, _, data| {
                    data.state.slow_key_accepted(code, time);
                    TimeoutAction::Drop
                });
                match token {
                    Ok(token) => self.key_access.waiting.push((code, token)),
                    Err(err) => {
                        warn!("slow keys timer: {err}");
                        self.sticky_key(code, state, time);
                    }
                }
            }
            KeyState::Released => {
                if let Some(index) = self.key_access.waiting.iter().position(|(c, _)| *c == code) {
                    // Let go too soon: the key never happened.
                    let (_, token) = self.key_access.waiting.remove(index);
                    self.loop_handle.remove(token);
                } else {
                    self.key_access.accepted.retain(|c| *c != code);
                    self.sticky_key(code, state, time);
                }
            }
        }
    }

    /// A key held through the slow-keys delay counts now.
    fn slow_key_accepted(&mut self, code: Keycode, time: u32) {
        let Some(index) = self.key_access.waiting.iter().position(|(c, _)| *c == code) else {
            return;
        };
        self.key_access.waiting.remove(index);
        self.key_access.accepted.push(code);
        self.sticky_key(code, KeyState::Pressed, time);
    }

    /// Sticky keys: a modifier pressed and released alone stays down for the next key.
    fn sticky_key(&mut self, code: Keycode, state: KeyState, time: u32) {
        if !self.accessibility.sticky_keys {
            self.release_latched(time);
            self.deliver_key(code, state, time);
            return;
        }
        let modifier = is_modifier(code);
        match state {
            KeyState::Pressed if modifier => {
                if let Some(index) = self.key_access.latched.iter().position(|c| *c == code) {
                    // Pressed again while latched: it lets go at the release.
                    self.key_access.latched.remove(index);
                    self.key_access.lone_modifier = None;
                    return;
                }
                self.key_access.lone_modifier = Some(code);
                self.deliver_key(code, state, time);
            }
            KeyState::Released if modifier => {
                if self.key_access.lone_modifier == Some(code) {
                    self.key_access.lone_modifier = None;
                    self.key_access.latched.push(code);
                } else {
                    self.deliver_key(code, state, time);
                }
            }
            KeyState::Pressed => {
                self.key_access.lone_modifier = None;
                self.deliver_key(code, state, time);
            }
            KeyState::Released => {
                self.deliver_key(code, state, time);
                self.release_latched(time);
            }
        }
    }

    /// The latched modifiers go up.
    fn release_latched(&mut self, time: u32) {
        for code in std::mem::take(&mut self.key_access.latched) {
            self.deliver_key(code, KeyState::Released, time);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_reads_the_settings_and_clamps_them() {
        let map = serde_json::json!({
            "zoom": 50.0,
            "colorFilter": "grayscale",
            "increaseContrast": true,
            "stickyKeys": true,
            "slowKeysDelay": 300
        });
        let config = AccessibilityConfig::parse(map.as_object());
        assert_eq!(config.zoom, MAX_ZOOM);
        assert_eq!(config.color_filter, ColorFilter::Grayscale);
        assert!(config.increase_contrast && config.sticky_keys);
        assert_eq!(config.slow_keys, Duration::from_millis(300));
        assert!(config.filters_frame());
        assert!(!AccessibilityConfig::parse(None).filters_frame());
    }

    #[test]
    fn the_zoom_follows_the_pointer_inside_the_screen() {
        let size = Size::from((1000.0, 800.0));
        let view = zoom_viewport(size, (500.0, 400.0).into(), 2.0);
        assert_eq!(
            view,
            Rectangle::new((250.0, 200.0).into(), (500.0, 400.0).into())
        );
        let corner = zoom_viewport(size, (0.0, 0.0).into(), 4.0);
        assert_eq!(corner.loc, (0.0, 0.0).into());
        let far = zoom_viewport(size, (1000.0, 800.0).into(), 4.0);
        assert_eq!(far.loc, (750.0, 600.0).into());
    }

    #[test]
    fn modifiers_are_known_by_their_keycode() {
        assert!(is_modifier(Keycode::new(50)));
        assert!(is_modifier(Keycode::new(133)));
        assert!(!is_modifier(Keycode::new(38)));
    }
}
