//! The `system.*` commands: Wi-Fi, Bluetooth, volume, brightness, Do Not Disturb.
//!
//! The state and the actions belong to `silica-daemon` (`dev.frascella.Silica.System1` on the
//! session bus, `docs/daemon.md`). The compositor never waits for the daemon in the event loop: one
//! thread keeps the state cached (initial read, then the `Changed` signals), another runs the
//! queued calls. The commands answer right away: `system.status` from the cache, the actions
//! with `queued`.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value as Json};
use tracing::{debug, warn};
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedValue, Value};

const DESTINATION: &str = "dev.frascella.Silica";
const PATH: &str = "/dev/frascella/Silica/System1";
const INTERFACE: &str = "dev.frascella.Silica.System1";
const RETRY: Duration = Duration::from_secs(2);

/// Step of the volume and brightness keys: 16 notches between 0 and the maximum (measured: 16
/// presses from 0 to the maximum).
pub const STEP: f64 = 1.0 / 16.0;

/// With ⌥⇧ the step is a quarter (measured: 50 → 52, 53, 55, 56, 58).
pub const FINE_STEP: f64 = STEP / 4.0;

/// A call to the daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Wifi(bool),
    Bluetooth(bool),
    Volume(f64),
    Muted(bool),
    Brightness(f64),
    KeyboardBrightness(f64),
    DoNotDisturb(bool),
    Dark(bool),
    /// What the compositor knows that apps need (pointer kinds, outputs): the whole report as JSON.
    Report(String),
    /// Sleep (logind, through the daemon).
    Suspend,
}

impl Call {
    fn send(&self, connection: &Connection) -> zbus::Result<()> {
        match *self {
            Call::Wifi(on) => set(connection, "SetWifiEnabled", on),
            Call::Bluetooth(on) => set(connection, "SetBluetoothPowered", on),
            Call::Volume(v) => set(connection, "SetVolume", v),
            Call::Muted(on) => set(connection, "SetMuted", on),
            Call::Brightness(v) => set(connection, "SetBrightness", v),
            Call::KeyboardBrightness(v) => set(connection, "SetKeyboardBrightness", v),
            Call::DoNotDisturb(on) => set(connection, "SetDoNotDisturb", on),
            Call::Dark(on) => set(connection, "SetDark", on),
            Call::Report(ref json) => set(connection, "ReportCompositor", json.as_str()),
            Call::Suspend => connection
                .call_method(Some(DESTINATION), PATH, Some(INTERFACE), "Suspend", &())
                .map(|_| ()),
        }
    }
}

fn set<T>(connection: &Connection, method: &str, value: T) -> zbus::Result<()>
where
    T: zbus::export::serde::Serialize + zbus::zvariant::Type,
{
    connection
        .call_method(Some(DESTINATION), PATH, Some(INTERFACE), method, &(value,))
        .map(|_| ())
}

/// The link with `silica-daemon`.
pub struct SystemLink {
    calls: Sender<Call>,
    cache: Arc<Mutex<Option<Json>>>,
    /// The compositor report (`report`), sent again whenever the daemon (re)appears.
    report: Arc<Mutex<Map<String, Json>>>,
}

impl SystemLink {
    /// Starts with the session bus `address` (the compositor's private one) or, without it, with the
    /// environment's.
    /// `look_changed` hears whenever the daemon's state changes (its look drives the glass).
    pub fn start(
        address: Option<String>,
        look_changed: Option<smithay::reexports::calloop::channel::Sender<()>>,
    ) -> Self {
        let cache = Arc::new(Mutex::new(None));
        let (calls, receiver) = mpsc::channel();
        let report = Arc::new(Mutex::new(Map::new()));
        let listener_cache = cache.clone();
        let listener_report = report.clone();
        let listener_address = address.clone();
        let spawned = std::thread::Builder::new()
            .name("compositor-system".to_owned())
            .spawn(move || {
                listen(
                    listener_address,
                    &listener_cache,
                    &listener_report,
                    look_changed.as_ref(),
                )
            })
            .and_then(|_| {
                std::thread::Builder::new()
                    .name("compositor-system-calls".to_owned())
                    .spawn(move || serve_calls(address, &receiver))
            });
        if let Err(err) = spawned {
            warn!("system.* thread: {err}");
        }
        Self {
            calls,
            cache,
            report,
        }
    }

    /// Before the loop starts: no thread, every command fails.
    pub fn disconnected() -> Self {
        let (calls, _) = mpsc::channel();
        Self {
            calls,
            cache: Arc::new(Mutex::new(None)),
            report: Arc::new(Mutex::new(Map::new())),
        }
    }

    /// The compositor report as it stands.
    pub fn reported(&self) -> Map<String, Json> {
        self.report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Sets one key of the compositor report (`ReportCompositor` in silica-daemon); sent only
    /// when it changes.
    pub fn report(&self, key: &str, value: Json) -> bool {
        let json = {
            let mut report = self.report.lock().unwrap_or_else(|e| e.into_inner());
            if report.get(key) == Some(&value) {
                return true;
            }
            report.insert(key.to_owned(), value);
            Json::Object(report.clone()).to_string()
        };
        self.queue(Call::Report(json))
    }

    /// The last known state, if the daemon answered at least once.
    pub fn status(&self) -> Option<Json> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn queue(&self, call: Call) -> bool {
        self.calls.send(call).is_ok()
    }

    fn number(&self, key: &str) -> Option<f64> {
        self.status()?.get(key)?.as_f64()
    }

    fn flag(&self, key: &str) -> Option<bool> {
        self.status()?.get(key)?.as_bool()
    }

    /// Volume up or down one step (or a quarter with `fine`); raising it unmutes.
    pub fn volume_step(&self, up: bool, fine: bool) -> bool {
        let Some(volume) = self.number("audio.volume") else {
            return false;
        };
        let target = step_by(volume, up, if fine { FINE_STEP } else { STEP });
        if up && self.flag("audio.muted") == Some(true) {
            self.queue(Call::Muted(false));
        }
        self.queue(Call::Volume(target))
    }

    /// The dark appearance, from the cache of the daemon state (light if unknown).
    pub fn dark(&self) -> bool {
        self.flag("appearance.dark").unwrap_or(false)
    }

    pub fn dark_toggle(&self) -> bool {
        self.queue(Call::Dark(!self.dark()))
    }

    pub fn mute_toggle(&self) -> bool {
        match self.flag("audio.muted") {
            Some(muted) => self.queue(Call::Muted(!muted)),
            None => false,
        }
    }

    /// The keyboard backlight up or down one 1/16 step; false without a backlit keyboard.
    pub fn keyboard_step(&self, up: bool) -> bool {
        if self.flag("keyboard.available") != Some(true) {
            return false;
        }
        match self.number("keyboard.brightness") {
            Some(value) => self.queue(Call::KeyboardBrightness(step_by(value, up, STEP))),
            None => false,
        }
    }

    pub fn brightness_step(&self, up: bool, fine: bool) -> bool {
        let size = if fine { FINE_STEP } else { STEP };
        match self.number("brightness") {
            Some(value) => self.queue(Call::Brightness(step_by(value, up, size))),
            None => false,
        }
    }
}

/// A 1/16 step up or down, snapped to the notches.
#[cfg(test)]
fn step(value: f64, up: bool) -> f64 {
    step_by(value, up, STEP)
}

/// A step of `size` snapped to its notches.
pub fn step_by(value: f64, up: bool, size: f64) -> f64 {
    let notch = (value / size).round();
    let next = if up { notch + 1.0 } else { notch - 1.0 };
    (next * size).clamp(0.0, 1.0)
}

fn connect(address: Option<&str>) -> zbus::Result<Connection> {
    match address {
        Some(address) => zbus::blocking::connection::Builder::address(address)?.build(),
        None => Connection::session(),
    }
}

/// The cached state: `State` at the start, then every `Changed`. If the daemon or the bus
/// disappear it retries.
fn listen(
    address: Option<String>,
    cache: &Mutex<Option<Json>>,
    report: &Mutex<Map<String, Json>>,
    look_changed: Option<&smithay::reexports::calloop::channel::Sender<()>>,
) {
    let store = |state: Option<Json>| {
        let mut cached = cache.lock().unwrap_or_else(|e| e.into_inner());
        let look = |state: &Option<Json>| {
            state.as_ref().and_then(Json::as_object).map(|map| {
                map.iter()
                    .filter(|(key, _)| key.starts_with("look."))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Vec<_>>()
            })
        };
        let changed = look(&cached) != look(&state);
        *cached = state;
        drop(cached);
        if changed && let Some(tx) = look_changed {
            let _ = tx.send(());
        }
    };
    loop {
        let result = (|| -> zbus::Result<()> {
            let connection = connect(address.as_deref())?;
            let rule = zbus::MatchRule::builder()
                .msg_type(MessageType::Signal)
                .interface(INTERFACE)?
                .member("Changed")?
                .path(PATH)?
                .build();
            let signals = MessageIterator::for_match_rule(rule, &connection, Some(16))?;
            let reply =
                connection.call_method(Some(DESTINATION), PATH, Some(INTERFACE), "State", &())?;
            store(Some(to_json(&reply.body().deserialize()?)));
            // The daemon is there: it gets the whole report (it may have started after it was
            // first sent, or restarted).
            let current = report.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if !current.is_empty() {
                Call::Report(Json::Object(current).to_string()).send(&connection)?;
            }
            for message in signals {
                let message = message?;
                let state: HashMap<String, OwnedValue> = message.body().deserialize()?;
                store(Some(to_json(&state)));
            }
            Ok(())
        })();
        if let Err(err) = result {
            debug!("silica-daemon (System1): {err}");
        }
        store(None);
        std::thread::sleep(RETRY);
    }
}

fn serve_calls(address: Option<String>, calls: &Receiver<Call>) {
    let mut connection: Option<Connection> = None;
    while let Ok(call) = calls.recv() {
        if connection.is_none() {
            connection = connect(address.as_deref())
                .map_err(|err| warn!("system.*: session bus: {err}"))
                .ok();
        }
        let Some(conn) = &connection else { continue };
        if let Err(err) = call.send(conn) {
            warn!("system.*: {call:?}: {err}");
            if matches!(err, zbus::Error::InputOutput(_)) {
                connection = None;
            }
        }
    }
}

fn to_json(state: &HashMap<String, OwnedValue>) -> Json {
    let mut map = Map::new();
    for (key, value) in state {
        let json = match &**value {
            Value::Bool(v) => Json::from(*v),
            Value::U8(v) => Json::from(*v),
            Value::I32(v) => Json::from(*v),
            Value::U32(v) => Json::from(*v),
            Value::F64(v) => serde_json::Number::from_f64(*v).map_or(Json::Null, Json::Number),
            Value::Str(v) => Json::from(v.as_str()),
            _ => Json::Null,
        };
        map.insert(key.clone(), json);
    }
    Json::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_snap_to_sixteenths() {
        assert_eq!(step(0.5, true), 0.5625);
        assert_eq!(step(0.5, false), 0.4375);
        // 0.46 lies between two notches: it snaps to the nearest (0.4375) and then moves.
        assert_eq!(step(0.46, true), 0.5);
        assert_eq!(step(1.0, true), 1.0);
        assert_eq!(step(0.0, false), 0.0);
        // A quarter step with ⌥⇧: 50 → 51.6 (the reference rounds to 52).
        assert_eq!(step_by(0.5, true, FINE_STEP), 0.515625);
    }
}
