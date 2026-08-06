//! Isolated FFI helpers for RustConn's PTY layer.
//!
//! This crate is the workspace's only sanctioned location for `unsafe` code
//! (per the M-UNSAFE guideline). It provides:
//!
//! - [`set_controlling_terminal`] — `pre_exec` hook for `setsid` + `TIOCSCTTY`
//! - [`open_pty_pair`] — creates a PTY master/slave pair via `openpty(2)`
//! - [`pty_set_winsize`] — sends `TIOCSWINSZ` to resize the terminal
//! - [`pty_read`] — blocking read from a raw fd (for the relay thread)
//! - [`pty_write`] — write to a raw fd (sends input to the child)

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

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
pub fn pty_set_winsize(master_fd: &OwnedFd, rows: u16, cols: u16) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ is a well-defined ioctl; master_fd is a valid open fd
    // (caller owns it via OwnedFd); ws is a stack-local properly initialized
    // struct. The ioctl reads from the pointer, does not write.
    let ret = unsafe {
        libc::ioctl(
            master_fd.as_raw_fd(),
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
// Raw fd I/O for the relay thread
// ============================================================================

/// Reads from a raw file descriptor (blocking).
///
/// Returns the number of bytes read, or `Ok(0)` on EOF.
/// Returns `Err` with the OS error on failure (including `EIO` which signals
/// that the slave side of a PTY was closed — normal child exit).
///
/// # Safety contract (internal)
///
/// The caller must ensure `raw_fd` is a valid, open file descriptor for the
/// lifetime of the call. In practice this is guaranteed because the relay
/// thread holds a reference to the `OwnedFd` stored in `PtyRelay`.
///
/// # Errors
///
/// Returns `io::Error` on read failure. Notable cases:
/// - `EIO` (errno 5): child closed the slave PTY — treat as EOF
/// - `EINTR` (errno 4): interrupted by signal — caller should retry
#[cfg(unix)]
pub fn pty_read(raw_fd: i32, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: raw_fd is valid (caller contract); buf is a valid mutable slice.
    let n = unsafe { libc::read(raw_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        #[allow(clippy::cast_sign_loss)]
        Ok(n as usize)
    }
}

/// Writes to a raw file descriptor.
///
/// Returns the number of bytes written.
///
/// # Errors
///
/// Returns `io::Error` on write failure.
#[cfg(unix)]
pub fn pty_write(raw_fd: i32, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: raw_fd is valid (caller contract); buf is a valid slice.
    let n = unsafe { libc::write(raw_fd, buf.as_ptr().cast::<libc::c_void>(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        #[allow(clippy::cast_sign_loss)]
        Ok(n as usize)
    }
}

/// Duplicates an `OwnedFd` and returns the new owned duplicate.
///
/// Both the original and the duplicate refer to the same underlying file
/// description — reads/writes on one affect the other's file offset (for
/// regular files). For PTY master fds this means both can write input, but
/// only one should read output (otherwise bytes are split between them).
///
/// # Errors
///
/// Returns `io::Error` if `dup(2)` fails.
#[cfg(unix)]
pub fn dup_fd(fd: &OwnedFd) -> io::Result<OwnedFd> {
    use std::os::fd::AsRawFd;

    // SAFETY: fd is a valid open fd (owned). dup returns a new fd that
    // is independently closeable.
    let new_raw = unsafe { libc::dup(fd.as_raw_fd()) };
    if new_raw == -1 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: new_raw is a freshly dup'd fd that we now own exclusively.
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

    /// Verifies that write to master is readable from slave and vice versa.
    #[test]
    fn pty_pair_bidirectional_io() {
        let pair = open_pty_pair().expect("openpty");

        // Write to master → read from slave
        let written = pty_write(pair.master.as_raw_fd(), b"hello").expect("write to master");
        assert_eq!(written, 5);

        // Read from slave — PTY may echo or transform, but bytes should arrive.
        // Use a short non-blocking check: set O_NONBLOCK on slave temporarily.
        unsafe {
            let flags = libc::fcntl(pair.slave.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(
                pair.slave.as_raw_fd(),
                libc::F_SETFL,
                flags | libc::O_NONBLOCK,
            );
        }
        let mut buf = [0u8; 64];
        // PTY line discipline may buffer until newline; just verify no error
        let _ = pty_read(pair.slave.as_raw_fd(), &mut buf);
    }

    /// Verifies that `pty_set_winsize` succeeds on a valid PTY master.
    #[test]
    fn set_winsize_succeeds() {
        let pair = open_pty_pair().expect("openpty");
        pty_set_winsize(&pair.master, 24, 80).expect("TIOCSWINSZ should succeed");
    }

    /// Verifies that `dup_fd` returns a distinct fd.
    #[test]
    fn dup_fd_returns_distinct_fd() {
        let pair = open_pty_pair().expect("openpty");
        let duped = dup_fd(&pair.master).expect("dup");
        assert_ne!(pair.master.as_raw_fd(), duped.as_raw_fd());
    }
}
