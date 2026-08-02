// Single reference site for the build-injected identity-delegate WASM:
// `define` substitutes the base64 textually at every usage, so referencing
// it from one shared module keeps the ~800 KB string out of the per-page
// entry chunks (both index.html and phase5.html need it).
export const IDENTITY_DELEGATE_WASM_B64 = __IDENTITY_DELEGATE_WASM_B64__;
