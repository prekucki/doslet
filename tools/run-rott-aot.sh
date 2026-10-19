#!/bin/sh
# Local ROTT acceptance launch; assets and generated procedures remain private.
set -eu
cd "$(dirname "$0")/.."
procedure_bundle=${DOSLET_ROTT_PROCEDURES:-target/rott-aot-embedded-before-irq/procedures.wasm}

if [ ! -x target/release/doslet ]; then
    echo 'Build first: cargo build -p doslet --features wasm-procedures --release --locked' >&2
    exit 1
fi
if [ ! -f "$procedure_bundle" ] || [ ! -f target/rott-aot/start.state ]; then
    echo 'Prepare the local ROTT bundle as described in docs/compatibility/wasm-core.md.' >&2
    exit 1
fi

exec target/release/doslet \
    --config games/ROTT.toml \
    --wasm-procedures "$procedure_bundle" \
    --no-speed-tiers --no-dynamic-core "$@"
