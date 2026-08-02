#!/usr/bin/env bash
# Phase 5 exit check on the local dev node, driven from a real browser:
# create an identity in the identity delegate, persist it across a page
# reload, sign a placement the tile contract accepts, and run the ghost key
# flow to one of its expected states. See web/tests/phase5.spec.ts.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${WS_API_PORT:-7510}"
OUT=.local/phase5
WEBGEN=web/.gen
REG_WASM=contracts/registry-contract/target/wasm32-unknown-unknown/release/freeplace_registry_contract.wasm
TILE_WASM=contracts/tile-contract/target/wasm32-unknown-unknown/release/freeplace_tile_contract.wasm
DELEGATE_WASM=delegates/identity-delegate/target/wasm32-unknown-unknown/release/freeplace_identity_delegate.wasm

contract_id() {
  fdev get-contract-id --code "$1" --parameters "$2" | tail -1 | awk '{print $NF}'
}

echo "== generating registry fixtures"
cargo run -q -p common --example phase5_smoke -- gen-registry "$OUT" "$WEBGEN"

REG_ID=$(contract_id "$REG_WASM" "$OUT/registry-params.bin")
echo "== registry id: $REG_ID"

echo "== generating tile fixtures bound to the registry id"
cargo run -q -p common --example phase5_smoke -- gen-tile "$OUT" "$WEBGEN" "$REG_ID"

TILE_ID=$(contract_id "$TILE_WASM" "$OUT/tile-params.bin")
echo "== tile id: $TILE_ID"

printf '%s' "$REG_ID" > "$WEBGEN/registry_contract_id.txt"
printf '%s' "$TILE_ID" > "$WEBGEN/tile_contract_id.txt"

echo "== computing delegate key bytes and packaging the versioned delegate"
cargo run -q -p common --example phase5_smoke -- delegate-keys "$DELEGATE_WASM" "$WEBGEN" "$OUT/identity-delegate-versioned.bin"

echo "== publishing the registry and tile contracts"
fdev -p "$PORT" publish --code "$REG_WASM" --parameters "$OUT/registry-params.bin" contract --state "$OUT/registry-state.bin"
fdev -p "$PORT" publish --code "$TILE_WASM" --parameters "$OUT/tile-params.bin" contract --state "$OUT/tile-state.bin"

echo "== publishing the identity delegate"
fdev -p "$PORT" publish --code "$OUT/identity-delegate-versioned.bin" delegate

echo "== running the browser exit check"
(cd web && WS_API_HOST="127.0.0.1:$PORT" npx playwright test)

echo "phase 5 smoke passed"
