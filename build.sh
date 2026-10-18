#!/usr/bin/env bash
# Build the frontend and the server using the project-local toolchains.
set -euo pipefail
cd "$(dirname "$0")"

export RUSTUP_HOME="$PWD/.toolchain/rustup"
export CARGO_HOME="$PWD/.toolchain/cargo"
export PATH="$PWD/.toolchain/cargo/bin:$PWD/.toolchain/node/bin:$PATH"

echo "==> building frontend"
(cd frontend && npm install --no-fund --no-audit && npm run build)

echo "==> building server (frontend gets embedded into the binary)"
(cd server && cargo build --release)

echo "==> done: server/target/release/ms01ctl"
