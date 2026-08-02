#!/usr/bin/env bash
# Phase 4 exit check on the local dev node: a post by an admitted identity is
# accepted and received via subscription, a tampered post is rejected, a post
# by an unadmitted identity does not survive the registry-gated validate pass,
# and a single-author flood beyond the caps evicts deterministically down to
# the per-author stored cap.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${WS_API_PORT:-7510}"
OUT=.local/phase4
REG_WASM=contracts/registry-contract/target/wasm32-unknown-unknown/release/freeplace_registry_contract.wasm
CHAT_WASM=contracts/chat-contract/target/wasm32-unknown-unknown/release/freeplace_chat_contract.wasm
POST_CONTENT="hello from the phase 4 smoke"

contract_id() {
  fdev get-contract-id --code "$1" --parameters "$2" | tail -1 | awk '{print $NF}'
}

echo "== generating registry fixtures (grinds a PoW nonce)"
cargo run -q -p common --example phase4_smoke -- gen-registry "$OUT"

REG_ID=$(contract_id "$REG_WASM" "$OUT/registry-params.bin")
echo "== registry id: $REG_ID"

echo "== publishing registry and admitting the smoke identity"
fdev -p "$PORT" publish --code "$REG_WASM" --parameters "$OUT/registry-params.bin" contract --state "$OUT/registry-state.bin"
fdev -p "$PORT" execute update "$REG_ID" "$OUT/delta-admit.bin"

echo "== generating chat fixtures bound to the registry id"
cargo run -q -p common --example phase4_smoke -- gen-chat "$OUT" "$REG_ID"

CHAT_ID=$(contract_id "$CHAT_WASM" "$OUT/chat-params.bin")
echo "== chat id: $CHAT_ID"

echo "== publishing the chat contract"
fdev -p "$PORT" publish --code "$CHAT_WASM" --parameters "$OUT/chat-params.bin" contract --state "$OUT/chat-state.bin"

echo "== subscribing to the chat contract in the background"
rm -f "$OUT/sub-update.bin"
fdev -p "$PORT" execute subscribe "$CHAT_ID" -o "$OUT/sub-update.bin" --timeout 60 \
  > "$OUT/subscribe.log" 2>&1 &
SUB_PID=$!
trap 'kill "$SUB_PID" 2>/dev/null || true' EXIT
sleep 5

echo "== post by the admitted identity (expect accepted)"
fdev -p "$PORT" execute update "$CHAT_ID" "$OUT/delta-post.bin"
fdev -p "$PORT" execute get "$CHAT_ID" -o "$OUT/chat-state-1.bin" --timeout 60
cargo run -q -p common --example phase4_smoke -- check-chat "$OUT/chat-state-1.bin"

echo "== waiting for the update notification on the subscription"
for _ in $(seq 1 30); do
  [ -s "$OUT/sub-update.bin" ] && break
  sleep 1
done
cargo run -q -p common --example phase4_smoke -- check-sub "$OUT/sub-update.bin" "$POST_CONTENT"

echo "== tampered post (expect rejected)"
if fdev -p "$PORT" execute update "$CHAT_ID" "$OUT/delta-post-tampered.bin"; then
  echo "ERROR: tampered post was accepted"
  exit 1
fi

echo "== post by an unadmitted identity (expect no state change)"
# update_state's cheap checks pass, so the update call itself may or may not
# report an error; what matters is that the host's registry-gated validate
# pass rolls the state back.
fdev -p "$PORT" execute update "$CHAT_ID" "$OUT/delta-post-unadmitted.bin" || true
fdev -p "$PORT" execute get "$CHAT_ID" -o "$OUT/chat-state-2.bin" --timeout 60
cargo run -q -p common --example phase4_smoke -- check-chat "$OUT/chat-state-2.bin"

echo "== single-author flood beyond the caps (expect deterministic per-author eviction)"
fdev -p "$PORT" execute update "$CHAT_ID" "$OUT/delta-flood.bin"
fdev -p "$PORT" execute get "$CHAT_ID" -o "$OUT/chat-state-3.bin" --timeout 60
cargo run -q -p common --example phase4_smoke -- check-evicted "$OUT/chat-state-3.bin"

echo "PHASE 4 SMOKE PASSED"
