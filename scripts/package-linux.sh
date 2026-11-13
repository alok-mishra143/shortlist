#!/usr/bin/env bash
# Build and package Shortlist as a portable Linux folder + release zip.
#
# Asset name contract (crates/update/src/lib.rs):
#   Shortlist-linux-x86_64.zip
#   Shortlist-linux-aarch64.zip
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)"
if [[ -z "$VERSION" ]]; then
  echo "Could not read the workspace version from Cargo.toml." >&2
  exit 1
fi

case "$(uname -m)" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *)             echo "Unsupported Linux architecture: $(uname -m)" >&2; exit 1 ;;
esac

if ! command -v zip >/dev/null 2>&1; then
  echo "zip is required: sudo apt-get install zip" >&2
  exit 1
fi

MODEL_DIR="$ROOT/assets/models/e5-small-v2"
if [[ ! -f "$MODEL_DIR/model_quantized.onnx" ]]; then
  echo "e5-small-v2 is not present; fetching it for the app bundle."
  "$ROOT/scripts/fetch-model.sh"
fi

echo "Building Shortlist $VERSION for Linux ${ARCH}…"
cargo build --release -p shortlist-app --bin shortlist
cargo build --release -p shortlist-parse --bin shortlist-pdf

OUT="$ROOT/dist"
APP="$OUT/Shortlist"
ZIP="$OUT/Shortlist-linux-$ARCH.zip"

rm -rf "$APP"
mkdir -p "$APP/assets/models"

cp target/release/shortlist "$APP/shortlist"
cp target/release/shortlist-pdf "$APP/shortlist-pdf"
chmod +x "$APP/shortlist" "$APP/shortlist-pdf"
cp -R "$MODEL_DIR" "$APP/assets/models/e5-small-v2"

cat > "$APP/shortlist-install.json" <<JSON
{
  "app": "shortlist",
  "version": "$VERSION",
  "platform": "linux",
  "executable": "shortlist"
}
JSON

rm -f "$ZIP" "$ZIP.sha256"
echo "Packaging ${ZIP}…"
(
  cd "$OUT"
  zip -q -r "Shortlist-linux-$ARCH.zip" "Shortlist"
  sha256sum "Shortlist-linux-$ARCH.zip" > "Shortlist-linux-$ARCH.zip.sha256"
)

echo
echo "Built $ZIP"
echo "Checksum: $(cut -d' ' -f1 "$ZIP.sha256")"
