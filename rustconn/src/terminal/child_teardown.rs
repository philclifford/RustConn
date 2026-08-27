//! Terminating the child process behind a session.
//!
//! Every VTE session runs its command on a PTY RustConn opened itself, with the
//! child placed in its own session and process group by
//! [`super::pty_spawn::spawn_on_pty`]. Closing the PTY master is not enough to
//! end it: `telnet` in particular ignores the `SIGHUP` that follows
//! (issue [#172](https://github.com/totoshko88/RustConn/issues/172)), so the
//! group is signalled explicitly.
//!
//! Two escalation strategies, because the two callers have different amounts of
//! main loop left. Closing a tab happens while the application keeps running, so
//! the `SIGKILL` fallback can be a GLib timeout and nothing blocks the UI.
//! Quitting has no such luxury — a `glib::timeout_add_local_once` registered on
//! the way out never fires, which is why the application-exit paths used to leave
//! `telnet` processes behind even though the per-tab path killed them
//! (issue [#304](https://github.com/totoshko88/RustConn/issues/304)).

use std::time::Duration;

use gtk4::glib;
use nix::sys::signal::{self, Signal};
use nix::unistd::{self, Pid};

/// How long a child gets to act on `SIGTERM` before `SIGKILL` follows, when the
/// wait costs nothing because the application keeps running.
const TERM_GRACE: Duration = Duration::from_millis(500);

/// The same grace on the way out, where it is paid as a blocked main thread.
///
/// Shorter than [`TERM_GRACE`] on purpose: it is added to the time the window
/// takes to disappear. It is enough for the clients this actually concerns —
/// `telnet`, `ssh` and `picocom` all terminate on `SIGTERM` in well under a
/// millisecond — and it is spent once for all sessions, not once per session.
const SHUTDOWN_TERM_GRACE: Duration = Duration::from_millis(100);

/// Sends `SIGTERM` to the child's process group and returns whether it landed.
///
/// `kill(-pid, …)` reaches the whole group, so a client that spawned helpers of
/// its own goes down with them. A failure means the group is already gone —
/// nothing to escalate to.
fn signal_group(pid: i32) -> bool {
    if signal::kill(Pid::from_raw(-pid), Signal::SIGTERM).is_ok() {
        return true;
    }
    // The group is gone, but the leader may still be around under a different
    // group (a client that called `setpgid` on itself). Cheap to cover.
    let _ = signal::kill(Pid::from_raw(pid), Signal::SIGKILL);
    false
}

/// Returns whether `pid` still leads the process group RustConn started.
///
/// The guard is against PID reuse: between the `SIGTERM` and the `SIGKILL` the
/// child may have exited and its number been handed to something unrelated,
/// which must not be killed. A zombie still reports its group, so this stays
/// true until the child is reaped — sending `SIGKILL` to a zombie is a no-op.
fn still_our_group_leader(pid: i32) -> bool {
    let probe = Pid::from_raw(pid);
    signal::kill(probe, None).is_ok()
        && unistd::getpgid(Some(probe)).is_ok_and(|pgid| pgid.as_raw() == pid)
}

/// Ends a session's child process group, escalating on the GTK main loop.
///
/// For use while the application keeps running (closing a tab, ending a single
/// session). The `SIGKILL` fallback is a [`TERM_GRACE`] timeout, so this returns
/// immediately and never blocks the UI.
pub(super) fn terminate_child_group(pid: i32) {
    if !signal_group(pid) {
        return;
    }
    glib::timeout_add_local_once(TERM_GRACE, move || {
        if still_our_group_leader(pid) {
            let _ = signal::kill(Pid::from_raw(-pid), Signal::SIGKILL);
            tracing::debug!(%pid, "session child ignored SIGTERM, sent SIGKILL");
        }
    });
}

/// Ends every listed child process group before returning.
///
/// For the application-exit paths, where a GLib timeout would never run. Every
/// group is signalled first and the grace is then waited out once for all of
/// them, so the cost is [`SHUTDOWN_TERM_GRACE`] regardless of how many sessions
/// were open — and nothing at all when there were none.
pub(super) fn terminate_child_groups_blocking(pids: &[i32]) {
    let pending: Vec<i32> = pids
        .iter()
        .copied()
        .filter(|pid| signal_group(*pid))
        .collect();
    if pending.is_empty() {
        return;
    }
    std::thread::sleep(SHUTDOWN_TERM_GRACE);
    for pid in pending {
        if still_our_group_leader(pid) {
            let _ = signal::kill(Pid::from_raw(-pid), Signal::SIGKILL);
            tracing::debug!(%pid, "session child ignored SIGTERM on shutdown, sent SIGKILL");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::pty_spawn;
    use super::*;

    /// Waits for a child to disappear, reaping it so the check is meaningful.
    ///
    /// Production code leaves the `waitpid` to a GLib child watch; a test has no
    /// main loop, so it collects the child itself. Returns whether the child was
    /// gone within the deadline.
    fn wait_gone(pid: u32) -> bool {
        let target = Pid::from_raw(pid as i32);
        for _ in 0..100 {
            match nix::sys::wait::waitpid(target, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
                Ok(nix::sys::wait::WaitStatus::StillAlive) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(_) | Err(_) => return true,
            }
        }
        false
    }

    #[test]
    fn a_child_that_ignores_sigterm_is_still_gone_after_a_blocking_teardown() {
        // The case the escalation exists for: `trap '' TERM` makes the child
        // ignore SIGTERM exactly as a misbehaving client would, so only the
        // SIGKILL can end it — and on the shutdown path there is no main loop
        // left to deliver a deferred one.
        let child =
            pty_spawn::spawn_on_pty(&["sh", "-c", "trap '' TERM; sleep 30"], &[], None, (24, 80))
                .expect("sh should spawn");

        terminate_child_groups_blocking(&[child.pid as i32]);

        assert!(
            wait_gone(child.pid),
            "the blocking teardown must not leave the child running"
        );
    }

    #[test]
    fn a_well_behaved_child_ends_on_sigterm() {
        let child = pty_spawn::spawn_on_pty(&["sleep", "30"], &[], None, (24, 80))
            .expect("sleep should spawn");

        terminate_child_groups_blocking(&[child.pid as i32]);

        assert!(wait_gone(child.pid), "SIGTERM must end an ordinary child");
    }

    #[test]
    fn tearing_down_nothing_costs_nothing() {
        // No sessions open is the common case on quit; it must not sleep.
        let start = std::time::Instant::now();
        terminate_child_groups_blocking(&[]);
        assert!(start.elapsed() < SHUTDOWN_TERM_GRACE);
    }

    #[test]
    fn a_dead_pid_is_not_waited_for() {
        // A session whose child already exited leaves a stale pid behind; the
        // grace must not be paid for it.
        let child =
            pty_spawn::spawn_on_pty(&["true"], &[], None, (24, 80)).expect("true should spawn");
        assert!(wait_gone(child.pid), "`true` should exit on its own");

        let start = std::time::Instant::now();
        terminate_child_groups_blocking(&[child.pid as i32]);
        assert!(start.elapsed() < SHUTDOWN_TERM_GRACE);
    }
}
