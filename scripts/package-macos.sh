#!/usr/bin/env bash
set -euo pipefail

# Build a native macOS Spot.app bundle (dist/Spot.app) with a compiled Spot.icns dog icon.
# Usage:
#   ./scripts/package-macos.sh          # builds dist/Spot.app
#   ./scripts/package-macos.sh --zip    # also creates dist/Spot-macos.zip

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_NAME="Spot"
BUNDLE_ID="dev.cartermp.spot"
BIN_NAME="spot"
VERSION="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)"

DIST_DIR="$ROOT_DIR/dist"
APP_DIR="$DIST_DIR/${APP_NAME}.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
ICONSET_DIR="$DIST_DIR/${APP_NAME}.iconset"
MASTER_PNG="$DIST_DIR/${APP_NAME}-1024.png"

CREATE_ZIP=0
for arg in "$@"; do
  case "$arg" in
    --zip) CREATE_ZIP=1 ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Usage: $0 [--zip]" >&2
      exit 1
      ;;
  esac
done

echo "==> Building ${APP_NAME} v${VERSION} (release)..."
cargo build --release -p spot-app

RELEASE_BIN="$ROOT_DIR/target/release/$BIN_NAME"
if [[ ! -x "$RELEASE_BIN" ]]; then
  echo "error: expected release binary at $RELEASE_BIN" >&2
  exit 1
fi

echo "==> Preparing ${APP_DIR}..."
rm -rf "$APP_DIR" "$ICONSET_DIR" "$MASTER_PNG"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR" "$ICONSET_DIR"

cp "$RELEASE_BIN" "$MACOS_DIR/$BIN_NAME"
chmod +x "$MACOS_DIR/$BIN_NAME"

echo "==> Rendering 1024x1024 Spot dog icon & building ${APP_NAME}.icns..."
"$RELEASE_BIN" --export-icon "$MASTER_PNG"

for sz in 16 32 128 256 512; do
  /usr/bin/sips -z "$sz" "$sz" "$MASTER_PNG" --out "$ICONSET_DIR/icon_${sz}x${sz}.png" >/dev/null
  sz2=$((sz * 2))
  /usr/bin/sips -z "$sz2" "$sz2" "$MASTER_PNG" --out "$ICONSET_DIR/icon_${sz}x${sz}@2x.png" >/dev/null
done

/usr/bin/iconutil -c icns "$ICONSET_DIR" -o "$RESOURCES_DIR/${APP_NAME}.icns"
rm -rf "$ICONSET_DIR" "$MASTER_PNG"

cat > "$CONTENTS_DIR/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleExecutable</key>
    <string>${BIN_NAME}</string>
    <key>CFBundleIconFile</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
EOF

if command -v codesign >/dev/null 2>&1; then
  echo "==> Ad-hoc signing ${APP_NAME}.app..."
  codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 || true
fi

if [[ "$CREATE_ZIP" -eq 1 ]]; then
  ZIP_PATH="$DIST_DIR/${APP_NAME}-macos.zip"
  rm -f "$ZIP_PATH"
  echo "==> Creating archive ${ZIP_PATH}..."
  /usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP_DIR" "$ZIP_PATH"
fi

echo "==> Packaged: $APP_DIR"
