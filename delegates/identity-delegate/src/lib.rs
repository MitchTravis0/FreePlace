//! Identity delegate: generates and stores the user's ed25519 signing key in
//! the node's encrypted secret storage, and signs placements, chat messages,
//! nickname updates, and admission records on request. The private key never
//! leaves the delegate; the UI only ever sees the verifying key and
//! ready-to-send contract deltas (see `common::delegate_protocol`).

mod handler;

use common::delegate_protocol::IdentityResponse;
use ed25519_dalek::SigningKey;
use freenet_stdlib::prelude::*;

pub struct Delegate;

/// Secret-store key for a web-container origin's identity signing key.
fn webapp_secret_key(id: &[u8]) -> Vec<u8> {
    let mut key = b"freeplace:identity:v1:webapp:".to_vec();
    key.extend_from_slice(id);
    key
}

/// Secret-store key for the identity signing key, namespaced by the attested
/// message origin so different apps sharing this delegate get isolated
/// identities. The layout is part of the delegate's storage format: changing
/// it strands existing keys just like a WASM re-key would.
fn secret_key_for(origin: Option<&MessageOrigin>) -> Vec<u8> {
    match origin {
        Some(MessageOrigin::WebApp(id)) => webapp_secret_key(id.as_bytes()),
        Some(MessageOrigin::Delegate(dk)) => {
            let mut key = b"freeplace:identity:v1:delegate:".to_vec();
            key.extend_from_slice(dk.bytes());
            key
        }
        None => b"freeplace:identity:v1:unattested".to_vec(),
        // MessageOrigin is #[non_exhaustive]; future variants get a stable
        // bucket rather than a compile break.
        Some(_) => b"freeplace:identity:v1:unknown-origin".to_vec(),
    }
}

/// Storage seam over the ctx secret API so the adoption logic is natively
/// testable (the host functions are WASM-only).
pub(crate) trait SecretStore {
    fn get(&mut self, key: &[u8]) -> Option<Vec<u8>>;
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool;
}

impl SecretStore for DelegateCtx {
    fn get(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        self.get_secret(key)
    }
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool {
        self.set_secret(key, value)
    }
}

/// A stored value is an identity only if it is exactly a 32-byte seed; blanked
/// slots (zero-length, written on adoption) and corrupt values read as absent.
fn stored_seed(store: &mut impl SecretStore, key: &[u8]) -> Option<[u8; 32]> {
    store.get(key)?.try_into().ok()
}

fn verifying_key_of(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

/// Move the identity left under a previous release's web-container origin
/// into `current` (the attested origin's slot). Only acts when `current` is
/// empty; blanks the source on success so it cannot be adopted twice
/// (first-caller-wins window, accepted in plan.md's risks).
pub(crate) fn adopt_legacy_origin(
    store: &mut impl SecretStore,
    current: &[u8],
    old_webapp_id: &[u8; 32],
) -> IdentityResponse {
    if let Some(existing) = stored_seed(store, current) {
        return IdentityResponse::AdoptResult {
            adopted: false,
            verifying_key: Some(verifying_key_of(&existing)),
        };
    }
    let old_key = webapp_secret_key(old_webapp_id);
    let Some(seed) = stored_seed(store, &old_key) else {
        return IdentityResponse::AdoptResult {
            adopted: false,
            verifying_key: None,
        };
    };
    if !store.set(current, &seed) {
        return IdentityResponse::Error {
            message: "failed to persist the adopted identity".to_string(),
        };
    }
    store.set(&old_key, &[]);
    IdentityResponse::AdoptResult {
        adopted: true,
        verifying_key: Some(verifying_key_of(&seed)),
    }
}

/// Load the origin's signing key, generating and persisting one from host
/// entropy on first use.
fn load_or_create_key(
    ctx: &mut DelegateCtx,
    origin: Option<&MessageOrigin>,
) -> Result<SigningKey, String> {
    let store_key = secret_key_for(origin);
    if let Some(stored) = ctx.get_secret(&store_key) {
        let bytes: [u8; 32] = stored
            .try_into()
            .map_err(|_| "stored identity key has the wrong length".to_string())?;
        return Ok(SigningKey::from_bytes(&bytes));
    }
    let entropy: [u8; 32] = freenet_stdlib::rand::rand_bytes(32)
        .try_into()
        .map_err(|_| "host returned the wrong number of random bytes".to_string())?;
    // The native (non-WASM) stub returns zeroes; an all-zero seed here means
    // the host entropy source failed, and a predictable key must never ship.
    if entropy == [0u8; 32] {
        return Err("host entropy source returned zeroes".to_string());
    }
    let key = SigningKey::from_bytes(&entropy);
    if !ctx.set_secret(&store_key, &entropy) {
        return Err("failed to persist the identity key".to_string());
    }
    Ok(key)
}

#[delegate]
impl DelegateInterface for Delegate {
    fn process(
        ctx: &mut DelegateCtx,
        _parameters: Parameters<'static>,
        origin: Option<MessageOrigin>,
        message: InboundDelegateMsg,
    ) -> Result<Vec<OutboundDelegateMsg>, DelegateError> {
        match message {
            InboundDelegateMsg::ApplicationMessage(app) => {
                let response = match common::from_cbor(&app.payload) {
                    Err(e) => IdentityResponse::Error {
                        message: format!("malformed request: {e}"),
                    },
                    // Adoption must run before load_or_create_key: probing for
                    // a legacy identity must never mint a fresh one.
                    Ok(common::delegate_protocol::IdentityRequest::AdoptLegacyOrigin {
                        old_webapp_id,
                    }) => {
                        let current = secret_key_for(origin.as_ref());
                        adopt_legacy_origin(ctx, &current, &old_webapp_id)
                    }
                    Ok(request) => match load_or_create_key(ctx, origin.as_ref()) {
                        Ok(key) => handler::handle_request(&key, request),
                        Err(message) => IdentityResponse::Error { message },
                    },
                };
                Ok(vec![OutboundDelegateMsg::ApplicationMessage(
                    ApplicationMessage::new(common::to_cbor(&response)).processed(true),
                )])
            }
            // InboundDelegateMsg is #[non_exhaustive]; everything else is
            // ignored (this delegate is purely request/response).
            _ => Ok(vec![]),
        }
    }
}

#[cfg(test)]
mod tests;
