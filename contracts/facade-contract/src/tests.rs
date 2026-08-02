//! Phase 7 battery at the facade contract boundary: genesis and signed states
//! validate, tampered metadata / web slots are rejected, updates converge
//! last-writer-wins regardless of arrival order (via State and Delta payloads
//! alike), and the delta to a converged peer is zero bytes.

use common::facade::{encode_facade_frame, FacadeMetadata, FacadeParameters, FacadePointer};
use ed25519_dalek::SigningKey;
use freenet_stdlib::prelude::*;

use crate::{update_state_impl, validate_state_impl, Contract};

fn owner() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn facade_params() -> FacadeParameters {
    FacadeParameters {
        owner_vk: owner().verifying_key(),
        instance: 0,
    }
}

fn params_bytes() -> Parameters<'static> {
    Parameters::from(common::to_cbor(&facade_params()))
}

fn signed_state(version: u64, current_app: [u8; 32], web: &[u8]) -> Vec<u8> {
    let pointer = FacadePointer {
        version,
        current_app: Some(current_app),
        prev_apps: vec![],
        web_hash: *blake3::hash(web).as_bytes(),
    };
    let meta = FacadeMetadata::sign(&owner(), &facade_params(), pointer);
    encode_facade_frame(&common::to_cbor(&meta), web)
}

fn validate(state: Vec<u8>) -> ValidateResult {
    validate_state_impl(params_bytes(), State::from(state)).unwrap()
}

fn update(state: Vec<u8>, data: Vec<UpdateData<'static>>) -> Result<Vec<u8>, ContractError> {
    update_state_impl(params_bytes(), State::from(state), data).map(|m| {
        m.new_state
            .expect("facade updates always produce a state")
            .as_ref()
            .to_vec()
    })
}

#[test]
fn genesis_and_signed_states_validate() {
    assert!(matches!(validate(vec![]), ValidateResult::Valid));
    assert!(matches!(
        validate(signed_state(1, [3; 32], b"loader")),
        ValidateResult::Valid
    ));
}

#[test]
fn unsigned_or_tampered_states_are_invalid() {
    // Not a frame at all.
    assert!(matches!(
        validate(b"junk".to_vec()),
        ValidateResult::Invalid
    ));
    // Valid frame, unsigned metadata slot.
    assert!(matches!(
        validate(encode_facade_frame(b"", b"loader")),
        ValidateResult::Invalid
    ));
    // Signed state with a swapped web slot (hash no longer matches).
    let mut state = signed_state(1, [3; 32], b"loader");
    let n = state.len();
    state[n - 1] ^= 0xFF;
    assert!(matches!(validate(state), ValidateResult::Invalid));
    // Signed by the wrong key.
    let other = SigningKey::from_bytes(&[2; 32]);
    let pointer = FacadePointer {
        version: 1,
        current_app: Some([3; 32]),
        prev_apps: vec![],
        web_hash: *blake3::hash(b"loader").as_bytes(),
    };
    let meta = FacadeMetadata::sign(&other, &facade_params(), pointer);
    assert!(matches!(
        validate(encode_facade_frame(&common::to_cbor(&meta), b"loader")),
        ValidateResult::Invalid
    ));
}

#[test]
fn updates_converge_last_writer_wins_in_any_order() {
    let v1 = signed_state(1, [3; 32], b"loader v1");
    let v2 = signed_state(2, [4; 32], b"loader v2");

    // Newer replaces older; older cannot roll back newer.
    let forward = update(v1.clone(), vec![UpdateData::State(State::from(v2.clone()))]).unwrap();
    let backward = update(v2.clone(), vec![UpdateData::State(State::from(v1.clone()))]).unwrap();
    assert_eq!(forward, v2);
    assert_eq!(backward, v2);

    // Delta payloads are interpreted as full framed states (fdev's default).
    let via_delta = update(
        v1.clone(),
        vec![UpdateData::Delta(StateDelta::from(v2.clone()))],
    )
    .unwrap();
    assert_eq!(via_delta, v2);

    // Genesis accepts the first signed state.
    let from_genesis = update(vec![], vec![UpdateData::State(State::from(v1.clone()))]).unwrap();
    assert_eq!(from_genesis, v1);
}

#[test]
fn invalid_update_payloads_are_rejected() {
    let v1 = signed_state(1, [3; 32], b"loader v1");
    assert!(update(
        v1.clone(),
        vec![UpdateData::State(State::from(b"junk".to_vec()))]
    )
    .is_err());
    // An empty update payload is not a valid signed replacement.
    assert!(update(v1, vec![UpdateData::State(State::from(Vec::new()))]).is_err());
}

#[test]
fn delta_to_converged_peer_is_zero_bytes() {
    let state = signed_state(5, [3; 32], b"loader");
    let summary = Contract::summarize_state(params_bytes(), State::from(state.clone())).unwrap();
    let delta =
        Contract::get_state_delta(params_bytes(), State::from(state.clone()), summary).unwrap();
    assert!(delta.as_ref().is_empty(), "converged delta must be empty");

    // A stale peer gets the full framed state.
    let stale = StateSummary::from(common::to_cbor(&1u64));
    let delta =
        Contract::get_state_delta(params_bytes(), State::from(state.clone()), stale).unwrap();
    assert_eq!(delta.as_ref(), state.as_slice());
}
