#!/usr/bin/env bash
# Fails if any translation catalogue has fuzzy or untranslated entries.
#
# Why this is worth a CI gate: gettext ignores a fuzzy entry and falls back to
# the English msgid, so a fuzzy translation renders exactly like a missing one —
# but `msgfmt --statistics` still counts the catalogue as populated, and a
# reviewer skimming "2401 translated" sees nothing wrong.
#
# It also hides actively wrong text. After the 0.19.10 logging work, msgmerge
# fuzzy-matched 7 new strings in 15 locales and guessed badly: "Play on the
# remote computer" came out as the German for "Trigger only once, then remove
# the rule". Nothing flagged it; it was found by accident.
#
# Usage:
#   scripts/check-po-complete.sh            # every locale must be complete
#   scripts/check-po-complete.sh uk         # only the named locales

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

if ! command -v msgfmt >/dev/null 2>&1; then
    printf 'FAIL: msgfmt not found (install gettext)\n' >&2
    exit 1
fi

catalogues=()
if [ "$#" -gt 0 ]; then
    for lang in "$@"; do
        catalogues+=("po/${lang}.po")
    done
else
    catalogues=(po/*.po)
fi

status=0

for po in "${catalogues[@]}"; do
    if [ ! -f "$po" ]; then
        printf 'FAIL: %s not found\n' "$po" >&2
        status=1
        continue
    fi

    # LC_ALL=C forces the summary into English so it can be parsed; msgfmt
    # otherwise translates it according to the ambient locale.
    #
    # --check also catches malformed catalogues and placeholder mismatches
    # between msgid and msgstr, so this one call covers both concerns.
    if ! stats=$(LC_ALL=C msgfmt --check --check-format --statistics -o /dev/null "$po" 2>&1); then
        printf 'FAIL: %s does not pass msgfmt --check\n' "$po" >&2
        printf '%s\n' "$stats" | sed 's/^/  /' >&2
        status=1
        continue
    fi

    # msgfmt mentions fuzzy/untranslated only when there are any, and the
    # wording varies with plural forms — so test for the words, not the counts.
    # Counting markers in the file directly does NOT work: a translated entry
    # whose text is wrapped begins with a bare `msgstr ""` continuation line,
    # which is indistinguishable from a genuinely empty one by grep alone.
    if printf '%s' "$stats" | grep -qE 'fuzzy|untranslated'; then
        printf 'FAIL: %s is incomplete — %s\n' "$po" "$stats" >&2
        status=1
    fi
done

if [ "$status" -eq 0 ]; then
    printf 'OK: %d catalogue(s) complete — no fuzzy, no untranslated\n' "${#catalogues[@]}"
else
    printf '\nFuzzy entries render as English at runtime even though the catalogue\n' >&2
    printf 'reports them as present. Review them with:\n' >&2
    printf '  msgattrib --only-fuzzy --no-obsolete po/<lang>.po\n' >&2
    printf '  msgattrib --untranslated --no-obsolete po/<lang>.po\n' >&2
fi

exit "$status"
