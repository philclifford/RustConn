#!/usr/bin/env bash
# PostFileSave check for GUI sources: keep the translation pipeline honest.
#
#   1. A file with i18n() calls must be listed in po/POTFILES.in, or its strings
#      are never extracted and stay untranslated in every locale.
#   2. No Rust-only \u{...} escape inside a translatable literal. xgettext runs
#      with --language=C, cannot decode it, and copies it verbatim into the
#      msgid — so the runtime lookup never matches and the string renders
#      untranslated everywhere while the .po files still report 100% complete.
#
# Reads the PostFileSave JSON on stdin. Silent and exit 0 when clean; prints to
# stdout (forwarded to the agent) only when something needs a decision.
#
# Was an agent prompt, which meant the model ran these three greps by hand after
# every single .rs save. Nothing here needs judgement except adding a POTFILES
# line, which is the one thing left to the agent.

set -uo pipefail

trap 'exit 0' ERR

payload=$(cat) || exit 0
command -v jq >/dev/null 2>&1 || exit 0

file=$(printf '%s' "$payload" | jq -r '.file_path // ""' 2>/dev/null) || exit 0
[ -n "$file" ] || exit 0

repo=$(printf '%s' "$payload" | jq -r '.cwd // ""' 2>/dev/null)
if [ -n "$repo" ]; then
    cd "$repo" 2>/dev/null || true
fi

rel=${file#"$PWD"/}
rel=${rel##*/RustConn/}

# Only GUI sources carry translatable strings.
case "$rel" in
rustconn/src/*.rs) ;;
*) exit 0 ;;
esac

[ -f "$rel" ] || exit 0

# No translatable strings -> nothing to keep in sync.
grep -q 'i18n(' "$rel" || exit 0

if [ -f po/POTFILES.in ] && ! grep -qxF "$rel" po/POTFILES.in; then
    printf 'translation-sync: %s calls i18n() but is not listed in po/POTFILES.in.\n' "$rel"
    printf '  Its strings will never be extracted. Add it in alphabetical order,\n'
    printf '  then run: bash po/update-pot.sh\n'
    printf '  (Skip if this is a scratch file about to be deleted — a POTFILES entry\n'
    printf '  pointing at a missing file breaks po/update-pot.sh.)\n'
fi

if [ -x scripts/check-i18n-escapes.sh ] && ! escapes=$(./scripts/check-i18n-escapes.sh 2>&1); then
    printf 'translation-sync: check-i18n-escapes.sh FAILED\n'
    printf '%s\n' "$escapes"
    printf '  Put the character directly in the literal instead of a \\u{...} escape\n'
    printf '  (ASCII apostrophe is the project convention, cf. the Save prompt in alert.rs).\n'
fi

exit 0
