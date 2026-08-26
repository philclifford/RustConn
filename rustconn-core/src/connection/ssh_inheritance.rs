//! SSH settings inheritance resolution.
//!
//! Resolves SSH settings (key path, auth method, proxy jump, agent socket)
//! by walking the group hierarchy from a connection up to the root group.
//! Each function checks the connection-level setting first, then walks
//! the parent chain returning the first `Some` value found.
//!
//! Cycle detection via `HashSet<Uuid>` ensures termination even with
//! malformed parent_id chains.
//!
//! # What the bastion resolvers key off
//!
//! [`resolve_ssh_proxy_jump`] and [`resolve_ssh_jump_host_id`] used to walk the
//! group chain only when `key_source == SshKeySource::Inherit` — i.e. whether a
//! connection inherited its *proxy* depended on where it got its *SSH key*.
//! Nothing chose that: the flag was standing in for "the user configured SSH
//! themselves", and it made two things impossible. A connection could not have
//! both a group bastion and its own key file, and there was no way to opt out of
//! an inherited bastion at all. It was also applied inconsistently — for RDP,
//! VNC and SPICE `ssh_config()` returns `None`, so the gate was skipped and
//! those protocols already inherited unconditionally.
//!
//! The flag is now [`crate::models::NetworkMode`] on the connection, which says
//! what it means and reads the same for every protocol.
//! [`resolve_ssh_key_path`] still matches on `key_source`, because there it *is*
//! the question being asked.

use std::collections::HashSet;
use std::path::PathBuf;

use uuid::Uuid;

use crate::config::NetworkSettings;
use crate::models::{
    Connection, ConnectionGroup, NetworkMode, ProtocolConfig, SshAuthMethod, SshKeySource,
};

/// Finds a group by ID in the slice.
fn find_group(id: Uuid, groups: &[ConnectionGroup]) -> Option<&ConnectionGroup> {
    groups.iter().find(|g| g.id == id)
}

/// Walks the group hierarchy starting from `start_group_id`, calling `extract`
/// on each group. Returns the first `Some` value, or `None` if the chain is
/// exhausted or a cycle is detected.
fn walk_group_chain<T>(
    start_group_id: Option<Uuid>,
    groups: &[ConnectionGroup],
    extract: impl Fn(&ConnectionGroup) -> Option<T>,
) -> Option<T> {
    let mut visited = HashSet::new();
    let mut current = start_group_id;

    while let Some(gid) = current {
        if !visited.insert(gid) {
            // Cycle detected
            return None;
        }
        let group = find_group(gid, groups)?;
        if let Some(value) = extract(group) {
            return Some(value);
        }
        current = group.parent_id;
    }

    None
}

/// Extracts the `SshConfig` from a connection's `protocol_config`, if it is
/// an SSH or SFTP variant.
fn ssh_config(connection: &Connection) -> Option<&crate::models::SshConfig> {
    match &connection.protocol_config {
        ProtocolConfig::Ssh(cfg) | ProtocolConfig::Sftp(cfg) => Some(cfg),
        _ => None,
    }
}

/// Resolves the SSH key path for a connection by checking the connection-level
/// setting first, then walking the group hierarchy.
///
/// Returns `Some(path)` if a key file path is found, `None` otherwise.
///
/// # Algorithm
///
/// 1. If the connection has `key_source = File { path }` → return `Some(path)`
/// 2. If `key_source = Agent` or `Default` → return `None` (no file-based key)
/// 3. If `key_source = Inherit` → walk the group chain for `ssh_key_path`
/// 4. If no SSH config exists on the connection → walk the group chain
#[must_use]
pub fn resolve_ssh_key_path(
    connection: &Connection,
    groups: &[ConnectionGroup],
) -> Option<PathBuf> {
    if let Some(cfg) = ssh_config(connection) {
        match &cfg.key_source {
            SshKeySource::File { path } if !path.as_os_str().is_empty() => {
                return Some(path.clone());
            }
            SshKeySource::Agent { .. } => return None,
            SshKeySource::Default => {
                // Legacy connections store key in key_path with key_source=Default
                if let Some(ref key_path) = cfg.key_path
                    && !key_path.as_os_str().is_empty()
                {
                    return Some(key_path.clone());
                }
                return None;
            }
            SshKeySource::Inherit | SshKeySource::File { .. } => {
                // Fall through to group chain walk
            }
        }
    }

    walk_group_chain(connection.group_id, groups, |g| g.ssh_key_path.clone())
}

/// Resolves the SSH authentication method for a connection.
///
/// Returns the connection-level auth method if set and not `Inherit`,
/// otherwise walks the group chain. Falls back to `SshAuthMethod::default()`
/// (Password) if nothing is found.
///
/// This one keeps looking at `key_source`, and the reason is different from the
/// bastion resolvers': `SshConfig::auth_method` is a plain `SshAuthMethod`, not
/// an `Option`, so there is no value that means "not set here". Until the field
/// can say that, `key_source == Inherit` is the only signal available, and
/// removing the check would let a group's method override a real choice.
#[must_use]
pub fn resolve_ssh_auth_method(
    connection: &Connection,
    groups: &[ConnectionGroup],
) -> SshAuthMethod {
    if let Some(cfg) = ssh_config(connection) {
        // If key_source is not Inherit, the connection has its own auth method
        if !matches!(cfg.key_source, SshKeySource::Inherit) {
            return cfg.auth_method.clone();
        }
    }

    walk_group_chain(connection.group_id, groups, |g| g.ssh_auth_method.clone()).unwrap_or_default()
}

/// Trims a stored `ProxyJump` and treats a blank one as unset.
///
/// A stored empty string is not "no bastion", it is a value that reached
/// `ssh -J ""`, breaking the command rather than disabling the proxy. Applied at
/// every tier — the connection, the group chain and
/// [`NetworkSettings::proxy_jump`] — because each of the three can hold one: the
/// dialogs normalise blanks away, but `config.toml` and `connections.toml` are
/// editable by hand, and `rustconn-cli group set --ssh-proxy-jump ""` used to
/// store one on purpose.
fn non_blank_proxy_jump(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Returns the connection's own free-text `ProxyJump`, treating blank as unset.
fn own_proxy_jump(connection: &Connection) -> Option<String> {
    match &connection.protocol_config {
        ProtocolConfig::Ssh(cfg) | ProtocolConfig::Sftp(cfg) => {
            non_blank_proxy_jump(cfg.proxy_jump.as_deref())
        }
        _ => None,
    }
}

/// Returns the connection's own `jump_host_id`, for any protocol that has one.
fn own_jump_host_id(connection: &Connection) -> Option<Uuid> {
    match &connection.protocol_config {
        ProtocolConfig::Ssh(c) | ProtocolConfig::Sftp(c) => c.jump_host_id,
        ProtocolConfig::Rdp(c) => c.jump_host_id,
        ProtocolConfig::Vnc(c) => c.jump_host_id,
        ProtocolConfig::Spice(c) => c.jump_host_id,
        _ => None,
    }
}

/// Resolves the free-text `ProxyJump` for a connection.
///
/// Order: the connection's own `proxy_jump`, then the group chain's
/// `ssh_proxy_jump`, then [`NetworkSettings::proxy_jump`].
/// [`NetworkMode::Direct`] returns `None` without consulting either inherited
/// tier — see the module docs for why this is not keyed off `key_source`.
#[must_use]
pub fn resolve_ssh_proxy_jump(
    connection: &Connection,
    groups: &[ConnectionGroup],
    network: &NetworkSettings,
) -> Option<String> {
    if let Some(own) = own_proxy_jump(connection) {
        return Some(own);
    }
    if connection.network_mode == NetworkMode::Direct {
        return None;
    }
    walk_group_chain(connection.group_id, groups, |g| {
        non_blank_proxy_jump(g.ssh_proxy_jump.as_deref())
    })
    .or_else(|| non_blank_proxy_jump(network.proxy_jump.as_deref()))
}

/// Resolves the jump-host connection ID for a connection.
///
/// Order: the connection's own `jump_host_id`, then the group chain's
/// `ssh_jump_host_id`, then [`NetworkSettings::jump_host_id`].
/// [`NetworkMode::Direct`] returns `None` without consulting either.
///
/// Reads `jump_host_id` from every protocol that has the field, not just
/// SSH/SFTP: RDP, VNC and SPICE reach a bastion through an SSH tunnel to the
/// same kind of saved connection.
#[must_use]
pub fn resolve_ssh_jump_host_id(
    connection: &Connection,
    groups: &[ConnectionGroup],
    network: &NetworkSettings,
) -> Option<Uuid> {
    if let Some(own) = own_jump_host_id(connection) {
        return Some(own);
    }
    if connection.network_mode == NetworkMode::Direct {
        return None;
    }
    walk_group_chain(connection.group_id, groups, |g| g.ssh_jump_host_id).or(network.jump_host_id)
}

/// Resolves the SSH agent socket path for a connection.
///
/// Checks the connection's SSH `ssh_agent_socket` first, then walks the group
/// chain for `ssh_agent_socket`.
///
/// No `NetworkMode` opt-out, unlike the two bastion resolvers: this is a path to
/// an ssh-agent rather than a route to the host, and a connection that wants a
/// different one says so by setting its own.
#[must_use]
pub fn resolve_ssh_agent_socket(
    connection: &Connection,
    groups: &[ConnectionGroup],
) -> Option<String> {
    if let Some(cfg) = ssh_config(connection)
        && cfg.ssh_agent_socket.is_some()
    {
        return cfg.ssh_agent_socket.clone();
    }

    walk_group_chain(connection.group_id, groups, |g| g.ssh_agent_socket.clone())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::models::{Connection, ConnectionGroup, ProtocolConfig, SshAuthMethod, SshKeySource};

    /// Helper: create an SSH connection with Inherit key_source, linked to a group.
    fn ssh_conn_inherit(group_id: Uuid) -> Connection {
        let mut conn = Connection::new_ssh("test".into(), "host".into(), 22);
        conn.group_id = Some(group_id);
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.key_source = SshKeySource::Inherit;
        }
        conn
    }

    /// Helper: an SSH connection in `group_id` with a key source of its own.
    ///
    /// The point of the tests below is that this used to be enough to suppress
    /// bastion inheritance entirely, so none of them can use
    /// [`ssh_conn_inherit`].
    fn ssh_conn_own_key(group_id: Option<Uuid>) -> Connection {
        let mut conn = Connection::new_ssh("test".into(), "host".into(), 22);
        conn.group_id = group_id;
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.key_source = SshKeySource::File {
                path: PathBuf::from("/my/key"),
            };
        }
        conn
    }

    fn group_with_proxy(proxy: &str) -> ConnectionGroup {
        let mut group = ConnectionGroup::new("G".into());
        group.ssh_proxy_jump = Some(proxy.into());
        group
    }

    // ── Bastion inheritance no longer keys off `key_source` ──

    #[test]
    fn proxy_jump_is_inherited_with_a_connection_level_key_file() {
        let group = group_with_proxy("bastion.example.com");
        let conn = ssh_conn_own_key(Some(group.id));

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[group], &NetworkSettings::default()),
            Some("bastion.example.com".into()),
            "a connection with its own key file must still inherit the group bastion"
        );
    }

    #[test]
    fn jump_host_id_is_inherited_with_a_connection_level_key_file() {
        let bastion_id = Uuid::new_v4();
        let mut group = ConnectionGroup::new("G".into());
        group.ssh_jump_host_id = Some(bastion_id);
        let conn = ssh_conn_own_key(Some(group.id));

        assert_eq!(
            resolve_ssh_jump_host_id(&conn, &[group], &NetworkSettings::default()),
            Some(bastion_id)
        );
    }

    #[test]
    fn agent_socket_is_inherited_with_a_connection_level_key_file() {
        let mut group = ConnectionGroup::new("G".into());
        group.ssh_agent_socket = Some("/tmp/agent.sock".into());
        let conn = ssh_conn_own_key(Some(group.id));

        assert_eq!(
            resolve_ssh_agent_socket(&conn, &[group]),
            Some("/tmp/agent.sock".into())
        );
    }

    // ── `NetworkMode::Direct` is the opt-out ──

    #[test]
    fn direct_mode_refuses_an_inherited_proxy_jump() {
        let group = group_with_proxy("bastion.example.com");
        let mut conn = ssh_conn_own_key(Some(group.id));
        conn.network_mode = NetworkMode::Direct;

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[group], &NetworkSettings::default()),
            None
        );
    }

    #[test]
    fn direct_mode_refuses_an_inherited_jump_host_id() {
        let mut group = ConnectionGroup::new("G".into());
        group.ssh_jump_host_id = Some(Uuid::new_v4());
        let mut conn = ssh_conn_own_key(Some(group.id));
        conn.network_mode = NetworkMode::Direct;

        assert_eq!(
            resolve_ssh_jump_host_id(&conn, &[group], &NetworkSettings::default()),
            None
        );
    }

    #[test]
    fn direct_mode_keeps_the_connections_own_proxy_jump() {
        // `Direct` refuses *inherited* bastions; an explicit one on the
        // connection is not inherited and stays in force.
        let group = group_with_proxy("group-bastion");
        let mut conn = ssh_conn_own_key(Some(group.id));
        conn.network_mode = NetworkMode::Direct;
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.proxy_jump = Some("own-bastion".into());
        }

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[group], &NetworkSettings::default()),
            Some("own-bastion".into())
        );
    }

    // ── The global tier, below the group chain ──

    #[test]
    fn global_proxy_jump_applies_to_an_ungrouped_connection() {
        // The tier exists for exactly this: an ungrouped connection has no
        // chain to walk, so a group can never reach it.
        let conn = ssh_conn_own_key(None);
        let network = NetworkSettings {
            proxy_jump: Some("global-bastion".into()),
            jump_host_id: None,
        };

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[], &network),
            Some("global-bastion".into())
        );
    }

    #[test]
    fn group_proxy_jump_outranks_the_global_one() {
        let group = group_with_proxy("group-bastion");
        let conn = ssh_conn_own_key(Some(group.id));
        let network = NetworkSettings {
            proxy_jump: Some("global-bastion".into()),
            jump_host_id: None,
        };

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[group], &network),
            Some("group-bastion".into())
        );
    }

    #[test]
    fn direct_mode_refuses_the_global_proxy_jump() {
        let mut conn = ssh_conn_own_key(None);
        conn.network_mode = NetworkMode::Direct;
        let network = NetworkSettings {
            proxy_jump: Some("global-bastion".into()),
            jump_host_id: None,
        };

        assert_eq!(resolve_ssh_proxy_jump(&conn, &[], &network), None);
    }

    #[test]
    fn global_jump_host_id_applies_to_an_ungrouped_connection() {
        let bastion_id = Uuid::new_v4();
        let conn = ssh_conn_own_key(None);
        let network = NetworkSettings {
            proxy_jump: None,
            jump_host_id: Some(bastion_id),
        };

        assert_eq!(
            resolve_ssh_jump_host_id(&conn, &[], &network),
            Some(bastion_id)
        );
    }

    // ── A blank `proxy_jump` means "nothing", not `ssh -J ""` ──

    #[test]
    fn blank_own_proxy_jump_falls_through_to_the_group() {
        // Only the CLI could store a blank value; it used to survive all the way
        // to `ssh -J ""`, which breaks the command instead of disabling the
        // proxy.
        let group = group_with_proxy("group-bastion");
        let mut conn = ssh_conn_own_key(Some(group.id));
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.proxy_jump = Some("   ".into());
        }

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[group], &NetworkSettings::default()),
            Some("group-bastion".into())
        );
    }

    #[test]
    fn blank_global_proxy_jump_is_not_a_bastion() {
        // The global tier is the one a user reaches by editing `config.toml`, so
        // it needs the same blank check as the other two rather than passing
        // `Some("")` straight through to `ssh -J ""`.
        let conn = ssh_conn_own_key(None);
        let network = NetworkSettings {
            proxy_jump: Some("  ".into()),
            jump_host_id: None,
        };

        assert_eq!(resolve_ssh_proxy_jump(&conn, &[], &network), None);
    }

    #[test]
    fn global_proxy_jump_is_trimmed() {
        let conn = ssh_conn_own_key(None);
        let network = NetworkSettings {
            proxy_jump: Some("  global-bastion \n".into()),
            jump_host_id: None,
        };

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[], &network),
            Some("global-bastion".into())
        );
    }

    #[test]
    fn blank_group_proxy_jump_falls_through_to_the_global_one() {
        let mut group = ConnectionGroup::new("G".into());
        group.ssh_proxy_jump = Some(String::new());
        let conn = ssh_conn_own_key(Some(group.id));
        let network = NetworkSettings {
            proxy_jump: Some("global-bastion".into()),
            jump_host_id: None,
        };

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[group], &network),
            Some("global-bastion".into())
        );
    }

    // ── A non-SSH protocol resolves the same way ──

    #[test]
    fn rdp_connection_honours_direct_mode() {
        // Before `NetworkMode`, `ssh_config()` returned `None` for RDP so the
        // `key_source` gate was skipped and RDP always inherited, with no way
        // to refuse.
        let group = group_with_proxy("bastion.example.com");
        let mut conn = Connection::new_rdp("rdp".into(), "host".into(), 3389);
        conn.group_id = Some(group.id);
        conn.network_mode = NetworkMode::Direct;

        assert_eq!(
            resolve_ssh_proxy_jump(
                &conn,
                std::slice::from_ref(&group),
                &NetworkSettings::default()
            ),
            None
        );

        conn.network_mode = NetworkMode::Inherit;
        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[group], &NetworkSettings::default()),
            Some("bastion.example.com".into())
        );
    }

    // ── 1. Three-level nesting: key set on root, inherited through middle to leaf ──

    #[test]
    fn key_path_inherited_through_three_levels() {
        let mut group_a = ConnectionGroup::new("A".into());
        group_a.ssh_key_path = Some(PathBuf::from("/keys/root"));

        let group_b = ConnectionGroup::with_parent("B".into(), group_a.id);
        let group_c = ConnectionGroup::with_parent("C".into(), group_b.id);

        let conn = ssh_conn_inherit(group_c.id);
        let groups = vec![group_a, group_b, group_c];

        assert_eq!(
            resolve_ssh_key_path(&conn, &groups),
            Some(PathBuf::from("/keys/root"))
        );
    }

    #[test]
    fn auth_method_inherited_through_three_levels() {
        let mut group_a = ConnectionGroup::new("A".into());
        group_a.ssh_auth_method = Some(SshAuthMethod::PublicKey);

        let group_b = ConnectionGroup::with_parent("B".into(), group_a.id);
        let group_c = ConnectionGroup::with_parent("C".into(), group_b.id);

        let conn = ssh_conn_inherit(group_c.id);
        let groups = vec![group_a, group_b, group_c];

        assert_eq!(
            resolve_ssh_auth_method(&conn, &groups),
            SshAuthMethod::PublicKey
        );
    }

    #[test]
    fn proxy_jump_inherited_through_three_levels() {
        let mut group_a = ConnectionGroup::new("A".into());
        group_a.ssh_proxy_jump = Some("bastion.example.com".into());

        let group_b = ConnectionGroup::with_parent("B".into(), group_a.id);
        let group_c = ConnectionGroup::with_parent("C".into(), group_b.id);

        let conn = ssh_conn_inherit(group_c.id);
        let groups = vec![group_a, group_b, group_c];

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &groups, &NetworkSettings::default()),
            Some("bastion.example.com".into())
        );
    }

    #[test]
    fn agent_socket_inherited_through_three_levels() {
        let mut group_a = ConnectionGroup::new("A".into());
        group_a.ssh_agent_socket = Some("/tmp/agent.sock".into());

        let group_b = ConnectionGroup::with_parent("B".into(), group_a.id);
        let group_c = ConnectionGroup::with_parent("C".into(), group_b.id);

        let conn = ssh_conn_inherit(group_c.id);
        let groups = vec![group_a, group_b, group_c];

        assert_eq!(
            resolve_ssh_agent_socket(&conn, &groups),
            Some("/tmp/agent.sock".into())
        );
    }

    // ── 2. Missing parent: group_id references a non-existent group ──

    #[test]
    fn missing_parent_returns_none_for_key_path() {
        let missing_id = Uuid::new_v4();
        let conn = ssh_conn_inherit(missing_id);

        assert_eq!(resolve_ssh_key_path(&conn, &[]), None);
    }

    #[test]
    fn missing_parent_returns_default_for_auth_method() {
        let missing_id = Uuid::new_v4();
        let conn = ssh_conn_inherit(missing_id);

        assert_eq!(
            resolve_ssh_auth_method(&conn, &[]),
            SshAuthMethod::default()
        );
    }

    #[test]
    fn missing_parent_returns_none_for_proxy_jump() {
        let missing_id = Uuid::new_v4();
        let conn = ssh_conn_inherit(missing_id);

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &[], &NetworkSettings::default()),
            None
        );
    }

    #[test]
    fn missing_parent_returns_none_for_agent_socket() {
        let missing_id = Uuid::new_v4();
        let conn = ssh_conn_inherit(missing_id);

        assert_eq!(resolve_ssh_agent_socket(&conn, &[]), None);
    }

    // ── 3. No key in chain: all groups have None ──

    #[test]
    fn no_key_in_chain_returns_none() {
        let group_a = ConnectionGroup::new("A".into());
        let group_b = ConnectionGroup::with_parent("B".into(), group_a.id);
        let group_c = ConnectionGroup::with_parent("C".into(), group_b.id);

        let conn = ssh_conn_inherit(group_c.id);
        let groups = vec![group_a, group_b, group_c];

        assert_eq!(resolve_ssh_key_path(&conn, &groups), None);
    }

    #[test]
    fn no_auth_method_in_chain_returns_default() {
        let group_a = ConnectionGroup::new("A".into());
        let group_b = ConnectionGroup::with_parent("B".into(), group_a.id);

        let conn = ssh_conn_inherit(group_b.id);
        let groups = vec![group_a, group_b];

        assert_eq!(
            resolve_ssh_auth_method(&conn, &groups),
            SshAuthMethod::default()
        );
    }

    // ── 4. Direct connection setting: File key_source returns path directly ──

    #[test]
    fn direct_file_key_source_returns_path() {
        let mut group = ConnectionGroup::new("G".into());
        group.ssh_key_path = Some(PathBuf::from("/group/key"));

        let mut conn = Connection::new_ssh("test".into(), "host".into(), 22);
        conn.group_id = Some(group.id);
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.key_source = SshKeySource::File {
                path: PathBuf::from("/my/key"),
            };
        }

        let groups = vec![group];

        // Connection-level File key takes precedence over group
        assert_eq!(
            resolve_ssh_key_path(&conn, &groups),
            Some(PathBuf::from("/my/key"))
        );
    }

    // ── 5. Middle of chain: key set on B, not on root A ──

    #[test]
    fn key_found_at_middle_group() {
        let group_a = ConnectionGroup::new("A".into());
        let mut group_b = ConnectionGroup::with_parent("B".into(), group_a.id);
        group_b.ssh_key_path = Some(PathBuf::from("/keys/middle"));

        let group_c = ConnectionGroup::with_parent("C".into(), group_b.id);

        let conn = ssh_conn_inherit(group_c.id);
        let groups = vec![group_a, group_b, group_c];

        assert_eq!(
            resolve_ssh_key_path(&conn, &groups),
            Some(PathBuf::from("/keys/middle"))
        );
    }

    #[test]
    fn proxy_jump_found_at_middle_group() {
        let group_a = ConnectionGroup::new("A".into());
        let mut group_b = ConnectionGroup::with_parent("B".into(), group_a.id);
        group_b.ssh_proxy_jump = Some("jump-host".into());

        let group_c = ConnectionGroup::with_parent("C".into(), group_b.id);

        let conn = ssh_conn_inherit(group_c.id);
        let groups = vec![group_a, group_b, group_c];

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &groups, &NetworkSettings::default()),
            Some("jump-host".into())
        );
    }

    // ── 6. Cycle detection: A → B → A terminates without infinite loop ──

    #[test]
    fn cycle_detection_terminates_for_key_path() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        let mut group_a = ConnectionGroup::new("A".into());
        group_a.id = id_a;
        group_a.parent_id = Some(id_b);

        let mut group_b = ConnectionGroup::new("B".into());
        group_b.id = id_b;
        group_b.parent_id = Some(id_a);

        let conn = ssh_conn_inherit(id_a);
        let groups = vec![group_a, group_b];

        // Should terminate and return None (no key set, cycle detected)
        assert_eq!(resolve_ssh_key_path(&conn, &groups), None);
    }

    #[test]
    fn cycle_detection_terminates_for_auth_method() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        let mut group_a = ConnectionGroup::new("A".into());
        group_a.id = id_a;
        group_a.parent_id = Some(id_b);

        let mut group_b = ConnectionGroup::new("B".into());
        group_b.id = id_b;
        group_b.parent_id = Some(id_a);

        let conn = ssh_conn_inherit(id_a);
        let groups = vec![group_a, group_b];

        // Should terminate and return default
        assert_eq!(
            resolve_ssh_auth_method(&conn, &groups),
            SshAuthMethod::default()
        );
    }

    #[test]
    fn cycle_detection_terminates_for_proxy_jump() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        let mut group_a = ConnectionGroup::new("A".into());
        group_a.id = id_a;
        group_a.parent_id = Some(id_b);

        let mut group_b = ConnectionGroup::new("B".into());
        group_b.id = id_b;
        group_b.parent_id = Some(id_a);

        let conn = ssh_conn_inherit(id_a);
        let groups = vec![group_a, group_b];

        assert_eq!(
            resolve_ssh_proxy_jump(&conn, &groups, &NetworkSettings::default()),
            None
        );
    }

    #[test]
    fn cycle_detection_terminates_for_agent_socket() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        let mut group_a = ConnectionGroup::new("A".into());
        group_a.id = id_a;
        group_a.parent_id = Some(id_b);

        let mut group_b = ConnectionGroup::new("B".into());
        group_b.id = id_b;
        group_b.parent_id = Some(id_a);

        let conn = ssh_conn_inherit(id_a);
        let groups = vec![group_a, group_b];

        assert_eq!(resolve_ssh_agent_socket(&conn, &groups), None);
    }

    // ── 7. Agent key source: returns None (agent handles keys) ──

    #[test]
    fn agent_key_source_returns_none() {
        let mut group = ConnectionGroup::new("G".into());
        group.ssh_key_path = Some(PathBuf::from("/group/key"));

        let mut conn = Connection::new_ssh("test".into(), "host".into(), 22);
        conn.group_id = Some(group.id);
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.key_source = SshKeySource::Agent {
                fingerprint: "SHA256:abc".into(),
                comment: "my-key".into(),
            };
        }

        let groups = vec![group];

        assert_eq!(resolve_ssh_key_path(&conn, &groups), None);
    }

    // ── 8. Default key source: returns None ──

    #[test]
    fn default_key_source_returns_none() {
        let mut group = ConnectionGroup::new("G".into());
        group.ssh_key_path = Some(PathBuf::from("/group/key"));

        let mut conn = Connection::new_ssh("test".into(), "host".into(), 22);
        conn.group_id = Some(group.id);
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.key_source = SshKeySource::Default;
        }

        let groups = vec![group];

        assert_eq!(resolve_ssh_key_path(&conn, &groups), None);
    }
}
