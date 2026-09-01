#!/usr/bin/env bash
# PostFileSave hook body for Cargo.lock: check the lockfile against the RustSec
# advisory database.
#
# This used to be a one-liner inside cargo-security-scan.json:
#
#   cargo deny check advisories 2>/dev/null \
#     || cargo audit 2>/dev/null \
#     || echo 'Neither cargo-deny nor cargo-audit installed. Run: cargo install cargo-deny'
#
# which could not report the one thing it existed to report. Three separate
# faults, all measured on 2026-09-02:
#
#   1. `||` conflates "the tool failed" with "the tool is absent". cargo-deny
#      exits non-zero *when it finds an advisory*, so a real finding fell through
#      to cargo-audit, which also exited non-zero, which printed "Neither
#      cargo-deny nor cargo-audit installed". Both were installed. The message
#      naming a configuration problem was in fact the vulnerability report.
#   2. `2>/dev/null` discarded the finding itself. cargo-deny writes its
#      diagnostics to stderr — 0 bytes on stdout against 96 on stderr for a
#      trivial failure — so the redirect threw away every line that names a CVE
#      and kept only the exit code, which fault 1 then misread.
#   3. `cargo deny` went through the cargo proxy, which makes rust-toolchain.toml
#      resolve a whole toolchain for a check that only parses Cargo.lock and the
#      advisory database. ci.yml already avoids this for cargo-machete, with the
#      reason written out; this is the same call.
#
# So: probe for the tool with `command -v`, keep both streams, judge on the exit
# code, and invoke the bare binary. Findings also go to a log under target/ —
# gitignored, and durable regardless of whether a PostFileSave hook's stdout
# reaches the model, which the SessionStart/UserPromptSubmit/PreToolUse
# forwarding rule does not promise.
#
# Never blocks: this is a report, and exit 2 means "block" only for PreToolUse
# anyway. Fails open like every other hook here.

set -uo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)" || exit 0

# Nothing to check if the lockfile matches HEAD — the save was a no-op edit, or
# a checkout. Keeps the hook silent on the common case.
git diff --quiet HEAD -- Cargo.lock 2>/dev/null && exit 0

log=target/cargo-advisories.log
mkdir -p target 2>/dev/null || exit 0

# deny.toml is the single source of truth for the ignore list, so cargo-deny is
# preferred; cargo-audit does not read it and will re-report accepted advisories.
if command -v cargo-deny >/dev/null 2>&1; then
    tool=cargo-deny
    cargo-deny check advisories >"$log" 2>&1
    rc=$?
elif command -v cargo-audit >/dev/null 2>&1; then
    tool=cargo-audit
    printf 'note: cargo-deny is absent, so deny.toml ignores are NOT applied\n' >"$log"
    cargo-audit audit >>"$log" 2>&1
    rc=$?
else
    printf 'cargo-security-scan: neither cargo-deny nor cargo-audit is installed.\n'
    printf '  Install the one that reads deny.toml: cargo install cargo-deny\n'
    exit 0
fi

[ "$rc" -eq 0 ] && exit 0

printf 'cargo-security-scan: %s exited %d for the updated Cargo.lock.\n' "$tool" "$rc"
printf '  Full output: %s\n\n' "$log"
cat "$log" 2>/dev/null
exit 0
