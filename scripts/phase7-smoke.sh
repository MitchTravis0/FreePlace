#!/usr/bin/env bash
# Phase 7 exit check (plan.md): full publish to the local dev node via the
# release pipeline; the stable facade URL reaches the app; a deliberate tile
# contract code change re-keys the tiles and the UI's migration probe carries
# seeded state to the new key; the preflight fails while the outgoing hash is
# unregistered and passes once it is. Uses a scratch PUBLISHED_DIR and a dev
# signing key so the real published/ manifest is untouched, and restores the
# tile source + registry TOML on exit.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${WS_API_PORT:-7510}"
OUT=.local/phase7
export PUBLISHED_DIR="$OUT/published"
export FREEPLACE_KEY_FILE="$PWD/$OUT/facade-owner.key"

TILE_SRC=contracts/tile-contract/src/lib.rs
TILE_TOML=contracts/tile-contract/legacy_contracts.toml
TILE_DIR=contracts/tile-contract
TILE_WASM=$TILE_DIR/target/wasm32-unknown-unknown/release/freeplace_tile_contract.wasm

cleanup() {
  [ -f "$OUT/lib.rs.bak" ] && cp "$OUT/lib.rs.bak" "$TILE_SRC"
  [ -f "$OUT/legacy_contracts.toml.bak" ] && cp "$OUT/legacy_contracts.toml.bak" "$TILE_TOML"
  return 0
}
trap cleanup EXIT

curl -sf -o /dev/null "http://127.0.0.1:$PORT/" \
  || { echo "dev node not reachable on $PORT; run 'make dev-node' first"; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT"
cp "$TILE_SRC" "$OUT/lib.rs.bak"
cp "$TILE_TOML" "$OUT/legacy_contracts.toml.bak"

tool() { cargo run -q -p common --example release_tool -- "$@"; }

echo "=== release A: full publish from a clean slate"
./scripts/release.sh
source "$PUBLISHED_DIR/release.env"
FACADE_A=$FACADE_ID WEBAPP_A=$WEBAPP_ID TILE00_A=$TILE_ID_0_0 REGISTRY_A=$REGISTRY_ID TILE_HASH_A=$TILE_HASH

echo "=== facade loader points at the release A container"
curl -s "http://127.0.0.1:$PORT/v1/contract/web/$FACADE_A/loader.js" | grep -q "$WEBAPP_A" \
  || { echo "FAIL: loader does not reference $WEBAPP_A"; exit 1; }

echo "=== browser check: stable facade URL reaches the app"
(cd web && FREEPLACE_FACADE_URL="http://127.0.0.1:$PORT/v1/contract/web/$FACADE_A/" \
  npx playwright test tests/phase7.spec.ts -g "facade")

echo "=== seeding state under the release A tile (0,0)"
tool gen-seed "$OUT" .local/release/registry-params.bin .local/release/tile-params-0-0.bin
fdev -p "$PORT" execute update "$REGISTRY_A" "$OUT/seed-admit.bin"
fdev -p "$PORT" execute update "$TILE00_A" "$OUT/seed-place.bin"
fdev -p "$PORT" execute get "$TILE00_A" --output "$OUT/tile-a-state.bin"
tool assert-tile-has "$OUT/tile-a-state.bin" 1234 7

echo "=== preflight passes while nothing changed"
./scripts/check-migration.sh

echo "=== deliberate tile contract code change (re-keys all 16 tiles)"
# An exported symbol, so LTO cannot strip it and the WASM bytes really change.
printf '\n/// Phase 7 smoke: deliberate re-key marker.\n#[no_mangle]\npub extern "C" fn phase7_rekey_marker() -> u8 {\n    1\n}\n' >> "$TILE_SRC"
(cd "$TILE_DIR" && CARGO_TARGET_DIR="$PWD/target" \
  cargo build --release --locked --target wasm32-unknown-unknown)
TILE_HASH_B=$(tool hash "$TILE_WASM")
[ "$TILE_HASH_B" != "$TILE_HASH_A" ] || { echo "FAIL: tile WASM did not change"; exit 1; }

echo "=== preflight FAILS while the outgoing hash is unregistered"
if ./scripts/check-migration.sh; then
  echo "FAIL: check-migration passed despite an unregistered tile WASM change"
  exit 1
fi
echo "(expected failure observed)"

echo "=== registering the outgoing hash makes the preflight pass"
# The version must be the next unused generation: the real registry grows an
# entry per shipped re-key, and duplicates fail the freenet-migrate codegen.
NEXT_GEN=$(($(grep -c '^\[\[entry\]\]' "$TILE_TOML") + 1))
cat >> "$TILE_TOML" <<EOF

[[entry]]
version = "V$NEXT_GEN"
description = "phase 7 smoke: deliberate re-key exercise"
date = "$(date +%F)"
code_hash = "$TILE_HASH_A"
EOF
./scripts/check-migration.sh

echo "=== release B: re-keyed tiles published, web rebuilt with legacy ids"
./scripts/release.sh
source "$PUBLISHED_DIR/release.env"
TILE00_B=$TILE_ID_0_0
[ "$TILE00_B" != "$TILE00_A" ] || { echo "FAIL: tile (0,0) id did not rotate"; exit 1; }
[ "$FACADE_ID" = "$FACADE_A" ] || { echo "FAIL: facade id rotated"; exit 1; }
grep -q "$TILE00_A" web/.gen/legacy_ids.json \
  || { echo "FAIL: legacy_ids.json is missing the old tile id"; exit 1; }

echo "=== facade pointer flipped to the release B container"
[ "$WEBAPP_ID" != "$WEBAPP_A" ] || { echo "FAIL: webapp id did not rotate"; exit 1; }
curl -s "http://127.0.0.1:$PORT/v1/contract/web/$FACADE_ID/loader.js" | grep -q "$WEBAPP_ID" \
  || { echo "FAIL: loader does not reference the new container $WEBAPP_ID"; exit 1; }

echo "=== browser check: the migration probe carries the seeded pixel forward"
# Seed coord 1234 in tile (0,0) = board (1234 % 256, 1234 / 256) = (210, 4).
(cd web && FREEPLACE_APP_URL="http://127.0.0.1:$PORT/v1/contract/web/$WEBAPP_ID/" \
  FREEPLACE_EXPECT_PIXEL="210,4,7" \
  npx playwright test tests/phase7.spec.ts -g "migration")

echo "=== node-side: the new tile instance holds the migrated placement"
ok=0
for _ in 1 2 3 4 5 6; do
  fdev -p "$PORT" execute get "$TILE00_B" --output "$OUT/tile-b-state.bin" || true
  if tool assert-tile-has "$OUT/tile-b-state.bin" 1234 7; then ok=1; break; fi
  sleep 5
done
[ "$ok" = 1 ] || { echo "FAIL: migrated placement not found under $TILE00_B"; exit 1; }

echo "PHASE 7 SMOKE PASSED"
