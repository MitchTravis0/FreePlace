//! Ghost key admission proof verification: certificate chain back to the
//! Freenet master key, Ed25519 signature over the scoped payload, and the
//! challenge binding to this registry instance and identity.

use common::registry::{admission_challenge_bytes, RegistryParameters};
use ed25519_dalek::{Signature, VerifyingKey};
use ghostkey_lib::armorable::Armorable;
use ghostkey_lib::ghost_key_certificate::GhostkeyCertificateV1;
use ghostkey_lib::FREENET_MASTER_VERIFYING_KEY_BASE64;
use serde::Deserialize;

/// Mirror of ghostkey-common's `ScopedPayload`, decoding only what the
/// contract checks. `requestor` is runtime-attested by the signing delegate
/// and opaque here; the challenge binding is what prevents replay.
#[derive(Deserialize)]
struct ScopedPayload {
    #[allow(dead_code)]
    requestor: ciborium::value::Value,
    payload: Vec<u8>,
}

pub(crate) fn verify_with_master(
    params: &RegistryParameters,
    identity_vk: &VerifyingKey,
    scoped_payload: &[u8],
    signature: &[u8],
    certificate_pem: &str,
    master: &VerifyingKey,
) -> Result<(), String> {
    let certificate = GhostkeyCertificateV1::from_armored_string(certificate_pem)
        .map_err(|e| format!("bad ghost key certificate: {e}"))?;
    certificate
        .verify(&Some(*master))
        .map_err(|e| format!("ghost key chain verification failed: {e}"))?;
    let signature = Signature::from_slice(signature)
        .map_err(|_| "malformed ghost key signature".to_string())?;
    certificate
        .verifying_key
        .verify_strict(scoped_payload, &signature)
        .map_err(|_| "ghost key signature does not verify".to_string())?;
    let scoped: ScopedPayload =
        ciborium::from_reader(scoped_payload).map_err(|e| format!("bad scoped payload: {e}"))?;
    if scoped.payload != admission_challenge_bytes(params, identity_vk) {
        return Err("scoped payload does not match the admission challenge".to_string());
    }
    Ok(())
}

/// Production check: chain must reach the real Freenet master key.
pub(crate) fn freenet_ghostkey_check(
    params: &RegistryParameters,
    identity_vk: &VerifyingKey,
    scoped_payload: &[u8],
    signature: &[u8],
    certificate_pem: &str,
) -> Result<(), String> {
    let master = VerifyingKey::from_base64(FREENET_MASTER_VERIFYING_KEY_BASE64)
        .map_err(|e| format!("cannot decode Freenet master key: {e}"))?;
    verify_with_master(
        params,
        identity_vk,
        scoped_payload,
        signature,
        certificate_pem,
        &master,
    )
}
