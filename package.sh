#!/usr/bin/env bash
# Build an installable .deb package (frontend + server + systemd unit).
set -euo pipefail
cd "$(dirname "$0")"

export RUSTUP_HOME="$PWD/.toolchain/rustup"
export CARGO_HOME="$PWD/.toolchain/cargo"
export PATH="$PWD/.toolchain/cargo/bin:$PWD/.toolchain/node/bin:$PATH"

echo "==> building frontend"
(cd frontend && npm install --no-fund --no-audit && npm run build)

echo "==> building .deb"
(cd server && cargo deb)

deb=$(ls -t server/target/debian/ms01ctl_*.deb | head -1)
echo "==> package ready: $deb"
echo "    install with: apt install ./$deb"
