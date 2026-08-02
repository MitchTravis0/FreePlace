import { defineConfig } from "vite";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

// Build-time constants are read from web/.gen/, written by the publish
// tooling (scripts/phase5-smoke.sh for the dev harness; the Phase 7 Makefile
// pipeline for releases). Defaults keep `vite dev` working with nothing
// published yet.
function readOrDefault(filename: string, fallback: string): string {
  const filePath = resolve(__dirname, ".gen", filename);
  return existsSync(filePath) ? readFileSync(filePath, "utf-8").trim() : fallback;
}

// Binary asset embedded as base64. Inlined (not fetched at runtime): the
// gateway iframe has an opaque origin, so a runtime fetch of an asset URL is
// a cross-origin request the gateway may reject.
function readBase64OrEmpty(filename: string): string {
  const filePath = resolve(__dirname, ".gen", filename);
  return existsSync(filePath) ? readFileSync(filePath).toString("base64") : "";
}

// base "./" is required: the gateway serves the app from an iframe at a
// nested path, so absolute asset URLs break in production.
export default defineConfig({
  base: "./",
  define: {
    __IDENTITY_DELEGATE_KEY_BYTES__: readOrDefault("identity_delegate_key_bytes.json", "[]"),
    // Raw delegate WASM: the UI registers it on the user's node at startup
    // (delegates never propagate over the network, so a node that has not
    // run `fdev publish delegate` locally would otherwise never answer).
    __IDENTITY_DELEGATE_WASM_B64__: JSON.stringify(readBase64OrEmpty("identity_delegate.wasm")),
    __IDENTITY_DELEGATE_CODE_HASH_BYTES__: readOrDefault(
      "identity_delegate_code_hash_bytes.json",
      "[]",
    ),
    __GHOSTKEYS_DELEGATE_KEY_BYTES__: readOrDefault("ghostkeys_delegate_key_bytes.json", "[]"),
    __GHOSTKEYS_DELEGATE_CODE_HASH_BYTES__: readOrDefault(
      "ghostkeys_delegate_code_hash_bytes.json",
      "[]",
    ),
    __REGISTRY_CONTRACT_ID__: JSON.stringify(readOrDefault("registry_contract_id.txt", "")),
    __TILE_CONTRACT_ID__: JSON.stringify(readOrDefault("tile_contract_id.txt", "")),
    __REGISTRY_PARAMS_BYTES__: readOrDefault("registry_params_bytes.json", "[]"),
    __TILE_PARAMS_BYTES__: readOrDefault("tile_params_bytes.json", "[]"),
    // Phase 6: all 16 tiles ({x, y, id, params}[]) and the chat contract.
    __TILES_JSON__: readOrDefault("tiles.json", "[]"),
    __CHAT_CONTRACT_ID__: JSON.stringify(readOrDefault("chat_contract_id.txt", "")),
    __CHAT_PARAMS_BYTES__: readOrDefault("chat_params_bytes.json", "[]"),
    // Phase 7: previous-release instance ids for the migration probe.
    __LEGACY_IDS_JSON__: readOrDefault(
      "legacy_ids.json",
      '{"registry":[],"chat":[],"tiles":[],"webapps":[]}',
    ),
  },
  build: {
    rollupOptions: {
      // Two pages: the real app and the phase 5 delegate harness.
      input: {
        main: resolve(__dirname, "index.html"),
        phase5: resolve(__dirname, "phase5.html"),
      },
    },
  },
});
