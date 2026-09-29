#!/usr/bin/env bash
# Build metrognome's DSP for the browser and drop it next to the page.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --manifest-path "$root/Cargo.toml" -p metrognome-wasm --release --target wasm32-unknown-unknown
cp "$root/target/wasm32-unknown-unknown/release/metrognome_wasm.wasm" "$root/site/metrognome.wasm"
echo "site/metrognome.wasm: $(wc -c < "$root/site/metrognome.wasm") bytes" >&2
