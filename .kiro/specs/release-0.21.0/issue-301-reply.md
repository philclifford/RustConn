# Draft reply — issue #301 (Global Jump Host not applied)

Verified against the code on 2026-08-28, not from memory. Decide yourself whether
to name a target version; the draft deliberately does not promise one.

Evidence behind every claim:

- `rustconn/src/window/protocols_ssh.rs:301` — free text goes through
  `resolve_ssh_proxy_jump(conn, groups, &network)`, all three tiers.
- `rustconn/src/window/protocols_ssh.rs:310` — the reference form is read as
  `ssh_config.jump_host_id`, i.e. the connection's own field, no inheritance.
- `resolve_ssh_jump_host_id` has no caller in the `rustconn` crate at all. Its
  only caller is `resolve_jump_chain`, whose non-test callers are the editor
  subtitle (`dialogs/connection/dialog/populate.rs:1137`), the SFTP browser
  (`window/edit_actions.rs:846`, `:1516`), `rustconn-cli/src/commands/sftp.rs:89`
  and `rustconn-core/src/mc_ssh.rs:164` — display and SFTP, never the launcher.
- Tunnel gates read the connection's own field only: RDP
  `rustconn/src/window/rdp_vnc.rs:273`, VNC `:1267`, SPICE
  `rustconn/src/window/protocols.rs:1224`.
- The precedence itself is correct and tested:
  `rustconn-core/src/connection/ssh_inheritance.rs:184-230`, with the global tier
  covered by `global_proxy_jump_applies_to_an_ungrouped_connection`,
  `global_jump_host_id_applies_to_an_ungrouped_connection`,
  `group_proxy_jump_outranks_the_global_one` and four more.
- Persistence round-trips: `collect_network_settings` /
  `load_network_settings` in `rustconn/src/dialogs/settings/network_tab.rs`
  write and read the same `NetworkSettings.jump_host_id` the resolver consumes.

---

You're right, and thank you for the precise steps — they made this quick to
confirm. **This is a bug, and your configuration cannot work as things stand.**
Not a misunderstanding on your side.

**The intended priority, per field**, first non-empty wins:

```
connection's own  →  nearest group  →  parent group  →  …  →  root group  →  global
```

`Network Mode` is not a tier in that ladder — it's a switch on the connection
that says whether the inherited tiers are consulted at all. `Direct` refuses the
group and global tiers but keeps a jump host set on the connection itself.
`Inherit from group or globally` is the default, so existing connections already
inherit.

One thing that isn't obvious: **Jump Host (the picker) and ProxyJump (the text
field) don't compete — they chain.** They're resolved independently and both end
up in the route, so setting Global Jump Host *and* Global ProxyJump together is
valid and gives you two hops, reaching the ProxyJump value *through* the picked
Jump Host.

**What actually happens today:**

| Where it's set | SSH / SFTP terminal | RDP / VNC / SPICE |
|---|---|---|
| Connection's own Jump Host or ProxyJump | works | works |
| Group **ProxyJump** (text) | works | ignored |
| Group **Jump Host** (picker) | **ignored** | ignored |
| Global **ProxyJump** (text) | works | ignored |
| Global **Jump Host** (picker) | **ignored** | ignored |

The launch path resolves the free-text field through all three tiers but reads
the picker straight off the connection, so anything selected with a picker above
connection level is stored, shown in the editor as inherited, and then dropped at
connect time. For RDP/VNC/SPICE the tunnel is only ever built from the
connection's own field, so neither form inherits there.

**Workaround for SSH/SFTP right now:** put the bastion in **Preferences →
Network → Global ProxyJump** as text in OpenSSH syntax instead of using the
picker:

```
user@bastion.example.com
user@bastion.example.com:2222
```

That goes through the full three-tier resolution and will be applied. The
trade-off versus the picker is that the text field carries only
`[user@]host[:port]` — no identity file, no stored password, no chain of its own
— so if your bastion needs a key you'll want it in `~/.ssh/config` or in the
connection's own Jump Host field. There's no workaround for RDP/VNC/SPICE other
than setting Jump Host on the connection itself.

A quick way to confirm the setting is stored correctly: the SFTP file browser
uses the fixed resolver already, so it will route through your global bastion
while an SSH terminal to the same host won't. Same config, two different answers
— that contrast is the bug.

The fix is to route the launch paths through the resolver that already exists and
is already tested; the precedence itself is correct and doesn't change.
