#!/usr/bin/env bash
# Verifies that every source file listed in po/POTFILES.in still exists, and
# that every GUI source calling i18n() is listed.
#
# Both directions rot silently:
#
#   * A stale entry pointing at a deleted file breaks `po/update-pot.sh`.
#     Three such entries (empty_state.rs, dashboard.rs, embedded_spice.rs)
#     survived in the list long after the files were gone, and nothing noticed
#     until someone tripped over it by hand.
#   * A missing entry means the file's strings are never extracted, so they
#     render untranslated in every locale while every catalogue still reports
#     100% complete — the worst kind of failure, because it looks fine.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

potfiles="po/POTFILES.in"
if [ ! -f "$potfiles" ]; then
    printf 'FAIL: %s not found\n' "$potfiles" >&2
    exit 1
fi

status=0

# --- Direction 1: every listed file must exist ------------------------------
missing=0
while IFS= read -r entry; do
    case "$entry" in
    '' | '#'*) continue ;;
    esac
    if [ ! -f "$entry" ]; then
        printf 'FAIL: POTFILES.in lists a file that does not exist: %s\n' "$entry" >&2
        missing=$((missing + 1))
    fi
done <"$potfiles"

if [ "$missing" -gt 0 ]; then
    printf '  %d stale entries. Remove them: po/update-pot.sh cannot scan a missing file.\n' \
        "$missing" >&2
    status=1
fi

# --- Direction 2: every GUI source with i18n() must be listed --------------
# Scoped to rustconn/src, which is what po/update-pot.sh reads POTFILES for.
unlisted=0
while IFS= read -r src; do
    grep -q 'i18n(' "$src" || continue
    if ! grep -qxF "$src" "$potfiles"; then
        printf 'FAIL: %s calls i18n() but is not listed in POTFILES.in\n' "$src" >&2
        unlisted=$((unlisted + 1))
    fi
done < <(find rustconn/src -name '*.rs' -type f | sort)

if [ "$unlisted" -gt 0 ]; then
    printf '  %d unlisted files. Their strings are never extracted and stay\n' "$unlisted" >&2
    printf '  untranslated in every locale while the catalogues report 100%%.\n' >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    listed=$(grep -cvE '^[[:space:]]*(#|$)' "$potfiles")
    printf 'OK: POTFILES.in is consistent (%s entries, all present; no unlisted i18n sources)\n' "$listed"
fi

exit "$status"
