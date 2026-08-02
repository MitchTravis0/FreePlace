#!/usr/bin/env bash
# Phase 6 exit check, iso tier: publish the registry, all 16 tiles, the chat
# room, and the identity delegate to the local dev node; build the web app
# against their ids; package and publish it as a gateway-served webapp (the
# facade stub accepts any state and serves as the interim web container until
# Phase 7); then drive the real UI through the gateway iframe with
# Playwright (web/tests/phase6-iso.spec.ts).
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${WS_API_PORT:-7510}"
OUT=.local/phase6
WEBGEN=web/.gen
REG_WASM=contracts/registry-contract/target/wasm32-unknown-unknown/release/freeplace_registry_contract.wasm
TILE_WASM=contracts/tile-contract/target/wasm32-unknown-unknown/release/freeplace_tile_contract.wasm
CHAT_WASM=contracts/chat-contract/target/wasm32-unknown-unknown/release/freeplace_chat_contract.wasm
FACADE_WASM=contracts/facade-contract/target/wasm32-unknown-unknown/release/freeplace_facade_contract.wasm
DELEGATE_WASM=delegates/identity-delegate/target/wasm32-unknown-unknown/release/freeplace_identity_delegate.wasm

contract_id() {
  fdev get-contract-id --code "$1" --parameters "$2" | tail -1 | awk '{print $NF}'
}

echo "== generating fixtures"
cargo run -q -p common --example phase6_smoke -- gen-registry "$OUT" "$WEBGEN"
REG_ID=$(contract_id "$REG_WASM" "$OUT/registry-params.bin")
echo "== registry id: $REG_ID"
cargo run -q -p common --example phase6_smoke -- gen-tiles "$OUT" "$WEBGEN" "$REG_ID"
cargo run -q -p common --example phase6_smoke -- gen-chat "$OUT" "$WEBGEN" "$REG_ID"
cargo run -q -p common --example phase5_smoke -- delegate-keys "$DELEGATE_WASM" "$WEBGEN" "$OUT/identity-delegate-versioned.bin"

CHAT_ID=$(contract_id "$CHAT_WASM" "$OUT/chat-params.bin")
echo "== chat id: $CHAT_ID"
printf '%s' "$REG_ID" > "$WEBGEN/registry_contract_id.txt"
printf '%s' "$CHAT_ID" > "$WEBGEN/chat_contract_id.txt"

echo "== assembling tiles.json and publishing the 16 tiles"
TILES_JSON="["
for x in 0 1 2 3; do
  for y in 0 1 2 3; do
    TILE_ID=$(contract_id "$TILE_WASM" "$OUT/tile-params-$x-$y.bin")
    PARAMS=$(cat "$WEBGEN/tile_params_bytes-$x-$y.json")
    TILES_JSON+="{\"x\":$x,\"y\":$y,\"id\":\"$TILE_ID\",\"params\":$PARAMS},"
    fdev -p "$PORT" publish --code "$TILE_WASM" --parameters "$OUT/tile-params-$x-$y.bin" \
      contract --state "$OUT/tile-state.bin"
  done
done
printf '%s]' "${TILES_JSON%,}" > "$WEBGEN/tiles.json"

# Keep the phase 5 harness page (second Vite entry) pointed at tile (0,0).
cp "$WEBGEN/tile_params_bytes-0-0.json" "$WEBGEN/tile_params_bytes.json"
printf '%s' "$(contract_id "$TILE_WASM" "$OUT/tile-params-0-0.bin")" > "$WEBGEN/tile_contract_id.txt"

echo "== publishing registry, chat, and the identity delegate"
fdev -p "$PORT" publish --code "$REG_WASM" --parameters "$OUT/registry-params.bin" contract --state "$OUT/registry-state.bin"
fdev -p "$PORT" publish --code "$CHAT_WASM" --parameters "$OUT/chat-params.bin" contract --state "$OUT/chat-state.bin"
fdev -p "$PORT" publish --code "$OUT/identity-delegate-versioned.bin" delegate

echo "== building the web app"
# Fresh standalone instances have no predecessors: overwrite whatever
# legacy_ids.json a previous release/phase7 run left behind, or the migration
# probe stalls syncAll on GETs for instances this node has never seen.
printf '{"registry":[],"chat":[],"tiles":[]}' > "$WEBGEN/legacy_ids.json"
(cd web && npm run build)

echo "== packaging and publishing the webapp as a signed web container"
tar -C web/dist --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  -cJf "$PWD/$OUT/webapp.tar.xz" .
# The facade contract is the real signed-pointer implementation since Phase 7,
# so the container must be signed; use a throwaway dev key. The unix-time
# version lands in the params, so each run gets a fresh id and the gateway
# serves this build, not a stale state under the same id.
tool() { cargo run -q -p common --example release_tool -- "$@"; }
tool keygen "$OUT/dev-owner.key" > /dev/null
tool sign-webapp "$OUT/dev-owner.key" "$OUT/webapp.tar.xz" "$(date +%s)" \
  "$OUT/webapp-params.bin" "$OUT/webapp-meta.bin"
fdev -p "$PORT" publish --code "$FACADE_WASM" --parameters "$OUT/webapp-params.bin" \
  contract --webapp-archive "$OUT/webapp.tar.xz" --webapp-metadata "$OUT/webapp-meta.bin"
WEB_ID=$(contract_id "$FACADE_WASM" "$OUT/webapp-params.bin")
echo "== webapp id: $WEB_ID"

echo "== running the gateway browser exit check"
(cd web && FREENET_BASE_URL="http://127.0.0.1:$PORT/v1/contract/web/$WEB_ID/" \
  npx playwright test tests/phase6-iso.spec.ts)

echo "PHASE 6 SMOKE PASSED"
