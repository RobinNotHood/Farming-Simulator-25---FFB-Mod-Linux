#!/usr/bin/env bash
# Convenience wrapper. `./build.sh` builds everything; `./build.sh install`
# also installs to /usr/local and the FS25 mods dir.
set -euo pipefail
cd "$(dirname "$0")"

echo "==> Building Rust daemon..."
(cd daemon && cargo build --release)

echo "==> Packaging Lua mod..."
mkdir -p dist
(cd mod && zip -qr "../dist/FS25_FFBEnhancer.zip" FS25_FFBEnhancer \
    -x "*.dds.README")

echo "==> Artifacts:"
echo "    daemon:  daemon/target/release/fs25-ffb"
echo "    mod:     dist/FS25_FFBEnhancer.zip"

if [[ "${1:-}" == "install" ]]; then
    exec ./packaging/install.sh
fi
