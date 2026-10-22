//! Where an app's window was when it closed: a new window of the same app opens there
//! (APP-LC: a single-window app that quit with its window, such as Calculator or Clock,
//! comes back in the same place). Kept per app id in memory and in
//! `$XDG_STATE_HOME/silica/window-frames.json`, so it survives the session.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// At most this many apps are remembered (the oldest entries are dropped).
const MAX_APPS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Default)]
pub struct AppFrames {
    frames: BTreeMap<String, Frame>,
    loaded: bool,
}

fn file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("silica").join("window-frames.json"))
}

/// Whether an app id is acceptable as a key (the same rule as the app registry).
fn usable(app_id: &str) -> bool {
    !app_id.is_empty()
        && app_id.len() <= 255
        && app_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

impl AppFrames {
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let Some(path) = file() else {
            return;
        };
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(frames) = serde_json::from_str::<BTreeMap<String, Frame>>(&text)
        {
            self.frames = frames.into_iter().filter(|(k, _)| usable(k)).collect();
        }
    }

    /// The saved position of `app_id`'s window.
    pub fn get(&mut self, app_id: &str) -> Option<Frame> {
        self.load();
        self.frames.get(app_id).copied()
    }

    /// Remembers where `app_id`'s window was, and saves the file.
    pub fn remember(&mut self, app_id: &str, frame: Frame) {
        if !usable(app_id) {
            return;
        }
        self.load();
        self.frames.insert(app_id.to_owned(), frame);
        while self.frames.len() > MAX_APPS {
            let Some(first) = self.frames.keys().next().cloned() else {
                break;
            };
            self.frames.remove(&first);
        }
        let Some(path) = file() else {
            return;
        };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let text = serde_json::to_string(&self.frames).map_err(std::io::Error::other)?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(err) = write() {
            tracing::warn!("window frames: {}: {err}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_are_checked() {
        assert!(usable("dev.frascella.Calculator"));
        assert!(!usable(""));
        assert!(!usable("../etc"));
        assert!(!usable("a b"));
    }

    #[test]
    fn frames_round_trip_through_json() {
        let mut frames = BTreeMap::new();
        frames.insert("dev.frascella.Clock".to_owned(), Frame { x: 120, y: 80 });
        let text = serde_json::to_string(&frames).unwrap();
        let back: BTreeMap<String, Frame> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, frames);
    }
}
