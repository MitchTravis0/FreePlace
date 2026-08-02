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

/// Secret-store key for the identity signing key, namespaced by the attested
/// message origin so different apps sharing this delegate get isolated
/// identities. The layout is part of the delegate's storage format: changing
/// it strands existing keys just like a WASM re-key would.
fn secret_key_for(origin: Option<&MessageOrigin>) -> Vec<u8> {
    let mut key = b"freeplace:identity:v1:".to_vec();
    match origin {
        Some(MessageOrigin::WebApp(id)) => {
            key.extend_from_slice(b"webapp:");
            key.extend_from_slice(id.as_bytes());
        }
        Some(MessageOrigin::Delegate(dk)) => {
            key.extend_from_slice(b"delegate:");
            key.extend_from_slice(dk.bytes());
        }
        None => key.extend_from_slice(b"unattested"),
        // MessageOrigin is #[non_exhaustive]; future variants get a stable
        // bucket rather than a compile break.
        Some(_) => key.extend_from_slice(b"unknown-origin"),
    }
    key
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
