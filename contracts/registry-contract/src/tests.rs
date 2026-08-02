//! Phase 2 exit-check battery at the contract boundary: PoW admission accepted,
//! invalid nonce and tampered records rejected, nickname replay protection,
//! ghost key chain verification, and delta/summary behavior.

use std::sync::OnceLock;

use common::identity::{AuthorId, Tier};
use common::registry::{
    admission_challenge_bytes, find_pow_nonce, pow_nonce_meets_target, AdmissionProof,
    AdmissionRecord, NicknameUpdate, RegistryDelta, RegistryParameters, RegistryState,
    RegistrySummary, SignedNickname,
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use freenet_stdlib::prelude::*;
use ghostkey_lib::ghost_key_certificate::GhostkeyCertificateV1;
use ghostkey_lib::notary_certificate::NotaryCertificateV1;
use ghostkey_lib::util::create_keypair;
use rand_core::OsRng;
use serde::Serialize;

use crate::{update_state_impl, Contract};

fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn reg_params() -> RegistryParameters {
    RegistryParameters { canvas_id: [7; 32] }
}

fn params_bytes() -> Parameters<'static> {
    Parameters::from(common::to_cbor(&reg_params()))
}

fn state_bytes(state: &RegistryState) -> State<'static> {
    State::from(common::to_cbor(state))
}

fn genesis() -> State<'static> {
    state_bytes(&RegistryState::default())
}

fn valid_nonce() -> u64 {
    static NONCE: OnceLock<u64> = OnceLock::new();
    *NONCE.get_or_init(|| find_pow_nonce(&reg_params(), &key(1).verifying_key()))
}

fn pow_record(nickname: Option<SignedNickname>) -> AdmissionRecord {
    AdmissionRecord::sign(
        &key(1),
        &reg_params(),
        AdmissionProof::Work {
            nonce: valid_nonce(),
        },
        nickname,
        1000,
    )
}

fn delta_update(delta: &RegistryDelta) -> Vec<UpdateData<'static>> {
    vec![UpdateData::Delta(StateDelta::from(common::to_cbor(delta)))]
}

fn admissions_delta(records: Vec<AdmissionRecord>) -> Vec<UpdateData<'static>> {
    delta_update(&RegistryDelta {
        admissions: records,
        nicknames: vec![],
    })
}

fn run_update(
    state: State<'static>,
    data: Vec<UpdateData<'static>>,
) -> Result<RegistryState, ContractError> {
    let modification = Contract::update_state(params_bytes(), state, data)?;
    let new_state = modification.new_state.expect("update returns a state");
    Ok(common::from_cbor(new_state.as_ref()).unwrap())
}

fn validate(state: &RegistryState) -> ValidateResult {
    Contract::validate_state(
        params_bytes(),
        state_bytes(state),
        RelatedContracts::default(),
    )
    .unwrap()
}

// --- PoW admission ---------------------------------------------------------

#[test]
fn genesis_state_validates() {
    let result =
        Contract::validate_state(params_bytes(), genesis(), RelatedContracts::default()).unwrap();
    assert!(matches!(result, ValidateResult::Valid));
    // Zero-length bytes are also the genesis state.
    let result = Contract::validate_state(
        params_bytes(),
        State::from(vec![]),
        RelatedContracts::default(),
    )
    .unwrap();
    assert!(matches!(result, ValidateResult::Valid));
}

#[test]
fn valid_pow_admission_is_accepted_and_validates() {
    let nickname = SignedNickname::sign(&key(1), &reg_params(), "smoke", 1);
    let record = pow_record(Some(nickname));
    let new_state = run_update(genesis(), admissions_delta(vec![record])).unwrap();

    let author = AuthorId::from(&key(1).verifying_key());
    let stored = new_state.identities.get(&author).expect("record admitted");
    assert_eq!(stored.tier(), Tier::Pow);
    assert_eq!(stored.nickname.as_ref().unwrap().name, "smoke");
    assert!(matches!(validate(&new_state), ValidateResult::Valid));
}

#[test]
fn invalid_nonce_is_rejected() {
    let bad_nonce = (0u64..)
        .find(|n| !pow_nonce_meets_target(&reg_params(), &key(1).verifying_key(), *n))
        .unwrap();
    let record = AdmissionRecord::sign(
        &key(1),
        &reg_params(),
        AdmissionProof::Work { nonce: bad_nonce },
        None,
        1000,
    );
    let err = run_update(genesis(), admissions_delta(vec![record])).unwrap_err();
    let ContractError::InvalidUpdateWithInfo { reason } = err else {
        panic!("expected InvalidUpdateWithInfo, got {err:?}");
    };
    assert!(reason.contains("difficulty target"), "got: {reason}");
}

#[test]
fn tampered_record_is_rejected() {
    let mut record = pow_record(None);
    record.admitted_ts += 1;
    let err = run_update(genesis(), admissions_delta(vec![record])).unwrap_err();
    let ContractError::InvalidUpdateWithInfo { reason } = err else {
        panic!("expected InvalidUpdateWithInfo, got {err:?}");
    };
    assert!(reason.contains("admission signature"), "got: {reason}");
}

#[test]
fn far_future_admission_is_rejected() {
    use common::constants::MAX_FUTURE_SKEW_SECS;
    let now = 1_000_000u64;
    let make = |ts: u64| {
        AdmissionRecord::sign(
            &key(1),
            &reg_params(),
            AdmissionProof::Work {
                nonce: valid_nonce(),
            },
            None,
            ts,
        )
    };

    let too_far = update_state_impl(
        params_bytes(),
        genesis(),
        admissions_delta(vec![make(now + MAX_FUTURE_SKEW_SECS + 1)]),
        &crate::ghostkey::freenet_ghostkey_check,
        now,
    );
    assert!(too_far.is_err());

    let within_skew = update_state_impl(
        params_bytes(),
        genesis(),
        admissions_delta(vec![make(now + MAX_FUTURE_SKEW_SECS)]),
        &crate::ghostkey::freenet_ghostkey_check,
        now,
    );
    assert!(within_skew.is_ok());
}

#[test]
fn forged_state_fails_validation() {
    // A malicious PUT: correctly signed record whose nonce fails the target.
    let bad_nonce = (0u64..)
        .find(|n| !pow_nonce_meets_target(&reg_params(), &key(1).verifying_key(), *n))
        .unwrap();
    let record = AdmissionRecord::sign(
        &key(1),
        &reg_params(),
        AdmissionProof::Work { nonce: bad_nonce },
        None,
        1000,
    );
    let mut forged = RegistryState::default();
    forged.insert_record(record);
    assert!(matches!(validate(&forged), ValidateResult::Invalid));
}

#[test]
fn full_state_update_merges_after_verification() {
    let mut incoming = RegistryState::default();
    incoming.insert_record(pow_record(None));
    let new_state = run_update(genesis(), vec![UpdateData::State(state_bytes(&incoming))]).unwrap();
    assert_eq!(new_state, incoming);

    // The same merge with a forged record inside is rejected wholesale.
    let bad_nonce = (0u64..)
        .find(|n| !pow_nonce_meets_target(&reg_params(), &key(2).verifying_key(), *n))
        .unwrap();
    let mut forged = incoming.clone();
    forged.insert_record(AdmissionRecord::sign(
        &key(2),
        &reg_params(),
        AdmissionProof::Work { nonce: bad_nonce },
        None,
        1000,
    ));
    assert!(run_update(genesis(), vec![UpdateData::State(state_bytes(&forged))]).is_err());
}

// --- Nickname updates ------------------------------------------------------

#[test]
fn nickname_update_applies_and_replay_is_a_no_op() {
    let admitted = run_update(
        genesis(),
        admissions_delta(vec![pow_record(Some(SignedNickname::sign(
            &key(1),
            &reg_params(),
            "first",
            1,
        )))]),
    )
    .unwrap();

    let rename = RegistryDelta {
        admissions: vec![],
        nicknames: vec![NicknameUpdate {
            identity_vk: key(1).verifying_key(),
            nickname: SignedNickname::sign(&key(1), &reg_params(), "second", 2),
        }],
    };
    let renamed = run_update(state_bytes(&admitted), delta_update(&rename)).unwrap();
    let author = AuthorId::from(&key(1).verifying_key());
    assert_eq!(
        renamed.identities[&author].nickname.as_ref().unwrap().name,
        "second"
    );

    // Replaying the old signed nickname (version 1) succeeds as a merge but
    // changes nothing: monotonic-version replay protection.
    let replay = RegistryDelta {
        admissions: vec![],
        nicknames: vec![NicknameUpdate {
            identity_vk: key(1).verifying_key(),
            nickname: SignedNickname::sign(&key(1), &reg_params(), "first", 1),
        }],
    };
    let after_replay = run_update(state_bytes(&renamed), delta_update(&replay)).unwrap();
    assert_eq!(after_replay, renamed);
}

#[test]
fn tampered_nickname_update_is_rejected() {
    let admitted = run_update(genesis(), admissions_delta(vec![pow_record(None)])).unwrap();
    let mut nickname = SignedNickname::sign(&key(1), &reg_params(), "name", 1);
    nickname.version = 2;
    let delta = RegistryDelta {
        admissions: vec![],
        nicknames: vec![NicknameUpdate {
            identity_vk: key(1).verifying_key(),
            nickname,
        }],
    };
    assert!(run_update(state_bytes(&admitted), delta_update(&delta)).is_err());
}

// --- Summary / delta at the contract boundary ------------------------------

#[test]
fn delta_to_converged_peer_is_zero_bytes() {
    let state = run_update(
        genesis(),
        admissions_delta(vec![pow_record(Some(SignedNickname::sign(
            &key(1),
            &reg_params(),
            "smoke",
            1,
        )))]),
    )
    .unwrap();
    let raw = state_bytes(&state);

    let summary = Contract::summarize_state(params_bytes(), raw.clone()).unwrap();
    assert!(summary.as_ref().len() < raw.as_ref().len() / 4);

    let delta = Contract::get_state_delta(params_bytes(), raw.clone(), summary).unwrap();
    assert_eq!(
        delta.as_ref().len(),
        0,
        "delta to a converged peer was {} bytes against a {} byte state",
        delta.as_ref().len(),
        raw.as_ref().len()
    );

    // A peer with an empty summary gets the full record.
    let empty_summary = StateSummary::from(common::to_cbor(&RegistrySummary::default()));
    let full = Contract::get_state_delta(params_bytes(), raw, empty_summary).unwrap();
    let parsed: RegistryDelta = common::from_cbor(full.as_ref()).unwrap();
    assert_eq!(parsed.admissions.len(), 1);
}

// --- Ghost key admission ---------------------------------------------------

#[derive(Serialize)]
struct TestScopedPayload {
    requestor: String,
    payload: Vec<u8>,
}

struct GhostFixture {
    master_vk: VerifyingKey,
    record: AdmissionRecord,
}

/// Mint a full test chain (master -> notary -> ghost key) and sign the
/// admission challenge for `identity`, exactly as the delegate would.
fn ghost_fixture(identity: &SigningKey, challenge_for: &VerifyingKey) -> GhostFixture {
    let (master_signing, master_vk) = create_keypair(&mut OsRng).unwrap();
    let (notary_cert, notary_rsa_key) =
        NotaryCertificateV1::new(&master_signing, &"$5 donation".to_string()).unwrap();
    let (certificate, ghost_signing) = GhostkeyCertificateV1::new(&notary_cert, &notary_rsa_key);

    let scoped_payload = common::to_cbor(&TestScopedPayload {
        requestor: "test-app".to_string(),
        payload: admission_challenge_bytes(&reg_params(), challenge_for),
    });
    let signature = ghost_signing.sign(&scoped_payload);

    use ghostkey_lib::armorable::Armorable;
    let record = AdmissionRecord::sign(
        identity,
        &reg_params(),
        AdmissionProof::Ghostkey {
            scoped_payload,
            signature: signature.to_bytes().to_vec(),
            certificate_pem: certificate.to_armored_string().unwrap(),
        },
        None,
        2000,
    );
    GhostFixture { master_vk, record }
}

fn run_ghost_update(
    record: AdmissionRecord,
    master_vk: VerifyingKey,
) -> Result<RegistryState, ContractError> {
    let check =
        move |params: &RegistryParameters, vk: &VerifyingKey, sp: &[u8], sig: &[u8], pem: &str| {
            crate::ghostkey::verify_with_master(params, vk, sp, sig, pem, &master_vk)
        };
    let modification = update_state_impl(
        params_bytes(),
        genesis(),
        admissions_delta(vec![record]),
        &check,
        1_000_000,
    )?;
    Ok(common::from_cbor(modification.new_state.unwrap().as_ref()).unwrap())
}

#[test]
fn ghostkey_admission_with_valid_chain_is_accepted() {
    let identity = key(5);
    let fixture = ghost_fixture(&identity, &identity.verifying_key());
    let state = run_ghost_update(fixture.record, fixture.master_vk).unwrap();
    let stored = &state.identities[&AuthorId::from(&identity.verifying_key())];
    assert_eq!(stored.tier(), Tier::Ghostkey);
}

#[test]
fn ghostkey_chain_to_wrong_master_is_rejected() {
    let identity = key(5);
    let fixture = ghost_fixture(&identity, &identity.verifying_key());
    let (_, wrong_master) = create_keypair(&mut OsRng).unwrap();
    let err = run_ghost_update(fixture.record, wrong_master).unwrap_err();
    let ContractError::InvalidUpdateWithInfo { reason } = err else {
        panic!("expected InvalidUpdateWithInfo, got {err:?}");
    };
    assert!(
        reason.contains("chain verification failed"),
        "got: {reason}"
    );
}

#[test]
fn ghostkey_challenge_for_another_identity_is_rejected() {
    // The delegate signed a challenge bound to key(6); the record claims key(5).
    let identity = key(5);
    let fixture = ghost_fixture(&identity, &key(6).verifying_key());
    let err = run_ghost_update(fixture.record, fixture.master_vk).unwrap_err();
    let ContractError::InvalidUpdateWithInfo { reason } = err else {
        panic!("expected InvalidUpdateWithInfo, got {err:?}");
    };
    assert!(reason.contains("admission challenge"), "got: {reason}");
}

#[test]
fn ghostkey_tampered_scoped_payload_is_rejected() {
    let identity = key(5);
    let fixture = ghost_fixture(&identity, &identity.verifying_key());
    let AdmissionProof::Ghostkey {
        mut scoped_payload,
        signature,
        certificate_pem,
    } = fixture.record.proof
    else {
        unreachable!()
    };
    let last = scoped_payload.len() - 1;
    scoped_payload[last] ^= 1;
    // Re-sign the record so only the ghost key signature is broken.
    let record = AdmissionRecord::sign(
        &identity,
        &reg_params(),
        AdmissionProof::Ghostkey {
            scoped_payload,
            signature,
            certificate_pem,
        },
        None,
        2000,
    );
    let err = run_ghost_update(record, fixture.master_vk).unwrap_err();
    let ContractError::InvalidUpdateWithInfo { reason } = err else {
        panic!("expected InvalidUpdateWithInfo, got {err:?}");
    };
    assert!(reason.contains("does not verify"), "got: {reason}");
}

#[test]
fn ghostkey_test_chain_fails_against_the_real_master_key() {
    // Through the production entry point the chain must reach the compiled-in
    // Freenet master key, which a test-minted chain cannot.
    let identity = key(5);
    let fixture = ghost_fixture(&identity, &identity.verifying_key());
    assert!(run_update(genesis(), admissions_delta(vec![fixture.record])).is_err());
}
