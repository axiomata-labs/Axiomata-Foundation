#!/bin/sh
# Builds the tests' plugin into ../probe.wasm. Needs rustup's toolchain with the target `wasm32-wasip2` (Homebrew's
# Rust has none: `brew install rustup`, then `rustup toolchain install stable --target wasm32-wasip2`). The
# toolchain's own cargo and rustc are named directly: `rustup run` leaves a Homebrew Rust first on the PATH.
set -eu
cd "$(dirname "$0")"
RUSTUP="$(command -v rustup || echo /opt/homebrew/opt/rustup/bin/rustup)"
SYSROOT="$("$RUSTUP" run stable rustc --print sysroot)"
RUSTC="$SYSROOT/bin/rustc" "$SYSROOT/bin/cargo" build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/probe.wasm ../probe.wasm
ls -l ../probe.wasm
