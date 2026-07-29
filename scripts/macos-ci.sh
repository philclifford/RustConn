#!/usr/bin/env bash
# Complete local-only macOS quality, build, bundle, and portability gate.
# Does not launch the GUI or use hosted CI runners.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

SKIP_TESTS=false
SKIP_BUNDLE=false
for arg in "$@"; do
    case "$arg" in
        --skip-tests) SKIP_TESTS=true ;;
        --skip-bundle) SKIP_BUNDLE=true ;;
        -h|--help)
            echo "Usage: $0 [--skip-tests] [--skip-bundle]"
            exit 0
            ;;
        *) echo "Unknown option: $arg" >&2; exit 2 ;;
    esac
done

[[ "$(uname -s)" == "Darwin" ]] || { echo "This gate must run on macOS" >&2; exit 1; }
FEATURES="$($SCRIPT_DIR/macos-build.sh --print-features)"
echo "[info] Canonical macOS features: $FEATURES"

cargo fmt --all -- --check
cargo clippy -p rustconn --no-default-features --features "$FEATURES" --all-targets -- -D warnings
cargo clippy -p rustconn-cli --features full --all-targets -- -D warnings
if ! $SKIP_TESTS; then
    # Cover every crate that ships in the bundle, not just core: a GUI or CLI
    # test failure must fail the gate instead of being reported as a pass.
    cargo test -p rustconn-core
    cargo test -p rustconn-cli --features full
    cargo test -p rustconn --no-default-features --features "$FEATURES"
fi
cargo deny check

# Supply-chain audit. Accepted advisories live in .cargo/audit.toml and deny.toml.
if command -v cargo-audit >/dev/null 2>&1; then
    cargo audit
else
    echo "[fail] cargo-audit missing: cargo install cargo-audit --locked" >&2
    exit 1
fi
if command -v cargo-outdated >/dev/null 2>&1; then
    cargo outdated --root-deps-only --workspace --exit-code 0
else
    echo "[fail] cargo-outdated missing: cargo install cargo-outdated --locked" >&2
    exit 1
fi

if ! $SKIP_BUNDLE; then
    "$SCRIPT_DIR/macos-build.sh" --release --clean --no-launch --adhoc
    APP_DIR="$PROJECT_DIR/dist/RustConn.app"

    "$APP_DIR/Contents/MacOS/rustconn-cli" --version
    "$APP_DIR/Contents/MacOS/rustconn-cli" --help >/dev/null
    plutil -lint "$APP_DIR/Contents/Info.plist" >/dev/null
    codesign --verify --deep --strict --verbose=2 "$APP_DIR"

    while IFS= read -r macho; do
        if otool -L "$macho" | tail -n +2 | grep -Eq '^[[:space:]]+/(opt/homebrew|usr/local)/'; then
            echo "Absolute Homebrew dependency remains in $macho" >&2
            otool -L "$macho" >&2
            exit 1
        fi
    done < <(find "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Frameworks" -type f -exec file {} \; | awk -F: '/Mach-O/{print $1}')

    otool -l "$APP_DIR/Contents/MacOS/rustconn" | grep -A2 LC_RPATH | \
        grep -q 'path @executable_path/../Frameworks'

    # gfx-h264 dlopen's OpenH264 from the bundle; verify the exact path the
    # runtime probes first, plus its architecture, so H.264 cannot silently
    # degrade to non-AVC codecs in a release artifact.
    OPENH264_BUNDLED="$APP_DIR/Contents/Frameworks/libopenh264.dylib"
    [[ -f "$OPENH264_BUNDLED" ]] || {
        echo "Bundled OpenH264 missing: $OPENH264_BUNDLED" >&2
        exit 1
    }
    file "$OPENH264_BUNDLED" | grep -q "Mach-O.*dynamically linked shared library.*$(uname -m)" || {
        echo "Bundled OpenH264 is not a $(uname -m) Mach-O dylib" >&2
        file "$OPENH264_BUNDLED" >&2
        exit 1
    }
    echo "[ ok ] Self-contained ad-hoc bundle passed local audit"
fi

echo "[ ok ] Local macOS CI passed"
