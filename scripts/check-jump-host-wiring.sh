#!/usr/bin/env bash
# Verifies that no launch path reads `jump_host_id` off a protocol config.
#
# Why this is a gate and not a code review note:
#
# 0.20.9 shipped the three-tier bastion resolver, wired the *free-text* ProxyJump
# to it everywhere, and left all four launchers reading the picker's field
# directly. A Jump Host chosen on a group or in Preferences → Network was stored,
# shown in the editor as inherited, synced between machines — and dropped at
# connect time. The release notes said inheritance "reads the same for every
# protocol". It was true of neither the picker nor of RDP/VNC/SPICE.
#
# The precedence logic itself was never the problem: it has seven tests in
# `rustconn-core/src/connection/ssh_inheritance.rs` and they all passed. What was
# untested is the *wiring* — that a launcher calls the resolver at all — and it
# cannot easily be tested, because these paths need a GTK window, an AppState and
# a real bastion. So the invariant is checked mechanically instead. That is the
# same reasoning as `check-potfiles.sh`: the thing that rots is a connection
# between two files, and grep can see it while a unit test cannot.
#
# What is allowed, and why:
#
#   * `window/protocols.rs` — defines `resolve_first_hop_id`, the one sanctioned
#     reader, and walks *outward* from an already-resolved hop, where reading the
#     hop's own field is correct (see `resolve_jump_chain`'s docs).
#   * `window/protocols_ssh.rs` — same outward walk, hop to hop.
#   * everything outside `rustconn/src/window/` — editors, wizards, the tunnel
#     builder and the settings pages read and write the field because that is
#     their job.
#
# Exit codes:
#   0 — no launch path reads the field directly
#   1 — one does, or the sanctioned reader has gone missing
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

RESOLVER="rustconn/src/window/protocols.rs"
status=0

# The sanctioned reader must exist. Without this check the gate would pass
# trivially if someone deleted the helper and inlined the raw reads elsewhere.
# `\(` anchors the match to the declaration rather than to a prefix of it: a
# rename to `resolve_first_hop_id_v2` used to satisfy a bare substring search,
# which made this check pass while claiming to have verified something.
if ! grep -qE 'pub fn resolve_first_hop_id\(' "$RESOLVER"; then
    printf 'FAIL: resolve_first_hop_id is gone from %s.\n' "$RESOLVER" >&2
    printf '      Launch paths have no sanctioned way to resolve a bastion.\n' >&2
    exit 1
fi

# Files under window/ that are allowed to read the field, for the reasons above.
# Anything else in that directory is a launch path as far as this gate is
# concerned.
allowed_outward_walk=(
    "rustconn/src/window/protocols.rs"
    "rustconn/src/window/protocols_ssh.rs"
)

is_allowed() {
    local candidate="$1" allowed
    for allowed in "${allowed_outward_walk[@]}"; do
        [ "$candidate" = "$allowed" ] && return 0
    done
    return 1
}

offenders=""
while IFS= read -r file; do
    is_allowed "$file" && continue
    # `\.jump_host_id` only: a struct literal (`jump_host_id: …`) is a write, and
    # writes are what the editors legitimately do.
    if hits="$(grep -nE '\.jump_host_id' "$file")"; then
        offenders="$offenders$file\n$hits\n"
        status=1
    fi
done < <(find rustconn/src/window -name '*.rs' -type f | LC_ALL=C sort)

if [ "$status" -ne 0 ]; then
    printf 'FAIL: a launch path reads jump_host_id off a protocol config:\n\n' >&2
    printf '%b' "$offenders" >&2
    printf '\nUse rustconn::window::protocols::resolve_first_hop_id instead, so a\n' >&2
    printf 'bastion set on a group or in Preferences reaches the launcher. Reading\n' >&2
    printf 'the field directly is issue #301, which survived the release that\n' >&2
    printf 'claimed to fix it.\n' >&2
    exit 1
fi

printf 'OK: every launch path resolves its bastion through resolve_first_hop_id.\n'
