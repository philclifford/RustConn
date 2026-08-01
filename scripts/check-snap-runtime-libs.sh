#!/usr/bin/env bash
# Verify every shared library the packed snap's binaries need is actually
# reachable at runtime.
#
# Why this exists: the snap links against the libraries of its build
# environment, but at runtime a strictly-confined snap only sees its own
# `prime` tree, the base snap (core24) and the platform snap provided by the
# `gnome` extension (gnome-46-2404). Adding a `-dev` package to
# `build-packages` without the matching runtime package in `stage-packages`
# therefore produces a snap that builds, packs, uploads and installs cleanly,
# and then dies at the dynamic linker before main():
#
#   error while loading shared libraries: libwebkitgtk-6.0.so.4:
#   cannot open shared object file: No such file or directory
#
# That is issue #244 — it shipped in 0.19.0 and went unnoticed through several
# releases because nothing in CI ever resolved the snap's own DT_NEEDED list.
#
# Usage:
#   ./scripts/check-snap-runtime-libs.sh <file.snap>
#
# Exit codes:
#   0 — every needed soname is provided by the snap, the base or the platform
#   1 — at least one soname is unresolvable (the snap would fail to start)
#   2 — error (missing tool, bad arguments, unreadable snap)
#
# Requires: squashfs-tools (unsquashfs), binutils (readelf), snapd

set -uo pipefail

SNAP_FILE="${1:-}"

if [ -z "$SNAP_FILE" ]; then
  echo "usage: $0 <file.snap>" >&2
  exit 2
fi

if [ ! -r "$SNAP_FILE" ]; then
  echo "error: cannot read '$SNAP_FILE'" >&2
  exit 2
fi

for tool in unsquashfs readelf; do
  command -v "$tool" > /dev/null 2>&1 || {
    echo "error: '$tool' not found (install squashfs-tools / binutils)" >&2
    exit 2
  }
done

# The runtime search path of a confined snap: its own prime tree plus the
# read-only snaps mounted next to it. mesa-2404 is included because the gnome
# extension wires it in as the GPU userspace provider.
RUNTIME_SNAPS=(
  /snap/core24/current
  /snap/gnome-46-2404/current
  /snap/mesa-2404/current
)

WORK_DIR=$(mktemp -d)
# shellcheck disable=SC2064 # expand WORK_DIR now, not at trap time
trap "rm -rf '$WORK_DIR'" EXIT

echo "Unpacking $SNAP_FILE"
unsquashfs -q -f -d "$WORK_DIR/root" "$SNAP_FILE" > /dev/null || {
  echo "error: unsquashfs failed on '$SNAP_FILE'" >&2
  exit 2
}

# Every soname that will be resolvable at runtime, by basename. LD_LIBRARY_PATH
# in a snap is a flat list of directories, so a basename match is exactly the
# granularity the dynamic linker works at.
PROVIDED="$WORK_DIR/provided.txt"
find "$WORK_DIR/root" -name '*.so*' -printf '%f\n' 2> /dev/null > "$PROVIDED"

# -H follows symlinks given on the command line only: /snap/<name>/current is a
# symlink to the revision directory, which plain find would not descend into,
# while -L would follow the symlinks *inside* those snaps back out into the
# host filesystem and crawl for minutes.
for snap_dir in "${RUNTIME_SNAPS[@]}"; do
  if [ -d "$snap_dir" ]; then
    find -H "$snap_dir" -name '*.so*' -printf '%f\n' 2> /dev/null >> "$PROVIDED"
  else
    echo "::warning::$snap_dir is not installed — its libraries cannot be verified"
  fi
done

sort -u -o "$PROVIDED" "$PROVIDED"
echo "Runtime provides $(wc -l < "$PROVIDED") libraries"

MISSING="$WORK_DIR/missing.txt"
: > "$MISSING"
CHECKED=0

while IFS= read -r binary; do
  # Skip anything that is not a dynamically linked ELF executable (scripts,
  # data files, and the staged CLI helpers' shell wrappers).
  readelf -d "$binary" > /dev/null 2>&1 || continue

  CHECKED=$((CHECKED + 1))
  rel_path="${binary#"$WORK_DIR/root/"}"
  echo "Checking $rel_path"

  # LC_ALL=C keeps readelf's output parseable regardless of the runner locale.
  needed=$(LC_ALL=C readelf -d "$binary" 2> /dev/null |
    sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p')

  while IFS= read -r soname; do
    [ -n "$soname" ] || continue
    if ! grep -qxF "$soname" "$PROVIDED"; then
      echo "  MISSING $soname"
      echo "$rel_path: $soname" >> "$MISSING"
    fi
  done <<< "$needed"
done < <(find "$WORK_DIR/root/usr/bin" -maxdepth 1 -type f 2> /dev/null)

if [ "$CHECKED" -eq 0 ]; then
  echo "error: no dynamically linked binaries found under usr/bin — wrong snap?" >&2
  exit 2
fi

if [ -s "$MISSING" ]; then
  echo
  echo "::error::the snap needs libraries that nothing provides at runtime:"
  sort -u "$MISSING" | sed 's/^/  /'
  echo
  echo "Add the runtime package to 'stage-packages' in snap/snapcraft.yaml, or"
  echo "drop the cargo feature that pulls the library in. A '-dev' entry in"
  echo "'build-packages' only satisfies the linker at build time."
  exit 1
fi

echo
echo "OK — all libraries needed by $CHECKED binaries resolve at runtime"
