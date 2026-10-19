#!/usr/bin/env bash
set -euo pipefail

# Package and install Spot.app into /Applications (plus ~/.local/bin/spot CLI symlink).
# Usage:
#   ./scripts/install.sh                # builds & installs to /Applications/Spot.app
#   ./scripts/install.sh --skip-build   # installs existing dist/Spot.app without rebuilding

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

SKIP_BUILD=0
INSTALL_DIR="${SPOT_INSTALL_DIR:-/Applications}"
BIN_LINK_DIR="${SPOT_BIN_DIR:-$HOME/.local/bin}"

for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=1 ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Usage: $0 [--skip-build]" >&2
      exit 1
      ;;
  esac
done

APP_SRC="$ROOT_DIR/dist/Spot.app"
if [[ "$SKIP_BUILD" -eq 0 || ! -d "$APP_SRC" ]]; then
  "$ROOT_DIR/scripts/package-macos.sh"
fi

TARGET_APP="$INSTALL_DIR/Spot.app"
echo "==> Installing Spot.app to $TARGET_APP..."
rm -rf "$TARGET_APP"
/usr/bin/ditto "$APP_SRC" "$TARGET_APP"

# Touch the bundle so Finder / Dock refreshes the icon cache immediately
touch "$TARGET_APP"

mkdir -p "$BIN_LINK_DIR"
ln -sf "$TARGET_APP/Contents/MacOS/spot" "$BIN_LINK_DIR/spot"

echo "==> Installed:"
echo "    App: $TARGET_APP"
echo "    CLI: $BIN_LINK_DIR/spot"
echo ""
echo "Launch with: open -a Spot"
