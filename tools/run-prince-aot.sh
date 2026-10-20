#!/bin/sh
# Private Machiavelli the Prince checkpoint and offline procedure bundle.
set -eu
cd "$(dirname "$0")/.."
procedure_bundle=${DOSLET_PRINCE_PROCEDURES:-target/prince-aot-embedded/procedures.wasm}
checkpoint=${DOSLET_PRINCE_STATE:-target/prince-aot-vga-waits/start.state}

if [ ! -x target/release/doslet ]; then
    echo 'Build first: cargo build -p doslet --features wasm-procedures --release --locked --offline' >&2
    exit 1
fi
if [ ! -f "$procedure_bundle" ] || [ ! -f "$checkpoint" ]; then
    echo 'Prepare the private PRINCE computation bundle and checkpoint as described in docs/compatibility/wasm-core.md.' >&2
    exit 1
fi

exec target/release/doslet \
    --config games/PRINCE.toml \
    --load-state "$checkpoint" \
    --wasm-procedures "$procedure_bundle" \
    --no-speed-tiers --no-dynamic-core "$@"
