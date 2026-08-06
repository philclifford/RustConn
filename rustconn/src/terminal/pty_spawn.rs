//! Starts a session's command on a pseudo-terminal that RustConn owns.
//!
//! RustConn used to let VTE create and own the PTY (and, on macOS, worked
//! around VTE's broken `spawn_async` with a near-copy of this code). It now
//! creates the PTY itself on every platform and keeps the master descriptor,
//! because the session transcript has to be a copy of what the child actually
//! wrote. Reconstructing that from the widget is not possible: VTE rewraps its
//! buffer when the window is resized and renumbers the rows underneath the
//! reader — see the contract tests in this module's parent — so a scraped
//! transcript both repeats and skips lines. Issue
//! [#247](https://github.com/totoshko88/RustConn/issues/247).
//!
//! What VTE keeps is everything it is good at: rendering, key handling,
//! selection, scrollback. What it loses is the descriptor, which means the
//! caller becomes responsible for three things it used to do implicitly —
//! feeding output in, forwarding `commit` back out, and pushing the window size
//! down. [`super::pty_relay`] carries all three.
//!
//! This module deliberately touches no GTK, so it can be tested with a real
//! child process and no display.

use std::io;
use std::os::fd::OwnedFd;
use std::process::{Command, Stdio};

/// A child process running on a PTY whose master end the caller owns.
#[derive(Debug)]
pub struct SpawnedChild {
    /// Process id, needed to signal the process group when the tab closes
    /// (issue [#172](https://github.com/totoshko88/RustConn/issues/172)).
    pub pid: u32,
    /// Master end of the PTY: the child's output arrives here and its input
    /// goes back the same way.
    pub master: OwnedFd,
}

/// Why a session's command could not be started.
///
/// The distinction exists because the two cases need different words in the
/// UI: a missing CLI tool is something the user can install, everything else
/// is a machine-level failure they can only be told about.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// The executable is not installed, or not on the session's `PATH`.
    #[error("command not found")]
    NotFound,
    /// PTY allocation, descriptor duplication or `exec` failed.
    #[error("{0}")]
    Failed(String),
}

impl SpawnError {
    /// Returns whether the failure was a missing executable.
    #[must_use]
    pub const fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }

    fn failed(context: &str, error: impl std::fmt::Display) -> Self {
        Self::Failed(format!("{context}: {error}"))
    }
}

/// Starts `argv` on a fresh PTY and returns the child and the master end.
///
/// `envv` holds the complete `KEY=VALUE` environment for the child; an empty
/// slice inherits the parent's. `size` is `(rows, columns)` and is applied
/// before `exec`, so a program that reads its geometry once at startup (`mc`,
/// `vim`, `less`) lays itself out correctly instead of correcting itself on the
/// first `SIGWINCH`.
///
/// The child is placed in its own session with the PTY slave as its controlling
/// terminal, which is what lets `ssh` open `/dev/tty` to prompt for a password
/// (issue [#175](https://github.com/totoshko88/RustConn/issues/175)).
///
/// # Errors
///
/// Returns [`SpawnError::NotFound`] when `argv[0]` cannot be found, and
/// [`SpawnError::Failed`] for any other failure.
pub fn spawn_on_pty(
    argv: &[&str],
    envv: &[&str],
    working_directory: Option<&str>,
    size: (u16, u16),
) -> Result<SpawnedChild, SpawnError> {
    let Some(program) = argv.first() else {
        return Err(SpawnError::Failed("argv is empty".to_owned()));
    };

    let pair = rustconn_pty_sys::open_pty_pair().map_err(|e| SpawnError::failed("openpty", e))?;

    let (rows, cols) = size;
    if let Err(e) = rustconn_pty_sys::pty_set_winsize(&pair.master, rows, cols) {
        // A child that starts at the wrong size corrects itself on the first
        // resize, so this is not worth failing the connection over.
        tracing::warn!(%e, "Initial TIOCSWINSZ failed (non-fatal)");
    }

    let mut cmd = Command::new(program);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    if let Some(dir) = working_directory {
        cmd.current_dir(dir);
    }

    cmd.env_clear();
    if envv.is_empty() {
        cmd.envs(std::env::vars());
    } else {
        for entry in envv {
            if let Some((key, value)) = entry.split_once('=') {
                cmd.env(key, value);
            }
        }
    }

    // The slave is duplicated three times rather than passed once, because
    // `Stdio::from` consumes the descriptor. `dup_fd` sets `FD_CLOEXEC`, so the
    // spares are dropped by `exec` instead of lingering in the child next to
    // the copies `dup2`'d onto 0, 1 and 2.
    let stdin_fd =
        rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| SpawnError::failed("dup stdin", e))?;
    let stdout_fd =
        rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| SpawnError::failed("dup stdout", e))?;
    let stderr_fd =
        rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| SpawnError::failed("dup stderr", e))?;
    cmd.stdin(Stdio::from(stdin_fd));
    cmd.stdout(Stdio::from(stdout_fd));
    cmd.stderr(Stdio::from(stderr_fd));

    rustconn_pty_sys::set_controlling_terminal(&mut cmd);

    let child = cmd.spawn().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            SpawnError::NotFound
        } else {
            SpawnError::failed("spawn", e)
        }
    })?;
    let pid = child.id();
    // The caller installs a GLib child watch, which owns the `waitpid`; letting
    // this handle drop would run a second one and the two would race for the
    // exit status.
    std::mem::forget(child);

    // The child holds its own duplicates, so the parent's copy is only keeping
    // the PTY from reporting end-of-file when the child exits.
    drop(pair.slave);

    tracing::info!(command = %program, pid, "Session command spawned on its own PTY");

    Ok(SpawnedChild {
        pid,
        master: pair.master,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    /// Reaps a child the test started, so it does not linger as a zombie.
    ///
    /// Production code leaves this to a GLib child watch; a test has no main
    /// loop, so it waits directly.
    fn reap(pid: u32) {
        let pid = nix::unistd::Pid::from_raw(pid as i32);
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        let _ = nix::sys::wait::waitpid(pid, None);
    }

    #[test]
    fn output_of_the_child_arrives_on_the_master() {
        let child = spawn_on_pty(&["echo", "hello-from-child"], &[], None, (24, 80))
            .expect("echo should spawn");
        let mut master = std::fs::File::from(child.master);

        let mut seen = String::new();
        let mut buf = [0_u8; 256];
        // The PTY reports EIO rather than EOF once the child has gone, which is
        // the read loop's normal exit condition too.
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if seen.contains("hello-from-child") {
                break;
            }
        }
        reap(child.pid);
        assert!(seen.contains("hello-from-child"), "got {seen:?}");
    }

    #[test]
    fn the_child_starts_at_the_requested_size() {
        // `stty size` reports the terminal geometry the child sees, which is
        // the point of sizing the PTY before exec.
        let child =
            spawn_on_pty(&["stty", "size"], &[], None, (31, 101)).expect("stty should spawn");
        let mut master = std::fs::File::from(child.master);

        let mut seen = String::new();
        let mut buf = [0_u8; 128];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if seen.contains('\n') {
                break;
            }
        }
        reap(child.pid);
        assert!(
            seen.contains("31 101"),
            "the child must see 31 rows by 101 columns, got {seen:?}"
        );
    }

    #[test]
    fn the_child_gets_a_controlling_terminal() {
        // Without setsid + TIOCSCTTY the child cannot open /dev/tty, which is
        // exactly how ssh reads a password (#175).
        let child = spawn_on_pty(&["sh", "-c", "echo tty-ok > /dev/tty"], &[], None, (24, 80))
            .expect("sh should spawn");
        let mut master = std::fs::File::from(child.master);

        let mut seen = String::new();
        let mut buf = [0_u8; 128];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if seen.contains("tty-ok") {
                break;
            }
        }
        reap(child.pid);
        assert!(
            seen.contains("tty-ok"),
            "the child must be able to write to /dev/tty, got {seen:?}"
        );
    }

    #[test]
    fn a_missing_command_is_reported_as_not_found() {
        let err = spawn_on_pty(&["rustconn-no-such-binary-xyz"], &[], None, (24, 80))
            .expect_err("a missing binary must fail");
        assert!(
            err.is_not_found(),
            "a missing binary must be distinguishable so the UI can say \
             'not installed' instead of an errno: {err:?}"
        );
    }

    #[test]
    fn an_empty_argv_is_rejected_without_touching_the_system() {
        let err = spawn_on_pty(&[], &[], None, (24, 80)).expect_err("empty argv must fail");
        assert!(!err.is_not_found());
    }

    #[test]
    fn the_environment_given_is_the_environment_the_child_sees() {
        let child = spawn_on_pty(
            &["sh", "-c", "printf %s \"$RUSTCONN_TEST_VAR\""],
            &["PATH=/usr/bin:/bin", "RUSTCONN_TEST_VAR=relayed"],
            None,
            (24, 80),
        )
        .expect("sh should spawn");
        let mut master = std::fs::File::from(child.master);

        let mut seen = String::new();
        let mut buf = [0_u8; 128];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if seen.contains("relayed") {
                break;
            }
        }
        reap(child.pid);
        assert!(seen.contains("relayed"), "got {seen:?}");
    }
}
