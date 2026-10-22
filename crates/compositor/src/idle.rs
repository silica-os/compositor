//! Energy: the display turns off and the computer sleeps after the idle times of `power.json`
//! (Settings › Energy / Lock Screen). Any input is activity: it wakes the display and starts
//! the count again.
//!
//! - Display off: the real session sets the CRTC's DPMS off (`DrmSurface::clear`) and stops
//!   drawing until the next input; nested and headless only keep the state.
//! - Sleep: silica-daemon asks logind to suspend (`System1.Suspend`), unless `preventSleep` is
//!   on (silica-daemon then also holds logind's `idle` inhibitor).

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracing::info;

use crate::config::{PowerConfig, read_json};
use crate::state::State;

/// How often the idle time is checked.
pub const CHECK: Duration = Duration::from_secs(1);

/// What the idle time calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleActions {
    pub display_off: bool,
    pub sleep: bool,
}

/// The actions due after `idle` with `config`.
pub fn due(config: &PowerConfig, idle: Duration) -> IdleActions {
    let after = |minutes: Option<f64>| minutes.is_some_and(|m| idle.as_secs_f64() >= m * 60.0);
    IdleActions {
        display_off: after(config.display_off_minutes),
        sleep: !config.prevent_sleep && after(config.sleep_minutes),
    }
}

impl State {
    /// Input from the user (real or synthetic): the display comes back, the count restarts.
    pub fn user_activity(&mut self) {
        self.last_activity = Instant::now();
        self.idle_slept = false;
        self.lock_activity();
        if self.display_off {
            self.display_off = false;
            info!("display on (activity)");
            #[cfg(feature = "drm")]
            self.drm_display_power(true);
        }
    }

    /// `power.json` changed.
    pub fn apply_power_config(&mut self) {
        let Some(dir) = self.config_dir.clone() else {
            return;
        };
        self.power_config = PowerConfig::parse(read_json(&dir, crate::config::POWER_FILE).as_ref());
    }

    /// Every `CHECK`: turns the display off or asks for sleep when their time has come.
    pub fn idle_check(&mut self) {
        let idle = self.last_activity.elapsed();
        let actions = due(&self.power_config, idle);
        if actions.display_off && !self.display_off {
            self.display_off = true;
            info!("display off after {} s idle", idle.as_secs());
            #[cfg(feature = "drm")]
            self.drm_display_power(false);
        }
        self.lock_check(idle);
        if actions.sleep && !self.idle_slept {
            self.idle_slept = true;
            info!("sleep after {} s idle", idle.as_secs());
            self.lock_before_sleep();
            self.system.queue(crate::system::Call::Suspend);
        }
    }

    /// `idle.state`.
    pub fn idle_state(&self) -> Value {
        json!({
            "idleSeconds": self.last_activity.elapsed().as_secs(),
            "displayOff": self.display_off,
            "displayOffMinutes": self.power_config.display_off_minutes,
            "sleepMinutes": self.power_config.sleep_minutes,
            "preventSleep": self.power_config.prevent_sleep,
            "lock": self.lock_info(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_times() {
        let config = PowerConfig {
            display_off_minutes: Some(2.0),
            sleep_minutes: Some(10.0),
            prevent_sleep: false,
        };
        let at = |s| due(&config, Duration::from_secs(s));
        assert_eq!(
            at(60),
            IdleActions {
                display_off: false,
                sleep: false
            }
        );
        assert_eq!(
            at(120),
            IdleActions {
                display_off: true,
                sleep: false
            }
        );
        assert_eq!(
            at(600),
            IdleActions {
                display_off: true,
                sleep: true
            }
        );
        let kept_awake = PowerConfig {
            prevent_sleep: true,
            ..config
        };
        assert!(!due(&kept_awake, Duration::from_secs(3600)).sleep);
        assert_eq!(
            due(&PowerConfig::default(), Duration::from_secs(1 << 30)),
            IdleActions {
                display_off: false,
                sleep: false
            }
        );
    }
}
