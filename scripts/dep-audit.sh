#!/usr/bin/env bash
# The mechanical half of a dependency audit: crates, advisories, bundled CLI
# tools. One report, one file, no judgement.
#
# Steering `dependency-audit.md` has six steps. The first three are commands whose
# output needs classifying, which is this script. Steps 4 and 5 — the GNOME
# runtime on Flathub, the pinned FreeRDP and cJSON tarballs, the snap base and its
# gnome extension — need a web lookup and a decision about whether a bump is worth
# taking, and stay with whoever is reading this report.
#
# Read-only. Never edits a manifest, never writes Cargo.lock: `cargo update` runs
# with --dry-run throughout.
#
# Usage:
#   scripts/dep-audit.sh              report to stdout and target/dep-audit.txt
#   scripts/dep-audit.sh --quiet      write the file, print only the summary
#
# ── On classifying cargo's output ────────────────────────────────────────────
#
# `cargo update --dry-run` reports what would move *inside* the declared semver
# ranges. What it holds back is only visible with --verbose, as
#
#     Unchanged quick-xml v0.41.0 (available: v0.42.0)
#
# and those are the interesting ones: a bump that needs a human to widen a
# requirement. This script classifies them by which component of the version
# actually differs, and separates two cases that look alike and are not:
#
#   * a pre-release pin (v5.0.0-rc.1 while v5.0.0 exists) is almost always a
#     transitive dependency holding the whole tree back, not a decision anyone in
#     this repo made;
#   * a patch-level hold (toml v0.8.2 while v0.8.23 exists) means something pins
#     it explicitly, because a patch update needs no requirement change. That is
#     worth looking at.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

quiet=0
case "${1:-}" in
--quiet) quiet=1 ;;
'') ;;
-h | --help)
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
*)
    printf 'usage: %s [--quiet]\n' "$0" >&2
    exit 2
    ;;
esac

CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
command -v "$CARGO" >/dev/null 2>&1 || CARGO=cargo

report="target/dep-audit.txt"
mkdir -p target
: >"$report"

emit() { printf '%s\n' "$*" >>"$report"; }

# Two cargo runs share the target-dir lock.
if pgrep -f "[c]argo (build|test|clippy|check|update)" >/dev/null 2>&1; then
    printf 'FAIL: another cargo run is in progress. Check: pgrep -af cargo\n' >&2
    exit 1
fi

emit "RustConn dependency audit — $(date -u '+%Y-%m-%d %H:%M UTC')"
emit "workspace version: $(awk -F'"' '/^\[workspace\.package\]/{p=1} p&&/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)"
emit ""

# ── 1. Cargo crates ──────────────────────────────────────────────────────────
emit "=============================================================="
emit "1. Cargo crates"
emit "=============================================================="

raw="target/dep-audit-cargo.log"
"$CARGO" update --dry-run --verbose >"$raw" 2>&1
inrange=$(sed -nE 's/^[[:space:]]*Locking ([0-9]+) packages.*/\1/p' "$raw" | head -1)
inrange=${inrange:-0}

emit ""
emit "In-range updates available (patch/minor inside the declared requirement): $inrange"
if [ "$inrange" != "0" ]; then
    emit "  Apply with: cargo update   (then record them under CHANGELOG ### Dependencies)"
fi

# Held back: classify by the component that differs.
emit ""
emit "Held back — a bump needs a requirement change or is pinned transitively:"
emit ""

awk '
    /^[[:space:]]*Unchanged / {
        name = $2
        cur  = $3
        # last field looks like "v0.42.0)"
        avail = $NF
        gsub(/[()v]/, "", avail)
        gsub(/^v/, "", cur)

        split(cur,   a, /[.-]/)
        split(avail, b, /[.-]/)

        prerelease = (cur ~ /-(rc|pre|beta|alpha)/)

        if (a[1] != b[1])      class = "MAJOR"
        else if (a[2] != b[2]) class = "minor"
        else if (a[3] != b[3]) class = "patch"
        else                   class = "prerel"

        if (prerelease && class != "MAJOR") class = "prerel"

        printf "%s\t%s\t%s\t%s\n", class, name, cur, avail
    }
' "$raw" | sort >"$raw.classified"

for class in MAJOR minor patch prerel; do
    hits=$(awk -F'\t' -v c="$class" '$1 == c' "$raw.classified")
    [ -n "$hits" ] || continue
    case "$class" in
    MAJOR) emit "  MAJOR — breaking; read the upstream changelog before taking it" ;;
    minor) emit "  minor — needs the requirement widened" ;;
    patch) emit "  patch held back — something pins it explicitly, worth a look" ;;
    prerel) emit "  pre-release pin — usually a transitive dependency, not our choice" ;;
    esac
    printf '%s\n' "$hits" | while IFS=$'\t' read -r _ name cur avail; do
        emit "      $name  $cur → $avail"
    done
    emit ""
done

if [ ! -s "$raw.classified" ]; then
    emit "  (nothing held back)"
    emit ""
fi

# ── 2. Security advisories ───────────────────────────────────────────────────
emit "=============================================================="
emit "2. Security advisories"
emit "=============================================================="
emit ""

adv="target/dep-audit-advisories.log"
if "$CARGO" deny --version >/dev/null 2>&1; then
    # deny.toml is the single source of truth for the RustSec ignore list.
    if "$CARGO" deny check advisories >"$adv" 2>&1; then
        emit "cargo deny check advisories: clean"
    else
        emit "cargo deny check advisories: FINDINGS"
        emit ""
        sed 's/^/    /' "$adv" >>"$report"
    fi
elif "$CARGO" audit --version >/dev/null 2>&1; then
    emit "(cargo-deny unavailable, fell back to cargo audit — deny.toml ignores do NOT apply)"
    if "$CARGO" audit >"$adv" 2>&1; then
        emit "cargo audit: clean"
    else
        emit "cargo audit: FINDINGS"
        sed 's/^/    /' "$adv" >>"$report"
    fi
else
    emit "SKIPPED: neither cargo-deny nor cargo-audit is installed."
fi
emit ""

# ── 3. Bundled CLI tools ─────────────────────────────────────────────────────
emit "=============================================================="
emit "3. Bundled CLI tools"
emit "=============================================================="
emit ""
if [ -x scripts/check-cli-versions.sh ]; then
    cli="target/dep-audit-cli.log"
    # Exit 1 means "updates available or an endpoint is unreachable", which is a
    # finding to report rather than a failure to abort on.
    scripts/check-cli-versions.sh >"$cli" 2>&1
    rc=$?
    sed 's/^/    /' "$cli" >>"$report"
    emit ""
    case "$rc" in
    0) emit "    all endpoints reachable, pinned tools current" ;;
    1) emit "    exit 1 — an update is available or an endpoint was unreachable" ;;
    *) emit "    exit $rc — script error (missing curl?)" ;;
    esac
else
    emit "SKIPPED: scripts/check-cli-versions.sh not found."
fi
emit ""

# ── What this script does not do ──────────────────────────────────────────────
emit "=============================================================="
emit "4. Still needs a human or an agent with web access"
emit "=============================================================="
emit ""
emit "  Flatpak  packaging/flatpak/*.yml and packaging/flathub/*.yml"
emit "             - org.gnome.Platform / org.gnome.Sdk runtime-version"
emit "             - org.freedesktop.Sdk.Extension.rust-stable"
emit "             - bundled pinned sources with x-checker-data (FreeRDP, cJSON):"
emit "               a version bump also needs its sha256"
emit "             - the flathub manifest must match the local one"
emit "  Snap     snap/snapcraft.yaml"
emit "             - base (core24) and the gnome extension (gnome-46-2404);"
emit "               a core26 gnome extension shipping is the trigger to revisit"
emit "             - pinned stage-packages / build-packages"
emit "  Nix      flake.nix tracks nixpkgs unstable — nothing to audit, but its"
emit "             version string must match the workspace version"
emit ""
emit "Report only. Nothing here was applied."

if [ "$quiet" -eq 0 ]; then
    cat "$report"
else
    printf 'Report written to %s\n' "$report"
    printf 'in-range updates: %s, held back: %s\n' \
        "$inrange" "$(wc -l <"$raw.classified" | tr -d ' ')"
fi

exit 0
