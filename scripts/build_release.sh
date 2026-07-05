#!/usr/bin/env bash
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"
VERSION="$(grep '^version =' "$ROOT/Cargo.toml" | head -n1 | sed -E 's/version = "([^"]+)"/\1/')"
TARGET_DIR="$(cargo metadata --format-version=1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"

mkdir -p "$DIST"
rm -f "$DIST"/tsbox-*

build_target() {
  local target="$1"
  local bin_name="$2"
  local archive_name="$3"

  echo "==> building $target"
  if ! cargo build --release --target "$target"; then
    echo "WARN: failed to build $target" >&2
    return 1
  fi

  local stage
  stage="$(mktemp -d)"
  mkdir -p "$stage/tsbox-$VERSION"
  cp "$TARGET_DIR/$target/release/$bin_name" "$stage/tsbox-$VERSION/"
  cp "$ROOT/README.md" "$ROOT/USAGE.md" "$stage/tsbox-$VERSION/"

  (
    cd "$stage"
    if [[ "$archive_name" == *.zip ]]; then
      zip -qr "$DIST/$archive_name" "tsbox-$VERSION"
    else
      tar -czf "$DIST/$archive_name" "tsbox-$VERSION"
    fi
  )
  rm -rf "$stage"
  return 0
}

cd "$ROOT"

build_target "x86_64-unknown-linux-gnu" "tsbox" "tsbox-$VERSION-x86_64-unknown-linux-gnu.tar.gz"
build_target "x86_64-pc-windows-gnu" "tsbox.exe" "tsbox-$VERSION-x86_64-pc-windows-gnu.zip" || true
build_target "x86_64-pc-windows-gnullvm" "tsbox.exe" "tsbox-$VERSION-x86_64-pc-windows-gnullvm.zip" || true

(
  cd "$DIST"
  sha256sum tsbox-* > "tsbox-$VERSION-checksums.txt"
)

echo "release artifacts:"
ls -lh "$DIST"
