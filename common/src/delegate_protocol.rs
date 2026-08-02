//! Wire protocol between the web UI and the identity delegate: CBOR-encoded
//! requests and responses carried in `ApplicationMessage` payloads.
//!
//! Contract parameters travel as opaque CBOR bytes (`Vec<u8>`) so the UI can
//! forward its build-time-injected parameter blobs without re-encoding them;
//! the delegate decodes and binds them into the canonical signature preimages.
//! Deltas come back the same way: ready-to-send contract update bytes.

use serde::{Deserialize, Serialize};

use crate::registry::AdmissionProof;

/// Requests the UI sends to the identity delegate.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum IdentityRequest {
    /// Return the identity's verifying key, generating and persisting a new
    /// keypair first if none exists for this origin.
    GetIdentity,
    /// Return the registry admission challenge bytes for this identity plus
    /// the proof-of-work difficulty, so the UI can run the grind.
    /// `registry_params` is CBOR of [`crate::registry::RegistryParameters`].
    AdmissionChallenge { registry_params: Vec<u8> },
    /// Sign an admission record over the given proof and return a
    /// [`crate::registry::RegistryDelta`] ready to send to the registry.
    /// An optional nickname is signed at version 1.
    SignAdmission {
        registry_params: Vec<u8>,
        proof: AdmissionProof,
        admitted_ts: u64,
        nickname: Option<String>,
    },
    /// Sign a pixel placement and return a [`crate::tile::TileDelta`] ready to
    /// send to that tile. `tile_params` is CBOR of
    /// [`crate::tile::TileParameters`].
    SignPlacement {
        tile_params: Vec<u8>,
        coord: u16,
        color: u8,
        ts: u64,
    },
    /// Sign a chat message and return a [`crate::chat::ChatDelta`] ready to
    /// send to the chat contract. `chat_params` is CBOR of
    /// [`crate::chat::ChatParameters`].
    SignChatMessage {
        chat_params: Vec<u8>,
        content: String,
        ts: u64,
        seq: u32,
    },
    /// Sign a nickname change at an explicit version and return a
    /// [`crate::registry::RegistryDelta`] carrying only the nickname update.
    SignNickname {
        registry_params: Vec<u8>,
        name: String,
        version: u64,
    },
}

/// Responses the identity delegate sends back to the UI.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum IdentityResponse {
    /// The identity's ed25519 verifying key bytes.
    Identity { verifying_key: [u8; 32] },
    /// Admission challenge bytes and the difficulty target for the PoW grind.
    Challenge {
        bytes: Vec<u8>,
        difficulty_bits: u32,
    },
    /// A signed registry update (admission or nickname), as CBOR
    /// [`crate::registry::RegistryDelta`] bytes.
    RegistryUpdate {
        verifying_key: [u8; 32],
        delta: Vec<u8>,
    },
    /// A signed placement, as CBOR [`crate::tile::TileDelta`] bytes.
    TileUpdate {
        verifying_key: [u8; 32],
        delta: Vec<u8>,
    },
    /// A signed chat message, as CBOR [`crate::chat::ChatDelta`] bytes.
    ChatUpdate {
        verifying_key: [u8; 32],
        delta: Vec<u8>,
    },
    /// The request could not be served (malformed parameters, bad proof, ...).
    Error { message: String },
}
