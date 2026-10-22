//! The system keymap: what `localectl set-x11-keymap` (and first boot) chose, read from
//! systemd-localed. It applies until the user picks layouts in Settings (`keyboard.*`).

use smithay::input::keyboard::XkbConfig;
use tracing::{info, warn};
use zbus::blocking::{Connection, Proxy};

use crate::state::State;

/// The X11 keymap names localed holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SystemKeymap {
    pub layout: String,
    pub variant: String,
    pub model: String,
    pub options: String,
}

/// localed's keymap, or `None` without the system bus or a layout.
pub fn system_keymap() -> Option<SystemKeymap> {
    let read = || -> zbus::Result<SystemKeymap> {
        let connection = Connection::system()?;
        let proxy = Proxy::new(
            &connection,
            "org.freedesktop.locale1",
            "/org/freedesktop/locale1",
            "org.freedesktop.locale1",
        )?;
        Ok(SystemKeymap {
            layout: proxy.get_property("X11Layout")?,
            variant: proxy.get_property("X11Variant")?,
            model: proxy.get_property("X11Model")?,
            options: proxy.get_property("X11Options")?,
        })
    };
    match read() {
        Ok(keymap) if !keymap.layout.is_empty() => Some(keymap),
        Ok(_) => None,
        Err(err) => {
            warn!("system keymap: {err}");
            None
        }
    }
}

impl State {
    /// The keymap of the real session comes from localed, when the user chose no layouts.
    pub fn apply_system_keymap(&mut self) {
        if !self.keyboard_config.layouts.is_empty() {
            return;
        }
        let (Some(keymap), Some(keyboard)) = (system_keymap(), self.seat.get_keyboard()) else {
            return;
        };
        let xkb = XkbConfig {
            rules: "",
            model: &keymap.model,
            layout: &keymap.layout,
            variant: &keymap.variant,
            options: (!keymap.options.is_empty()).then(|| keymap.options.clone()),
        };
        match keyboard.set_xkb_config(self, xkb) {
            Ok(()) => info!("system keymap: {} ({})", keymap.layout, keymap.variant),
            Err(err) => warn!("system keymap {}: {err:?}", keymap.layout),
        }
    }
}
