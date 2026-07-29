#!/usr/bin/env bash
# Compatibility entry point; the canonical app producer lives in scripts/.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
exec "$PROJECT_DIR/scripts/macos-build.sh" "$@"
