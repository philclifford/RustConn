//! TCP liveness settings for long-lived interactive sockets.
//!
//! Remote-desktop sessions (RDP, VNC) hold one TCP connection open for hours
//! and read from it in a blocking loop. Without kernel-level probing such a
//! socket cannot be distinguished from an idle one: when the local machine
//! suspends, the connection is silently half-open on resume, the read never
//! returns, and the session appears frozen with the last frame still on screen
//! and no error anywhere (issue #248).
//!
//! Enabling keepalive makes the kernel discover that state on its own, in both
//! directions, whatever the cause — suspend, a VPN drop, or a changed address.

use std::time::Duration;

/// Kernel-level liveness settings for an interactive remote-desktop socket.
///
/// The defaults are tuned for a session a human is watching: a freeze must be
/// reported in tens of seconds, not minutes. Servers are not affected — these
/// are local socket options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepaliveConfig {
    /// Idle time before the first keepalive probe (`TCP_KEEPIDLE`).
    pub idle: Duration,
    /// Gap between probes once probing has started (`TCP_KEEPINTVL`).
    pub interval: Duration,
    /// Unanswered probes tolerated before the connection is dropped
    /// (`TCP_KEEPCNT`).
    pub retries: u32,
    /// How long unacknowledged *data* may be retransmitted before the
    /// connection is dropped (`TCP_USER_TIMEOUT`, Linux only).
    pub user_timeout: Duration,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self::interactive()
    }
}

impl KeepaliveConfig {
    /// Settings for a session a user is watching in real time.
    ///
    /// `idle` matches the `ServerAliveInterval=15` RustConn already uses for
    /// SSH, so a dropped RDP or VNC session is noticed on the same timescale as
    /// a dropped shell. With `interval`/`retries` that puts the worst case for
    /// an idle session at 15 + 3 x 5 = 30 s.
    ///
    /// `user_timeout` covers the other half of the problem. Keepalive probes
    /// only run while the socket is idle; once there is unacknowledged data in
    /// flight — the keystrokes and clicks a user aims at a frozen picture — the
    /// kernel falls back to retransmitting it under `tcp_retries2`, which takes
    /// 13 to 30 minutes by default. 30 s keeps both paths on the same budget.
    #[must_use]
    pub const fn interactive() -> Self {
        Self {
            idle: Duration::from_secs(15),
            interval: Duration::from_secs(5),
            retries: 3,
            user_timeout: Duration::from_secs(30),
        }
    }

    /// Worst-case time to notice that an otherwise idle connection is dead.
    ///
    /// Purely informational — the kernel enforces the individual options. Used
    /// in log lines and to keep the documented behaviour honest if the
    /// constants above are ever retuned.
    #[must_use]
    pub fn idle_detection_window(&self) -> Duration {
        self.idle
            .saturating_add(self.interval.saturating_mul(self.retries))
    }
}

/// Enables keepalive probing on an established interactive connection.
///
/// Applies [`KeepaliveConfig`] to `stream` in place. `TCP_USER_TIMEOUT` is set
/// only where the platform has it (Linux and friends); everywhere else the
/// keepalive probes alone provide the liveness signal.
///
/// # Errors
/// Returns the underlying `io::Error` if the socket rejects an option. Callers
/// treat this as non-fatal: a session without keepalive still works, it just
/// takes the kernel default to notice a dead peer.
pub fn apply_interactive_keepalive(
    stream: &tokio::net::TcpStream,
    config: &KeepaliveConfig,
) -> std::io::Result<()> {
    let sock = socket2::SockRef::from(stream);

    let keepalive = socket2::TcpKeepalive::new()
        .with_time(config.idle)
        .with_interval(config.interval)
        .with_retries(config.retries);
    sock.set_tcp_keepalive(&keepalive)?;

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "fuchsia"))]
    sock.set_tcp_user_timeout(Some(config.user_timeout))?;

    Ok(())
}

/// Applies [`KeepaliveConfig::interactive`] and logs the outcome.
///
/// Convenience for the protocol clients, which all want the same preset and
/// must not fail a working connection just because a socket option did not
/// stick.
pub fn set_interactive_keepalive(stream: &tokio::net::TcpStream, protocol: &'static str) {
    let config = KeepaliveConfig::interactive();
    match apply_interactive_keepalive(stream, &config) {
        Ok(()) => {
            tracing::debug!(
                protocol,
                idle_secs = config.idle.as_secs(),
                interval_secs = config.interval.as_secs(),
                retries = config.retries,
                user_timeout_secs = config.user_timeout.as_secs(),
                detect_secs = config.idle_detection_window().as_secs(),
                "TCP keepalive enabled on session socket"
            );
        }
        Err(e) => {
            // Not fatal: the session works, it just takes the kernel default
            // (minutes) to notice a dead peer.
            tracing::warn!(
                protocol,
                error = %e,
                "Could not enable TCP keepalive; a dropped connection will take longer to detect"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_preset_detects_within_thirty_seconds() {
        let config = KeepaliveConfig::interactive();
        assert_eq!(config.idle_detection_window(), Duration::from_secs(30));
    }

    #[test]
    fn interactive_idle_matches_the_ssh_server_alive_interval() {
        // Kept in step deliberately: a dropped RDP/VNC session should surface on
        // the same timescale as a dropped SSH session.
        assert_eq!(KeepaliveConfig::interactive().idle, Duration::from_secs(15));
    }

    #[test]
    fn default_is_the_interactive_preset() {
        assert_eq!(KeepaliveConfig::default(), KeepaliveConfig::interactive());
    }

    #[test]
    fn detection_window_saturates_instead_of_overflowing() {
        let config = KeepaliveConfig {
            idle: Duration::MAX,
            interval: Duration::MAX,
            retries: u32::MAX,
            user_timeout: Duration::from_secs(1),
        };
        assert_eq!(config.idle_detection_window(), Duration::MAX);
    }

    /// Applies the options to a real loopback socket and reads back what the
    /// kernel accepted. Asserting the constants alone would not prove that the
    /// call actually reaches the socket.
    #[tokio::test]
    async fn applying_to_a_live_socket_enables_keepalive() {
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            // A sandbox without loopback networking cannot exercise this.
            return;
        };
        let Ok(addr) = listener.local_addr() else {
            return;
        };

        let accept = tokio::spawn(async move { listener.accept().await });
        let Ok(stream) = tokio::net::TcpStream::connect(addr).await else {
            return;
        };

        let config = KeepaliveConfig::interactive();
        assert!(apply_interactive_keepalive(&stream, &config).is_ok());

        let sock = socket2::SockRef::from(&stream);
        assert_eq!(sock.keepalive().ok(), Some(true), "SO_KEEPALIVE not set");

        #[cfg(any(target_os = "linux", target_os = "android", target_os = "fuchsia"))]
        assert_eq!(
            sock.tcp_user_timeout().ok(),
            Some(Some(config.user_timeout)),
            "TCP_USER_TIMEOUT not set"
        );

        accept.abort();
    }
}
