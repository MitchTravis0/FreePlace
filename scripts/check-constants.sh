#!/usr/bin/env bash
# Guard against drift between common/src/constants.rs (the source of truth;
# these values live in contract WASM) and the hand-mirrored block in
# web/src/state.ts. Both sides must derive the same canvases and filters.
set -euo pipefail
cd "$(dirname "$0")/.."

RUST=common/src/constants.rs
TS=web/src/state.ts

SHARED="TILE_SIZE TILES_PER_SIDE PALETTE_COLORS EMPTY_PIXEL \
MAX_PLACEMENTS_PER_AUTHOR MAX_BAKED_PER_AUTHOR \
POW_TILE_COOLDOWN_SECS GHOSTKEY_TILE_COOLDOWN_SECS \
CHAT_MESSAGE_CAP CHAT_MAX_MESSAGES_PER_AUTHOR CHAT_MIN_MESSAGE_INTERVAL_SECS \
MAX_CHAT_MESSAGE_BYTES MAX_NICKNAME_BYTES"

fail=0
for name in $SHARED; do
  rust_val=$(sed -n "s/^pub const $name: [a-z0-9]* = \(.*\);.*/\1/p" "$RUST")
  ts_val=$(sed -n "s/^export const $name = \(.*\);.*/\1/p" "$TS")
  if [ -z "$rust_val" ] || [ -z "$ts_val" ]; then
    echo "check-constants: FAIL $name missing (rust='$rust_val' ts='$ts_val')"
    fail=1
    continue
  fi
  if [ "$((rust_val))" -ne "$((ts_val))" ]; then
    echo "check-constants: FAIL $name differs (rust=$rust_val ts=$ts_val)"
    fail=1
  fi
done

[ "$fail" -eq 0 ] || exit 1
echo "check-constants: OK ($(echo "$SHARED" | wc -w) shared constants match)"
