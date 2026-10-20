#!/usr/bin/env bash
set -euo pipefail

# Build a native macOS Spot.app bundle (dist/Spot.app) with a compiled Spot.icns dog icon,
# and (with --zip) generate dist/Spot-macos.zip + dist/spot-update-manifest.json + dist/SHA256SUMS.txt.
# Usage:
#   ./scripts/package-macos.sh                  # builds dist/Spot.app
#   ./scripts/package-macos.sh --zip            # also creates dist/Spot-macos.zip & dist/spot-update-manifest.json
#   ./scripts/package-macos.sh --store-local    # also stores the packaged zip in ~/Library/Application Support/Spot/updates

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_NAME="Spot"
BUNDLE_ID="dev.cartermp.spot"
BIN_NAME="spot"
REPO="${SPOT_UPDATE_REPO:-cartermp/spot}"
VERSION="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)"

COMMIT_SHA="${SPOT_BUILD_COMMIT:-$(git rev-parse HEAD 2>/dev/null || echo "0000000000000000000000000000000000000000")}"
SHORT_SHA="${COMMIT_SHA:0:7}"
COMMIT_COUNT="$(git rev-list --count HEAD 2>/dev/null || echo "1")"
if [[ -n "${SPOT_BUILD_NUMBER:-}" ]]; then
  BUILD_NUMBER="$SPOT_BUILD_NUMBER"
elif [[ -n "${GITHUB_RUN_NUMBER:-}" ]]; then
  if (( GITHUB_RUN_NUMBER > COMMIT_COUNT )); then
    BUILD_NUMBER="$GITHUB_RUN_NUMBER"
  else
    BUILD_NUMBER="$COMMIT_COUNT"
  fi
else
  BUILD_NUMBER="$COMMIT_COUNT"
fi

RELEASE_TAG="${SPOT_RELEASE_TAG:-main-b${BUILD_NUMBER}-${SHORT_SHA}}"
COMMIT_TITLE="$(git log -1 --format=%s HEAD 2>/dev/null || echo "Spot main build #${BUILD_NUMBER}")"
PUBLISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

DIST_DIR="$ROOT_DIR/dist"
APP_DIR="$DIST_DIR/${APP_NAME}.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
ICONSET_DIR="$DIST_DIR/${APP_NAME}.iconset"
MASTER_PNG="$DIST_DIR/${APP_NAME}-1024.png"

CREATE_ZIP=0
STORE_LOCAL=0
for arg in "$@"; do
  case "$arg" in
    --zip) CREATE_ZIP=1 ;;
    --store-local) CREATE_ZIP=1; STORE_LOCAL=1 ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Usage: $0 [--zip] [--store-local]" >&2
      exit 1
      ;;
  esac
done

echo "==> Building ${APP_NAME} v${VERSION} (build #${BUILD_NUMBER}, commit ${SHORT_SHA})..."
SPOT_BUILD_COMMIT="$COMMIT_SHA" SPOT_BUILD_NUMBER="$BUILD_NUMBER" cargo build --release -p spot-app

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
    <string>${BUILD_NUMBER}.${SHORT_SHA}</string>
    <key>SpotCommitSHA</key>
    <string>${COMMIT_SHA}</string>
    <key>SpotBuildNumber</key>
    <string>${BUILD_NUMBER}</string>
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
  ASSET_NAME="${APP_NAME}-macos.zip"
  ZIP_PATH="$DIST_DIR/${ASSET_NAME}"
  MANIFEST_PATH="$DIST_DIR/spot-update-manifest.json"
  SUMS_PATH="$DIST_DIR/SHA256SUMS.txt"

  rm -f "$ZIP_PATH" "$MANIFEST_PATH" "$SUMS_PATH"
  echo "==> Creating release archive ${ZIP_PATH}..."
  /usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP_DIR" "$ZIP_PATH"

  SHA256="$(shasum -a 256 "$ZIP_PATH" | awk '{print $1}')"
  SIZE_BYTES="$(stat -f%z "$ZIP_PATH")"
  DOWNLOAD_URL="${SPOT_DOWNLOAD_URL:-https://github.com/${REPO}/releases/download/${RELEASE_TAG}/${ASSET_NAME}}"

  printf "%s  %s\n" "$SHA256" "$ASSET_NAME" > "$SUMS_PATH"

  python3 - "$MANIFEST_PATH" "$VERSION" "$BUILD_NUMBER" "$COMMIT_SHA" "$SHORT_SHA" "$RELEASE_TAG" "$COMMIT_TITLE" "$PUBLISHED_AT" "$ASSET_NAME" "$DOWNLOAD_URL" "$SHA256" "$SIZE_BYTES" <<'PY'
import json, sys
out_path, version, build_num, sha, short_sha, tag, title, pub_at, asset, url, sha256, size_b = sys.argv[1:]
manifest = {
    "version": version,
    "build_number": int(build_num),
    "commit_sha": sha,
    "short_sha": short_sha,
    "tag": tag,
    "commit_title": title,
    "published_at": pub_at,
    "asset_name": asset,
    "download_url": url,
    "sha256": sha256,
    "size_bytes": int(size_b),
}
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PY

  echo "==> Release manifest: $MANIFEST_PATH (sha256=${SHA256:0:12}..., size=${SIZE_BYTES} bytes)"

  if [[ "$STORE_LOCAL" -eq 1 ]]; then
    echo "==> Storing build #${BUILD_NUMBER} (${SHORT_SHA}) into local Spot update store..."
    LOCAL_MANIFEST="$DIST_DIR/spot-local-manifest.json"
    python3 - "$MANIFEST_PATH" "$LOCAL_MANIFEST" "$ZIP_PATH" <<'PY'
import json, sys
src, dst, zip_path = sys.argv[1:]
with open(src, "r", encoding="utf-8") as f:
    data = json.load(f)
data["download_url"] = zip_path
with open(dst, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
PY
    SPOT_UPDATE_MANIFEST_URL="$LOCAL_MANIFEST" "$RELEASE_BIN" --download-update
    rm -f "$LOCAL_MANIFEST"
  fi
fi

echo "==> Packaged: $APP_DIR"
