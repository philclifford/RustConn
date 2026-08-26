#!/usr/bin/env bash
# SessionStart: record the working tree so the Stop hook can tell what THIS
# session changed.
#
# Why this exists: the Stop hook used to ask the agent to run
# `git diff --name-only HEAD`, which lists the whole dirty tree — every file
# carrying uncommitted work from before the session started. On 2026-08-26 that
# made three consecutive Stop hooks run getDiagnostics over 13 .rs files in a
# session whose only edit was a markdown file. Three round-trips to the model to
# learn that nothing had changed.
#
# Content hashes rather than a diff, deliberately: a commit during the session
# moves HEAD and invalidates any diff-based baseline, but the hash of a file on
# disk does not care where HEAD points.
#
# Silent always. SessionStart forwards stdout to the agent, and a baseline is not
# something the agent needs to hear about.

set -uo pipefail

# Fail open: a broken baseline must not block a session from starting.
trap 'exit 0' ERR

repo=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$repo" 2>/dev/null || exit 0

baseline="target/.kiro-session-baseline"
mkdir -p target 2>/dev/null || exit 0

{
    # Tracked files with uncommitted modifications, plus untracked ones. Both
    # count as "already dirty before the session" and must not be reported later.
    git diff --name-only HEAD 2>/dev/null
    git ls-files --others --exclude-standard 2>/dev/null
} | sort -u | while IFS= read -r f; do
    [ -f "$f" ] || continue
    printf '%s\t%s\n' "$(sha256sum -- "$f" 2>/dev/null | cut -d' ' -f1)" "$f"
done >"$baseline.tmp" 2>/dev/null

mv -f "$baseline.tmp" "$baseline" 2>/dev/null || rm -f "$baseline.tmp" 2>/dev/null

exit 0
