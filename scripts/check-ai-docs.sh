#!/usr/bin/env bash
# Verifies that the counts docs/AI_DEVELOPMENT.md asserts still match .kiro/.
#
# That document opens by declaring the files in `.kiro/steering/` and `.kiro/hooks/`
# to be the authoritative inventory, and then states how many of each there are.
# Both numbers went stale anyway: on 2026-08-26 it claimed 14 steering files and 20
# hooks against an actual 27 and 16. A hand-maintained count inside a document that
# warns against hand-maintained inventories is the same failure the document is
# about.
#
# This is deliberately narrow. It checks the two numbers, not the prose, because a
# gate that tries to verify prose fails on every legitimate edit and gets switched
# off. If a count is wrong, the fix is one number.
#
# Exit codes:
#   0 — both counts match
#   1 — a count is stale, or the sentence it lives in has changed shape
#
# Run it from a hook, from CI, or by hand. It needs nothing but the repo.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

doc="docs/AI_DEVELOPMENT.md"
if [ ! -f "$doc" ]; then
    printf 'FAIL: %s not found\n' "$doc" >&2
    exit 1
fi

status=0

# actual_count <glob-dir> <pattern>
actual_steering=$(find .kiro/steering -maxdepth 1 -name '*.md' -type f 2>/dev/null | wc -l | tr -d ' ')
actual_hooks=$(find .kiro/hooks -maxdepth 1 -name '*.json' -type f 2>/dev/null | wc -l | tr -d ' ')

# claimed <regex-prefix>  -> the bolded number in that sentence
claimed() {
    sed -nE "s/.*$1.*\*\*([0-9]+)\*\*.*/\1/p" "$doc" | head -1
}

claimed_steering=$(claimed 'kiro\/steering\/. currently holds')
claimed_hooks=$(claimed 'kiro\/hooks\/. currently holds')

check() {
    # check <label> <claimed> <actual> <hint>
    local label="$1" want="$2" got="$3" hint="$4"
    if [ -z "$want" ]; then
        printf 'FAIL: %s — could not find the count sentence in %s.\n' "$label" "$doc" >&2
        printf '      Expected a line like: `.kiro/%s/` currently holds **N** …\n' "$hint" >&2
        printf '      Either restore that shape or drop this check from the script.\n' >&2
        status=1
        return
    fi
    if [ "$want" != "$got" ]; then
        printf 'FAIL: %s — %s says %s, actual is %s.\n' "$label" "$doc" "$want" "$got" >&2
        status=1
        return
    fi
    printf 'ok: %s (%s)\n' "$label" "$got"
}

check 'steering file count' "$claimed_steering" "$actual_steering" 'steering'
check 'hook count' "$claimed_hooks" "$actual_hooks" 'hooks'

# The two checks above catch a wrong *number*. They cannot catch a hook that no
# document describes, which is the more useful failure: steering `hooks-map.md`
# opens by calling itself a reference for all of `.kiro/hooks/*.json`, and on
# 2026-09-02 it covered 15 of 16. `session-baseline` had been missing since it
# landed on 2026-08-26, and the file had no SessionStart section at all.
#
# Name-presence only, deliberately. Verifying that a row is *accurate* means
# parsing matchers and latencies out of a markdown table, which breaks on every
# legitimate edit; a check that fires on rename or deletion is the part worth
# automating.
map=".kiro/steering/hooks-map.md"
if [ ! -f "$map" ]; then
    printf 'FAIL: hook documentation coverage — %s not found.\n' "$map" >&2
    status=1
else
    undocumented=""
    while IFS= read -r hook; do
        name=$(basename "$hook" .json)
        grep -qF "$name" "$map" || undocumented="$undocumented $name"
    done < <(find .kiro/hooks -maxdepth 1 -name '*.json' -type f 2>/dev/null | sort)

    if [ -n "$undocumented" ]; then
        printf 'FAIL: hook documentation coverage — %s has no row for:%s\n' "$map" "$undocumented" >&2
        printf '      Add one to the section for its trigger, or delete the hook.\n' >&2
        status=1
    else
        printf 'ok: hook documentation coverage (%s hooks, all in hooks-map.md)\n' "$actual_hooks"
    fi
fi

if [ "$status" -ne 0 ]; then
    printf '\n%s is not an inventory and should not try to be one. If keeping these\n' "$doc" >&2
    printf 'numbers current is not worth it, rewrite the sentences to stop asserting a\n' >&2
    printf 'number and delete the matching check here.\n' >&2
fi

exit "$status"
