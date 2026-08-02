//! Request handling as a pure function of the signing key, so it is testable
//! natively (the `DelegateCtx` host functions are WASM-only stubs off-target).

use common::delegate_protocol::{IdentityRequest, IdentityResponse};
use common::registry::{
    admission_challenge_bytes, pow_nonce_meets_target, AdmissionProof, AdmissionRecord,
    NicknameUpdate, RegistryDelta, RegistryParameters, SignedNickname,
};
use common::tile::{SignedPlacement, TileDelta, TileParameters};
use common::{chat, constants};
use ed25519_dalek::SigningKey;

pub fn handle_request(key: &SigningKey, request: IdentityRequest) -> IdentityResponse {
    match try_handle(key, request) {
        Ok(response) => response,
        Err(message) => IdentityResponse::Error { message },
    }
}

fn try_handle(key: &SigningKey, request: IdentityRequest) -> Result<IdentityResponse, String> {
    let verifying_key = key.verifying_key().to_bytes();
    match request {
        IdentityRequest::GetIdentity => Ok(IdentityResponse::Identity { verifying_key }),
        IdentityRequest::AdmissionChallenge { registry_params } => {
            let params: RegistryParameters = common::from_cbor(&registry_params)?;
            Ok(IdentityResponse::Challenge {
                bytes: admission_challenge_bytes(&params, &key.verifying_key()),
                difficulty_bits: constants::POW_DIFFICULTY_BITS,
            })
        }
        IdentityRequest::SignAdmission {
            registry_params,
            proof,
            admitted_ts,
            nickname,
        } => {
            let params: RegistryParameters = common::from_cbor(&registry_params)?;
            // Cheap pre-check so a bad grind result fails here with a clear
            // message instead of as a contract-side rejection.
            if let AdmissionProof::Work { nonce } = &proof {
                if !pow_nonce_meets_target(&params, &key.verifying_key(), *nonce) {
                    return Err("nonce does not meet the difficulty target".to_string());
                }
            }
            let nickname = nickname.map(|name| SignedNickname::sign(key, &params, &name, 1));
            let record = AdmissionRecord::sign(key, &params, proof, nickname, admitted_ts);
            let delta = RegistryDelta {
                admissions: vec![record],
                nicknames: vec![],
            };
            Ok(IdentityResponse::RegistryUpdate {
                verifying_key,
                delta: common::to_cbor(&delta),
            })
        }
        IdentityRequest::SignPlacement {
            tile_params,
            coord,
            color,
            ts,
        } => {
            let params: TileParameters = common::from_cbor(&tile_params)?;
            if color >= constants::PALETTE_COLORS {
                return Err(format!("color {color} out of palette range"));
            }
            let placement = SignedPlacement::sign(key, &params, coord, color, ts);
            let delta = TileDelta {
                placements: vec![placement],
            };
            Ok(IdentityResponse::TileUpdate {
                verifying_key,
                delta: common::to_cbor(&delta),
            })
        }
        IdentityRequest::SignChatMessage {
            chat_params,
            content,
            ts,
            seq,
        } => {
            let params: chat::ChatParameters = common::from_cbor(&chat_params)?;
            if content.is_empty() || content.len() > constants::MAX_CHAT_MESSAGE_BYTES {
                return Err(format!(
                    "message content must be 1..={} bytes, got {}",
                    constants::MAX_CHAT_MESSAGE_BYTES,
                    content.len()
                ));
            }
            let message = chat::SignedMessage::sign(key, &params, &content, ts, seq);
            let delta = chat::ChatDelta {
                messages: vec![message],
            };
            Ok(IdentityResponse::ChatUpdate {
                verifying_key,
                delta: common::to_cbor(&delta),
            })
        }
        IdentityRequest::SignNickname {
            registry_params,
            name,
            version,
        } => {
            let params: RegistryParameters = common::from_cbor(&registry_params)?;
            let nickname = SignedNickname::sign(key, &params, &name, version);
            nickname.verify(&params, &key.verifying_key())?;
            let delta = RegistryDelta {
                admissions: vec![],
                nicknames: vec![NicknameUpdate {
                    identity_vk: key.verifying_key(),
                    nickname,
                }],
            };
            Ok(IdentityResponse::RegistryUpdate {
                verifying_key,
                delta: common::to_cbor(&delta),
            })
        }
    }
}
