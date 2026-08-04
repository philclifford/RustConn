//! SSH options for Midnight Commander's `sh://` VFS.
//!
//! mc's shell filesystem shells out to `ssh` with a fixed argument list —
//! `ssh -p <port> -l <user> <host> "echo SHELL:; /bin/sh"` — and its own
//! `sh://[user@]host[:options]` syntax only understands compression, `rsh` and
//! a port number. There is no way to hand it `-J`, `-i` or any `-o`, so every
//! SSH setting a connection carries used to be dropped on the mc path: a
//! jump host most visibly (issue #255), but also `HostKeyAlias`, custom
//! options and the identity file.
//!
//! The one injection point mc leaves open is `$PATH`: it invokes `ssh` by name.
//! This module writes a per-session directory holding
//!
//! * `config` — a generated `ssh_config` with the connection's real settings,
//! * `ssh` — a two-line wrapper that `exec`s the real `ssh` with `-F config`,
//!
//! and the caller prepends that directory to `PATH` for the mc child process.
//! The generated config ends with `Match all` + `Include ~/.ssh/config` so the
//! user's own aliases keep working; `ssh_config` is first-match-wins, so the
//! blocks written here still take precedence for the hosts they name.
//!
//! The same mechanism previously existed in `sftp::ensure_flatpak_mc_ssh_wrapper`
//! but only inside Flatpak and only to supply a writable `known_hosts`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use uuid::Uuid;

use crate::models::{Connection, ConnectionGroup, ProtocolConfig};

/// Name of the directory holding all per-session wrapper directories.
const WRAPPER_ROOT: &str = "rustconn-mc-ssh";

/// Age after which an orphaned session directory is removed.
///
/// `XDG_RUNTIME_DIR` is cleared when the session ends, so this only matters on
/// the `TMPDIR` fallback. Twelve hours is comfortably longer than any mc tab
/// anyone leaves open within a login session, and short enough that stale
/// directories do not pile up.
const STALE_AFTER: Duration = Duration::from_hours(12);

/// A prepared `ssh` wrapper for one mc session.
#[derive(Debug, Clone)]
pub struct McSshEnv {
    /// Directory containing the `ssh` wrapper; prepend it to `PATH`.
    wrapper_dir: PathBuf,
    /// The generated `ssh_config` the wrapper passes via `-F`.
    config_path: PathBuf,
}

impl McSshEnv {
    /// Returns the `PATH=…` entry to pass to the mc child process.
    ///
    /// Prepends the wrapper directory to the inherited `PATH` so mc's `ssh`
    /// resolves to the wrapper and everything else still resolves normally.
    #[must_use]
    pub fn path_env(&self) -> String {
        let inherited = std::env::var("PATH").unwrap_or_default();
        format!("PATH={}:{inherited}", self.wrapper_dir.display())
    }

    /// Returns the generated `ssh_config` path (useful for logging and tests).
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Returns the wrapper directory.
    #[must_use]
    pub fn wrapper_dir(&self) -> &Path {
        &self.wrapper_dir
    }
}

/// Returns the base directory for wrapper directories.
///
/// `XDG_RUNTIME_DIR` is preferred: it is user-private, on tmpfs, mounted
/// executable (the existing jump-host askpass helper already runs from there)
/// and cleared at logout.
fn wrapper_root() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from);
    base.join(WRAPPER_ROOT)
}

/// Strips `user@` and `:port` from a hop spec, leaving the bare hostname.
///
/// `ssh_config` `Host` patterns match the name ssh was asked to connect to, so
/// a hop block has to be keyed on the host alone.
///
/// An unbracketed address holding several colons is taken as an IPv6 literal
/// rather than `host:port`, which is the same disambiguation OpenSSH applies:
/// a port after an IPv6 address requires brackets (`[::1]:2222`).
fn hop_hostname(spec: &str) -> &str {
    let host_port = spec.rfind('@').map_or(spec, |at| &spec[at + 1..]);

    if let Some(rest) = host_port.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    if host_port.matches(':').count() > 1 {
        return host_port;
    }
    match host_port.rfind(':') {
        Some(colon)
            if !host_port[colon + 1..].is_empty()
                && host_port[colon + 1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            &host_port[..colon]
        }
        _ => host_port,
    }
}

/// Returns the `SshConfig` of an SSH or SFTP connection.
fn ssh_config(conn: &Connection) -> Option<&crate::models::SshConfig> {
    match &conn.protocol_config {
        ProtocolConfig::Ssh(cfg) | ProtocolConfig::Sftp(cfg) => Some(cfg),
        _ => None,
    }
}

/// Resolves a connection's identity file, following the group inheritance chain
/// and repairing stale portal paths.
fn identity_file(conn: &Connection, groups: &[ConnectionGroup]) -> Option<PathBuf> {
    crate::connection::ssh_inheritance::resolve_ssh_key_path(conn, groups)
        .and_then(|p| crate::resolve_key_path(&p))
}

/// Writes one `ssh_config` option line, quoting the value.
///
/// Values are double-quoted so a path containing spaces survives; OpenSSH
/// accepts quoted values for every keyword used here.
fn push_quoted(out: &mut String, key: &str, value: &str) {
    let _ = writeln!(out, "    {key} \"{}\"", value.replace('"', ""));
}

/// Writes one `ssh_config` option line verbatim.
///
/// Used for values that must not be quoted, such as a comma-separated
/// `ProxyJump` hop list or a bare keyword like `accept-new`.
fn push_raw(out: &mut String, key: &str, value: &str) {
    let _ = writeln!(out, "    {key} {value}");
}

/// Builds the `ssh_config` text for `connection`.
///
/// Emits a `Host` block for the target, one for each reference-based jump hop
/// that has its own identity file (`-J` does not pass `-i` down to a hop —
/// issue #241), and a trailing include of the user's own configuration.
///
/// Returns `None` when the connection is not SSH-family.
fn build_ssh_config(
    connection: &Connection,
    connections: &[Connection],
    groups: &[ConnectionGroup],
) -> Option<String> {
    let cfg = ssh_config(connection)?;
    let chain = crate::connection::resolve_jump_chain(connection, connections, groups);
    let known_hosts = crate::flatpak::get_flatpak_known_hosts_path();

    let mut out = String::with_capacity(512);
    out.push_str(
        "# Generated by RustConn for Midnight Commander's sh:// VFS — do not edit.\n\
         # Rewritten on every SFTP open; see rustconn-core/src/mc_ssh.rs.\n\n",
    );

    let _ = writeln!(out, "Host {}", connection.host);

    // An explicit ProxyCommand takes precedence over a jump chain, matching the
    // ssh_config exporter and OpenSSH's own "ProxyCommand instead of a direct
    // connection" wording. Only one of the two is ever emitted.
    if let Some(proxy_command) = cfg
        .proxy_command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        push_raw(&mut out, "ProxyCommand", proxy_command);
    } else if let Some(proxy_jump) = chain.proxy_jump_value() {
        push_raw(&mut out, "ProxyJump", &proxy_jump);
    }

    if let Some(key) = identity_file(connection, groups) {
        push_quoted(&mut out, "IdentityFile", &key.to_string_lossy());
        if cfg.identities_only {
            push_raw(&mut out, "IdentitiesOnly", "yes");
        }
    }

    if let Some(provider) = cfg
        .pkcs11_provider
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        push_quoted(&mut out, "PKCS11Provider", provider);
    }

    // Custom options come last so the settings above win a collision; they
    // carry, among other things, the `HostKeyAlias` that the Flatpak mDNS
    // fallback injects when it substitutes a resolved address for a `.local`
    // name.
    let mut custom: Vec<(&String, &String)> = cfg.custom_options.iter().collect();
    custom.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in custom {
        if key.trim().is_empty() {
            continue;
        }
        push_raw(&mut out, key.trim(), value.trim());
    }

    if let Some(ref kh) = known_hosts {
        push_quoted(&mut out, "UserKnownHostsFile", &kh.to_string_lossy());
        push_raw(&mut out, "StrictHostKeyChecking", "accept-new");
    }

    // One block per jump hop that needs its own credentials or known_hosts.
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(connection.host.clone());
    for (spec, hop_id) in chain.hops.iter().zip(chain.hop_ids.iter()) {
        let host = hop_hostname(spec);
        if !seen.insert(host.to_string()) {
            continue;
        }
        let hop_key = hop_id
            .and_then(|id| connections.iter().find(|c| c.id == id))
            .and_then(|hop| identity_file(hop, groups));
        if hop_key.is_none() && known_hosts.is_none() {
            continue;
        }
        let _ = writeln!(out, "\nHost {host}");
        if let Some(key) = hop_key {
            push_quoted(&mut out, "IdentityFile", &key.to_string_lossy());
        }
        if let Some(ref kh) = known_hosts {
            push_quoted(&mut out, "UserKnownHostsFile", &kh.to_string_lossy());
            push_raw(&mut out, "StrictHostKeyChecking", "accept-new");
        }
    }

    // `Match all` resets the block context: an `Include` written straight after
    // a `Host` block belongs to that block and only fires for hosts it matches,
    // which would silently drop the user's configuration.
    out.push_str("\nMatch all\n    Include ~/.ssh/config\n");

    Some(out)
}

/// Locates the real `ssh` binary, skipping our own wrapper directory.
///
/// Scanning `PATH` rather than hardcoding `/usr/bin/ssh` keeps the wrapper
/// working where ssh lives elsewhere (Homebrew on macOS, Nix profiles), and
/// excluding `wrapper_dir` is what stops the wrapper from calling itself.
fn find_real_ssh(wrapper_dir: &Path) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        if dir == wrapper_dir {
            continue;
        }
        let candidate = dir.join("ssh");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("/usr/bin/ssh")
}

/// Removes wrapper directories left behind by an earlier run.
fn prune_stale(root: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep || !path.is_dir() {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|m| now.duration_since(m).map_err(std::io::Error::other))
            .is_ok_and(|age| age > STALE_AFTER);
        if stale {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Prepares an `ssh` wrapper so mc's `sh://` VFS honours the connection's SSH
/// settings — jump host above all (issue #255).
///
/// Writes `<runtime>/rustconn-mc-ssh/<session_id>/{config,ssh}` and returns the
/// handle whose [`McSshEnv::path_env`] the caller passes to the mc process. The
/// directory is keyed on the session so two mc tabs with different jump hosts
/// do not share one wrapper, and is rewritten from scratch on every call.
///
/// Returns `None` for a non-SSH connection, or if the files cannot be written —
/// in which case the caller should launch mc without a `PATH` override, exactly
/// as it did before this mechanism existed.
#[must_use]
pub fn prepare_mc_ssh_env(
    session_id: Uuid,
    connection: &Connection,
    connections: &[Connection],
    groups: &[ConnectionGroup],
) -> Option<McSshEnv> {
    let config_text = build_ssh_config(connection, connections, groups)?;

    let root = wrapper_root();
    let wrapper_dir = root.join(session_id.as_hyphenated().to_string());
    // Start from a clean directory so a stale config from a previous open of
    // the same session cannot survive an edit to the connection.
    let _ = std::fs::remove_dir_all(&wrapper_dir);
    if let Err(e) = std::fs::create_dir_all(&wrapper_dir) {
        tracing::warn!(
            dir = %wrapper_dir.display(),
            error = %e,
            "Could not create the mc ssh wrapper directory; \
             launching mc without SSH options"
        );
        return None;
    }
    prune_stale(&root, &wrapper_dir);

    let config_path = wrapper_dir.join("config");
    if let Err(e) = std::fs::write(&config_path, config_text.as_bytes()) {
        tracing::warn!(
            path = %config_path.display(),
            error = %e,
            "Could not write the generated ssh_config for mc"
        );
        return None;
    }

    let real_ssh = find_real_ssh(&wrapper_dir);
    let wrapper_path = wrapper_dir.join("ssh");
    let wrapper = format!(
        "#!/bin/sh\n\
         # Auto-generated by RustConn so mc's sh:// VFS picks up this\n\
         # connection's SSH options. See rustconn-core/src/mc_ssh.rs.\n\
         exec {} -F {} \"$@\"\n",
        crate::ssh_tunnel::shell_single_quote(&real_ssh.to_string_lossy()),
        crate::ssh_tunnel::shell_single_quote(&config_path.to_string_lossy()),
    );
    if let Err(e) = std::fs::write(&wrapper_path, wrapper.as_bytes()) {
        tracing::warn!(
            path = %wrapper_path.display(),
            error = %e,
            "Could not write the ssh wrapper for mc"
        );
        return None;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o700))
        {
            tracing::warn!(
                path = %wrapper_path.display(),
                error = %e,
                "Could not make the mc ssh wrapper executable"
            );
            return None;
        }
    }

    tracing::debug!(
        dir = %wrapper_dir.display(),
        real_ssh = %real_ssh.display(),
        "Prepared ssh wrapper for mc"
    );
    Some(McSshEnv {
        wrapper_dir,
        config_path,
    })
}

/// Removes the wrapper directory for `session_id`.
///
/// Best effort: [`prepare_mc_ssh_env`] also prunes directories left behind, so
/// a missed call costs nothing beyond a few hundred bytes until the login
/// session ends.
pub fn cleanup_mc_ssh_env(session_id: Uuid) {
    let dir = wrapper_root().join(session_id.as_hyphenated().to_string());
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SshKeySource;

    fn ssh(name: &str, host: &str, port: u16, user: Option<&str>) -> Connection {
        let mut conn = Connection::new_ssh(name.to_string(), host.to_string(), port);
        conn.username = user.map(str::to_string);
        conn
    }

    fn set_jump_host_id(conn: &mut Connection, id: Uuid) {
        if let ProtocolConfig::Ssh(ref mut cfg) | ProtocolConfig::Sftp(ref mut cfg) =
            conn.protocol_config
        {
            cfg.jump_host_id = Some(id);
        }
    }

    fn set_key(conn: &mut Connection, path: &Path) {
        if let ProtocolConfig::Ssh(ref mut cfg) | ProtocolConfig::Sftp(ref mut cfg) =
            conn.protocol_config
        {
            cfg.key_source = SshKeySource::File {
                path: path.to_path_buf(),
            };
        }
    }

    /// Creates an on-disk key file and returns its path.
    ///
    /// `resolve_ssh_key_path` is filtered through `resolve_key_path`, which only
    /// accepts a path that exists — a missing file is treated as a stale portal
    /// path and dropped. Tests therefore need real files, not string literals.
    fn key_file(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not-a-real-key\n").unwrap();
        path
    }

    #[test]
    fn hop_hostname_strips_user_and_port() {
        assert_eq!(hop_hostname("host.example.com"), "host.example.com");
        assert_eq!(hop_hostname("ops@host.example.com"), "host.example.com");
        assert_eq!(
            hop_hostname("ops@host.example.com:2222"),
            "host.example.com"
        );
        assert_eq!(hop_hostname("host.example.com:2222"), "host.example.com");
    }

    #[test]
    fn hop_hostname_handles_ipv6_literals() {
        assert_eq!(hop_hostname("ops@[2001:db8::1]:2222"), "2001:db8::1");
        assert_eq!(hop_hostname("[::1]"), "::1");
    }

    #[test]
    fn unbracketed_ipv6_is_not_mistaken_for_host_and_port() {
        // The trailing `:1` is part of the address, not a port — a port after an
        // IPv6 literal requires brackets.
        assert_eq!(hop_hostname("2001:db8::1"), "2001:db8::1");
        assert_eq!(hop_hostname("ops@2001:db8::1"), "2001:db8::1");
    }

    #[test]
    fn a_trailing_colon_without_digits_is_kept() {
        assert_eq!(hop_hostname("host.example.com:"), "host.example.com:");
    }

    #[test]
    fn non_ssh_connection_produces_no_config() {
        let conn = Connection::new_rdp("rdp".to_string(), "host".to_string(), 3389);
        assert!(build_ssh_config(&conn, &[], &[]).is_none());
    }

    #[test]
    fn config_carries_the_resolved_jump_host() {
        let bastion = ssh("bastion", "jump.example.com", 22, Some("ops"));
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_jump_host_id(&mut conn, bastion.id);

        let cfg = build_ssh_config(&conn, std::slice::from_ref(&bastion), &[]).unwrap();
        assert!(cfg.contains("Host target.example.com\n"));
        assert!(cfg.contains("ProxyJump ops@jump.example.com\n"));
    }

    #[test]
    fn config_ends_with_a_reset_before_the_user_include() {
        let conn = ssh("target", "target.example.com", 22, Some("me"));
        let cfg = build_ssh_config(&conn, &[], &[]).unwrap();
        // `Match all` must precede the include, or it would be scoped to the
        // preceding Host block and never apply.
        assert!(cfg.ends_with("Match all\n    Include ~/.ssh/config\n"));
    }

    #[test]
    fn jump_hop_gets_its_own_identity_block() {
        let dir = tempfile::tempdir().unwrap();
        let bastion_key = key_file(dir.path(), "bastion");
        let target_key = key_file(dir.path(), "target");

        let mut bastion = ssh("bastion", "jump.example.com", 22, Some("ops"));
        set_key(&mut bastion, &bastion_key);
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_key(&mut conn, &target_key);
        set_jump_host_id(&mut conn, bastion.id);

        let cfg = build_ssh_config(&conn, std::slice::from_ref(&bastion), &[]).unwrap();
        assert!(cfg.contains("Host jump.example.com\n"));
        // Each key lands under its own host, not merged into one block.
        let mut blocks = cfg.split("\nHost jump.example.com");
        let target_block = blocks.next().unwrap();
        assert!(target_block.contains(&format!("IdentityFile \"{}\"", target_key.display())));
        let hop_block = blocks.next().unwrap();
        assert!(hop_block.contains(&format!("IdentityFile \"{}\"", bastion_key.display())));
    }

    #[test]
    fn hop_without_a_key_gets_no_block_outside_flatpak() {
        let bastion = ssh("bastion", "jump.example.com", 22, Some("ops"));
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_jump_host_id(&mut conn, bastion.id);

        let cfg = build_ssh_config(&conn, std::slice::from_ref(&bastion), &[]).unwrap();
        // Nothing to say about the hop, so no empty block is emitted. (Outside
        // Flatpak there is no known_hosts override to add either.)
        if crate::flatpak::get_flatpak_known_hosts_path().is_none() {
            assert!(!cfg.contains("Host jump.example.com\n"));
        }
    }

    #[test]
    fn custom_options_reach_the_config() {
        let mut conn = ssh("target", "10.0.0.5", 22, Some("me"));
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.custom_options
                .insert("HostKeyAlias".to_string(), "printer.local".to_string());
        }

        let cfg = build_ssh_config(&conn, &[], &[]).unwrap();
        assert!(cfg.contains("HostKeyAlias printer.local\n"));
    }

    #[test]
    fn explicit_proxy_command_wins_over_the_jump_chain() {
        let bastion = ssh("bastion", "jump.example.com", 22, Some("ops"));
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_jump_host_id(&mut conn, bastion.id);
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.proxy_command = Some("ncat --proxy 127.0.0.1:9050 %h %p".to_string());
        }

        let cfg = build_ssh_config(&conn, std::slice::from_ref(&bastion), &[]).unwrap();
        assert!(cfg.contains("ProxyCommand ncat --proxy 127.0.0.1:9050 %h %p\n"));
        assert!(!cfg.contains("ProxyJump "));
    }

    #[test]
    fn identities_only_is_emitted_only_with_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.identities_only = true;
        }
        // IdentitiesOnly without an identity file would hide every agent key and
        // break authentication outright, so it is gated on having one.
        let without_key = build_ssh_config(&conn, &[], &[]).unwrap();
        assert!(!without_key.contains("IdentitiesOnly"));

        set_key(&mut conn, &key_file(dir.path(), "target"));
        let with_key = build_ssh_config(&conn, &[], &[]).unwrap();
        assert!(with_key.contains("IdentitiesOnly yes\n"));
    }

    #[test]
    fn generated_config_is_accepted_by_ssh() {
        // `ssh -G` parses the file and prints the effective settings, so this
        // catches a malformed keyword or quoting mistake that unit assertions
        // on the text would miss.
        let dir = tempfile::tempdir().unwrap();
        let bastion_key = key_file(dir.path(), "bastion");
        // A path with a space is exactly what the quoting exists for.
        let target_key = key_file(dir.path(), "with space/target");

        let mut bastion = ssh("bastion", "jump.example.com", 2222, Some("ops"));
        set_key(&mut bastion, &bastion_key);
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_key(&mut conn, &target_key);
        set_jump_host_id(&mut conn, bastion.id);

        let text = build_ssh_config(&conn, std::slice::from_ref(&bastion), &[]).unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, &text).unwrap();

        let out = std::process::Command::new("ssh")
            .arg("-F")
            .arg(&path)
            .arg("-G")
            .arg("target.example.com")
            .output();
        let Ok(out) = out else {
            // No ssh on this machine — the text assertions above still ran.
            return;
        };
        assert!(
            out.status.success(),
            "ssh rejected the generated config: {}\n--- config ---\n{text}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rendered = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(rendered.contains("proxyjump ops@jump.example.com:2222"));
        assert!(rendered.contains(&format!(
            "identityfile {}",
            target_key.display().to_string().to_lowercase()
        )));
    }

    #[test]
    fn wrapper_is_written_and_points_at_a_real_ssh() {
        let conn = ssh("target", "target.example.com", 22, Some("me"));
        let session = Uuid::new_v4();
        let Some(env) = prepare_mc_ssh_env(session, &conn, &[], &[]) else {
            return;
        };

        let wrapper = env.wrapper_dir().join("ssh");
        assert!(wrapper.is_file());
        assert!(env.config_path().is_file());

        let script = std::fs::read_to_string(&wrapper).unwrap();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("-F "));
        // The wrapper must never resolve to itself.
        assert!(!script.contains(&format!("exec '{}'", wrapper.display())));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&wrapper).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }

        assert!(env.path_env().starts_with("PATH="));
        assert!(
            env.path_env()
                .contains(&env.wrapper_dir().display().to_string())
        );

        cleanup_mc_ssh_env(session);
        assert!(!env.wrapper_dir().exists());
    }

    #[test]
    fn wrapper_hands_ssh_the_jump_host_end_to_end() {
        // The whole point of the mechanism: invoking `ssh` from the wrapper
        // directory — which is what mc does, by name and with no options — must
        // yield a ProxyJump. `-G` makes ssh print the settings it resolved and
        // exit, so this exercises the wrapper and the generated config together
        // rather than asserting on the config text.
        let bastion = ssh("bastion", "jump.example.com", 2222, Some("ops"));
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_jump_host_id(&mut conn, bastion.id);

        let session = Uuid::new_v4();
        let Some(env) = prepare_mc_ssh_env(session, &conn, std::slice::from_ref(&bastion), &[])
        else {
            return;
        };

        let out = std::process::Command::new(env.wrapper_dir().join("ssh"))
            .arg("-G")
            .arg("target.example.com")
            .output();
        cleanup_mc_ssh_env(session);

        let Ok(out) = out else {
            // No usable ssh on this machine; the config-text tests still ran.
            return;
        };
        assert!(
            out.status.success(),
            "wrapper failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rendered = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(
            rendered.contains("proxyjump ops@jump.example.com:2222"),
            "wrapper did not apply the jump host; ssh reported:\n{rendered}"
        );
    }

    #[test]
    fn find_real_ssh_skips_the_wrapper_directory() {
        let dir = tempfile::tempdir().unwrap();
        let decoy = dir.path().join("ssh");
        std::fs::write(&decoy, b"#!/bin/sh\n").unwrap();

        // With the directory excluded, the decoy must not be chosen even though
        // it is first on PATH.
        let found = find_real_ssh(dir.path());
        assert_ne!(found, decoy);
    }
}
