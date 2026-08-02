//! Facade / web-container pointer core (Phase 7). One contract serves both
//! roles: the never-rebuilt facade instance (`instance == 0`) whose web slot is
//! the loader pointing at the current web container, and per-release container
//! instances (`instance == release version`) whose web slot is the app archive.
//!
//! State is the gateway webapp framing `[u64 BE meta_len][meta][u64 BE
//! web_len][web]` (what `fdev publish --webapp-archive --webapp-metadata`
//! produces); `meta` is CBOR [`FacadeMetadata`], whose signature covers the
//! parameters, the pointer fields, and the blake3 of the web slot, so every
//! byte of state is signature-covered. Convergence is last-writer-wins on the
//! deterministic key `(version, signature bytes)`.

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::constants::FACADE_MAX_PREV_APPS;

/// Domain-separation prefix for pointer signature preimages.
const FACADE_SIGNING_CONTEXT: &[u8] = b"freeplace:facade:pointer:v1";

/// Instance parameters. `instance` 0 is the stable facade; container instances
/// use the release version so each release gets a fresh contract id.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FacadeParameters {
    pub owner_vk: VerifyingKey,
    pub instance: u64,
}

/// The signed pointer. For the facade instance `current_app` is the container
/// to serve now and `prev_apps` a rollback ring; container instances carry
/// `None` and an empty ring.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FacadePointer {
    /// Strictly monotonic across updates (unix timestamp at signing time).
    pub version: u64,
    /// ContractInstanceId bytes of the current web container, if pointing.
    pub current_app: Option<[u8; 32]>,
    /// Most recent previous containers, newest first, for rollback.
    pub prev_apps: Vec<[u8; 32]>,
    /// blake3 of the web slot bytes (the tar.xz archive).
    pub web_hash: [u8; 32],
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FacadeMetadata {
    pub pointer: FacadePointer,
    /// By `params.owner_vk` over [`facade_signing_bytes`].
    pub signature: Signature,
}

/// Canonical signature preimage: context, the full parameters (owner key and
/// instance, cross-context binding), then every pointer field with the
/// variable-length ring length-prefixed.
pub fn facade_signing_bytes(params: &FacadeParameters, pointer: &FacadePointer) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(FACADE_SIGNING_CONTEXT);
    out.extend_from_slice(params.owner_vk.as_bytes());
    out.extend_from_slice(&params.instance.to_le_bytes());
    out.extend_from_slice(&pointer.version.to_le_bytes());
    match &pointer.current_app {
        None => out.push(0),
        Some(app) => {
            out.push(1);
            out.extend_from_slice(app);
        }
    }
    let count: u32 = pointer.prev_apps.len().try_into().expect("ring < 4 Gi");
    out.extend_from_slice(&count.to_le_bytes());
    for app in &pointer.prev_apps {
        out.extend_from_slice(app);
    }
    out.extend_from_slice(&pointer.web_hash);
    out
}

impl FacadeMetadata {
    pub fn sign(key: &SigningKey, params: &FacadeParameters, pointer: FacadePointer) -> Self {
        use ed25519_dalek::Signer;
        let bytes = facade_signing_bytes(params, &pointer);
        FacadeMetadata {
            pointer,
            signature: key.sign(&bytes),
        }
    }

    /// Verify against the parameters and the web slot the state carries.
    pub fn verify(&self, params: &FacadeParameters, web: &[u8]) -> Result<(), String> {
        if self.pointer.prev_apps.len() > FACADE_MAX_PREV_APPS {
            return Err(format!(
                "prev_apps exceeds the cap of {FACADE_MAX_PREV_APPS}"
            ));
        }
        if self.pointer.web_hash != *blake3::hash(web).as_bytes() {
            return Err("web slot does not match the signed web_hash".to_string());
        }
        let bytes = facade_signing_bytes(params, &self.pointer);
        params
            .owner_vk
            .verify_strict(&bytes, &self.signature)
            .map_err(|_| "invalid facade pointer signature".to_string())
    }

    /// Deterministic total order; the larger key wins a merge.
    pub fn order_key(&self) -> (u64, [u8; 64]) {
        (self.pointer.version, self.signature.to_bytes())
    }
}

/// Encode the gateway webapp state framing.
pub fn encode_facade_frame(meta: &[u8], web: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + meta.len() + web.len());
    out.extend_from_slice(&(meta.len() as u64).to_be_bytes());
    out.extend_from_slice(meta);
    out.extend_from_slice(&(web.len() as u64).to_be_bytes());
    out.extend_from_slice(web);
    out
}

/// Split the framing into `(meta, web)`. Rejects trailing or missing bytes.
pub fn parse_facade_frame(bytes: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let take_len = |at: usize| -> Result<usize, String> {
        let raw: [u8; 8] = bytes
            .get(at..at + 8)
            .ok_or("state too short for length header")?
            .try_into()
            .expect("slice is 8 bytes");
        usize::try_from(u64::from_be_bytes(raw)).map_err(|_| "length overflow".to_string())
    };
    let range = |start: usize, len: usize| -> Result<&[u8], String> {
        let end = start.checked_add(len).ok_or("length overflow")?;
        bytes
            .get(start..end)
            .ok_or_else(|| "state too short".to_string())
    };
    let meta_len = take_len(0)?;
    let meta = range(8, meta_len)?;
    let web_len = take_len(8 + meta_len)?;
    let web_start = 16 + meta_len;
    let web = range(web_start, web_len)?;
    if bytes.len() != web_start + web_len {
        return Err("trailing bytes after web slot".to_string());
    }
    Ok((meta, web))
}

/// Decode and fully verify a framed facade state.
pub fn decode_facade_state(
    params: &FacadeParameters,
    bytes: &[u8],
) -> Result<FacadeMetadata, String> {
    let (meta, web) = parse_facade_frame(bytes)?;
    let metadata: FacadeMetadata = crate::from_cbor(meta)?;
    metadata.verify(params, web)?;
    Ok(metadata)
}
