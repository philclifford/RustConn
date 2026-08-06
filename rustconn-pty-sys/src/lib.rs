//! Isolated FFI helpers for RustConn's PTY layer.
//!
//! This crate is the workspace's only sanctioned location for `unsafe` code
//! (per the M-UNSAFE guideline). It provides:
//!
//! - [`set_controlling_terminal`] — `pre_exec` hook for `setsid` + `TIOCSCTTY`
//! - [`open_pty_pair`] — creates a PTY master/slave pair via `openpty(2)`
//! - [`pty_set_winsize`] — sends `TIOCSWINSZ` to size the terminal
//! - [`pty_wait_readable`] — `poll(2)` with a timeout, so a reader thread can stop
//! - [`dup_fd`] — close-on-exec duplicate of a descriptor
//!
//! Reading and writing the descriptor itself is deliberately absent: the caller
//! turns the master into a [`std::fs::File`], so no session data passes through
//! any `unsafe` code.

use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

// ============================================================================
// Controlling terminal (macOS PTY fix, #175)
// ============================================================================

#[cfg(unix)]
mod controlling_terminal {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    /// Arranges for `cmd`'s child to acquire its standard input terminal as a
    /// controlling terminal.
    ///
    /// The child is placed in a new session via `setsid(2)` and then claims
    /// the terminal on file descriptor 0 with the `TIOCSCTTY` ioctl. This lets
    /// interactive programs (notably `ssh`) open `/dev/tty` to prompt for a
    /// password. Without it, a child of a GUI process that has no controlling
    /// terminal cannot read the password and authentication fails instantly.
    ///
    /// # Preconditions
    ///
    /// * The caller MUST connect a PTY slave to the child's stdin (fd 0)
    ///   before spawning (e.g. via [`Command::stdin`]).
    /// * The caller MUST NOT also set [`CommandExt::process_group`]: `setsid(2)`
    ///   fails with `EPERM` when the calling process is already a process-group
    ///   leader. `setsid(2)` already makes the child a session and
    ///   process-group leader, which is sufficient for job control
    ///   (`Ctrl-C` → `SIGINT` to the foreground group).
    pub fn set_controlling_terminal(cmd: &mut Command) {
        // SAFETY: the registered hook runs in the forked child, after `std`
        // has wired up the stdio descriptors and before `execvp`. It calls
        // only async-signal-safe libc functions (`setsid`, `ioctl`) and does
        // not allocate, lock, or touch shared state, satisfying the contract
        // of `CommandExt::pre_exec`.
        unsafe {
            cmd.pre_exec(|| {
                // New session: detach from any inherited controlling terminal
                // and become a session + process-group leader.
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                // Claim fd 0 (the PTY slave) as the controlling terminal.
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(unix)]
pub use controlling_terminal::set_controlling_terminal;

// ============================================================================
// PTY pair creation
// ============================================================================

/// A PTY master/slave pair.
///
/// Both file descriptors are owned; the caller must arrange for the slave to
/// be passed to the child process (via `Command::stdin`/`stdout`/`stderr`)
/// and then dropped in the parent.
pub struct PtyPair {
    /// Master side — the parent reads output and writes input here.
    pub master: OwnedFd,
    /// Slave side — connected to the child's stdio.
    pub slave: OwnedFd,
}

/// Creates a new PTY master/slave pair via `openpty(2)`.
///
/// # Errors
///
/// Returns `io::Error` if `openpty` fails (e.g. out of PTY devices).
#[cfg(unix)]
pub fn open_pty_pair() -> io::Result<PtyPair> {
    let result =
        nix::pty::openpty(None, None).map_err(|e| io::Error::other(format!("openpty: {e}")))?;
    Ok(PtyPair {
        master: result.master,
        slave: result.slave,
    })
}

// ============================================================================
// Window size (TIOCSWINSZ)
// ============================================================================

/// Sets the terminal window size on the given PTY master fd.
///
/// This triggers `SIGWINCH` in the child's process group so programs like
/// `vim`, `less`, and shells redraw at the new size.
///
/// # Errors
///
/// Returns `io::Error` if the ioctl fails.
#[cfg(unix)]
pub fn pty_set_winsize(master_fd: impl AsFd, rows: u16, cols: u16) -> io::Result<()> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ is a well-defined ioctl; the descriptor is valid for
    // the duration of the call because `AsFd` borrows it; `ws` is a stack-local
    // fully initialised struct that the ioctl only reads.
    let ret = unsafe {
        libc::ioctl(
            master_fd.as_fd().as_raw_fd(),
            libc::TIOCSWINSZ as libc::c_ulong,
            &ws as *const libc::winsize,
        )
    };
    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// ============================================================================
// Readable wait
// ============================================================================

/// Waits until the descriptor has data to read, or the timeout expires.
///
/// Returns `true` when a following read will not block. This exists so a
/// blocking reader thread can notice that it has been asked to stop: without a
/// timeout it would sit in `read` until the child exits and the PTY reports
/// `EIO`, which is too late for a session the user just closed. `POLLHUP`
/// counts as readable, because the read that follows is what turns a hangup
/// into the end of the stream.
///
/// `EINTR` is reported as "not ready" rather than as an error, since the caller
/// simply loops.
///
/// # Errors
///
/// Returns `io::Error` if `poll(2)` fails for any reason other than `EINTR`.
#[cfg(unix)]
pub fn pty_wait_readable(fd: impl AsFd, timeout: std::time::Duration) -> io::Result<bool> {
    let mut pollfd = libc::pollfd {
        fd: fd.as_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // `poll` takes whole milliseconds; a sub-millisecond timeout would busy-loop,
    // so it is rounded up to one.
    let timeout_ms = i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX);

    // SAFETY: `pollfd` is a single fully initialised struct and the length
    // passed matches; the descriptor is borrowed for the duration of the call.
    let ret = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    match ret {
        -1 => {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(err)
            }
        }
        0 => Ok(false),
        _ => Ok(pollfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0),
    }
}

// ============================================================================
// Descriptor duplication
// ============================================================================

/// Duplicates an `OwnedFd` as a close-on-exec descriptor.
///
/// Both descriptors refer to the same open file description, so either can be
/// used to talk to the same PTY. `FD_CLOEXEC` is set on the duplicate because
/// the callers hand these to `Command::stdin`/`stdout`/`stderr`: the standard
/// library `dup2`s them onto 0/1/2 in the child and leaves the originals open,
/// which would give every session's child three stray descriptors pointing at
/// its own terminal. `dup2` clears the flag on its target, so 0/1/2 survive
/// `exec` as intended while the spares do not.
///
/// # Errors
///
/// Returns `io::Error` if `fcntl(F_DUPFD_CLOEXEC)` fails.
#[cfg(unix)]
pub fn dup_fd(fd: &OwnedFd) -> io::Result<OwnedFd> {
    // SAFETY: fd is a valid open fd (owned). F_DUPFD_CLOEXEC returns the
    // lowest-numbered free descriptor above the third argument, which we now
    // own exclusively.
    let new_raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if new_raw == -1 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: new_raw is a freshly duplicated fd that we now own.
        Ok(unsafe { OwnedFd::from_raw_fd(new_raw) })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(all(unix, test))]
mod tests {
    use std::os::fd::AsRawFd;
    use std::process::{Command, Stdio};

    use super::*;

    /// Contract test (M-UNSAFE): proves the `pre_exec` hook is actually
    /// registered and runs in the forked child.
    ///
    /// With stdin redirected to `/dev/null` (never a terminal), the hook's
    /// `setsid()` succeeds but `ioctl(0, TIOCSCTTY)` fails with `ENOTTY`,
    /// returning `Err` from the closure — which makes `spawn()` fail. If the
    /// hook were not wired, `true` would spawn fine.
    #[test]
    fn pre_exec_hook_runs_and_fails_without_a_tty() {
        let mut cmd = Command::new("true");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        set_controlling_terminal(&mut cmd);

        let result = cmd.spawn();
        assert!(
            result.is_err(),
            "spawn should fail: TIOCSCTTY on a /dev/null stdin returns an error, \
             proving the pre_exec hook executed in the child",
        );
    }

    /// Verifies that `open_pty_pair` returns valid fds.
    #[test]
    fn open_pty_pair_returns_valid_fds() {
        let pair = open_pty_pair().expect("openpty should succeed");
        assert!(pair.master.as_raw_fd() >= 0);
        assert!(pair.slave.as_raw_fd() >= 0);
        assert_ne!(pair.master.as_raw_fd(), pair.slave.as_raw_fd());
    }

    /// Verifies that `pty_set_winsize` succeeds on a valid PTY master.
    #[test]
    fn set_winsize_succeeds() {
        let pair = open_pty_pair().expect("openpty");
        pty_set_winsize(&pair.master, 24, 80).expect("TIOCSWINSZ should succeed");
    }

    /// An idle PTY reports "not readable" and returns after the timeout.
    #[test]
    fn wait_readable_times_out_on_a_quiet_pty() {
        let pair = open_pty_pair().expect("openpty");
        let started = std::time::Instant::now();
        let ready = pty_wait_readable(&pair.master, std::time::Duration::from_millis(50))
            .expect("poll should succeed");
        assert!(!ready, "nothing was written, so nothing is readable");
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(40),
            "the call must actually wait, otherwise a reader thread busy-loops"
        );
    }

    /// Data written to the slave makes the master readable before the timeout.
    #[test]
    fn wait_readable_reports_pending_output() {
        use std::io::Write;

        let pair = open_pty_pair().expect("openpty");
        let mut slave = std::fs::File::from(pair.slave);
        slave.write_all(b"output\n").expect("write to slave");
        slave.flush().expect("flush");

        assert!(
            pty_wait_readable(&pair.master, std::time::Duration::from_secs(2))
                .expect("poll should succeed"),
            "the master must become readable once the child writes"
        );
    }

    /// Verifies that `dup_fd` returns a distinct fd and marks it close-on-exec.
    ///
    /// The flag is the point of the function: without it every session's child
    /// would inherit three spare descriptors pointing at its own PTY.
    #[test]
    fn dup_fd_returns_distinct_cloexec_fd() {
        let pair = open_pty_pair().expect("openpty");
        let duped = dup_fd(&pair.master).expect("dup");
        assert_ne!(pair.master.as_raw_fd(), duped.as_raw_fd());

        // SAFETY: duped is a valid open fd; F_GETFD only reads flags.
        let flags = unsafe { libc::fcntl(duped.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD should succeed");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "the duplicate must be close-on-exec"
        );
    }
}
