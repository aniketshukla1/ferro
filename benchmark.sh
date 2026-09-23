#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cargo build --release --manifest-path "$ROOT/Cargo.toml"
BIN="$ROOT/target/release/ferro"
echo "ferro built: $BIN"
echo "usage: $BIN ~/src/linux --no-open"
"$BIN" --help | head -20
