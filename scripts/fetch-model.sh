#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/models/e5-small-v2"
BASE="https://huggingface.co/Xenova/e5-small-v2/resolve/main"

mkdir -p "$DIR"

fetch() {
  local remote="$1"
  local name="${remote##*/}"
  local out="$DIR/$name"
  if [[ -f "$out" ]]; then
    echo "  ok  $name"
    return
  fi
  echo "  get $name"
  curl -fL --retry 2 "$BASE/$remote" -o "$out"
}

fetch "onnx/model_quantized.onnx"
fetch "tokenizer.json"
fetch "tokenizer_config.json"
fetch "special_tokens_map.json"
fetch "config.json"

echo "e5-small-v2 ready: $DIR"
