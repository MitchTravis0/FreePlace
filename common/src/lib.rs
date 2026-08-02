//! Shared types for FreePlace: state structs, canonical signing-byte builders,
//! the cooldown filter, and LWW canvas derivation.

pub mod chat;
pub mod constants;
pub mod delegate_protocol;
pub mod facade;
pub mod identity;
pub mod registry;
pub mod tile;

/// Lineage of previous identity-delegate WASM builds, generated at build time
/// from `delegates/identity-delegate/legacy_delegates.toml` by
/// `freenet-migrate-build` (which cross-checks the recorded delegate keys
/// against the `blake3(code_hash || params)` derivation). The migration probe
/// walks these `(delegate_key, code_hash)` pairs to reach secrets stored under
/// old delegate keys.
pub mod legacy {
    include!(concat!(env!("OUT_DIR"), "/legacy_identity_delegates.rs"));
    include!(concat!(env!("OUT_DIR"), "/legacy_registry_contracts.rs"));
    include!(concat!(env!("OUT_DIR"), "/legacy_tile_contracts.rs"));
    include!(concat!(env!("OUT_DIR"), "/legacy_chat_contracts.rs"));
}

#[cfg(test)]
mod chat_tests;
#[cfg(test)]
mod delegate_protocol_tests;
#[cfg(test)]
mod facade_tests;
#[cfg(test)]
mod registry_tests;
#[cfg(test)]
mod tests;

use serde::{de::DeserializeOwned, Serialize};

/// CBOR-encode a value. State/summary/delta bytes on the wire are CBOR;
/// only signature preimages use the manual canonical layout.
pub fn to_cbor<T: Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).expect("CBOR serialization cannot fail");
    bytes
}

/// Decode a CBOR-encoded value.
pub fn from_cbor<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    ciborium::from_reader(bytes).map_err(|e| format!("CBOR decode failed: {e}"))
}
