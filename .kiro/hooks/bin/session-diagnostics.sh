#!/usr/bin/env bash
# Stop: report what THIS session changed, and only that.
#
# Replaces the mechanical half of an agent prompt. The prompt used to be:
#
#   run `git diff --name-only HEAD`, run getDiagnostics on up to 10 .rs files,
#   then run `git diff HEAD` and scan for dbg!/todo!/println!/eprintln!
#
# which is two shell commands plus up to ten tool calls on every Stop, executed
# by the model, against the whole dirty tree rather than the session. This script
# does the file selection and the leftover scan; the agent is left with the one
# step a script cannot do, which is calling getDiagnostics.
#
# Output contract, read by the Stop hook prompt:
#
#   (nothing)            nothing changed -> the agent says nothing
#   RS_CHANGED: a.rs b.rs   run getDiagnostics on exactly these
#   LEFTOVER: path:line: …  a debug leftover on a line this session added
#
# Silent and exit 0 when clean, so a quiet session costs one shell call and no
# reasoning. Fails open: no baseline, no git, no session scope -> say nothing
# rather than fall back to reporting the whole tree, which is the bug this
# replaces.

set -uo pipefail

trap 'exit 0' ERR

repo=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$repo" 2>/dev/null || exit 0

baseline="target/.kiro-session-baseline"
[ -f "$baseline" ] || exit 0

# Current dirty set, same definition the baseline used.
current=$(
    {
        git diff --name-only HEAD 2>/dev/null
        git ls-files --others --exclude-standard 2>/dev/null
    } | sort -u
)
[ -n "$current" ] || exit 0

changed=""
while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ -f "$f" ] || continue
    now=$(sha256sum -- "$f" 2>/dev/null | cut -d' ' -f1)
    was=$(awk -F'\t' -v p="$f" '$2 == p { print $1; exit }' "$baseline" 2>/dev/null)
    # Unchanged since the session started -> not this session's business, even
    # though it is dirty in the working tree.
    [ "$now" = "$was" ] && continue
    changed="$changed$f"$'\n'
done <<<"$current"

changed=$(printf '%s' "$changed" | sed '/^$/d')
[ -n "$changed" ] || exit 0

rs_files=$(printf '%s\n' "$changed" | grep '\.rs$' || true)

# --- Debug leftovers, scoped to lines this session added ---------------------
#
# `git diff` covers tracked files; an untracked new file has no diff, so its
# whole content counts as added. Both are scanned, .rs only: a `println!` in a
# markdown code block is documentation, and rustconn-cli prints on purpose.
leftovers=""
if [ -n "$rs_files" ]; then
    pattern='dbg!|todo!|unimplemented!|println!|eprintln!|allow\(dead_code\)'
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        if git ls-files --error-unmatch -- "$f" >/dev/null 2>&1; then
            hits=$(git diff HEAD -- "$f" 2>/dev/null |
                grep -nE "^\+" |
                grep -E "$pattern" || true)
        else
            hits=$(grep -nE "$pattern" -- "$f" 2>/dev/null || true)
        fi
        [ -n "$hits" ] || continue
        while IFS= read -r line; do
            [ -n "$line" ] || continue
            leftovers="$leftovers  $f: ${line}"$'\n'
        done <<<"$hits"
    done <<<"$rs_files"
fi

# --- Report ------------------------------------------------------------------

if [ -n "$rs_files" ]; then
    # The prompt's old cap of 10 files, kept: getDiagnostics on a 40-file change
    # is not what a Stop hook is for.
    count=$(printf '%s\n' "$rs_files" | wc -l | tr -d ' ')
    printf 'RS_CHANGED: %s\n' "$(printf '%s\n' "$rs_files" | head -10 | tr '\n' ' ')"
    if [ "$count" -gt 10 ]; then
        printf 'NOTE: %s .rs files changed this session, showing the first 10.\n' "$count"
    fi
fi

if [ -n "$leftovers" ]; then
    printf 'LEFTOVER: debug macros on lines this session added:\n'
    printf '%s' "$leftovers"
fi

exit 0
