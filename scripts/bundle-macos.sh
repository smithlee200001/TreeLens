#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Please run this script on macOS with Xcode Command Line Tools and Rust installed." >&2
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust is required: https://rustup.rs" >&2
  exit 1
fi
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
# Native architecture by default. Optionally pass aarch64-apple-darwin or x86_64-apple-darwin.
TARGET="${1:-}"
ARGS=(--release --locked)
BINARY="target/release/tree-lens"
if [[ -n "$TARGET" ]]; then
  case "$TARGET" in
    aarch64-apple-darwin|x86_64-apple-darwin) ;;
    *) echo "Unsupported target: $TARGET" >&2; exit 1 ;;
  esac
  ARGS+=(--target "$TARGET")
  BINARY="target/$TARGET/release/tree-lens"
fi
cargo build "${ARGS[@]}"
APP="dist/Tree Lens.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/tree-lens"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Tree Lens</string>
  <key>CFBundleDisplayName</key><string>Tree Lens</string>
  <key>CFBundleExecutable</key><string>tree-lens</string>
  <key>CFBundleIdentifier</key><string>local.treelens.app</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSHumanReadableCopyright</key><string>Tree Lens contributors</string>
</dict></plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"
echo "Created: $PWD/$APP"
echo "Open locally with: open \"$APP\""
echo "This development bundle is not Developer ID signed or notarized. Distribution needs your own signing identity."
