/// <reference types="vite/client" />

// Build-time constants injected by vite.config.ts `define` from files written
// by the publish tooling (see scripts/phase5-smoke.sh). Key bytes and code
// hash bytes are DIFFERENT values: key = blake3(code_hash || params),
// code_hash = blake3(wasm bytes).
declare const __IDENTITY_DELEGATE_KEY_BYTES__: number[];
// Raw identity-delegate WASM (base64, "" when not built yet); registered on
// the user's node at startup because delegates never propagate on their own.
declare const __IDENTITY_DELEGATE_WASM_B64__: string;
declare const __IDENTITY_DELEGATE_CODE_HASH_BYTES__: number[];
declare const __GHOSTKEYS_DELEGATE_KEY_BYTES__: number[];
declare const __GHOSTKEYS_DELEGATE_CODE_HASH_BYTES__: number[];
declare const __REGISTRY_CONTRACT_ID__: string;
declare const __TILE_CONTRACT_ID__: string;
declare const __REGISTRY_PARAMS_BYTES__: number[];
declare const __TILE_PARAMS_BYTES__: number[];
declare const __TILES_JSON__: { x: number; y: number; id: string; params: number[] }[];
declare const __CHAT_CONTRACT_ID__: string;
declare const __CHAT_PARAMS_BYTES__: number[];
// Instance ids of previous releases (newest first), per contract, from
// published/legacy/. The backward migration probe carries state forward from
// these when the current instance is still empty.
declare const __LEGACY_IDS_JSON__: {
  registry: string[];
  chat: string[];
  tiles: { x: number; y: number; ids: string[] }[];
};
