#!/usr/bin/env bash
# Ops migration for a tile re-key (plan.md, "255-color palette" entry): the
# browser-side fold-forward write times out on the real network, so carry each
# displaced tile's state by hand — fdev GET the legacy instance, re-PUT the
# bytes under the new key with `fdev publish ... contract --state` (the tile
# contract re-validates every byte; CRDT merges make re-runs idempotent; the
# legacy instances are left untouched).
#
#   DRY_RUN=1 WS_API_PORT=7509 ./scripts/migrate-tiles-ops.sh   # read-only
#   WS_API_PORT=7509 ./scripts/migrate-tiles-ops.sh             # migrate
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${WS_API_PORT:-7510}"
PUBLISHED_DIR="${PUBLISHED_DIR:-published}"
DRY_RUN="${DRY_RUN:-}"
OUT=.local/tile-migrate
TILE_WASM=contracts/tile-contract/target/wasm32-unknown-unknown/release/freeplace_tile_contract.wasm

tool() { cargo run -q -p common --example release_tool -- "$@"; }
contract_id() { fdev get-contract-id --code "$1" --parameters "$2" | tail -1 | awk '{print $NF}'; }

# shellcheck source=/dev/null
source "$PUBLISHED_DIR/release.env"
[ -f "$TILE_WASM" ] || { echo "tile WASM missing; run: make build-contracts"; exit 1; }
mkdir -p "$OUT/webgen"

# The current WASM must be the released one, or the derived ids below would
# silently target instances that do not exist.
head=$(tool hash "$TILE_WASM")
if [ "$head" != "$TILE_HASH" ]; then
  echo "!! built tile WASM ($head) is not the released one ($TILE_HASH); aborting"
  exit 1
fi

# Regenerate the tile params (deterministic from canvas id + registry id) and
# prove each produces the published instance id before touching the network.
tool gen-registry "$OUT" "$OUT/webgen" "$(cat "$PUBLISHED_DIR/canvas-id.hex")"
tool gen-tiles "$OUT" "$OUT/webgen" "$REGISTRY_ID"

migrated=0 skipped=0 failed=0
for x in 0 1 2 3; do
  for y in 0 1 2 3; do
    params="$OUT/tile-params-$x-$y.bin"
    var="TILE_ID_${x}_${y}"
    new_id="${!var}"
    derived=$(contract_id "$TILE_WASM" "$params")
    if [ "$derived" != "$new_id" ]; then
      echo "!! tile($x,$y): derived id $derived != manifest $new_id; aborting"
      exit 1
    fi
    old_id=$(head -1 "$PUBLISHED_DIR/legacy/tile-$x-$y.ids" 2>/dev/null || true)
    if [ -z "$old_id" ]; then
      echo "-- tile($x,$y): no legacy id recorded; skipping"
      skipped=$((skipped + 1)); continue
    fi
    state="$OUT/tile-state-$x-$y.bin"
    if ! fdev -p "$PORT" execute get "$old_id" -o "$state" --timeout 60; then
      echo "!! tile($x,$y): GET of legacy $old_id failed"
      failed=$((failed + 1)); continue
    fi
    if cmp -s "$state" "$OUT/tile-state.bin"; then
      echo "-- tile($x,$y): legacy state is genesis-empty; nothing to carry"
      skipped=$((skipped + 1)); continue
    fi
    size=$(wc -c < "$state")
    if [ -n "$DRY_RUN" ]; then
      echo "== tile($x,$y): would carry $size bytes  $old_id -> $new_id"
      migrated=$((migrated + 1)); continue
    fi
    echo "== tile($x,$y): carrying $size bytes  $old_id -> $new_id"
    if ! fdev -p "$PORT" publish --code "$TILE_WASM" --parameters "$params" \
        contract --state "$state"; then
      echo "!! tile($x,$y): re-PUT under $new_id failed"
      failed=$((failed + 1)); continue
    fi
    # The destination was empty, so merge(old, empty) must GET back the exact
    # bytes we carried.
    if fdev -p "$PORT" execute get "$new_id" -o "$OUT/verify-$x-$y.bin" --timeout 60 \
        && cmp -s "$state" "$OUT/verify-$x-$y.bin"; then
      echo "   verified: $new_id serves the carried bytes"
    else
      echo "!! tile($x,$y): verification GET of $new_id did not match; inspect by hand"
      failed=$((failed + 1)); continue
    fi
    migrated=$((migrated + 1))
  done
done

echo "---"
echo "migrate-tiles-ops: ${migrated} carried${DRY_RUN:+ (dry run)}, ${skipped} skipped, ${failed} failed"
[ "$failed" -eq 0 ] || exit 1
