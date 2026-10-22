//! The user's settings from the Settings service (`dev.frascella.Silica.Settings1` of
//! silica-daemon, silica-os/silica-desktop#240): key repeat and input sources (`keyboard.*`), mice and
//! touchpads (`input.*`), hot corners (`hotcorners.*`), shortcuts (`shortcuts.*`), the title
//! bar's double click and minimizing into the app (`dock.*`), the password delay and the screen
//! saver (`lockscreen.passwordDelay`, `screensaver.*`) and the pointer size
//! (`accessibility.cursorSize`). Settings writes them there; the compositor no longer reads JSON files
//! for them.
//!
//! One thread reads every key (`GetAll`), then follows `Changed`, and hands each to the event
//! loop, which applies it at once. The compositor never waits for the daemon: until the service answers
//! (the daemon is started by the compositor's session) the defaults apply; if the daemon or the bus go
//! away the thread connects again.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use smithay::reexports::calloop::channel::Sender;
use tracing::{debug, warn};
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedValue, Value};

const DESTINATION: &str = "dev.frascella.Silica";
const PATH: &str = "/dev/frascella/Silica/Settings1";
const INTERFACE: &str = "dev.frascella.Silica.Settings1";
const RETRY: Duration = Duration::from_secs(2);

/// A value of the service (its D-Bus types: `b`, `x`, `d`, `s`, `as`, `ad`).
#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    Bool(bool),
    Int(i64),
    Num(f64),
    Text(String),
    Texts(Vec<String>),
    Nums(Vec<f64>),
}

/// What the thread tells the event loop.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Every key, at the start and whenever the service comes back.
    All(BTreeMap<String, Setting>),
    /// One key took a new value.
    Changed(String, Setting),
}

/// Starts the thread on the session bus `address` (the compositor's private one) or, without it, the
/// environment's.
pub fn start(address: Option<String>, events: Sender<Event>) {
    let spawned = std::thread::Builder::new()
        .name("compositor-settings".to_owned())
        .spawn(move || listen(address.as_deref(), &events));
    if let Err(err) = spawned {
        warn!("settings thread: {err}");
    }
}

fn connect(address: Option<&str>) -> zbus::Result<Connection> {
    match address {
        Some(address) => zbus::blocking::connection::Builder::address(address)?.build(),
        None => Connection::session(),
    }
}

fn listen(address: Option<&str>, events: &Sender<Event>) {
    loop {
        let result = (|| -> zbus::Result<bool> {
            let connection = connect(address)?;
            let rule = zbus::MatchRule::builder()
                .msg_type(MessageType::Signal)
                .interface(INTERFACE)?
                .member("Changed")?
                .path(PATH)?
                .build();
            // Subscribed before reading, so nothing changes unseen in between.
            let signals = MessageIterator::for_match_rule(rule, &connection, Some(64))?;
            let reply =
                connection.call_method(Some(DESTINATION), PATH, Some(INTERFACE), "GetAll", &())?;
            let all: HashMap<String, OwnedValue> = reply.body().deserialize()?;
            let values = all
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), setting_of(value)?)))
                .collect();
            if events.send(Event::All(values)).is_err() {
                // The event loop is gone: The compositor is ending.
                return Ok(true);
            }
            for message in signals {
                let message = message?;
                let (key, value): (String, OwnedValue) = message.body().deserialize()?;
                let Some(value) = setting_of(&value) else {
                    continue;
                };
                if events.send(Event::Changed(key, value)).is_err() {
                    return Ok(true);
                }
            }
            Ok(false)
        })();
        match result {
            Ok(true) => return,
            Ok(false) => {}
            Err(err) => debug!("silica-daemon (Settings1): {err}"),
        }
        std::thread::sleep(RETRY);
    }
}

/// A D-Bus value as a [`Setting`] (variants inside variants are opened).
pub fn setting_of(value: &Value<'_>) -> Option<Setting> {
    Some(match value {
        Value::Value(inner) => return setting_of(inner),
        Value::Bool(b) => Setting::Bool(*b),
        Value::I64(i) => Setting::Int(*i),
        Value::I32(i) => Setting::Int(i64::from(*i)),
        Value::U32(i) => Setting::Int(i64::from(*i)),
        Value::F64(d) => Setting::Num(*d),
        Value::Str(s) => Setting::Text(s.as_str().to_owned()),
        Value::Array(array) => {
            let items: Vec<&Value<'_>> = array.iter().collect();
            if items.iter().all(|v| matches!(v, Value::Str(_))) {
                Setting::Texts(
                    items
                        .iter()
                        .filter_map(|v| match v {
                            Value::Str(s) => Some(s.as_str().to_owned()),
                            _ => None,
                        })
                        .collect(),
                )
            } else {
                Setting::Nums(
                    items
                        .iter()
                        .filter_map(|v| match v {
                            Value::F64(d) => Some(*d),
                            _ => None,
                        })
                        .collect(),
                )
            }
        }
        _ => return None,
    })
}

/// The settings of the service as the compositor has them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceSettings(BTreeMap<String, Setting>);

impl ServiceSettings {
    /// Takes an event; the keys whose value changed.
    pub fn take(&mut self, event: Event) -> Vec<String> {
        match event {
            Event::All(values) => {
                let mut changed: Vec<String> = values
                    .iter()
                    .filter(|(key, value)| self.0.get(*key) != Some(value))
                    .map(|(key, _)| key.clone())
                    .collect();
                changed.extend(
                    self.0
                        .keys()
                        .filter(|key| !values.contains_key(*key))
                        .cloned(),
                );
                self.0 = values;
                changed
            }
            Event::Changed(key, value) => {
                if self.0.get(&key) == Some(&value) {
                    return Vec::new();
                }
                self.0.insert(key.clone(), value);
                vec![key]
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&Setting> {
        self.0.get(key)
    }

    /// Whether the service has answered (its keys are known).
    pub fn known(&self) -> bool {
        !self.0.is_empty()
    }

    /// The keys under `prefix.` (without it), in order.
    pub fn group<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = (&'a str, &'a Setting)> {
        self.0.iter().filter_map(move |(key, value)| {
            let rest = key.strip_prefix(prefix)?.strip_prefix('.')?;
            Some((rest, value))
        })
    }

    /// The keys under `prefix.` as an object, nested at the dots (`input.mouse.speed` is
    /// `{"mouse": {"speed": …}}` of `input`): the shape the parsers of `config.rs` read.
    /// `None` until the service has answered, so the defaults apply.
    pub fn object(&self, prefix: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
        if !self.known() {
            return None;
        }
        let mut root = serde_json::Map::new();
        for (path, value) in self.group(prefix) {
            let mut map = &mut root;
            let mut parts = path.split('.').peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_none() {
                    map.insert(part.to_owned(), json_of(value));
                    break;
                }
                let entry = map
                    .entry(part.to_owned())
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                if !entry.is_object() {
                    *entry = serde_json::Value::Object(serde_json::Map::new());
                }
                let serde_json::Value::Object(next) = entry else {
                    break;
                };
                map = next;
            }
        }
        Some(root)
    }
}

fn json_of(value: &Setting) -> serde_json::Value {
    match value {
        Setting::Bool(b) => serde_json::Value::Bool(*b),
        Setting::Int(i) => serde_json::Value::from(*i),
        Setting::Num(d) => serde_json::Number::from_f64(*d)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Setting::Text(s) => serde_json::Value::from(s.as_str()),
        Setting::Texts(list) => serde_json::Value::Array(
            list.iter()
                .map(|s| serde_json::Value::from(s.as_str()))
                .collect(),
        ),
        Setting::Nums(list) => serde_json::Value::Array(
            list.iter()
                .filter_map(|d| serde_json::Number::from_f64(*d).map(serde_json::Value::Number))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(pairs: &[(&str, Setting)]) -> ServiceSettings {
        let mut s = ServiceSettings::default();
        s.take(Event::All(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        ));
        s
    }

    #[test]
    fn groups_become_the_objects_the_parsers_read() {
        let s = settings(&[
            ("input.doubleClickMs", Setting::Int(500)),
            ("input.mouse.speed", Setting::Num(0.5)),
            ("input.mouse.secondaryClick", Setting::Text("left".into())),
            ("input.touchpad.tapToClick", Setting::Bool(true)),
            ("keyboard.layouts", Setting::Texts(vec!["it".into()])),
            ("inputs.other", Setting::Bool(true)),
        ]);
        assert_eq!(
            serde_json::Value::Object(s.object("input").unwrap()),
            json!({
                "doubleClickMs": 500,
                "mouse": {"speed": 0.5, "secondaryClick": "left"},
                "touchpad": {"tapToClick": true},
            })
        );
        assert_eq!(
            serde_json::Value::Object(s.object("keyboard").unwrap()),
            json!({"layouts": ["it"]})
        );
        // Before the service answers: nothing, the defaults apply.
        assert!(ServiceSettings::default().object("input").is_none());
    }

    #[test]
    fn events_report_the_keys_that_changed() {
        let mut s = settings(&[
            ("dock.titleBarDoubleClick", Setting::Text("zoom".into())),
            ("hotcorners.topLeft", Setting::Text("none".into())),
        ]);
        assert!(
            s.take(Event::Changed(
                "dock.titleBarDoubleClick".into(),
                Setting::Text("zoom".into())
            ))
            .is_empty()
        );
        assert_eq!(
            s.take(Event::Changed(
                "hotcorners.topLeft".into(),
                Setting::Text("panorama".into())
            )),
            ["hotcorners.topLeft"]
        );
        let changed = s.take(Event::All(BTreeMap::from([(
            "hotcorners.topLeft".to_owned(),
            Setting::Text("panorama".into()),
        )])));
        assert_eq!(changed, ["dock.titleBarDoubleClick"]);
    }

    #[test]
    fn dbus_values_are_read() {
        assert_eq!(setting_of(&Value::I64(3)), Some(Setting::Int(3)));
        assert_eq!(
            setting_of(&Value::Value(Box::new(Value::Bool(true)))),
            Some(Setting::Bool(true))
        );
        assert_eq!(
            setting_of(&Value::from(vec!["us".to_owned(), "it".to_owned()])),
            Some(Setting::Texts(vec!["us".into(), "it".into()]))
        );
        assert_eq!(setting_of(&Value::U8(1)), None);
    }
}
