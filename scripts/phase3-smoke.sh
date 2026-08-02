#!/usr/bin/env bash
# Phase 3 exit check on the local dev node: 16 tile instances published with
# distinct (tile_x, tile_y) parameters, a placement by an admitted identity is
# accepted, a tampered placement is rejected, and a placement by an unadmitted
# identity does not survive the registry-gated validate pass.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${WS_API_PORT:-7510}"
OUT=.local/phase3
REG_WASM=contracts/registry-contract/target/wasm32-unknown-unknown/release/freeplace_registry_contract.wasm
TILE_WASM=contracts/tile-contract/target/wasm32-unknown-unknown/release/freeplace_tile_contract.wasm

contract_id() {
  fdev get-contract-id --code "$1" --parameters "$2" | tail -1 | awk '{print $NF}'
}

echo "== generating registry fixtures (grinds a PoW nonce)"
cargo run -q -p common --example phase3_smoke -- gen-registry "$OUT"

REG_ID=$(contract_id "$REG_WASM" "$OUT/registry-params.bin")
echo "== registry id: $REG_ID"

echo "== publishing registry and admitting the smoke identity"
fdev -p "$PORT" publish --code "$REG_WASM" --parameters "$OUT/registry-params.bin" contract --state "$OUT/registry-state.bin"
fdev -p "$PORT" execute update "$REG_ID" "$OUT/delta-admit.bin"

echo "== generating tile fixtures bound to the registry id"
cargo run -q -p common --example phase3_smoke -- gen-tiles "$OUT" "$REG_ID"

echo "== publishing 16 tile instances with distinct (tile_x, tile_y)"
IDS=""
for x in 0 1 2 3; do
  for y in 0 1 2 3; do
    fdev -p "$PORT" publish --code "$TILE_WASM" --parameters "$OUT/tile-params-$x-$y.bin" contract --state "$OUT/tile-state.bin"
    IDS="$IDS $(contract_id "$TILE_WASM" "$OUT/tile-params-$x-$y.bin")"
  done
done
DISTINCT=$(echo "$IDS" | tr ' ' '\n' | sed '/^$/d' | sort -u | wc -l)
if [ "$DISTINCT" -ne 16 ]; then
  echo "ERROR: expected 16 distinct tile contract ids, got $DISTINCT"
  exit 1
fi
echo "== 16 distinct tile contract ids confirmed"

TILE00_ID=$(contract_id "$TILE_WASM" "$OUT/tile-params-0-0.bin")
echo "== tile (0,0) id: $TILE00_ID"

echo "== placement by the admitted identity (expect accepted)"
fdev -p "$PORT" execute update "$TILE00_ID" "$OUT/delta-place.bin"
fdev -p "$PORT" execute get "$TILE00_ID" -o "$OUT/tile-state-1.bin" --timeout 60
cargo run -q -p common --example phase3_smoke -- check-tile "$OUT/tile-state-1.bin"

echo "== tampered placement (expect rejected)"
if fdev -p "$PORT" execute update "$TILE00_ID" "$OUT/delta-place-tampered.bin"; then
  echo "ERROR: tampered placement was accepted"
  exit 1
fi

echo "== placement by an unadmitted identity (expect no state change)"
# update_state's cheap checks pass, so the update call itself may or may not
# report an error; what matters is that the host's registry-gated validate
# pass rolls the state back.
fdev -p "$PORT" execute update "$TILE00_ID" "$OUT/delta-place-unadmitted.bin" || true
fdev -p "$PORT" execute get "$TILE00_ID" -o "$OUT/tile-state-2.bin" --timeout 60
cargo run -q -p common --example phase3_smoke -- check-tile "$OUT/tile-state-2.bin"

echo "PHASE 3 SMOKE PASSED"
