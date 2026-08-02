#!/usr/bin/env bash
# Phase 2 exit check on the local dev node: a valid PoW admission is accepted,
# an invalid nonce and a tampered record are rejected, nickname updates apply
# with monotonic-version replay protection.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${WS_API_PORT:-7510}"
OUT=.local/phase2
WASM=contracts/registry-contract/target/wasm32-unknown-unknown/release/freeplace_registry_contract.wasm

echo "== generating fixtures (grinds two PoW nonces)"
cargo run -q -p common --example phase2_smoke -- gen "$OUT"

KEY=$(fdev get-contract-id --code "$WASM" --parameters "$OUT/params.bin" | tail -1 | awk '{print $NF}')
echo "== contract id: $KEY"

echo "== publishing genesis registry to the dev node on port $PORT"
fdev -p "$PORT" publish --code "$WASM" --parameters "$OUT/params.bin" contract --state "$OUT/state.bin"

echo "== admission with valid PoW nonce (expect accepted)"
fdev -p "$PORT" execute update "$KEY" "$OUT/delta-valid.bin"
fdev -p "$PORT" execute get "$KEY" -o "$OUT/state-1.bin" --timeout 60
cargo run -q -p common --example phase2_smoke -- check "$OUT/state-1.bin" smoke 1

echo "== nickname update to v2 (expect accepted)"
fdev -p "$PORT" execute update "$KEY" "$OUT/delta-nick.bin"
fdev -p "$PORT" execute get "$KEY" -o "$OUT/state-2.bin" --timeout 60
cargo run -q -p common --example phase2_smoke -- check "$OUT/state-2.bin" renamed 2

echo "== nickname replay of v1 (expect no state change)"
fdev -p "$PORT" execute update "$KEY" "$OUT/delta-nick-replay.bin"
fdev -p "$PORT" execute get "$KEY" -o "$OUT/state-3.bin" --timeout 60
cargo run -q -p common --example phase2_smoke -- check "$OUT/state-3.bin" renamed 2

echo "== admission with invalid nonce (expect rejected)"
if fdev -p "$PORT" execute update "$KEY" "$OUT/delta-bad-nonce.bin"; then
  echo "ERROR: invalid nonce was accepted"
  exit 1
fi

echo "== tampered admission record (expect rejected)"
if fdev -p "$PORT" execute update "$KEY" "$OUT/delta-tampered.bin"; then
  echo "ERROR: tampered record was accepted"
  exit 1
fi

echo "== final state must still contain only the valid admission"
fdev -p "$PORT" execute get "$KEY" -o "$OUT/state-final.bin" --timeout 60
cargo run -q -p common --example phase2_smoke -- check "$OUT/state-final.bin" renamed 2

echo "PHASE 2 SMOKE PASSED"
