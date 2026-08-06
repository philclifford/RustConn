//! PTY relay: owns the master fd, relays output to VTE and session loggers.
//!
//! Instead of letting VTE read from the PTY master fd directly (which makes
//! it impossible to intercept the data stream), we:
//!
//! 1. Create the PTY pair ourselves (`rustconn_pty_sys::open_pty_pair`)
//! 2. Spawn the child process on the slave side
//! 3. Run a background thread that reads from the master fd
//! 4. Deliver output chunks to the GTK main thread via a GLib channel
//! 5. On the main thread: `terminal.feed(chunk)` for display, plus logging
//!
//! This solves three problems at once (issue #247):
//! - **No delay**: output is written to the log as soon as it arrives
//! - **Correct ordering**: input and output go through the same event queue
//! - **No truncation**: we see every byte from the PTY, not a VTE buffer snapshot

use std::cell::RefCell;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use gtk4::glib;
use uuid::Uuid;

/// Size of the read buffer for the relay thread.
///
/// 8 KiB matches VTE's internal buffer and keeps syscall overhead low while
/// ensuring interactive latency stays sub-millisecond.
const READ_BUF_SIZE: usize = 8192;

/// Events delivered from the relay thread to the GTK main thread.
#[derive(Debug)]
pub enum PtyEvent {
    /// A chunk of output data read from the child process.
    Output(Vec<u8>),
    /// The child process has exited (read returned 0 or EIO).
    ChildEof,
}

/// Callback type invoked on the GTK main thread for each PTY event.
///
/// The closure receives the session id and the event. It runs inside the
/// GLib main loop so it is safe to touch GTK widgets.
pub type PtyEventHandler = Box<dyn Fn(Uuid, PtyEvent)>;

/// A PTY relay that owns the master fd and drives a read thread.
///
/// Created per-session; lives as long as the terminal tab. On drop, signals
/// the relay thread to stop and joins it.
pub struct PtyRelay {
    /// Session this relay belongs to.
    session_id: Uuid,
    /// Master PTY fd (write-side duplicate) — we write input here.
    write_fd: OwnedFd,
    /// Flag signalling the read thread to exit.
    stop_flag: Arc<AtomicBool>,
    /// Join handle for the relay thread.
    thread_handle: Option<JoinHandle<()>>,
    /// Child PID (needed for kill on close, glib child watch).
    child_pid: u32,
}

impl PtyRelay {
    /// Creates a new PTY relay and starts the read thread.
    ///
    /// The `event_handler` is invoked on the GLib main thread for every output
    /// chunk and on EOF. It must not block.
    ///
    /// # Arguments
    ///
    /// * `session_id` — identifies the session this relay belongs to
    /// * `master_fd` — the master side of the PTY pair (caller transfers ownership)
    /// * `child_pid` — PID of the child process on the slave side
    /// * `event_handler` — callback for delivering events to the main thread
    pub fn new(
        session_id: Uuid,
        master_fd: OwnedFd,
        child_pid: u32,
        event_handler: PtyEventHandler,
    ) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Duplicate the master fd: original goes to the read thread,
        // duplicate stays in Self for write_input() and resize().
        let write_fd = rustconn_pty_sys::dup_fd(&master_fd)
            .expect("dup master fd for write side should not fail");

        // The read thread gets the raw fd value. The OwnedFd is moved into
        // the thread closure so it stays alive for the thread's lifetime.
        let stop = stop_flag.clone();
        let sid = session_id;

        // Channel for delivering events to the main thread.
        // async_channel is used because glib::MainContext::channel was removed
        // in newer gtk4-rs. The receiver is polled on the GLib main context
        // via spawn_future_local.
        let (tx, rx) = async_channel::unbounded::<(Uuid, PtyEvent)>();

        let thread_handle = std::thread::Builder::new()
            .name(format!("pty-relay-{}", &session_id.to_string()[..8]))
            .spawn(move || {
                Self::read_loop(master_fd, sid, &stop, tx);
                // master_fd is dropped here, closing the read side
            })
            .expect("spawning pty-relay thread should not fail");

        // Receive events on the GLib main context (main thread).
        let handler = Rc::new(event_handler);
        glib::spawn_future_local(async move {
            while let Ok((sid, event)) = rx.recv().await {
                handler(sid, event);
            }
        });

        Self {
            session_id,
            write_fd,
            stop_flag,
            thread_handle: Some(thread_handle),
            child_pid,
        }
    }

    /// Sends input data to the child process (writes to master fd).
    ///
    /// This is the equivalent of VTE's `feed_child()` — it sends keystrokes
    /// and pasted text to the child's stdin via the PTY master.
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if the write fails (e.g. child has exited).
    pub fn write_input(&self, data: &[u8]) -> io::Result<usize> {
        rustconn_pty_sys::pty_write(self.write_fd.as_raw_fd(), data)
    }

    /// Resizes the child's terminal (sends TIOCSWINSZ to the master fd).
    ///
    /// Called when the VTE widget changes size (`char-size-changed` signal).
    pub fn resize(&self, rows: u16, cols: u16) {
        if let Err(e) = rustconn_pty_sys::pty_set_winsize(&self.write_fd, rows, cols) {
            tracing::warn!(
                session_id = %self.session_id,
                %e,
                "TIOCSWINSZ failed"
            );
        }
    }

    /// Returns the child PID.
    #[must_use]
    pub const fn child_pid(&self) -> u32 {
        self.child_pid
    }

    /// Returns the session ID.
    #[must_use]
    pub const fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// The read loop running on the background thread.
    ///
    /// Reads from the PTY master fd in a loop and sends chunks through the
    /// GLib channel. Exits on EOF, EIO (normal for PTY when child exits), or
    /// when the stop flag is set.
    fn read_loop(
        master_fd: OwnedFd,
        session_id: Uuid,
        stop: &AtomicBool,
        tx: async_channel::Sender<(Uuid, PtyEvent)>,
    ) {
        let raw_fd = master_fd.as_raw_fd();
        let mut buf = [0u8; READ_BUF_SIZE];

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            match rustconn_pty_sys::pty_read(raw_fd, &mut buf) {
                Ok(0) => {
                    // EOF — child closed the slave side
                    let _ = tx.send_blocking((session_id, PtyEvent::ChildEof));
                    break;
                }
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    if tx
                        .send_blocking((session_id, PtyEvent::Output(chunk)))
                        .is_err()
                    {
                        // Receiver dropped (tab closed) — exit quietly
                        break;
                    }
                }
                Err(e) => {
                    #[expect(
                        clippy::needless_continue,
                        reason = "explicit continue aids readability of match arms in a read loop"
                    )]
                    match e.raw_os_error() {
                        Some(libc::EIO) => {
                            // Normal on Linux: master read returns EIO when child exits
                            let _ = tx.send_blocking((session_id, PtyEvent::ChildEof));
                            break;
                        }
                        Some(libc::EINTR) => {
                            // Interrupted by signal — retry
                            continue;
                        }
                        Some(libc::EAGAIN) => {
                            // Non-blocking fd (shouldn't happen, but handle gracefully)
                            std::thread::sleep(std::time::Duration::from_millis(1));
                            continue;
                        }
                        _ => {
                            tracing::error!(
                                %session_id,
                                %e,
                                "PTY relay read error"
                            );
                            let _ = tx.send_blocking((session_id, PtyEvent::ChildEof));
                            break;
                        }
                    }
                }
            }
        }
        // master_fd dropped here → closes the read side of the PTY
    }

    /// Signals the relay thread to stop and waits for it to finish.
    fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            // The thread will exit when it gets EIO/EOF from the closed master fd.
            // Give it a moment, then proceed (don't block the UI indefinitely).
            let _ = handle.join();
        }
    }
}

impl Drop for PtyRelay {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawns a child process on a new PTY and returns the relay.
///
/// This is the unified PTY spawn that replaces both VTE's `spawn_async` (Linux)
/// and the macOS-specific `spawn_native_pty`. It:
///
/// 1. Creates a PTY pair via `openpty()`
/// 2. Spawns the child with the slave as stdin/stdout/stderr
/// 3. Sets the slave as the child's controlling terminal
/// 4. Starts a relay thread on the master fd
///
/// # Arguments
///
/// * `session_id` — session identifier
/// * `argv` — command and arguments
/// * `envv` — environment variables as `KEY=VALUE` strings
/// * `working_directory` — optional cwd for the child
/// * `initial_size` — initial terminal size (rows, cols)
/// * `event_handler` — callback for output events on the main thread
///
/// # Errors
///
/// Returns a string describing the failure (PTY allocation, spawn, etc.).
pub fn spawn_with_relay(
    session_id: Uuid,
    argv: &[&str],
    envv: &[&str],
    working_directory: Option<&str>,
    initial_size: (u16, u16),
    event_handler: PtyEventHandler,
) -> Result<PtyRelay, String> {
    use std::process::{Command, Stdio};

    if argv.is_empty() {
        return Err("argv is empty".to_string());
    }

    // 1. Create PTY pair
    let pair = rustconn_pty_sys::open_pty_pair().map_err(|e| format!("openpty failed: {e}"))?;

    // Set initial terminal size
    let (rows, cols) = initial_size;
    if let Err(e) = rustconn_pty_sys::pty_set_winsize(&pair.master, rows, cols) {
        tracing::warn!(%e, "Initial TIOCSWINSZ failed (non-fatal)");
    }

    // 2. Build child process
    let mut cmd = Command::new(argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }

    if let Some(dir) = working_directory {
        cmd.current_dir(dir);
    }

    // Set environment
    cmd.env_clear();
    if envv.is_empty() {
        // No explicit env → inherit parent
        for (key, value) in std::env::vars() {
            cmd.env(&key, &value);
        }
    } else {
        for env_str in envv {
            if let Some(eq_pos) = env_str.find('=') {
                let key = &env_str[..eq_pos];
                let value = &env_str[eq_pos + 1..];
                cmd.env(key, value);
            }
        }
    }

    // Ensure TERM is set
    if !envv.iter().any(|e| e.starts_with("TERM=")) {
        cmd.env("TERM", "xterm-256color");
    }

    // 3. Connect slave fd as stdin/stdout/stderr
    let stdin_fd = rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| format!("dup stdin: {e}"))?;
    let stdout_fd =
        rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| format!("dup stdout: {e}"))?;
    let stderr_fd =
        rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| format!("dup stderr: {e}"))?;

    cmd.stdin(Stdio::from(stdin_fd));
    cmd.stdout(Stdio::from(stdout_fd));
    cmd.stderr(Stdio::from(stderr_fd));

    // 4. Set controlling terminal (setsid + TIOCSCTTY)
    rustconn_pty_sys::set_controlling_terminal(&mut cmd);

    // 5. Spawn
    let child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let child_pid = child.id();

    // Forget the Child handle — GLib's child_watch_add_local will reap via waitpid.
    std::mem::forget(child);

    tracing::info!(
        command = %argv[0],
        %session_id,
        pid = child_pid,
        "PTY relay: child spawned"
    );

    // 6. Close slave fd in parent (child has its own copies via dup'd fds)
    drop(pair.slave);

    // 7. Start the relay
    let relay = PtyRelay::new(session_id, pair.master, child_pid, event_handler);

    Ok(relay)
}

/// Spawns a child on a new PTY and gives VTE ownership for I/O.
///
/// Unlike [`spawn_with_relay`] (which intercepts output via a relay thread),
/// this function lets VTE handle both reading and writing to the PTY —
/// exactly like VTE's built-in `spawn_async`, but with our own PTY creation
/// and `set_controlling_terminal` for cross-platform consistency.
///
/// VTE's `feed_child` and `commit` signal work normally. Output observers
/// should be wired via `connect_contents_changed` on the VTE terminal.
///
/// Returns the child PID on success, or an error string on failure.
///
/// # Errors
///
/// Returns a string describing the failure.
pub fn spawn_on_vte_pty(
    session_id: Uuid,
    terminal: &vte4::Terminal,
    argv: &[&str],
    envv: &[&str],
    working_directory: Option<&str>,
) -> Result<u32, String> {
    use gtk4::gio;
    use std::process::{Command, Stdio};
    use vte4::prelude::*;

    if argv.is_empty() {
        return Err("argv is empty".to_string());
    }

    // 1. Create PTY pair
    let pair =
        rustconn_pty_sys::open_pty_pair().map_err(|e| format!("openpty failed: {e}"))?;

    // Set initial terminal size from VTE widget
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "VTE row/col counts are always small positive numbers"
    )]
    let (rows, cols) = (terminal.row_count() as u16, terminal.column_count() as u16);
    if let Err(e) = rustconn_pty_sys::pty_set_winsize(&pair.master, rows, cols) {
        tracing::warn!(%e, "Initial TIOCSWINSZ failed (non-fatal)");
    }

    // 2. Build child process
    let mut cmd = Command::new(argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }

    if let Some(dir) = working_directory {
        cmd.current_dir(dir);
    }

    // Set environment
    cmd.env_clear();
    if envv.is_empty() {
        for (key, value) in std::env::vars() {
            cmd.env(&key, &value);
        }
    } else {
        for env_str in envv {
            if let Some(eq_pos) = env_str.find('=') {
                cmd.env(&env_str[..eq_pos], &env_str[eq_pos + 1..]);
            }
        }
    }
    if !envv.iter().any(|e| e.starts_with("TERM=")) {
        cmd.env("TERM", "xterm-256color");
    }

    // 3. Connect slave fd as stdin/stdout/stderr
    let stdin_fd =
        rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| format!("dup stdin: {e}"))?;
    let stdout_fd =
        rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| format!("dup stdout: {e}"))?;
    let stderr_fd =
        rustconn_pty_sys::dup_fd(&pair.slave).map_err(|e| format!("dup stderr: {e}"))?;

    cmd.stdin(Stdio::from(stdin_fd));
    cmd.stdout(Stdio::from(stdout_fd));
    cmd.stderr(Stdio::from(stderr_fd));

    // 4. Set controlling terminal
    rustconn_pty_sys::set_controlling_terminal(&mut cmd);

    // 5. Spawn
    let child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let child_pid = child.id();
    std::mem::forget(child);

    tracing::info!(
        command = %argv[0],
        %session_id,
        pid = child_pid,
        "spawn_on_vte_pty: child spawned"
    );

    // 6. Close slave fd in parent
    drop(pair.slave);

    // 7. Give VTE the master fd — VTE takes over reading and writing
    let vte_pty = vte4::Pty::foreign_sync(pair.master, gio::Cancellable::NONE)
        .map_err(|e| format!("Pty::foreign_sync failed: {e}"))?;
    terminal.set_pty(Some(&vte_pty));

    // 8. Watch for child exit
    let terminal_weak = terminal.downgrade();
    glib::child_watch_add_local(glib::Pid(child_pid as i32), move |_pid, status| {
        if let Some(term) = terminal_weak.upgrade() {
            term.emit_by_name::<()>("child-exited", &[&status]);
        }
    });

    Ok(child_pid)
}

/// Shared relay handle stored per-session in the notebook.
///
/// Multiple GTK callbacks (input, resize, close) need access to the relay,
/// so it lives behind `Rc<RefCell<>>` like other per-session state.
pub type SharedPtyRelay = Rc<RefCell<Option<PtyRelay>>>;
