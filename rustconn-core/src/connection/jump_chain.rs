//! Jump-host chain resolution independent of the GUI.
//!
//! A connection can name its bastion two ways: the free-text `proxy_jump`
//! field (raw OpenSSH `ProxyJump` syntax, also inheritable from a group) or
//! `jump_host_id`, a reference to another RustConn connection — which is what
//! the jump-host picker in the connection editor writes. Resolving the second
//! form needs the whole connection list, so until now it only happened inside
//! the GUI crate (`window::protocols_ssh::build_ssh_command_args` and
//! `window::protocols::resolve_jump_chain_for_tunnel`). Everything in
//! `rustconn-core` therefore saw only the string form, and the SFTP paths —
//! which live here — silently dropped a picker-selected bastion (issue #255).
//!
//! ## Hop order
//!
//! Chains are resolved **target-first**: `hops[0]` is the bastion closest to
//! the target, walking outward towards the client. That is the order
//! [`crate::ssh_tunnel::build_nested_proxy_command`] consumes, and the order
//! the two GUI resolvers already produce. OpenSSH's `-J` visits hops in the
//! opposite direction, so use [`JumpChain::proxy_jump_value`] rather than
//! joining `hops` by hand.

use std::collections::HashSet;

use uuid::Uuid;

use crate::models::{Connection, ConnectionGroup, ProtocolConfig};

/// Maximum number of hops followed before giving up.
///
/// Matches the limit the GUI resolvers use. A chain this long is a
/// configuration mistake, not a topology anyone maintains deliberately, and the
/// cap is a second line of defence behind the visited-set cycle check.
const MAX_HOPS: usize = 10;

/// A resolved jump-host chain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JumpChain {
    /// Hops in `[user@]host[:port]` form, **target-first**: `hops[0]` is the
    /// bastion closest to the target.
    pub hops: Vec<String>,
    /// Connection id of each hop, parallel to `hops`. `None` for a hop that
    /// came from a free-text `proxy_jump` field and so has no connection
    /// behind it.
    pub hop_ids: Vec<Option<Uuid>>,
}

impl JumpChain {
    /// Returns `true` when no bastion is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
    }

    /// Returns the value for OpenSSH's `-J` / `ProxyJump`, or `None` when the
    /// chain is empty.
    ///
    /// Reverses the target-first order into the client-first order `-J`
    /// expects; see [`crate::ssh_tunnel::proxy_jump_arg`].
    #[must_use]
    pub fn proxy_jump_value(&self) -> Option<String> {
        if self.hops.is_empty() {
            return None;
        }
        Some(crate::ssh_tunnel::proxy_jump_arg(&self.hops.join(",")))
    }

    /// Returns the hop the client contacts first, i.e. the last of `hops`.
    ///
    /// This is the hop whose own credentials must be supplied separately: `-J`
    /// does not pass `-i`/`-o` down to it (issue #241).
    #[must_use]
    pub fn client_side_hop(&self) -> Option<&str> {
        self.hops.last().map(String::as_str)
    }

    /// Returns the connection id of [`Self::client_side_hop`], when that hop is
    /// reference-based.
    #[must_use]
    pub fn client_side_hop_id(&self) -> Option<Uuid> {
        self.hop_ids.last().copied().flatten()
    }
}

/// Formats a connection as an OpenSSH hop spec: `[user@]host[:port]`.
///
/// The port is omitted when it is 22, matching what the GUI resolvers emit.
fn hop_spec(conn: &Connection) -> String {
    let mut spec = conn.host.clone();
    if let Some(user) = &conn.username {
        spec = format!("{user}@{spec}");
    }
    if conn.port != 22 {
        spec = format!("{spec}:{}", conn.port);
    }
    spec
}

/// Returns the `jump_host_id` of an SSH-family connection.
fn jump_host_id_of(conn: &Connection) -> Option<Uuid> {
    match &conn.protocol_config {
        ProtocolConfig::Ssh(c) | ProtocolConfig::Sftp(c) => c.jump_host_id,
        ProtocolConfig::Rdp(c) => c.jump_host_id,
        ProtocolConfig::Vnc(c) => c.jump_host_id,
        ProtocolConfig::Spice(c) => c.jump_host_id,
        _ => None,
    }
}

/// Returns the free-text `proxy_jump` of an SSH-family connection.
fn proxy_jump_of(conn: &Connection) -> Option<&str> {
    match &conn.protocol_config {
        ProtocolConfig::Ssh(c) | ProtocolConfig::Sftp(c) => c.proxy_jump.as_deref(),
        _ => None,
    }
}

/// Resolves the full jump-host chain for `connection`.
///
/// Walks both forms of bastion configuration in the same order the SSH terminal
/// path uses, so an SFTP connection reaches its target exactly the way the
/// equivalent SSH connection does:
///
/// 1. the connection's own `proxy_jump`, including a value inherited from a
///    group ([`crate::connection::ssh_inheritance::resolve_ssh_proxy_jump`]);
/// 2. then the `jump_host_id` reference chain, following each hop's own
///    `jump_host_id` outward, and splicing in any `proxy_jump` a hop carries.
///
/// Returns an empty chain when nothing is configured. Terminates on a
/// self-reference or cycle, and after [`MAX_HOPS`] hops.
///
/// A free-text `proxy_jump` holding several comma-separated hops is kept as one
/// opaque entry: the field mirrors OpenSSH syntax, so the user has already
/// written those hops client-first and reordering them would break the value
/// they tested by hand.
#[must_use]
pub fn resolve_jump_chain(
    connection: &Connection,
    connections: &[Connection],
    groups: &[ConnectionGroup],
) -> JumpChain {
    let mut chain = JumpChain::default();

    if let Some(proxy) =
        crate::connection::ssh_inheritance::resolve_ssh_proxy_jump(connection, groups)
    {
        chain.hops.push(proxy);
        chain.hop_ids.push(None);
    }

    let mut visited = HashSet::new();
    visited.insert(connection.id);
    let mut current = jump_host_id_of(connection);

    for _ in 0..MAX_HOPS {
        let Some(id) = current else { break };
        if !visited.insert(id) {
            break;
        }
        let Some(hop) = connections.iter().find(|c| c.id == id) else {
            break;
        };

        chain.hops.push(hop_spec(hop));
        chain.hop_ids.push(Some(id));

        // A `proxy_jump` on the hop sits between the hop and the client, so it
        // belongs one position further out — i.e. immediately before the entry
        // just pushed in target-first order.
        if let Some(proxy) = proxy_jump_of(hop) {
            let insert_at = chain.hops.len() - 1;
            chain.hops.insert(insert_at, proxy.to_string());
            chain.hop_ids.insert(insert_at, None);
        }

        current = jump_host_id_of(hop);
    }

    chain
}

/// Resolves the chain and returns the `-J` / `ProxyJump` value directly.
///
/// Convenience wrapper over [`resolve_jump_chain`] for callers that only need
/// the option value; see [`JumpChain::proxy_jump_value`] for the ordering note.
#[must_use]
pub fn resolve_proxy_jump_value(
    connection: &Connection,
    connections: &[Connection],
    groups: &[ConnectionGroup],
) -> Option<String> {
    resolve_jump_chain(connection, connections, groups).proxy_jump_value()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SshKeySource;

    /// Builds an SSH connection with an explicit id.
    fn ssh(name: &str, host: &str, port: u16, user: Option<&str>) -> Connection {
        let mut conn = Connection::new_ssh(name.to_string(), host.to_string(), port);
        conn.username = user.map(str::to_string);
        conn
    }

    fn set_jump_host_id(conn: &mut Connection, id: Option<Uuid>) {
        if let ProtocolConfig::Ssh(ref mut cfg) | ProtocolConfig::Sftp(ref mut cfg) =
            conn.protocol_config
        {
            cfg.jump_host_id = id;
        }
    }

    fn set_proxy_jump(conn: &mut Connection, value: Option<&str>) {
        if let ProtocolConfig::Ssh(ref mut cfg) | ProtocolConfig::Sftp(ref mut cfg) =
            conn.protocol_config
        {
            cfg.proxy_jump = value.map(str::to_string);
        }
    }

    #[test]
    fn no_jump_host_yields_empty_chain() {
        let conn = ssh("target", "target.example.com", 22, Some("me"));
        let chain = resolve_jump_chain(&conn, &[], &[]);
        assert!(chain.is_empty());
        assert_eq!(chain.proxy_jump_value(), None);
    }

    #[test]
    fn single_reference_hop_resolves_to_user_host() {
        let bastion = ssh("bastion", "jump.example.com", 22, Some("ops"));
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_jump_host_id(&mut conn, Some(bastion.id));

        let chain = resolve_jump_chain(&conn, std::slice::from_ref(&bastion), &[]);
        assert_eq!(chain.hops, vec!["ops@jump.example.com".to_string()]);
        assert_eq!(
            chain.proxy_jump_value(),
            Some("ops@jump.example.com".to_string())
        );
        assert_eq!(chain.client_side_hop_id(), Some(bastion.id));
    }

    #[test]
    fn non_default_hop_port_is_included() {
        let bastion = ssh("bastion", "jump.example.com", 2222, Some("ops"));
        let mut conn = ssh("target", "target.example.com", 22, None);
        set_jump_host_id(&mut conn, Some(bastion.id));

        let chain = resolve_jump_chain(&conn, std::slice::from_ref(&bastion), &[]);
        assert_eq!(
            chain.proxy_jump_value(),
            Some("ops@jump.example.com:2222".to_string())
        );
    }

    #[test]
    fn hop_without_username_omits_at_sign() {
        let bastion = ssh("bastion", "jump.example.com", 22, None);
        let mut conn = ssh("target", "target.example.com", 22, None);
        set_jump_host_id(&mut conn, Some(bastion.id));

        let chain = resolve_jump_chain(&conn, std::slice::from_ref(&bastion), &[]);
        assert_eq!(
            chain.proxy_jump_value(),
            Some("jump.example.com".to_string())
        );
    }

    #[test]
    fn two_hop_chain_is_reversed_for_proxy_jump() {
        // Topology: client → far → near → target.
        let far = ssh("far", "far.example.com", 22, Some("a"));
        let mut near = ssh("near", "near.example.com", 22, Some("b"));
        set_jump_host_id(&mut near, Some(far.id));
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_jump_host_id(&mut conn, Some(near.id));

        let connections = vec![far, near];
        let chain = resolve_jump_chain(&conn, &connections, &[]);

        // Resolved target-first…
        assert_eq!(
            chain.hops,
            vec![
                "b@near.example.com".to_string(),
                "a@far.example.com".to_string()
            ]
        );
        // …and handed to ssh client-first.
        assert_eq!(
            chain.proxy_jump_value(),
            Some("a@far.example.com,b@near.example.com".to_string())
        );
        assert_eq!(chain.client_side_hop(), Some("a@far.example.com"));
    }

    #[test]
    fn string_proxy_jump_is_used_when_no_reference_exists() {
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_proxy_jump(&mut conn, Some("ops@gw.example.com"));

        let chain = resolve_jump_chain(&conn, &[], &[]);
        assert_eq!(
            chain.proxy_jump_value(),
            Some("ops@gw.example.com".to_string())
        );
        assert_eq!(chain.hop_ids, vec![None]);
    }

    #[test]
    fn group_inherited_proxy_jump_is_picked_up() {
        let mut group = ConnectionGroup::new("prod".to_string());
        group.ssh_proxy_jump = Some("ops@bastion.example.com".to_string());

        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        conn.group_id = Some(group.id);
        if let ProtocolConfig::Ssh(ref mut cfg) = conn.protocol_config {
            cfg.key_source = SshKeySource::Inherit;
        }

        let chain = resolve_jump_chain(&conn, &[], std::slice::from_ref(&group));
        assert_eq!(
            chain.proxy_jump_value(),
            Some("ops@bastion.example.com".to_string())
        );
    }

    #[test]
    fn self_reference_does_not_loop() {
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        let id = conn.id;
        set_jump_host_id(&mut conn, Some(id));

        let chain = resolve_jump_chain(&conn, std::slice::from_ref(&conn), &[]);
        assert!(chain.is_empty());
    }

    #[test]
    fn cycle_between_two_hops_terminates() {
        let mut a = ssh("a", "a.example.com", 22, None);
        let mut b = ssh("b", "b.example.com", 22, None);
        set_jump_host_id(&mut a, Some(b.id));
        set_jump_host_id(&mut b, Some(a.id));

        let mut conn = ssh("target", "target.example.com", 22, None);
        set_jump_host_id(&mut conn, Some(a.id));

        let connections = vec![a.clone(), b.clone()];
        let chain = resolve_jump_chain(&conn, &connections, &[]);
        // Both hops are reported once; the walk stops when it revisits `a`.
        assert_eq!(chain.hops.len(), 2);
    }

    #[test]
    fn missing_hop_connection_stops_the_walk() {
        let mut conn = ssh("target", "target.example.com", 22, None);
        set_jump_host_id(&mut conn, Some(Uuid::new_v4()));

        let chain = resolve_jump_chain(&conn, &[], &[]);
        assert!(chain.is_empty());
    }

    #[test]
    fn sftp_connections_resolve_like_ssh() {
        let bastion = ssh("bastion", "jump.example.com", 22, Some("ops"));
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        // Same shape the connection editor produces for protocol SFTP.
        if let ProtocolConfig::Ssh(cfg) = conn.protocol_config.clone() {
            conn.protocol_config = ProtocolConfig::Sftp(cfg);
        }
        set_jump_host_id(&mut conn, Some(bastion.id));

        let chain = resolve_jump_chain(&conn, std::slice::from_ref(&bastion), &[]);
        assert_eq!(
            chain.proxy_jump_value(),
            Some("ops@jump.example.com".to_string())
        );
    }

    #[test]
    fn hop_carrying_its_own_string_proxy_sits_further_out() {
        let mut near = ssh("near", "near.example.com", 22, Some("b"));
        set_proxy_jump(&mut near, Some("gw.example.com"));
        let mut conn = ssh("target", "target.example.com", 22, Some("me"));
        set_jump_host_id(&mut conn, Some(near.id));

        let chain = resolve_jump_chain(&conn, std::slice::from_ref(&near), &[]);
        assert_eq!(
            chain.hops,
            vec![
                "gw.example.com".to_string(),
                "b@near.example.com".to_string()
            ]
        );
        assert_eq!(
            chain.proxy_jump_value(),
            Some("b@near.example.com,gw.example.com".to_string())
        );
    }

    #[test]
    fn chain_longer_than_the_cap_is_truncated() {
        // Build 15 chained hops; only MAX_HOPS should be followed.
        let mut hops: Vec<Connection> = (0..15)
            .map(|i| ssh(&format!("h{i}"), &format!("h{i}.example.com"), 22, None))
            .collect();
        for i in 0..hops.len() - 1 {
            let next = hops[i + 1].id;
            set_jump_host_id(&mut hops[i], Some(next));
        }
        let mut conn = ssh("target", "target.example.com", 22, None);
        set_jump_host_id(&mut conn, Some(hops[0].id));

        let chain = resolve_jump_chain(&conn, &hops, &[]);
        assert_eq!(chain.hops.len(), MAX_HOPS);
    }
}
