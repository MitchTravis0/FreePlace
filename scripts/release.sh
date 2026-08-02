#!/usr/bin/env bash
# Phase 7 release pipeline: build outputs -> hashes -> key bytes -> Vite build
# -> reproducible web archive -> publish everything to the node -> flip the
# facade pointer. Idempotent: unchanged components re-publish as no-ops, and
# the manifest ($PUBLISHED_DIR/release.env) is only written after everything
# published. Contracts and the delegate must already be built (make
# build-contracts build-delegates).
#
# Environment:
#   WS_API_PORT        node port (default 7510, the isolated dev node; 7509 is
#                      the system node joined to the REAL network)
#   PUBLISHED_DIR      manifest dir (default published/; committed by hand)
#   FREEPLACE_KEY_FILE facade owner signing key (default
#                      ~/.config/freeplace/facade-owner.key; NEVER commit it)
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${WS_API_PORT:-7510}"
PUBLISHED_DIR="${PUBLISHED_DIR:-published}"
KEY_FILE="${FREEPLACE_KEY_FILE:-$HOME/.config/freeplace/facade-owner.key}"
WEBGEN=web/.gen
BUILD=.local/release

REG_WASM=contracts/registry-contract/target/wasm32-unknown-unknown/release/freeplace_registry_contract.wasm
TILE_WASM=contracts/tile-contract/target/wasm32-unknown-unknown/release/freeplace_tile_contract.wasm
CHAT_WASM=contracts/chat-contract/target/wasm32-unknown-unknown/release/freeplace_chat_contract.wasm
FACADE_WASM=contracts/facade-contract/target/wasm32-unknown-unknown/release/freeplace_facade_contract.wasm
DELEGATE_WASM=delegates/identity-delegate/target/wasm32-unknown-unknown/release/freeplace_identity_delegate.wasm

tool() { cargo run -q -p common --example release_tool -- "$@"; }
contract_id() { fdev get-contract-id --code "$1" --parameters "$2" | tail -1 | awk '{print $NF}'; }

# Publish, tolerating "already published": on failure the contract must at
# least be retrievable, otherwise the release aborts.
publish_contract() { # <code> <params> <state-args...>
  local code=$1 params=$2
  shift 2
  if ! fdev -p "$PORT" publish --code "$code" --parameters "$params" contract "$@"; then
    local id
    id=$(contract_id "$code" "$params")
    echo "== publish of $id returned nonzero; verifying it is present"
    fdev -p "$PORT" execute get "$id" --timeout 30 --output /dev/null
  fi
}

mkdir -p "$PUBLISHED_DIR/legacy" "$BUILD" "$WEBGEN"

PREV_ENV="$BUILD/prev-release.env"
rm -f "$PREV_ENV"
[ -f "$PUBLISHED_DIR/release.env" ] && cp "$PUBLISHED_DIR/release.env" "$PREV_ENV"

# ---- stable anchors: canvas id and facade owner key ------------------------

if [ ! -f "$PUBLISHED_DIR/canvas-id.hex" ]; then
  od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > "$PUBLISHED_DIR/canvas-id.hex"
  echo "== generated stable canvas id"
fi
CANVAS_HEX=$(cat "$PUBLISHED_DIR/canvas-id.hex")
OWNER_HEX=$(tool keygen "$KEY_FILE")
printf '%s' "$OWNER_HEX" > "$PUBLISHED_DIR/facade-owner.pub.hex"

# ---- instance parameters and ids -------------------------------------------

tool gen-registry "$BUILD" "$WEBGEN" "$CANVAS_HEX"
REGISTRY_ID=$(contract_id "$REG_WASM" "$BUILD/registry-params.bin")
tool gen-tiles "$BUILD" "$WEBGEN" "$REGISTRY_ID"
tool gen-chat "$BUILD" "$WEBGEN" "$REGISTRY_ID"
CHAT_ID=$(contract_id "$CHAT_WASM" "$BUILD/chat-params.bin")
printf '%s' "$REGISTRY_ID" > "$WEBGEN/registry_contract_id.txt"
printf '%s' "$CHAT_ID" > "$WEBGEN/chat_contract_id.txt"
echo "== registry id: $REGISTRY_ID"
echo "== chat id:     $CHAT_ID"

cargo run -q -p common --example phase5_smoke -- delegate-keys \
  "$DELEGATE_WASM" "$WEBGEN" "$BUILD/identity-delegate-versioned.bin"

# delegate-keys writes a placeholder ghostkeys address (right for the dev
# node, which has no ghostkeys delegate). A real release bakes in the actual
# platform delegate address recorded in $PUBLISHED_DIR (derived from the
# installed delegate WASM: key = blake3(code_hash), code_hash = blake3(wasm)).
for f in ghostkeys_delegate_key_bytes.json ghostkeys_delegate_code_hash_bytes.json; do
  [ -f "$PUBLISHED_DIR/$f" ] && cp "$PUBLISHED_DIR/$f" "$WEBGEN/$f"
done

declare -A TILE_ID
TILES_JSON="["
for x in 0 1 2 3; do
  for y in 0 1 2 3; do
    TILE_ID[$x-$y]=$(contract_id "$TILE_WASM" "$BUILD/tile-params-$x-$y.bin")
    TILES_JSON+="{\"x\":$x,\"y\":$y,\"id\":\"${TILE_ID[$x-$y]}\",\"params\":$(cat "$WEBGEN/tile_params_bytes-$x-$y.json")},"
  done
done
printf '%s]' "${TILES_JSON%,}" > "$WEBGEN/tiles.json"
# The phase 5 harness page stays pointed at tile (0,0).
cp "$WEBGEN/tile_params_bytes-0-0.json" "$WEBGEN/tile_params_bytes.json"
printf '%s' "${TILE_ID[0-0]}" > "$WEBGEN/tile_contract_id.txt"

# ---- legacy bookkeeping: displaced ids feed the UI's migration probe -------

record_legacy() { # <file> <old_id> <new_id>
  local file="$PUBLISHED_DIR/legacy/$1" old=$2 new=$3
  [ -z "$old" ] || [ "$old" = "$new" ] && return 0
  touch "$file"
  grep -qxF "$old" "$file" || echo "$old" >> "$file"
}
# Read the previous manifest in subshells so its plain variable names cannot
# clobber this run's values.
if [ -f "$PREV_ENV" ]; then
  OLD_REGISTRY_ID=$(source "$PREV_ENV" && echo "$REGISTRY_ID")
  OLD_CHAT_ID=$(source "$PREV_ENV" && echo "$CHAT_ID")
  OLD_FACADE_ID=$(source "$PREV_ENV" && echo "${FACADE_ID:-}")
  OLD_WEBAPP_ID=$(source "$PREV_ENV" && echo "${WEBAPP_ID:-}")
  record_legacy registry.ids "$OLD_REGISTRY_ID" "$REGISTRY_ID"
  record_legacy chat.ids "$OLD_CHAT_ID" "$CHAT_ID"
  for x in 0 1 2 3; do
    for y in 0 1 2 3; do
      old=$(source "$PREV_ENV" && eval "echo \$TILE_ID_${x}_${y}")
      record_legacy "tile-$x-$y.ids" "$old" "${TILE_ID[$x-$y]}"
    done
  done
fi

ids_json() { # <file> -> JSON array, newest first
  local file="$PUBLISHED_DIR/legacy/$1"
  if [ -f "$file" ] && [ -s "$file" ]; then
    printf '['
    tac "$file" | awk '{printf "%s\"%s\"", (NR>1 ? "," : ""), $0}'
    printf ']'
  else
    printf '[]'
  fi
}
{
  printf '{"registry":%s,"chat":%s,"tiles":[' "$(ids_json registry.ids)" "$(ids_json chat.ids)"
  sep=""
  for x in 0 1 2 3; do
    for y in 0 1 2 3; do
      printf '%s{"x":%s,"y":%s,"ids":%s}' "$sep" "$x" "$y" "$(ids_json "tile-$x-$y.ids")"
      sep=","
    done
  done
  printf ']}'
} > "$WEBGEN/legacy_ids.json"

# ---- web build and reproducible archive ------------------------------------

echo "== building the web app"
(cd web && npm run build)
tar -C web/dist --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  -cJf "$PWD/$BUILD/webapp.tar.xz" .

# ---- sign and publish ------------------------------------------------------

VERSION=$(date +%s)
tool sign-webapp "$KEY_FILE" "$BUILD/webapp.tar.xz" "$VERSION" \
  "$BUILD/webapp-params.bin" "$BUILD/webapp-meta.bin"
WEBAPP_ID=$(contract_id "$FACADE_WASM" "$BUILD/webapp-params.bin")
echo "== webapp container id: $WEBAPP_ID (version $VERSION)"
# Displaced container ids feed the facade pointer's rollback ring.
record_legacy webapp.ids "${OLD_WEBAPP_ID:-}" "$WEBAPP_ID"

echo "== publishing contracts and delegate"
publish_contract "$REG_WASM" "$BUILD/registry-params.bin" --state "$BUILD/registry-state.bin"
for x in 0 1 2 3; do
  for y in 0 1 2 3; do
    publish_contract "$TILE_WASM" "$BUILD/tile-params-$x-$y.bin" --state "$BUILD/tile-state.bin"
  done
done
publish_contract "$CHAT_WASM" "$BUILD/chat-params.bin" --state "$BUILD/chat-state.bin"
fdev -p "$PORT" publish --code "$BUILD/identity-delegate-versioned.bin" delegate

echo "== publishing the web container"
publish_contract "$FACADE_WASM" "$BUILD/webapp-params.bin" \
  --webapp-archive "$BUILD/webapp.tar.xz" --webapp-metadata "$BUILD/webapp-meta.bin"

# ---- loader + facade pointer -----------------------------------------------

echo "== rendering and signing the facade loader"
rm -rf "$BUILD/loader"
mkdir -p "$BUILD/loader"
cp web/loader/index.html.tmpl "$BUILD/loader/index.html"
sed "s/{{CURRENT_APP_ID}}/$WEBAPP_ID/" web/loader/loader.js.tmpl > "$BUILD/loader/loader.js"
tar -C "$BUILD/loader" --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  -cJf "$PWD/$BUILD/loader.tar.xz" .

PREV_APPS=()
if [ -f "$PUBLISHED_DIR/legacy/webapp.ids" ]; then
  while IFS= read -r id; do PREV_APPS+=("$id"); done < <(tac "$PUBLISHED_DIR/legacy/webapp.ids" | head -8)
fi
tool sign-facade "$KEY_FILE" "$BUILD/loader.tar.xz" "$VERSION" "$WEBAPP_ID" \
  "$BUILD/facade-params.bin" "$BUILD/facade-meta.bin" "$BUILD/facade-state.bin" \
  ${PREV_APPS[@]+"${PREV_APPS[@]}"}
FACADE_ID=$(contract_id "$FACADE_WASM" "$BUILD/facade-params.bin")

if [ -n "${OLD_FACADE_ID:-}" ] && [ "$OLD_FACADE_ID" = "$FACADE_ID" ]; then
  echo "== flipping the facade pointer at $FACADE_ID"
  fdev -p "$PORT" execute update --as-state "$FACADE_ID" "$BUILD/facade-state.bin"
else
  echo "== publishing the facade (stable URL) at $FACADE_ID"
  publish_contract "$FACADE_WASM" "$BUILD/facade-params.bin" \
    --webapp-archive "$BUILD/loader.tar.xz" --webapp-metadata "$BUILD/facade-meta.bin"
fi

# The gateway caches extracted webapps and an UPDATE does not invalidate the
# cache (facade-pattern.md); bust the known cache locations best-effort.
for cache in "$HOME/.cache/freenet/webapp_cache" \
             "$HOME/.local/share/freeplace-dev-node/data/webapp_cache"; do
  rm -rf "$cache/$FACADE_ID" "$cache/$FACADE_ID.hash" 2>/dev/null || true
done
curl -s -o /dev/null "http://127.0.0.1:$PORT/v1/contract/web/$FACADE_ID/" || true

# ---- manifest (written last, so a failed release leaves the old one) -------

{
  echo "# FreePlace release manifest; written by scripts/release.sh."
  echo "RELEASE_VERSION=$VERSION"
  echo "REGISTRY_ID=$REGISTRY_ID"
  echo "CHAT_ID=$CHAT_ID"
  for x in 0 1 2 3; do
    for y in 0 1 2 3; do
      echo "TILE_ID_${x}_${y}=${TILE_ID[$x-$y]}"
    done
  done
  echo "WEBAPP_ID=$WEBAPP_ID"
  echo "FACADE_ID=$FACADE_ID"
  echo "REGISTRY_HASH=$(tool hash "$REG_WASM")"
  echo "TILE_HASH=$(tool hash "$TILE_WASM")"
  echo "CHAT_HASH=$(tool hash "$CHAT_WASM")"
  echo "FACADE_HASH=$(tool hash "$FACADE_WASM")"
  echo "DELEGATE_HASH=$(tool hash "$DELEGATE_WASM")"
} > "$PUBLISHED_DIR/release.env.tmp"
mv "$PUBLISHED_DIR/release.env.tmp" "$PUBLISHED_DIR/release.env"

echo "== release complete"
echo "   stable URL: http://127.0.0.1:$PORT/v1/contract/web/$FACADE_ID/"
echo "   webapp:     http://127.0.0.1:$PORT/v1/contract/web/$WEBAPP_ID/"
