//! Reaping of the processes the compositor starts: an exited child stays a zombie until its parent
//! waits for it, and a session launches many apps over its life.

use std::process::Child;

/// Waits for `child` on its own thread, so it is reaped as soon as it exits, and then runs
/// `exited` (`keep.rs` restarts from there). Waiting on the handle reaps exactly that pid, so
/// every process keeps its own exit status.
pub fn reap(mut child: Child, exited: impl FnOnce() + Send + 'static) {
    let pid = child.id();
    let spawned = std::thread::Builder::new()
        .name(format!("reap-{pid}"))
        .spawn(move || {
            let _ = child.wait();
            exited();
        });
    if let Err(err) = spawned {
        tracing::warn!("no thread to reap pid {pid}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

    /// The state letter of /proc/<pid>/stat, or None once the process is gone.
    fn state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit(") ").next()?.chars().next()
    }

    #[test]
    fn exited_child_is_reaped_not_a_zombie() {
        let child = Command::new("sh").args(["-c", "exit 3"]).spawn().unwrap();
        let pid = child.id();
        let (sender, receiver) = mpsc::channel();
        reap(child, move || sender.send(()).unwrap());
        receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_ne!(state(pid), Some('Z'), "pid {pid} is still a zombie");
        assert_eq!(state(pid), None, "pid {pid} should be gone");
    }
}
