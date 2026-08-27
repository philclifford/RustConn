#!/usr/bin/env bash
# Publish a tagged RustConn release to the openSUSE Build Service (OBS).
#
# Shared by the CI release pipeline (.github/workflows/release.yml → update-obs)
# and the manual OBS workflow (.github/workflows/obs-update.yml). Keeping the
# logic here (instead of duplicated inline in both workflows) means the vendor
# tarball, bundled Rust toolchain, changelog propagation, and osc commit only
# ever need fixing in one place.
#
# Usage:
#   OBS_USERNAME=... OBS_PASSWORD=... ./scripts/obs-publish.sh <version>
#
# <version> is the bare semver (e.g. 0.16.2), WITHOUT the leading 'v'.
#
# Requirements on PATH: git, cargo, rustc, tar, zstd, osc, awk, sed, date.
#
# Idempotency: safe to re-run for the same version. Changelog entries are only
# prepended if this version is not already present, so a retry after a transient
# network failure will not produce duplicate changelog blocks.
#
# Exit codes:
#   0 — committed (or nothing to change) and rebuild triggered
#   2 — error (missing tool, missing credentials, bad arguments)

set -euo pipefail

# ──────────────────────────────────────────────────────────────────────────────
# Arguments & environment
# ──────────────────────────────────────────────────────────────────────────────
VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    echo "::error::usage: obs-publish.sh <version> (bare semver, no leading v)" >&2
    exit 2
fi
# Dot-escaped form for use inside grep -E patterns (idempotency checks).
VERSION_RE="${VERSION//./\\.}"

if [[ -z "${OBS_USERNAME:-}" ]]; then
    echo "::error::OBS_USERNAME is not set" >&2
    exit 2
fi
if [[ -z "${OBS_PASSWORD:-}" ]]; then
    echo "::error::OBS_PASSWORD is not set" >&2
    exit 2
fi

for tool in git cargo rustc tar zstd osc awk sed date; do
    command -v "$tool" >/dev/null || { echo "::error::missing tool: $tool" >&2; exit 2; }
done

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

OBS_PROJECT="home:totoshko88:rustconn"
OBS_PKG="${OBS_PROJECT}/rustconn"
OBS_DIR="$OBS_PKG"

# ──────────────────────────────────────────────────────────────────────────────
# 1. Vendor tarball — all dependencies (incl. optional) for offline OBS VMs.
#    --locked: never silently update Cargo.lock while vendoring a release.
# ──────────────────────────────────────────────────────────────────────────────
echo "=== Generating vendor tarball ==="
cargo vendor --locked --versioned-dirs
tar --zstd -cf vendor.tar.zst vendor/
ls -lh vendor.tar.zst

# ──────────────────────────────────────────────────────────────────────────────
# 2. Standalone Rust toolchain — OBS VMs have no internet.
#    Copy from sysroot, NOT ~/.cargo/bin (those are rustup shims).
# ──────────────────────────────────────────────────────────────────────────────
echo "=== Bundling Rust toolchain ==="
SYSROOT="$(rustc --print sysroot)"
RUST_VERSION="$(rustc --version | grep -oP '\d+\.\d+\.\d+')"
echo "Bundling Rust toolchain $RUST_VERSION from $SYSROOT"
rm -rf rust-toolchain
mkdir -p rust-toolchain
cp -r "$SYSROOT/bin" rust-toolchain/
cp -r "$SYSROOT/lib" rust-toolchain/
file rust-toolchain/bin/rustc
rust-toolchain/bin/rustc --version
rust-toolchain/bin/cargo --version
tar --zstd -cf rust-toolchain.tar.zst rust-toolchain/
ls -lh rust-toolchain.tar.zst

# ──────────────────────────────────────────────────────────────────────────────
# 3. Configure osc
# ──────────────────────────────────────────────────────────────────────────────
echo "=== Configuring osc ==="
mkdir -p ~/.config/osc
cat > ~/.config/osc/oscrc << EOF
[general]
apiurl = https://api.opensuse.org

[https://api.opensuse.org]
user = ${OBS_USERNAME}
pass = ${OBS_PASSWORD}
EOF
chmod 600 ~/.config/osc/oscrc

# ──────────────────────────────────────────────────────────────────────────────
# 4. Checkout OBS package
#    Delete potentially corrupted large files first (Content-Length mismatches
#    from interrupted prior uploads).
# ──────────────────────────────────────────────────────────────────────────────
echo "=== Checking out OBS package ==="
osc api -X DELETE "/source/${OBS_PKG}/rust-toolchain.tar.zst" 2>/dev/null || true
osc api -X DELETE "/source/${OBS_PKG}/vendor.tar.zst" 2>/dev/null || true
osc checkout "$OBS_PKG"
( cd "$OBS_DIR" && osc up )

# ──────────────────────────────────────────────────────────────────────────────
# 5. Extract this version's changelog bullets from CHANGELOG.md
# ──────────────────────────────────────────────────────────────────────────────
echo "=== Extracting changelog for ${VERSION} ==="
awk -v ver="${VERSION}" '
    /^## \[/ {
        if (found) exit
        if (index($0, "[" ver "]") > 0) found=1
        next
    }
    found && /^### / { next }
    found && NF { print "  * " $0 }
' CHANGELOG.md > version_changes.txt
cat version_changes.txt

# ──────────────────────────────────────────────────────────────────────────────
# 6. Prepend OBS .changes entry (idempotent: skip if version already present)
# ──────────────────────────────────────────────────────────────────────────────
CHANGES_FILE="$OBS_DIR/rustconn.changes"
if [[ -f "$CHANGES_FILE" ]] && grep -qE "^[A-Z][a-z]{2} .* - ${VERSION_RE}$" "$CHANGES_FILE"; then
    echo "OBS changelog already contains ${VERSION} — skipping .changes prepend"
else
    DATE_CHANGES="$(LC_ALL=C date '+%a %b %d %Y')"
    cat > new_entry.txt << EOF
-------------------------------------------------------------------
${DATE_CHANGES} Anton Isaiev <totoshko88@gmail.com> - ${VERSION}

- Update to version ${VERSION}
EOF
    cat version_changes.txt >> new_entry.txt
    echo "" >> new_entry.txt

    if [[ -f "$CHANGES_FILE" ]]; then
        cat new_entry.txt "$CHANGES_FILE" > temp_changes
        mv temp_changes "$CHANGES_FILE"
    else
        mv new_entry.txt "$CHANGES_FILE"
    fi
    echo "=== Prepended OBS .changes entry ==="
    head -30 "$CHANGES_FILE"
fi

# ──────────────────────────────────────────────────────────────────────────────
# 7. Overwrite the OBS packaging files from this repo
#
# The OBS package is a *copy*, not a checkout: anything not written here keeps
# whatever it had when someone last uploaded it by hand. This step used to patch
# version strings only, so every other edit to packaging/obs/ stayed in git and
# never reached the builds.
#
# That is not theoretical. 0.20.10's five RPM builds all failed on
#
#     error: Directory not found: .../usr/share/locale/zh-cn
#
# because the 0.20.9 spec work — %find_lang, the %check section, the Summary
# length fix, the dropped explicit Requires — was never copied over, and the
# stale %files still listed `%dir %{_datadir}/locale/zh-cn` by hand. Renaming the
# catalogue to zh_CN.po (the fix that made Chinese load at all) stopped that
# directory from being created, and the old file list demanded it. The Debian
# builds passed only because debian.rules happened not to have changed.
# debian.dsc had been carrying `Files: rustconn-0.1.0.tar.xz` since 0.1.0 for the
# same reason: the sed covered `DEBTRANSFORM-TAR:` and not that line.
#
# Overwriting is safe because the repo is the source of truth — release.sh gates
# every one of these files before a tag exists. Nothing is edited OBS-side.
# `rustconn.changes` and `debian.changelog` are deliberately excluded: the
# prepend steps above and below own those, and OBS holds accumulated history.
echo "=== Syncing OBS packaging files from the repo ==="
for pkg_file in rustconn.spec debian.control debian.rules debian.copyright \
    debian.dsc AppImageBuilder.yml _service; do
    if [[ -f "packaging/obs/$pkg_file" ]]; then
        cp "packaging/obs/$pkg_file" "$OBS_DIR/$pkg_file"
        echo "  copied packaging/obs/$pkg_file"
    else
        echo "::warning::packaging/obs/$pkg_file not found in the repo — leaving the OBS copy alone"
    fi
done
cp cargo_config "$OBS_DIR/cargo_config"
echo "  copied cargo_config"

# The version is already correct in the copies above — bump-version.sh writes it
# and release.sh verifies it. These seds stay as a safety net for a manual
# obs-update.yml run against a ref whose packaging files carry a different
# version than the one being published.
echo "=== Updating OBS version fields ==="
sed -i "s|<param name=\"revision\">v[^<]*</param>|<param name=\"revision\">v${VERSION}</param>|" "$OBS_DIR/_service"
sed -i "s/^Version:.*$/Version:        ${VERSION}/" "$OBS_DIR/rustconn.spec"
sed -i "s/^Version:.*$/Version: ${VERSION}-1/" "$OBS_DIR/debian.dsc"
sed -i "s/^DEBTRANSFORM-TAR:.*$/DEBTRANSFORM-TAR: rustconn-${VERSION}.tar.xz/" "$OBS_DIR/debian.dsc"
sed -i "s|^ 00000000000000000000000000000000 0 rustconn-.*\.tar\.xz$| 00000000000000000000000000000000 0 rustconn-${VERSION}.tar.xz|" "$OBS_DIR/debian.dsc"
sed -i "s/^    version: .*$/    version: ${VERSION}/" "$OBS_DIR/AppImageBuilder.yml"

# ──────────────────────────────────────────────────────────────────────────────
# 8. Prepend debian.changelog entry (idempotent)
# ──────────────────────────────────────────────────────────────────────────────
DCH_FILE="$OBS_DIR/debian.changelog"
if [[ -f "$DCH_FILE" ]] && grep -qE "^rustconn \(${VERSION_RE}-1\)" "$DCH_FILE"; then
    echo "debian.changelog already contains ${VERSION} — skipping prepend"
elif [[ -f "$DCH_FILE" ]]; then
    DATE_RFC2822="$(LC_ALL=C date -R)"
    {
        printf 'rustconn (%s-1) unstable; urgency=medium\n\n' "$VERSION"
        while IFS= read -r line; do
            printf '  %s\n' "$line"
        done < version_changes.txt
        printf '\n -- Anton Isaiev <totoshko88@gmail.com>  %s\n\n' "$DATE_RFC2822"
        cat "$DCH_FILE"
    } > temp_dch
    mv temp_dch "$DCH_FILE"
    echo "=== Prepended debian.changelog entry ==="
fi

# ──────────────────────────────────────────────────────────────────────────────
# 9. Copy large source tarballs into the OBS checkout
# ──────────────────────────────────────────────────────────────────────────────
cp vendor.tar.zst "$OBS_DIR/vendor.tar.zst"
cp rust-toolchain.tar.zst "$OBS_DIR/rust-toolchain.tar.zst"
ls -lh "$OBS_DIR/vendor.tar.zst" "$OBS_DIR/rust-toolchain.tar.zst"

# ──────────────────────────────────────────────────────────────────────────────
# 10. Commit & trigger rebuild (commit is a no-op-safe: osc commit with no
#     changes exits cleanly)
# ──────────────────────────────────────────────────────────────────────────────
echo "=== Committing to OBS ==="
(
    cd "$OBS_DIR"
    osc status || true
    osc addremove
    if osc status | grep -qE '^[ADM]'; then
        osc commit -m "Update to version ${VERSION}

Automated update from GitHub release v${VERSION}
https://github.com/totoshko88/RustConn/releases/tag/v${VERSION}"
    else
        echo "No OBS changes to commit for ${VERSION}"
    fi
)

echo "=== Triggering OBS rebuild ==="
osc rebuild "$OBS_PROJECT" rustconn
echo "OBS publish for ${VERSION} complete."
