#!/usr/bin/env bash
set -euo pipefail

# Download, store, and install Spot updates published from pushes to `main`.
# Usage:
#   ./scripts/update.sh                       # download latest `main` build, store in ~/Library/Application Support/Spot/updates, and install to /Applications
#   ./scripts/update.sh --check               # check if a newer `main` push is available on GitHub Releases
#   ./scripts/update.sh --download            # download & store latest `main` build without installing
#   ./scripts/update.sh --list                # list locally stored updates in ~/Library/Application Support/Spot/updates
#   ./scripts/update.sh --rollback <target>   # install a previously stored build by commit SHA or build number

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${SPOT_UPDATE_REPO:-cartermp/spot}"
INSTALL_DIR="${SPOT_INSTALL_DIR:-/Applications}"
BIN_DIR="${SPOT_BIN_DIR:-$HOME/.local/bin}"
DATA_DIR="${SPOT_TEST_CACHE_DIR:-$HOME/Library/Application Support/Spot}"
UPDATES_DIR="$DATA_DIR/updates"
ARCHIVES_DIR="$UPDATES_DIR/archives"
MANIFESTS_DIR="$UPDATES_DIR/manifests"

# Locate an existing `spot` binary if available
SPOT_BIN=""
if [[ -x "$ROOT_DIR/target/release/spot" ]]; then
  SPOT_BIN="$ROOT_DIR/target/release/spot"
elif [[ -x "$INSTALL_DIR/Spot.app/Contents/MacOS/spot" ]]; then
  SPOT_BIN="$INSTALL_DIR/Spot.app/Contents/MacOS/spot"
elif command -v spot >/dev/null 2>&1; then
  SPOT_BIN="$(command -v spot)"
fi

MODE="update"
ROLLBACK_TARGET=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check|--check-update)
      MODE="check"
      shift
      ;;
    --download|--download-only|--download-update)
      MODE="download"
      shift
      ;;
    --list|--list-updates)
      MODE="list"
      shift
      ;;
    --rollback)
      MODE="rollback"
      ROLLBACK_TARGET="${2:-}"
      if [[ -z "$ROLLBACK_TARGET" ]]; then
        echo "Usage: $0 --rollback <commit_sha_or_build_number>" >&2
        exit 1
      fi
      shift 2
      ;;
    *)
      echo "Unknown option: $1" >&2
      echo "Usage: $0 [--check | --download | --list | --rollback <sha_or_build>]" >&2
      exit 1
      ;;
  esac
done

if [[ -n "$SPOT_BIN" ]]; then
  case "$MODE" in
    check)    exec "$SPOT_BIN" --check-update ;;
    download) exec "$SPOT_BIN" --download-update ;;
    list)     exec "$SPOT_BIN" --list-updates ;;
    rollback) exec "$SPOT_BIN" --rollback "$ROLLBACK_TARGET" ;;
    update)   exec "$SPOT_BIN" --update ;;
  esac
fi

# Fallback bootstrap mode (when `spot` is not yet compiled or installed locally)
mkdir -p "$ARCHIVES_DIR" "$MANIFESTS_DIR"

if [[ "$MODE" == "list" ]]; then
  echo "Update Store: $ARCHIVES_DIR"
  ls -lh "$ARCHIVES_DIR"/*.zip 2>/dev/null || echo "No downloaded updates stored locally yet."
  exit 0
fi

echo "==> Fetching latest main release from https://github.com/${REPO}/releases..."
TMP_MANIFEST="$(mktemp -t spot-manifest.XXXXXX.json)"
trap 'rm -f "$TMP_MANIFEST"' EXIT

if [[ -n "${SPOT_UPDATE_MANIFEST_URL:-}" && -f "${SPOT_UPDATE_MANIFEST_URL}" ]]; then
  cp "${SPOT_UPDATE_MANIFEST_URL}" "$TMP_MANIFEST"
else
  MANIFEST_URL="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases?per_page=10" | python3 -c '
import json, sys
releases = json.load(sys.stdin)
for rel in releases:
    if rel.get("draft"):
        continue
    for a in rel.get("assets", []):
        if a.get("name") == "spot-update-manifest.json":
            print(a["browser_download_url"])
            sys.exit(0)
sys.exit(1)
')"
  curl -fsSL "$MANIFEST_URL" -o "$TMP_MANIFEST"
fi

read -r BUILD_NUMBER SHORT_SHA EXPECTED_SHA256 DOWNLOAD_URL < <(python3 - "$TMP_MANIFEST" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    m = json.load(f)
print(m["build_number"], m["short_sha"], m["sha256"].lower(), m["download_url"])
PY
)

ARCHIVE_NAME="Spot-b${BUILD_NUMBER}-${SHORT_SHA}.zip"
ARCHIVE_PATH="$ARCHIVES_DIR/$ARCHIVE_NAME"

if [[ "$MODE" == "check" ]]; then
  echo "Latest main release: build #${BUILD_NUMBER} (${SHORT_SHA})"
  exit 0
fi

echo "==> Downloading ${ARCHIVE_NAME}..."
if [[ -f "$DOWNLOAD_URL" ]]; then
  cp "$DOWNLOAD_URL" "$ARCHIVE_PATH.part"
else
  curl -fL "$DOWNLOAD_URL" -o "$ARCHIVE_PATH.part"
fi

ACTUAL_SHA256="$(shasum -a 256 "$ARCHIVE_PATH.part" | awk '{print tolower($1)}')"
if [[ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]]; then
  rm -f "$ARCHIVE_PATH.part"
  echo "error: SHA-256 mismatch! expected $EXPECTED_SHA256, got $ACTUAL_SHA256" >&2
  exit 1
fi

mv "$ARCHIVE_PATH.part" "$ARCHIVE_PATH"
cp "$TMP_MANIFEST" "$MANIFESTS_DIR/Spot-b${BUILD_NUMBER}-${SHORT_SHA}.json"
echo "==> Stored verified update at $ARCHIVE_PATH"

if [[ "$MODE" == "download" ]]; then
  exit 0
fi

STAGING_DIR="$(mktemp -d -t spot-staging.XXXXXX)"
/usr/bin/ditto -x -k "$ARCHIVE_PATH" "$STAGING_DIR"
mkdir -p "$INSTALL_DIR" "$BIN_DIR"
rm -rf "$INSTALL_DIR/Spot.app"
/usr/bin/ditto "$STAGING_DIR/Spot.app" "$INSTALL_DIR/Spot.app"
rm -rf "$STAGING_DIR"
ln -sf "$INSTALL_DIR/Spot.app/Contents/MacOS/spot" "$BIN_DIR/spot"
echo "==> Installed Spot build #${BUILD_NUMBER} (${SHORT_SHA}) to $INSTALL_DIR/Spot.app"
