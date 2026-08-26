#!/usr/bin/env bash
# Collects every `// ponytail:` marker in the workspace into one debt ledger,
# grouped by crate, and flags the ones that do not name both a ceiling and an
# upgrade path.
#
# A ponytail marker records a deliberate simplification. Project rules require it
# to name two things: the ceiling it is fine below, and what to do when the
# ceiling is reached. A marker with only the first half is how "later" becomes
# "never".
#
# This replaces the mechanical half of the `ponytail-debt` steering prompt, which
# had the agent run the grep, group the hits and count them by hand on every
# invocation. What is left to a reader is the one thing a script cannot do:
# decide whether a stated ceiling is still honest.
#
# Read-only. Never edits code.
#
# Usage:
#   scripts/ponytail-ledger.sh            report, always exit 0
#   scripts/ponytail-ledger.sh --strict   exit 1 if any marker is incomplete
#
# Markers wrap. Most of them continue onto the following comment lines, so a
# line-at-a-time check reports almost everything as incomplete — the first
# version of this script did exactly that. The block is reassembled before it is
# judged.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

strict=0
case "${1:-}" in
--strict) strict=1 ;;
'') ;;
*)
    printf 'usage: %s [--strict]\n' "$0" >&2
    exit 2
    ;;
esac

# Every crate that may contain one. Derived from the workspace members rather
# than hardcoded, so a new crate is covered the day it is added.
mapfile -t src_dirs < <(
    awk '/^members = \[/,/^\]/' Cargo.toml |
        sed -n 's/^[[:space:]]*"\([^"]*\)".*/\1/p' |
        while IFS= read -r m; do
            [ -d "$m/src" ] && printf '%s/src\n' "$m"
        done
)

if [ "${#src_dirs[@]}" -eq 0 ]; then
    printf 'FAIL: no crate src/ directories found — is this the repo root?\n' >&2
    exit 1
fi

ledger=$(
    for dir in "${src_dirs[@]}"; do
        crate=${dir%/src}
        find "$dir" -name '*.rs' -type f -print0 2>/dev/null |
            xargs -0 -r awk -v crate="$crate" '
            function flush() {
                if (start == 0) return
                gsub(/[[:space:]]+/, " ", text)
                sub(/^ /, "", text)
                sub(/ $/, "", text)
                # A complete marker names a ceiling and then what to do about
                # it. In practice the two halves are separated by a semicolon or
                # a sentence break. This is a heuristic, and the report says so.
                complete = (text ~ /;/ || text ~ /\. [A-Z]/) ? "OK  " : "FLAG"
                printf "%s\t%s\t%s:%d\t%s\n", complete, crate, FILENAME, start, text
                start = 0
                text = ""
            }
            {
                line = $0
            }
            /ponytail:/ {
                flush()
                start = FNR
                sub(/^.*ponytail:[[:space:]]*/, "", line)
                text = line
                capturing = 1
                next
            }
            capturing == 1 {
                # Continuation: another comment line, no new marker, and not
                # blank once the comment leader is stripped.
                if (line ~ /^[[:space:]]*(\/\/\/|\/\/!|\/\/)/) {
                    sub(/^[[:space:]]*(\/\/\/|\/\/!|\/\/)[[:space:]]*/, "", line)
                    if (line != "") {
                        text = text " " line
                        next
                    }
                }
                flush()
                capturing = 0
            }
            END { flush() }
        '
    done | sort
)

if [ -z "$ledger" ]; then
    printf 'No ponytail markers found.\n'
    exit 0
fi

total=$(printf '%s\n' "$ledger" | wc -l | tr -d ' ')
flagged=$(printf '%s\n' "$ledger" | grep -c '^FLAG' || true)

printf 'Ponytail debt ledger — %s markers in %s crates\n\n' \
    "$total" "$(printf '%s\n' "$ledger" | cut -f2 | sort -u | wc -l | tr -d ' ')"

current=""
while IFS=$'\t' read -r state crate loc text; do
    if [ "$crate" != "$current" ]; then
        [ -n "$current" ] && printf '\n'
        printf '── %s\n' "$crate"
        current="$crate"
    fi
    printf '  [%s] %s\n         %s\n' "$state" "$loc" "$text"
done <<<"$ledger"

printf '\n%s markers, %s flagged as missing a ceiling or an upgrade path.\n' \
    "$total" "$flagged"

if [ "$flagged" -gt 0 ]; then
    printf 'FLAG is a heuristic: it fires when the text has no ";" and no sentence\n'
    printf 'break, which is where the two halves normally separate. Read the flagged\n'
    printf 'ones before believing them — and read the OK ones before trusting the\n'
    printf 'ceiling they claim, which no script can check.\n'
fi

if [ "$strict" -eq 1 ] && [ "$flagged" -gt 0 ]; then
    exit 1
fi

exit 0
