#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ ! -f assets/models/e5-small-v2/model_quantized.onnx ]]; then
  echo "e5-small-v2 not found; fetching it once."
  ./scripts/fetch-model.sh
fi

# Build the app and the isolated PDF helper together.
cargo build -p shortlist-app -p shortlist-parse --bins

exec ./target/debug/shortlist "$@"
