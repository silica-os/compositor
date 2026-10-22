//! The lock screen (ext-session-lock-v1) and the screen saver.
//!
//! The lock screen is a shell client of `ext_session_lock_v1`: it takes the lock, draws one lock
//! surface per output and unlocks (`unlock_and_destroy`) after silica-daemon has checked the
//! password (`System1.Unlock`). The compositor owns the state:
//!
//! - While the session is locked only the lock surfaces are drawn and only they get the pointer
//!   (motion included) and the keyboard; no shortcut, virtual terminal switch, gesture or hot
//!   corner runs, and no new window or layer takes the focus.
//! - The session stays locked if the locker dies: a plain black screen is drawn and nothing
//!   gets input until a locker (the shell, started again) takes the lock over.
//! - `session.lock` (the Logo menu, the shortcut, the password delay, before sleeping) locks at
//!   once and tells the shell (`session.locked` = `1`), whose locker then takes the lock; there
//!   is no command to unlock. It is refused when no shell is running to unlock again.
//!
//! The screen saver starts after `screensaver.json` `minutes` without input; the compositor tells the
//! shell with the state `screensaver.active` (the id of the saver, empty when none). Locking is
//! automatic after the delay of `lockscreen.json` counted from the moment the screen saver
//! starts or the display turns off, and before sleeping.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use smithay::input::pointer::MotionEvent;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use tracing::{info, warn};

use crate::state::State;

/// The savers the shell draws; `SaverConfig` accepts only these.
pub const SAVERS: &[&str] = &["drift", "clock", "colors"];

/// The saver used when the file names none (the first).
pub const SAVER_DEFAULT: &str = "drift";

/// What the compositor keeps of the lock.
#[derive(Default)]
pub struct LockClient {
    /// The lock object of the client that holds the lock now (it may have died).
    pub lock: Option<ExtSessionLockV1>,
    /// Its lock surfaces.
    pub surfaces: Vec<LockSurface>,
}

impl LockClient {
    /// A locker is there: it holds the lock and is connected.
    pub fn alive(&self) -> bool {
        self.lock.as_ref().is_some_and(Resource::is_alive)
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if self.lock_client.alive() {
            // Someone holds the lock: the new client is told it is `finished`.
            return;
        }
        if self.lock_client.lock.is_some() {
            // The previous locker died with the session locked: a new one takes over. The
            // outputs of the dead one's surfaces are free again.
            self.session_lock_state = crate::lock::new_manager_state(self);
            self.lock_client = LockClient::default();
        }
        self.begin_lock();
        self.lock_client.lock = Some(confirmation.ext_session_lock().clone());
        // Only the lock surfaces are drawn and get input from now on.
        confirmation.lock();
    }

    fn unlock(&mut self) {
        // Only the client that holds the lock is shell, and the protocol lets only its own
        // lock object unlock; nothing else unlocks.
        info!("session unlocked");
        self.locked = false;
        self.lock_client = LockClient::default();
        self.lock_started = None;
        self.broadcast_to_shell("session.locked".to_owned(), "0".to_owned());
        let window = self
            .locked_focus
            .take()
            .filter(|w| self.space.elements().any(|e| e == w))
            .or_else(|| self.space.elements().last().cloned());
        self.focus_window(window);
        self.refresh_pointer();
        self.wake();
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        let size = self
            .space
            .output_geometry(&self.output)
            .map(|geometry| geometry.size)
            .unwrap_or_default();
        surface.with_pending_state(|state| {
            state.size = Some((size.w.max(0) as u32, size.h.max(0) as u32).into());
        });
        self.lock_client.surfaces.push(surface);
        self.lock_focus();
        self.wake();
    }
}

/// A fresh manager state (and global): smithay never frees the outputs a dead locker
/// locked, so a new one is made for the next locker.
fn new_manager_state(state: &State) -> SessionLockManagerState {
    SessionLockManagerState::new::<State, _>(&state.display_handle, crate::shell::is_shell_client)
}

impl State {
    /// Locks the session: from now on nothing but the lock surfaces is drawn or gets input.
    fn begin_lock(&mut self) {
        if self.locked {
            return;
        }
        info!("session locked");
        self.locked = true;
        self.locked_focus = self.focused_window();
        self.gesture = None;
        self.moving = None;
        self.resizing = None;
        self.dismiss_transients();
        self.broadcast_to_shell("session.locked".to_owned(), "1".to_owned());
        self.lock_focus();
        self.wake();
    }

    /// `session.lock`: locks at once and asks the shell's locker to take the lock. Refused when
    /// no shell is running (nobody could unlock again).
    pub fn lock_session(&mut self) -> Result<(), String> {
        self.shell_managers.retain(|manager| manager.is_alive());
        if self.shell_managers.is_empty() {
            return Err("no shell is running to lock the screen".to_owned());
        }
        self.begin_lock();
        Ok(())
    }

    /// The wl_surfaces of the lock surfaces, drawn instead of everything else while locked.
    pub fn lock_wl_surfaces(&self) -> Option<Vec<WlSurface>> {
        self.locked.then(|| {
            self.lock_client
                .surfaces
                .iter()
                .filter(|s| s.alive())
                .map(|s| s.wl_surface().clone())
                .collect()
        })
    }

    /// The keyboard goes to the lock surface (to nobody if there is none): whatever had it.
    pub fn lock_focus(&mut self) {
        let focus = self.lock_wl_surfaces().and_then(|s| s.into_iter().next());
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        if keyboard.current_focus() != focus {
            keyboard.set_focus(self, focus, SERIAL_COUNTER.next_serial());
        }
    }

    /// The pointer while locked: only a lock surface (the whole screen) sees it, motion
    /// included; nothing else gets enter, leave or motion.
    pub fn locked_pointer_motion(&mut self, location: Point<f64, Logical>, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let origin = self
            .space
            .output_geometry(&self.output)
            .map(|geometry| geometry.loc)
            .unwrap_or_default();
        let under = self
            .lock_wl_surfaces()
            .and_then(|surfaces| surfaces.into_iter().next())
            .map(|surface| (surface, origin.to_f64()));
        self.cursor_override = under
            .is_none()
            .then_some(smithay::input::pointer::CursorIcon::Default);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    /// `lockscreen.passwordDelay` or `screensaver.*` changed.
    pub fn apply_lock_config(&mut self) {
        // `lockscreen.passwordDelay` -1 is never (`null` for the parser).
        let lock = self.service_settings.object("lockscreen").map(|mut map| {
            if map.get("passwordDelay").and_then(serde_json::Value::as_i64) == Some(-1) {
                map.insert("passwordDelay".to_owned(), serde_json::Value::Null);
            }
            map
        });
        self.lock_config = crate::config::LockConfig::parse(lock.as_ref());
        self.saver_config =
            crate::config::SaverConfig::parse(self.service_settings.object("screensaver").as_ref());
    }

    /// Every idle check: starts the screen saver, locks when the time has come.
    pub fn lock_check(&mut self, idle: Duration) {
        let saver_due = self
            .saver_config
            .minutes
            .is_some_and(|m| idle.as_secs_f64() >= m * 60.0);
        if saver_due && !self.saver_active {
            self.saver_active = true;
            let saver = self.saver_config.saver.clone();
            info!("screen saver {saver} after {} s idle", idle.as_secs());
            self.broadcast_to_shell("screensaver.active".to_owned(), saver);
        }
        // The password is asked for a while after the saver or the display off began.
        let began = self.saver_active || self.display_off;
        if began && self.lock_started.is_none() {
            self.lock_started = Some(Instant::now());
        }
        if self.locked {
            return;
        }
        if let (Some(delay), Some(since)) = (self.lock_config.password_delay, self.lock_started)
            && since.elapsed() >= Duration::from_secs(u64::from(delay))
            && let Err(err) = self.lock_session()
        {
            warn!("lock: {err}");
            // Not again at every check.
            self.lock_started = None;
        }
    }

    /// Any input: the saver goes away, and the count for the lock starts again.
    pub fn lock_activity(&mut self) {
        if self.saver_active {
            self.saver_active = false;
            self.broadcast_to_shell("screensaver.active".to_owned(), String::new());
        }
        if !self.locked {
            self.lock_started = None;
        }
    }

    /// Before the computer sleeps: lock if a password is asked for.
    pub fn lock_before_sleep(&mut self) {
        if self.lock_config.password_delay.is_some()
            && let Err(err) = self.lock_session()
        {
            warn!("lock before sleep: {err}");
        }
    }

    /// The lock state for `idle.state`.
    pub fn lock_info(&self) -> Value {
        json!({
            "locked": self.locked,
            "locker": self.lock_client.alive(),
            "screenSaver": self.saver_active,
            "passwordDelaySeconds": self.lock_config.password_delay,
            "screenSaverMinutes": self.saver_config.minutes,
        })
    }
}

smithay::delegate_session_lock!(State);
