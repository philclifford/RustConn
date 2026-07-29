//! Multicast-DNS (`*.local`) hostname fallback for sandboxed builds (issue #241).
//!
//! `.local` names are resolved by the NSS stack, normally through `nss-mdns`
//! talking to `avahi-daemon`. The Flatpak runtime carries neither: its
//! `/etc/nsswitch.conf` `hosts:` line is `files myhostname resolve … dns`, so
//! the only non-DNS resolver available inside the sandbox is `nss-resolve`
//! (systemd-resolved over the varlink socket Flatpak mounts). A host that
//! answers `.local` through Avahi therefore resolves fine for `ssh myhost.local`
//! in a terminal, but not for anything running inside the sandbox — see
//! [flatpak#4044](https://github.com/flatpak/flatpak/issues/4044).
//!
//! The workaround is to ask the *host* to resolve the name (`getent`, falling
//! back to `avahi-resolve-host-name`) through `flatpak-spawn --host`, and to
//! connect to the address instead of the name. For SSH the original name is
//! preserved as `HostKeyAlias`, so `known_hosts` entries and host-key
//! verification are unaffected by the substitution.
//!
//! Outside Flatpak this module does nothing: the platform resolver is the same
//! one the user's shell uses.

use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::models::{Connection, ProtocolConfig};

/// Wall-clock ceiling for one host-side resolution attempt.
///
/// A `.local` lookup is a multicast round trip on the local segment; Avahi
/// answers in milliseconds when the host is up. Two seconds leaves room for a slow
/// `flatpak-spawn` round trip while keeping the one-off blocking call short
/// enough to be invisible in the UI.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a resolution outcome (address or failure) stays cached.
///
/// mDNS leases are short-lived and a laptop may change address between
/// connects, so the cache exists only to keep repeated connect attempts from
/// re-spawning the helper — not as a long-term address store.
const CACHE_TTL: Duration = Duration::from_mins(1);

/// A cached resolution outcome and when it was recorded.
type CachedResolution = (Option<IpAddr>, Instant);

/// Cache of host-side resolution outcomes, including negative ones.
static CACHE: OnceLock<Mutex<HashMap<String, CachedResolution>>> = OnceLock::new();

/// Returns `true` when `host` is a multicast-DNS name (`something.local`).
///
/// The trailing dot form (`host.local.`) is accepted as well; RFC 6762 treats
/// both as the same name.
#[must_use]
pub fn is_mdns_name(host: &str) -> bool {
    let trimmed = host.trim().trim_end_matches('.');
    trimmed.len() > ".local".len()
        && trimmed
            .rsplit_once('.')
            .is_some_and(|(_, tld)| tld.eq_ignore_ascii_case("local"))
}

/// Returns `true` when the platform resolver can turn `host` into an address.
fn resolves_locally(host: &str) -> bool {
    // Port 0 is never dialled — `to_socket_addrs` is used purely as the
    // getaddrinfo wrapper, mirroring what the pre-connect probe does.
    format!("{host}:0")
        .to_socket_addrs()
        .is_ok_and(|mut addrs| addrs.next().is_some())
}

/// Runs `program args…` on the Flatpak host, bounded by [`RESOLVE_TIMEOUT`].
///
/// Returns the child's stdout on a successful exit. A child that outlives the
/// timeout is killed and reaped so it cannot leak into the session.
fn spawn_on_host_bounded(program: &str, args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut command = Command::new("flatpak-spawn");
    command.arg("--host").arg(program).args(args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + RESOLVE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    tracing::debug!(
                        program,
                        "Host-side name resolution timed out; killing helper"
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                // ponytail: 25 ms poll — the helper normally exits within one or
                // two ticks, and this path runs at most once per connect.
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }

    let mut stdout = child.stdout.take()?;
    let mut buf = String::new();
    stdout.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// Extracts the first IP address from `getent ahosts`-style output.
///
/// `getent` prints one address per line followed by the resolved name; only the
/// first whitespace-separated field of each line is an address candidate.
fn first_address_in(output: &str) -> Option<IpAddr> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .find_map(|token| token.parse::<IpAddr>().ok())
}

/// Asks the Flatpak host to resolve `host`.
fn resolve_on_flatpak_host(host: &str) -> Option<IpAddr> {
    // `getent ahosts` goes through the host's full NSS stack, which is exactly
    // the resolver the user's own `ssh myhost.local` used.
    let via_getent = spawn_on_host_bounded("getent", &["ahosts", host])
        .as_deref()
        .and_then(first_address_in);
    if let Some(ip) = via_getent {
        return Some(ip);
    }
    // A host without `nss-mdns` may still run Avahi; ask it directly. Avahi
    // prints "<name>\t<address>", which `first_address_in` also accepts.
    spawn_on_host_bounded("avahi-resolve-host-name", &["-4", host])
        .as_deref()
        .and_then(|out| {
            out.split_whitespace()
                .find_map(|token| token.parse::<IpAddr>().ok())
        })
}

/// Resolves a hostname the sandbox resolver cannot see, if the host can.
///
/// Returns `None` — leaving the caller to use the name unchanged — when the
/// name is an address literal, resolves inside the sandbox, is not a `.local`
/// name, or the process is not sandboxed by Flatpak.
///
/// Outcomes are cached for [`CACHE_TTL`], negative ones included, so repeated
/// connect attempts do not re-spawn the helper.
#[must_use]
pub fn resolve_sandboxed_hostname(host: &str) -> Option<IpAddr> {
    let host = host.trim();
    if host.is_empty() || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    // Limited to `.local` on purpose: a public name that fails to resolve is a
    // real error the client should report, not something to work around.
    if !is_mdns_name(host) || !crate::flatpak::is_flatpak() {
        return None;
    }

    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = cache.lock()
        && let Some((cached, at)) = map.get(host)
        && at.elapsed() < CACHE_TTL
    {
        return *cached;
    }

    if resolves_locally(host) {
        // The sandbox can resolve it after all (systemd-resolved with mDNS
        // enabled) — no substitution needed.
        if let Ok(mut map) = cache.lock() {
            map.insert(host.to_string(), (None, Instant::now()));
        }
        return None;
    }

    let resolved = resolve_on_flatpak_host(host);
    if let Some(ip) = resolved {
        tracing::info!(
            %host,
            address = %ip,
            "Resolved .local name on the Flatpak host; connecting by address"
        );
    } else {
        tracing::warn!(
            %host,
            "Could not resolve .local name inside the sandbox or on the host"
        );
    }
    if let Ok(mut map) = cache.lock() {
        map.insert(host.to_string(), (resolved, Instant::now()));
    }
    resolved
}

/// Rewrites `conn` to connect by address when its `.local` host only resolves
/// on the Flatpak host.
///
/// Returns the substituted address, or `None` when the connection was left
/// untouched. For SSH-family protocols the original hostname is kept as
/// `HostKeyAlias` so `known_hosts` and host-key verification still key on the
/// name the user configured; an explicit user-set `HostKeyAlias` is preserved.
pub fn apply_mdns_fallback(conn: &mut Connection) -> Option<IpAddr> {
    let ip = resolve_sandboxed_hostname(&conn.host)?;
    let original = std::mem::replace(&mut conn.host, ip.to_string());

    if let ProtocolConfig::Ssh(ref mut ssh) | ProtocolConfig::Sftp(ref mut ssh) =
        conn.protocol_config
        && !ssh
            .custom_options
            .keys()
            .any(|k| k.eq_ignore_ascii_case("HostKeyAlias"))
    {
        ssh.custom_options
            .insert("HostKeyAlias".to_string(), original);
    }

    Some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdns_names_are_recognised() {
        assert!(is_mdns_name("myhost.local"));
        assert!(is_mdns_name("MyHost.LOCAL"));
        assert!(is_mdns_name("myhost.local."));
        assert!(is_mdns_name("a.b.local"));
    }

    #[test]
    fn non_mdns_names_are_rejected() {
        assert!(!is_mdns_name("example.com"));
        assert!(!is_mdns_name("localhost"));
        assert!(!is_mdns_name(".local"));
        assert!(!is_mdns_name("local"));
        assert!(!is_mdns_name(""));
    }

    #[test]
    fn address_literals_are_never_substituted() {
        assert!(resolve_sandboxed_hostname("192.168.1.10").is_none());
        assert!(resolve_sandboxed_hostname("::1").is_none());
    }

    #[test]
    fn public_names_are_never_substituted() {
        // Not a `.local` name — a resolution failure there is the client's to
        // report, so no host-side lookup is attempted.
        assert!(resolve_sandboxed_hostname("host.invalid").is_none());
    }

    #[test]
    fn getent_output_is_parsed() {
        let output = "192.168.1.42   myhost.local\n192.168.1.42   myhost.local\n";
        assert_eq!(
            first_address_in(output),
            Some("192.168.1.42".parse::<IpAddr>().expect("valid literal"))
        );
    }

    #[test]
    fn empty_output_yields_no_address() {
        assert!(first_address_in("").is_none());
        assert!(first_address_in("no addresses here\n").is_none());
    }
}
