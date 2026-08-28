#!/usr/bin/env bash
# Verifies that po/rustconn.pot still describes the strings in the sources.
#
# This is the gate the project went three releases without, and paid for each
# time. 0.20.0 found seven translatable strings that were in the source and in no
# catalogue, and recorded in its own changelog that "nothing yet checks that the
# committed POT matches the sources; that gap is real and outlives this fix".
# 0.20.1 found three more and repeated the note verbatim. 0.20.2 found two from
# PR #286. Every one of them rendered in English in every locale while
# check-po-complete.sh reported all catalogues at 100%, because that gate reads
# the committed .po files and never regenerates the template to compare against.
# A string missing from the template is invisible to every other check there is.
#
# What this does NOT check: `#:` source references. They move on every code edit
# and say nothing about whether a string is translatable, so comparing them would
# make the gate fail on unrelated work and get it switched off. Only the set of
# msgids matters here.
#
# Exit codes:
#   0 — the template covers exactly the strings the sources contain
#   1 — drift, or a required tool is missing
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

POT="po/rustconn.pot"

for tool in xgettext msgcat; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'FAIL: %s not found — install gettext.\n' "$tool" >&2
        # Deliberately a failure and not a skip. A gate that reports success
        # because its tool is absent is worse than no gate: it is believed.
        exit 1
    fi
done

if [ ! -f "$POT" ]; then
    printf 'FAIL: %s not found\n' "$POT" >&2
    exit 1
fi

tmpdir=$(mktemp -d) || exit 1
trap 'rm -rf "$tmpdir"' EXIT
fresh="$tmpdir/fresh.pot"

# Reuse the real extractor rather than repeating its keyword list. Two copies of
# that list is how a string ends up extracted by one and missed by the other,
# which is the exact failure this gate exists to catch.
if ! bash po/update-pot.sh "$fresh" >"$tmpdir/extract.log" 2>&1; then
    printf 'FAIL: po/update-pot.sh could not extract the strings:\n' >&2
    cat "$tmpdir/extract.log" >&2
    exit 1
fi

# Normalise both templates before comparing. `--no-wrap` is what makes this
# exact: it puts each msgid on a single line, so the msgid set can be compared
# line by line even though a msgid may span many lines in the file, carry
# escapes, or be one half of a plural pair. `--sort-output` removes the entry
# ordering from the comparison, and `--no-location`/`--omit-header` remove the
# two things that change on every run without meaning anything.
#
# `msgcomm --unique` looks like the tool for this and is not: it prints the
# messages that appear in exactly one input, i.e. the symmetric difference, so
# calling it twice with the arguments swapped returns the same set both times
# and cannot say which side a string is missing from.
# The header entry is `msgid ""` in both files and cancels out, so it needs no
# special handling — which is just as well, because msgcat has no --omit-header.
ids() {
    if ! msgcat --no-wrap --sort-output --no-location -o "$2" "$1"; then
        printf 'FAIL: msgcat could not read %s\n' "$1" >&2
        exit 1
    fi
    grep '^msgid ' "$2" | LC_ALL=C sort -u >"$2.ids"
    # A guard against passing for the wrong reason. If an option name changes or
    # a pipe breaks, both sides come back empty, the sets match, and the gate
    # reports success without having compared anything — the failure mode this
    # whole gate was written to eliminate, reproduced inside the gate. It has
    # already happened once here: msgcat rejected --omit-header, both sides
    # yielded nothing, and a source string that was genuinely missing from the
    # template passed.
    if [ ! -s "$2.ids" ]; then
        printf 'FAIL: extracted no msgids from %s — the comparison would be vacuous.\n' "$1" >&2
        exit 1
    fi
}

ids "$fresh" "$tmpdir/fresh.norm"
ids "$POT" "$tmpdir/pot.norm"
mv "$tmpdir/fresh.norm.ids" "$tmpdir/fresh.ids"
mv "$tmpdir/pot.norm.ids" "$tmpdir/pot.ids"

missing=$(LC_ALL=C comm -23 "$tmpdir/fresh.ids" "$tmpdir/pot.ids")
stale=$(LC_ALL=C comm -13 "$tmpdir/fresh.ids" "$tmpdir/pot.ids")

status=0

if [ -n "$missing" ]; then
    count=$(printf '%s\n' "$missing" | grep -c .)
    printf 'FAIL: %d translatable string(s) are in the sources but not in %s.\n' \
        "$count" "$POT" >&2
    printf '      They render in English in every locale, and every other i18n\n' >&2
    printf '      gate reports the catalogues as complete.\n\n' >&2
    printf '%s\n\n' "$missing" >&2
    status=1
fi

if [ -n "$stale" ]; then
    count=$(printf '%s\n' "$stale" | grep -c .)
    printf 'FAIL: %d string(s) in %s no longer exist in the sources.\n' \
        "$count" "$POT" >&2
    printf '      Every catalogue carries a translation for something unreachable.\n\n' >&2
    printf '%s\n\n' "$stale" >&2
    status=1
fi

if [ "$status" -ne 0 ]; then
    printf 'Fix: run `bash po/update-pot.sh`, then merge the catalogues:\n' >&2
    printf '  for f in po/*.po; do msgmerge --update --no-fuzzy-matching "$f" %s; done\n' "$POT" >&2
    printf '\n--no-fuzzy-matching on purpose: a guessed translation counts as\n' >&2
    printf 'translated and still renders as the wrong sentence.\n' >&2
    exit 1
fi

ok_count=$(grep -c '^msgid ' "$POT")
printf 'OK: %s covers the sources exactly (%s entries).\n' "$POT" "$ok_count"
