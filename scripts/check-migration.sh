#!/usr/bin/env bash
# Preflight migration guard (plan.md Phase 7): a contract/delegate WASM whose
# hash changed since the last release MUST have the outgoing hash registered in
# its legacy_*.toml, or migration would strand state under the old key. The
# facade WASM must never change at all (it is the stable URL). Compares the
# freshly built WASMs against the hashes recorded in the last release manifest
# ($PUBLISHED_DIR/release.env); before the first release there is nothing to
# guard.
set -euo pipefail
cd "$(dirname "$0")/.."

PUBLISHED_DIR="${PUBLISHED_DIR:-published}"
RELEASE_ENV="$PUBLISHED_DIR/release.env"

if [ ! -f "$RELEASE_ENV" ]; then
  echo "check-migration: OK (no $RELEASE_ENV yet; nothing has shipped)"
  exit 0
fi
# shellcheck source=/dev/null
source "$RELEASE_ENV"

tool() { cargo run -q -p common --example release_tool -- "$@"; }

fail=0

check() { # <name> <wasm> <recorded_hash> <registry_toml> <component>
  local name=$1 wasm=$2 recorded=$3 registry=$4 component=$5
  if [ ! -f "$wasm" ]; then
    echo "check-migration: FAIL $name WASM missing ($wasm); build first"
    fail=1
    return
  fi
  local head
  head=$(tool hash "$wasm")
  if [ "$head" = "$recorded" ]; then
    echo "check-migration: $name unchanged"
  elif tool guard "$component" "$registry" "$recorded" "$head"; then
    echo "check-migration: $name changed, outgoing hash is registered"
  else
    echo "check-migration: FAIL $name changed without a $registry entry for $recorded"
    fail=1
  fi
}

check registry-contract \
  contracts/registry-contract/target/wasm32-unknown-unknown/release/freeplace_registry_contract.wasm \
  "$REGISTRY_HASH" contracts/registry-contract/legacy_contracts.toml contract
check tile-contract \
  contracts/tile-contract/target/wasm32-unknown-unknown/release/freeplace_tile_contract.wasm \
  "$TILE_HASH" contracts/tile-contract/legacy_contracts.toml contract
check chat-contract \
  contracts/chat-contract/target/wasm32-unknown-unknown/release/freeplace_chat_contract.wasm \
  "$CHAT_HASH" contracts/chat-contract/legacy_contracts.toml contract
check identity-delegate \
  delegates/identity-delegate/target/wasm32-unknown-unknown/release/freeplace_identity_delegate.wasm \
  "$DELEGATE_HASH" delegates/identity-delegate/legacy_delegates.toml delegate

# The facade is the stable URL: any byte change re-keys it and breaks every
# bookmark. There is no registry to append to; a change is always an error.
FACADE_WASM=contracts/facade-contract/target/wasm32-unknown-unknown/release/freeplace_facade_contract.wasm
if [ ! -f "$FACADE_WASM" ]; then
  echo "check-migration: FAIL facade WASM missing ($FACADE_WASM); build first"
  fail=1
elif [ "$(tool hash "$FACADE_WASM")" != "$FACADE_HASH" ]; then
  echo "check-migration: FAIL facade-contract WASM changed; the facade must never be rebuilt" \
       "(see facade-pattern.md — this rotates the stable URL)"
  fail=1
else
  echo "check-migration: facade-contract unchanged"
fi

[ "$fail" -eq 0 ] || exit 1
echo "check-migration: OK"
