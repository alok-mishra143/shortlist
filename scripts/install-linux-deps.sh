#!/usr/bin/env bash
# System libraries needed to compile GPUI, the PDF helper, and ONNX Runtime on
# Ubuntu/Debian. Used by CI and by the Linux release job.
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "install-linux-deps.sh only runs on Linux." >&2
  exit 1
fi

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential pkg-config cmake clang lld zip \
  libssl-dev libbz2-dev liblzma-dev zlib1g-dev libzstd-dev libdbus-1-dev \
  libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libx11-dev libx11-xcb-dev libxcb1-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxcb-dri3-dev libxcb-randr0-dev libxcb-shm0-dev \
  libxcb-sync-dev libxcb-xinput-dev libfontconfig1-dev \
  libfreetype6-dev libvulkan-dev
