//! Native tests for the request handler. The `DelegateCtx` secret/entropy
//! host functions are WASM-only, so the key is injected directly here; the
//! ctx wiring is exercised by the Phase 5 browser smoke against a real node.

use common::delegate_protocol::{IdentityRequest, IdentityResponse};
use common::registry::{
    find_pow_nonce, AdmissionProof, RegistryDelta, RegistryParameters, RegistryState,
};
use common::tile::{TileDelta, TileParameters, TileState};
use common::{chat, constants};
use ed25519_dalek::SigningKey;

use crate::handler::handle_request;

fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn registry_params() -> RegistryParameters {
    RegistryParameters {
        canvas_id: [3u8; 32],
    }
}

fn tile_params() -> TileParameters {
    TileParameters {
        canvas_id: [3u8; 32],
        tile_x: 1,
        tile_y: 2,
        registry: [9u8; 32],
    }
}

fn chat_params() -> chat::ChatParameters {
    chat::ChatParameters {
        canvas_id: [3u8; 32],
        registry: [9u8; 32],
    }
}

/// Ghost key hook for tests: no chain verification available natively.
fn reject_ghostkey(
    _: &RegistryParameters,
    _: &ed25519_dalek::VerifyingKey,
    _: &[u8],
    _: &[u8],
    _: &str,
) -> Result<(), String> {
    Err("ghost key verification unavailable in tests".to_string())
}

#[test]
fn get_identity_returns_verifying_key() {
    let key = test_key();
    let response = handle_request(&key, IdentityRequest::GetIdentity);
    assert_eq!(
        response,
        IdentityResponse::Identity {
            verifying_key: key.verifying_key().to_bytes()
        }
    );
}

#[test]
fn challenge_matches_canonical_bytes_and_difficulty() {
    let key = test_key();
    let params = registry_params();
    let response = handle_request(
        &key,
        IdentityRequest::AdmissionChallenge {
            registry_params: common::to_cbor(&params),
        },
    );
    let IdentityResponse::Challenge {
        bytes,
        difficulty_bits,
    } = response
    else {
        panic!("expected Challenge, got {response:?}");
    };
    assert_eq!(
        bytes,
        common::registry::admission_challenge_bytes(&params, &key.verifying_key())
    );
    assert_eq!(difficulty_bits, constants::POW_DIFFICULTY_BITS);
}

#[test]
fn signed_admission_delta_is_accepted_by_the_registry_core() {
    let key = test_key();
    let params = registry_params();
    let nonce = find_pow_nonce(&params, &key.verifying_key());
    let response = handle_request(
        &key,
        IdentityRequest::SignAdmission {
            registry_params: common::to_cbor(&params),
            proof: AdmissionProof::Work { nonce },
            admitted_ts: 1_700_000_000,
            nickname: Some("smoke".to_string()),
        },
    );
    let IdentityResponse::RegistryUpdate {
        verifying_key,
        delta,
    } = response
    else {
        panic!("expected RegistryUpdate, got {response:?}");
    };
    assert_eq!(verifying_key, key.verifying_key().to_bytes());
    let delta: RegistryDelta = common::from_cbor(&delta).unwrap();
    let mut state = RegistryState::default();
    state.apply_delta(&delta);
    state.verify(&params, &reject_ghostkey).unwrap();
    assert_eq!(state.identities.len(), 1);
    let record = state.identities.values().next().unwrap();
    assert_eq!(record.nickname.as_ref().unwrap().name, "smoke");
    assert_eq!(record.nickname.as_ref().unwrap().version, 1);
}

#[test]
fn admission_with_a_failing_nonce_is_refused() {
    let key = test_key();
    let params = registry_params();
    let nonce = find_pow_nonce(&params, &key.verifying_key());
    let response = handle_request(
        &key,
        IdentityRequest::SignAdmission {
            registry_params: common::to_cbor(&params),
            proof: AdmissionProof::Work { nonce: nonce + 1 },
            admitted_ts: 1_700_000_000,
            nickname: None,
        },
    );
    assert!(
        matches!(response, IdentityResponse::Error { .. }),
        "expected Error, got {response:?}"
    );
}

#[test]
fn signed_placement_verifies_against_its_tile_only() {
    let key = test_key();
    let params = tile_params();
    let response = handle_request(
        &key,
        IdentityRequest::SignPlacement {
            tile_params: common::to_cbor(&params),
            coord: 1234,
            color: 5,
            ts: 1_700_000_000,
        },
    );
    let IdentityResponse::TileUpdate { delta, .. } = response else {
        panic!("expected TileUpdate, got {response:?}");
    };
    let delta: TileDelta = common::from_cbor(&delta).unwrap();
    assert_eq!(delta.placements.len(), 1);
    let placement = delta.placements[0];
    placement.verify(&params).unwrap();
    // Cross-context binding: the same placement must not verify for another tile.
    let mut other = params;
    other.tile_x = 3;
    assert!(placement.verify(&other).is_err());
    // And it lands in tile state as sent.
    let mut state = TileState::default();
    state.apply_delta(&delta);
    assert_eq!(state.placements.len(), 1);
}

#[test]
fn out_of_palette_color_is_refused() {
    let key = test_key();
    let response = handle_request(
        &key,
        IdentityRequest::SignPlacement {
            tile_params: common::to_cbor(&tile_params()),
            coord: 0,
            color: constants::PALETTE_COLORS,
            ts: 1_700_000_000,
        },
    );
    assert!(matches!(response, IdentityResponse::Error { .. }));
}

#[test]
fn signed_chat_message_verifies() {
    let key = test_key();
    let params = chat_params();
    let response = handle_request(
        &key,
        IdentityRequest::SignChatMessage {
            chat_params: common::to_cbor(&params),
            content: "hello canvas".to_string(),
            ts: 1_700_000_000,
            seq: 0,
        },
    );
    let IdentityResponse::ChatUpdate { delta, .. } = response else {
        panic!("expected ChatUpdate, got {response:?}");
    };
    let delta: chat::ChatDelta = common::from_cbor(&delta).unwrap();
    assert_eq!(delta.messages.len(), 1);
    delta.messages[0].verify(&params).unwrap();
}

#[test]
fn signed_nickname_update_applies_over_an_admission() {
    let key = test_key();
    let params = registry_params();
    let nonce = find_pow_nonce(&params, &key.verifying_key());
    let admit = handle_request(
        &key,
        IdentityRequest::SignAdmission {
            registry_params: common::to_cbor(&params),
            proof: AdmissionProof::Work { nonce },
            admitted_ts: 1_700_000_000,
            nickname: None,
        },
    );
    let IdentityResponse::RegistryUpdate { delta, .. } = admit else {
        panic!("expected RegistryUpdate");
    };
    let mut state = RegistryState::default();
    state.apply_delta(&common::from_cbor(&delta).unwrap());

    let rename = handle_request(
        &key,
        IdentityRequest::SignNickname {
            registry_params: common::to_cbor(&params),
            name: "renamed".to_string(),
            version: 2,
        },
    );
    let IdentityResponse::RegistryUpdate { delta, .. } = rename else {
        panic!("expected RegistryUpdate, got {rename:?}");
    };
    let delta: RegistryDelta = common::from_cbor(&delta).unwrap();
    state.apply_delta(&delta);
    state.verify(&params, &reject_ghostkey).unwrap();
    let record = state.identities.values().next().unwrap();
    assert_eq!(record.nickname.as_ref().unwrap().name, "renamed");
    assert_eq!(record.nickname.as_ref().unwrap().version, 2);
}

#[test]
fn zero_version_nickname_is_refused() {
    let key = test_key();
    let response = handle_request(
        &key,
        IdentityRequest::SignNickname {
            registry_params: common::to_cbor(&registry_params()),
            name: "x".to_string(),
            version: 0,
        },
    );
    assert!(matches!(response, IdentityResponse::Error { .. }));
}

// --- AdoptLegacyOrigin: identity carry-forward across webapp re-keys -------

use std::collections::HashMap;

use crate::{adopt_legacy_origin, SecretStore};

#[derive(Default)]
struct MapStore(HashMap<Vec<u8>, Vec<u8>>);

impl SecretStore for MapStore {
    fn get(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.get(key).cloned()
    }
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool {
        self.0.insert(key.to_vec(), value.to_vec());
        true
    }
}

fn webapp_slot(id: u8) -> Vec<u8> {
    crate::webapp_secret_key(&[id; 32])
}

#[test]
fn adopts_a_legacy_identity_and_blanks_the_source() {
    let seed = [42u8; 32];
    let mut store = MapStore::default();
    store.set(&webapp_slot(1), &seed);

    let current = webapp_slot(2);
    let response = adopt_legacy_origin(&mut store, &current, &[1u8; 32]);
    let expected_vk = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    assert_eq!(
        response,
        IdentityResponse::AdoptResult {
            adopted: true,
            verifying_key: Some(expected_vk),
        }
    );
    // The identity now lives under the new origin; GetIdentity there loads
    // the same signing key.
    assert_eq!(store.get(&current), Some(seed.to_vec()));
    // The source slot is blanked, not deleted: any later read treats it as
    // absent, and a second adopter gets nothing.
    assert_eq!(store.get(&webapp_slot(1)), Some(vec![]));
    let response = adopt_legacy_origin(&mut store, &webapp_slot(3), &[1u8; 32]);
    assert_eq!(
        response,
        IdentityResponse::AdoptResult {
            adopted: false,
            verifying_key: None,
        }
    );
}

#[test]
fn adoption_is_a_no_op_when_the_origin_already_has_an_identity() {
    let mine = [5u8; 32];
    let theirs = [6u8; 32];
    let mut store = MapStore::default();
    let current = webapp_slot(2);
    store.set(&current, &mine);
    store.set(&webapp_slot(1), &theirs);

    let response = adopt_legacy_origin(&mut store, &current, &[1u8; 32]);
    assert_eq!(
        response,
        IdentityResponse::AdoptResult {
            adopted: false,
            verifying_key: Some(SigningKey::from_bytes(&mine).verifying_key().to_bytes()),
        }
    );
    // Neither slot changed.
    assert_eq!(store.get(&current), Some(mine.to_vec()));
    assert_eq!(store.get(&webapp_slot(1)), Some(theirs.to_vec()));
}

#[test]
fn probing_never_mints_an_identity() {
    let mut store = MapStore::default();
    let current = webapp_slot(2);
    let response = adopt_legacy_origin(&mut store, &current, &[1u8; 32]);
    assert_eq!(
        response,
        IdentityResponse::AdoptResult {
            adopted: false,
            verifying_key: None,
        }
    );
    assert!(store.0.is_empty(), "probe must not write anything");
}

#[test]
fn malformed_parameters_are_reported_not_panicked() {
    let key = test_key();
    let response = handle_request(
        &key,
        IdentityRequest::AdmissionChallenge {
            registry_params: vec![0xff, 0x00, 0x13],
        },
    );
    assert!(matches!(response, IdentityResponse::Error { .. }));
}
