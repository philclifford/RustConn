#!/usr/bin/env bash
# PreToolUse guard: block writes that violate the two hardest-to-reverse
# architectural invariants.
#
#   1. rustconn-core / rustconn-cli are GUI-free — no gtk4 / adw / vte4.
#   2. `unsafe` lives only in rustconn-pty-sys (unsafe_code = "forbid"
#      workspace-wide, Cargo.toml).
#
# Reads the PreToolUse JSON on stdin, exits 2 with an explanation on stderr to
# block, exits 0 silently otherwise.
#
# Why a command hook and not an agent prompt: an agent action cannot decide, so
# every write was cancelled, acknowledged and re-issued — two round trips per
# edit, on every edit. This does the same two greps for free.
#
# Fails OPEN. A guard that blocks every write because jq changed its output
# format would be far worse than one that occasionally misses a violation, and
# both invariants are also hard compile errors, so the build is the real net.

set -uo pipefail

# Anything unexpected -> allow the write.
trap 'exit 0' ERR

payload=$(cat) || exit 0
command -v jq >/dev/null 2>&1 || exit 0

# fs_write / fs_append use `path`; delete_file uses `targetFile`.
path=$(printf '%s' "$payload" | jq -r '.tool_input.path // .tool_input.targetFile // ""' 2>/dev/null) || exit 0
[ -n "$path" ] || exit 0
case "$path" in
*.rs) ;;
*) exit 0 ;;
esac

# fs_write / fs_append carry `text`, str_replace carries `newStr`.
# delete_file has neither: nothing is being introduced, so nothing to check.
content=$(printf '%s' "$payload" | jq -r '.tool_input.text // .tool_input.newStr // ""' 2>/dev/null) || exit 0
[ -n "$content" ] || exit 0

# Normalise to a repo-relative path so the crate prefixes below match whether
# the caller passed an absolute or a relative path.
rel=${path#"$PWD"/}
rel=${rel##*/RustConn/}

fail() {
    printf 'crate-boundary-guard: %s\n' "$1" >&2
    printf '  file: %s\n' "$rel" >&2
    printf '  offending line(s):\n' >&2
    printf '%s\n' "$2" | sed 's/^/    /' >&2
    exit 2
}

# --- Invariant 1: no GUI toolkit in the GUI-free crates ---------------------
# Their own tests/ are exempt, matching the original guard.
case "$rel" in
rustconn-core/tests/* | rustconn-cli/tests/*) ;;
rustconn-core/* | rustconn-cli/*)
    gui=$(printf '%s' "$content" | grep -nE '(^|[^[:alnum:]_])(use[[:space:]]+(gtk4|adw|libadwaita|vte4)|(gtk4|adw|vte4)::)' || true)
    if [ -n "$gui" ]; then
        fail 'GUI toolkit import in a GUI-free crate. Move this code to rustconn/.' "$gui"
    fi
    ;;
esac

# --- Invariant 2: unsafe only in the sanctioned FFI crate -------------------
case "$rel" in
rustconn-pty-sys/*) ;;
*)
    bad=$(printf '%s' "$content" | grep -nE '(^|[^[:alnum:]_])unsafe[[:space:]]+(fn|impl|trait)|(^|[^[:alnum:]_])unsafe[[:space:]]*\{' || true)
    if [ -n "$bad" ]; then
        fail 'unsafe outside rustconn-pty-sys (unsafe_code = "forbid"). New FFI belongs in a dedicated rustconn-*-sys crate.' "$bad"
    fi
    ;;
esac

exit 0
