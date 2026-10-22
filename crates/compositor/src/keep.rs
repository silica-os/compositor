//! Shell components kept running (`--keep-shell`): started like `--spawn-shell`, and started
//! again when they exit. The file manager's desktop (#147) is one: on the reference the system
//! restarts it, and the desktop icons come back.
//!
//! A component that keeps failing is not restarted forever: after [`MAX_RESTARTS`] exits within
//! [`WINDOW`] the compositor gives up and logs it. Each restart waits a little longer than the one before
//! ([`delay`]).

use std::process::Child;
use std::time::{Duration, Instant};

use smithay::reexports::calloop::channel;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use tracing::{info, warn};

use crate::state::{Compositor, State};

/// How many exits within [`WINDOW`] are tolerated before giving up.
pub const MAX_RESTARTS: usize = 5;

/// The window over which exits are counted.
pub const WINDOW: Duration = Duration::from_secs(60);

/// The wait before restarting, given the exits still inside [`WINDOW`] (this one included):
/// 0.5 s, doubling up to 8 s; `None` once there were more than [`MAX_RESTARTS`].
pub fn delay(recent_exits: usize) -> Option<Duration> {
    if recent_exits > MAX_RESTARTS {
        return None;
    }
    let shift = u32::try_from(recent_exits.saturating_sub(1)).unwrap_or(u32::MAX);
    let millis = 500u64.saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX));
    Some(Duration::from_millis(millis.min(8000)))
}

/// A component kept running.
pub struct Kept {
    pub command: String,
    /// When it exited, the old ones dropped as they leave [`WINDOW`].
    exits: Vec<Instant>,
}

/// The kept components and the channel their watcher threads report exits on.
#[derive(Default)]
pub struct Keeper {
    kept: Vec<Kept>,
    exits: Option<channel::Sender<usize>>,
}

impl Keeper {
    /// Records an exit of component `index` at `now`; the wait before restarting it, or `None`
    /// to give up.
    pub fn exited(&mut self, index: usize, now: Instant) -> Option<Duration> {
        let kept = self.kept.get_mut(index)?;
        kept.exits.retain(|t| now.duration_since(*t) < WINDOW);
        kept.exits.push(now);
        delay(kept.exits.len())
    }
}

impl State {
    /// Starts `command` as a shell component and keeps it running.
    pub fn keep_shell(&mut self, command: &str) {
        if self.keeper.exits.is_none() {
            let (sender, receiver) = channel::channel::<usize>();
            let inserted = self.loop_handle.insert_source(
                receiver,
                |event, _, compositor: &mut Compositor| {
                    compositor.state.wake();
                    if let channel::Event::Msg(index) = event {
                        compositor.state.kept_exited(index);
                    }
                },
            );
            if let Err(err) = inserted {
                warn!("keep-shell: {err}");
                return;
            }
            self.keeper.exits = Some(sender);
        }
        let index = self.keeper.kept.len();
        self.keeper.kept.push(Kept {
            command: command.to_owned(),
            exits: Vec::new(),
        });
        self.keep_start(index);
    }

    fn keep_start(&mut self, index: usize) {
        let Some(kept) = self.keeper.kept.get(index) else {
            return;
        };
        let command = kept.command.clone();
        match self.spawn_child(&command, true) {
            Ok(child) => self.watch(index, child),
            Err(err) => {
                warn!("keep-shell: {err}");
                self.kept_exited(index);
            }
        }
    }

    /// The process is reaped on its own thread (`reap.rs`), which reports its exit on the loop.
    fn watch(&self, index: usize, child: Child) {
        let Some(sender) = self.keeper.exits.clone() else {
            return;
        };
        crate::reap::reap(child, move || {
            let _ = sender.send(index);
        });
    }

    fn kept_exited(&mut self, index: usize) {
        let Some(command) = self.keeper.kept.get(index).map(|k| k.command.clone()) else {
            return;
        };
        match self.keeper.exited(index, Instant::now()) {
            Some(wait) => {
                info!("keep-shell: {command} exited, starting it again in {wait:?}");
                let timer = Timer::from_duration(wait);
                let inserted = self.loop_handle.insert_source(
                    timer,
                    move |_, _, compositor: &mut Compositor| {
                        compositor.state.wake();
                        compositor.state.keep_start(index);
                        TimeoutAction::Drop
                    },
                );
                if let Err(err) = inserted {
                    warn!("keep-shell: {err}");
                }
            }
            None => warn!("keep-shell: {command} keeps exiting, not started again"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_back_off_then_give_up() {
        assert_eq!(delay(1), Some(Duration::from_millis(500)));
        assert_eq!(delay(2), Some(Duration::from_millis(1000)));
        assert_eq!(delay(4), Some(Duration::from_millis(4000)));
        assert_eq!(delay(5), Some(Duration::from_millis(8000)));
        assert_eq!(delay(MAX_RESTARTS + 1), None);
    }

    #[test]
    fn old_exits_leave_the_window() {
        let mut keeper = Keeper::default();
        keeper.kept.push(Kept {
            command: "x".to_owned(),
            exits: Vec::new(),
        });
        let start = Instant::now();
        for i in 0..MAX_RESTARTS {
            assert!(
                keeper
                    .exited(0, start + Duration::from_secs(i as u64))
                    .is_some()
            );
        }
        // One more within the minute: give up.
        assert_eq!(keeper.exited(0, start + Duration::from_secs(10)), None);
        // Much later the count starts again.
        assert_eq!(
            keeper.exited(0, start + Duration::from_secs(600)),
            Some(Duration::from_millis(500))
        );
    }

    #[test]
    fn an_unknown_component_is_ignored() {
        assert_eq!(Keeper::default().exited(3, Instant::now()), None);
    }
}
