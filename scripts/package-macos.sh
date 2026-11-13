#!/usr/bin/env bash
# Build and package Shortlist as a macOS .app bundle + release zip.
#
# The asset name must stay in sync with `shortlist_update::expected_asset_name`:
#   Shortlist-macos-aarch64.zip  (Apple Silicon)
#   Shortlist-macos-x86_64.zip   (Intel)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Match the minimum macOS version written into Info.plist.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"

VERSION="$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)"
if [[ -z "$VERSION" ]]; then
  echo "Could not read the workspace version from Cargo.toml." >&2
  exit 1
fi

case "$(uname -m)" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64|amd64)  ARCH="x86_64" ;;
  *)             ARCH="$(uname -m)" ;;
esac

OUT="$ROOT/dist"
APP="$OUT/Shortlist.app"
ZIP="$OUT/Shortlist-macos-$ARCH.zip"

MODEL_DIR="$ROOT/assets/models/e5-small-v2"
if [[ ! -f "$MODEL_DIR/model_quantized.onnx" ]]; then
  echo "e5-small-v2 is not present; fetching it for the app bundle."
  "$ROOT/scripts/fetch-model.sh"
fi

echo "Building Shortlist $VERSION for macOS ${ARCH}…"
cargo build --release -p shortlist-app --bin shortlist
cargo build --release -p shortlist-parse --bin shortlist-pdf

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/models"

cp target/release/shortlist "$APP/Contents/MacOS/shortlist"
cp target/release/shortlist-pdf "$APP/Contents/MacOS/shortlist-pdf"
chmod +x "$APP/Contents/MacOS/shortlist" "$APP/Contents/MacOS/shortlist-pdf"
cp -R "$MODEL_DIR" "$APP/Contents/Resources/models/e5-small-v2"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>Shortlist</string>
  <key>CFBundleExecutable</key>
  <string>shortlist</string>
  <key>CFBundleIdentifier</key>
  <string>com.shortlist.app</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Shortlist</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.business</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSHumanReadableCopyright</key>
  <string>Shortlist · Apache-2.0</string>
</dict>
</plist>
PLIST

if command -v plutil >/dev/null 2>&1; then
  plutil -lint "$APP/Contents/Info.plist" >/dev/null
fi

if [[ -f "$ROOT/assets/brand/AppIcon.icns" ]]; then
  cp "$ROOT/assets/brand/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
  /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string AppIcon" "$APP/Contents/Info.plist" 2>/dev/null || true
fi

# Ad-hoc signing is enough to keep macOS from treating the app as damaged.
# Public distribution should replace this with a Developer ID signature and
# notarization; the updater itself only trusts the SHA-256 manifest.
if command -v codesign >/dev/null 2>&1; then
  echo "Ad-hoc signing the app bundle…"
  codesign --force --sign - "$APP" >/dev/null 2>&1 || echo "warning: ad-hoc signing failed" >&2
fi

rm -f "$ZIP" "$ZIP.sha256"
echo "Packaging ${ZIP}…"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
(
  cd "$OUT"
  shasum -a 256 "Shortlist-macos-$ARCH.zip" > "Shortlist-macos-$ARCH.zip.sha256"
)

echo
echo "Built $ZIP"
echo "Checksum: $(cut -d' ' -f1 "$ZIP.sha256")"
