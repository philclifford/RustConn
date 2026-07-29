#!/usr/bin/env bash
# Package the canonical RustConn.app as a DMG and optionally notarize it.
#
# Usage:
#   ./packaging/macos/build-dmg.sh --adhoc
#   ./packaging/macos/build-dmg.sh --sign-identity "Developer ID Application: ..."
#   ./packaging/macos/build-dmg.sh --sign-identity "..." --notary-profile rustconn-notary
#   ./packaging/macos/build-dmg.sh --skip-build

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
APP_DIR="$PROJECT_DIR/dist/RustConn.app"
DMG_DIR="$PROJECT_DIR/dist"
VERSION="$(awk -F'"' '/^\[workspace\.package\]/{p=1} p&&/^version[[:space:]]*=/{print $2; exit}' "$PROJECT_DIR/Cargo.toml")"
ARCH="$(uname -m)"
DMG_PATH="$DMG_DIR/RustConn-${VERSION}-macOS-${ARCH}.dmg"
STAGING_DIR="$DMG_DIR/dmg-staging"
SKIP_BUILD=false
SIGN_MODE="none"
SIGN_IDENTITY="${MACOS_SIGN_IDENTITY:-}"
NOTARY_PROFILE="${MACOS_NOTARY_PROFILE:-}"
if [[ -n "$SIGN_IDENTITY" ]]; then
    SIGN_MODE="developer"
fi

fail() { printf '[fail] %s\n' "$*" >&2; exit 1; }
info() { printf '[info] %s\n' "$*"; }
ok() { printf '[ ok ] %s\n' "$*"; }

BUILD_ARGS=(--release --clean --no-launch)
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) ;;
        --skip-build) SKIP_BUILD=true ;;
        --adhoc)
            [[ "$SIGN_MODE" != "developer" ]] || fail "--adhoc conflicts with Developer ID signing"
            SIGN_MODE="adhoc"
            BUILD_ARGS+=(--adhoc)
            ;;
        --no-sign)
            SIGN_MODE="none"
            SIGN_IDENTITY=""
            BUILD_ARGS+=(--no-sign)
            ;;
        --sign-identity)
            shift
            [[ $# -gt 0 ]] || fail "--sign-identity requires a value"
            [[ "$SIGN_MODE" != "adhoc" ]] || fail "--sign-identity conflicts with --adhoc"
            SIGN_MODE="developer"
            SIGN_IDENTITY="$1"
            BUILD_ARGS+=(--sign-identity "$1")
            ;;
        --sign-identity=*)
            [[ "$SIGN_MODE" != "adhoc" ]] || fail "--sign-identity conflicts with --adhoc"
            SIGN_MODE="developer"
            SIGN_IDENTITY="${1#*=}"
            BUILD_ARGS+=(--sign-identity "$SIGN_IDENTITY")
            ;;
        --notary-profile)
            shift
            [[ $# -gt 0 ]] || fail "--notary-profile requires a value"
            NOTARY_PROFILE="$1"
            ;;
        --notary-profile=*) NOTARY_PROFILE="${1#*=}" ;;
        -h|--help)
            sed -n '2,8p' "$0"
            exit 0
            ;;
        *) fail "Unknown option: $1" ;;
    esac
    shift
done

[[ "$(uname -s)" == "Darwin" ]] || fail "DMG creation must run on macOS"
[[ -n "$VERSION" ]] || fail "Cannot read workspace version"
if [[ -n "$NOTARY_PROFILE" && "$SIGN_MODE" != "developer" ]]; then
    fail "Notarization requires --sign-identity or MACOS_SIGN_IDENTITY"
fi

if ! $SKIP_BUILD; then
    "$PROJECT_DIR/scripts/macos-build.sh" "${BUILD_ARGS[@]}"
fi
[[ -d "$APP_DIR" ]] || fail "Missing app bundle: $APP_DIR"
plutil -lint "$APP_DIR/Contents/Info.plist" >/dev/null

# Establish the provenance of the enclosed app before it is wrapped, signed or
# notarized. With --skip-build the bundle is whatever is already on disk, so a
# Developer ID DMG could otherwise ship an unsigned or ad-hoc app and only fail
# late at Apple's notary service.
verify_enclosed_app() {
    local signature_info
    codesign --verify --deep --strict --verbose=2 "$APP_DIR" 2>/dev/null || \
        fail "Enclosed app fails strict signature verification: $APP_DIR"
    signature_info="$(codesign -d --verbose=4 "$APP_DIR" 2>&1)"

    case "$SIGN_MODE" in
        developer)
            if grep -q 'Signature=adhoc' <<<"$signature_info"; then
                fail "Refusing to sign a Developer ID DMG around an ad-hoc app; rebuild without --skip-build"
            fi
            grep -qF "Authority=$SIGN_IDENTITY" <<<"$signature_info" || \
                fail "Enclosed app is not signed by '$SIGN_IDENTITY'; rebuild without --skip-build"
            grep -qE '^CodeDirectory .*flags=.*runtime' <<<"$signature_info" || \
                fail "Enclosed app lacks the hardened runtime required for notarization"
            ok "Enclosed app verified: Developer ID, hardened runtime"
            ;;
        adhoc)
            grep -q 'Signature=adhoc' <<<"$signature_info" || \
                fail "Expected an ad-hoc signed app for --adhoc; found a different signature"
            ok "Enclosed app verified: ad-hoc signature"
            ;;
    esac
}

if [[ "$SIGN_MODE" == "none" ]]; then
    info "Unsigned mode: skipping app signature provenance checks"
else
    verify_enclosed_app
fi

rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR"
trap 'rm -rf "$STAGING_DIR"' EXIT
cp -R "$APP_DIR" "$STAGING_DIR/"
ln -s /Applications "$STAGING_DIR/Applications"
rm -f "$DMG_PATH"

info "Creating compressed DMG"
hdiutil create -volname "RustConn" -srcfolder "$STAGING_DIR" -ov -format UDZO "$DMG_PATH"

if [[ "$SIGN_MODE" == "developer" ]]; then
    codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG_PATH"
    codesign --verify --verbose=2 "$DMG_PATH"
    ok "DMG signed with Developer ID"
fi

if [[ -n "$NOTARY_PROFILE" ]]; then
    info "Submitting DMG to Apple notary service"
    xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG_PATH"
    xcrun stapler validate "$DMG_PATH"
    spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG_PATH"
    ok "Notarization ticket stapled and validated"
else
    info "Notarization skipped; pass --notary-profile for a distribution artifact"
fi

ok "DMG ready: $DMG_PATH"
