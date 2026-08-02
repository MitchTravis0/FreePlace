//! Phase 2 test battery for the registry core: merge monoid properties,
//! delta/summary behavior, PoW target checks, per-field tamper tests, replay
//! protection, and hard-coded wire-format locks.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use proptest::prelude::*;

use crate::identity::{AuthorId, Tier};
use crate::registry::{
    admission_challenge_bytes, admission_signing_bytes, find_pow_nonce, nickname_signing_bytes,
    pow_digest, pow_nonce_meets_target, serialize_registry_delta, AdmissionProof, AdmissionRecord,
    NicknameUpdate, RegistryDelta, RegistryParameters, RegistryState, RegistrySummary,
    SignedNickname,
};
use crate::{from_cbor, to_cbor};

fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn params() -> RegistryParameters {
    RegistryParameters { canvas_id: [7; 32] }
}

/// Mock chain verification: accepts exactly one magic PEM string. Proof-field
/// tampering is still caught by the record signature, which covers the proof.
fn mock_ghostkey_check(
    _params: &RegistryParameters,
    _identity_vk: &VerifyingKey,
    _scoped_payload: &[u8],
    _signature: &[u8],
    certificate_pem: &str,
) -> Result<(), String> {
    if certificate_pem == "valid-cert" {
        Ok(())
    } else {
        Err("mock: unknown certificate".to_string())
    }
}

/// Ground nonce for key(1) under params(), computed once per test run.
fn valid_nonce() -> u64 {
    static NONCE: OnceLock<u64> = OnceLock::new();
    *NONCE.get_or_init(|| find_pow_nonce(&params(), &key(1).verifying_key()))
}

/// First nonce that fails the target for key(1) (almost always 0 or 1).
fn failing_nonce() -> u64 {
    (0u64..)
        .find(|n| !pow_nonce_meets_target(&params(), &key(1).verifying_key(), *n))
        .unwrap()
}

fn valid_record(nickname: Option<SignedNickname>) -> AdmissionRecord {
    AdmissionRecord::sign(
        &key(1),
        &params(),
        AdmissionProof::Work {
            nonce: valid_nonce(),
        },
        nickname,
        1000,
    )
}

fn valid_ghostkey_record() -> AdmissionRecord {
    AdmissionRecord::sign(
        &key(2),
        &params(),
        AdmissionProof::Ghostkey {
            scoped_payload: vec![1, 2, 3],
            signature: vec![9; 64],
            certificate_pem: "valid-cert".to_string(),
        },
        None,
        2000,
    )
}

// --- Merge monoid properties ----------------------------------------------

/// (author index, admitted_ts, nickname version or 0). Proofs use arbitrary
/// nonces: merge semantics never verify, so grinding is not needed here.
type Spec = (u8, u64, u64);

fn record_from(spec: Spec) -> AdmissionRecord {
    let (idx, ts, nick_version) = spec;
    let signer = key(idx + 1);
    let nickname = (nick_version > 0).then(|| {
        SignedNickname::sign(
            &signer,
            &params(),
            &format!("nick-{nick_version}"),
            nick_version,
        )
    });
    AdmissionRecord::sign(
        &signer,
        &params(),
        AdmissionProof::Work { nonce: ts },
        nickname,
        ts,
    )
}

fn state_from(specs: &[Spec]) -> RegistryState {
    let mut state = RegistryState::default();
    for spec in specs {
        state.insert_record(record_from(*spec));
    }
    state
}

fn merged(a: &RegistryState, b: &RegistryState) -> RegistryState {
    let mut out = a.clone();
    out.merge(b);
    out
}

fn arb_specs() -> impl Strategy<Value = Vec<Spec>> {
    prop::collection::vec((0..3u8, 0..48u64, 0..4u64), 0..10)
}

proptest! {
    #[test]
    fn merge_is_commutative(a in arb_specs(), b in arb_specs()) {
        let (sa, sb) = (state_from(&a), state_from(&b));
        prop_assert_eq!(merged(&sa, &sb), merged(&sb, &sa));
    }

    #[test]
    fn merge_is_associative(a in arb_specs(), b in arb_specs(), c in arb_specs()) {
        let (sa, sb, sc) = (state_from(&a), state_from(&b), state_from(&c));
        prop_assert_eq!(merged(&merged(&sa, &sb), &sc), merged(&sa, &merged(&sb, &sc)));
    }

    #[test]
    fn merge_identity(a in arb_specs()) {
        let sa = state_from(&a);
        let empty = RegistryState::default();
        prop_assert_eq!(merged(&sa, &empty), sa.clone());
        prop_assert_eq!(merged(&empty, &sa), sa);
    }

    /// Any insertion order of the same records converges byte-identically.
    #[test]
    fn insertion_order_is_irrelevant(
        (original, shuffled) in arb_specs().prop_flat_map(|v| {
            let orig = v.clone();
            (Just(orig), Just(v).prop_shuffle())
        })
    ) {
        prop_assert_eq!(state_from(&original), state_from(&shuffled));
    }

    /// A converged peer always gets a zero-byte delta.
    #[test]
    fn delta_to_self_is_always_none(a in arb_specs()) {
        let sa = state_from(&a);
        let delta = sa.delta(&sa.summarize());
        prop_assert!(delta.is_none());
        prop_assert_eq!(serialize_registry_delta(&delta).len(), 0);
    }

    /// Summary + delta reconciliation reaches the extended state exactly, when
    /// the extension is new identities plus newer nicknames.
    #[test]
    fn delta_summary_roundtrip(
        base in arb_specs(),
        new_authors in prop::collection::vec((10..13u8, 0..48u64, 0..4u64), 0..4),
        nick_bumps in prop::collection::vec((0..3u8, 100..104u64), 0..4),
    ) {
        let state_a = state_from(&base);
        let mut state_b = state_a.clone();
        for spec in &new_authors {
            state_b.insert_record(record_from(*spec));
        }
        for (idx, version) in &nick_bumps {
            let signer = key(idx + 1);
            let update = NicknameUpdate {
                identity_vk: signer.verifying_key(),
                nickname: SignedNickname::sign(&signer, &params(), "bumped", *version),
            };
            state_b.apply_nickname(&update);
        }

        let delta = state_b.delta(&state_a.summarize());
        prop_assert_eq!(delta.is_none(), state_a == state_b);
        let mut reconstructed = state_a.clone();
        if let Some(d) = &delta {
            reconstructed.apply_delta(d);
        }
        prop_assert_eq!(reconstructed, state_b);
    }
}

// --- Deterministic merge details ------------------------------------------

#[test]
fn readmission_converges_to_smallest_core_in_both_orders() {
    let early = record_from((0, 100, 0));
    let late = record_from((0, 200, 0));
    for order in [[early.clone(), late.clone()], [late.clone(), early.clone()]] {
        let mut state = RegistryState::default();
        for r in order {
            state.insert_record(r);
        }
        let stored = state.identities.values().next().unwrap();
        assert_eq!(stored.admitted_ts, 100);
    }
}

#[test]
fn nickname_survives_core_conflict_resolution() {
    // The losing (later) record carries the only nickname; it must survive.
    let early = record_from((0, 100, 0));
    let late = record_from((0, 200, 3));
    for order in [[early.clone(), late.clone()], [late.clone(), early.clone()]] {
        let mut state = RegistryState::default();
        for r in order {
            state.insert_record(r);
        }
        let stored = state.identities.values().next().unwrap();
        assert_eq!(stored.admitted_ts, 100);
        assert_eq!(stored.nickname.as_ref().unwrap().version, 3);
    }
}

#[test]
fn nickname_replay_of_older_version_is_a_no_op() {
    let signer = key(1);
    let mut state = RegistryState::default();
    state.insert_record(record_from((0, 100, 0)));
    let v2 = NicknameUpdate {
        identity_vk: signer.verifying_key(),
        nickname: SignedNickname::sign(&signer, &params(), "second", 2),
    };
    let v1 = NicknameUpdate {
        identity_vk: signer.verifying_key(),
        nickname: SignedNickname::sign(&signer, &params(), "first", 1),
    };
    state.apply_nickname(&v2);
    let before = state.clone();
    state.apply_nickname(&v1);
    assert_eq!(state, before);
    assert_eq!(
        state
            .identities
            .values()
            .next()
            .unwrap()
            .nickname
            .as_ref()
            .unwrap()
            .name,
        "second"
    );
}

#[test]
fn nickname_same_version_conflict_is_deterministic() {
    let signer = key(1);
    let a = NicknameUpdate {
        identity_vk: signer.verifying_key(),
        nickname: SignedNickname::sign(&signer, &params(), "alpha", 2),
    };
    let b = NicknameUpdate {
        identity_vk: signer.verifying_key(),
        nickname: SignedNickname::sign(&signer, &params(), "beta", 2),
    };
    let mut ab = RegistryState::default();
    ab.insert_record(record_from((0, 100, 0)));
    let mut ba = ab.clone();
    ab.apply_nickname(&a);
    ab.apply_nickname(&b);
    ba.apply_nickname(&b);
    ba.apply_nickname(&a);
    assert_eq!(ab, ba);
}

#[test]
fn nickname_for_unadmitted_identity_is_dropped() {
    let signer = key(9);
    let mut state = RegistryState::default();
    state.apply_nickname(&NicknameUpdate {
        identity_vk: signer.verifying_key(),
        nickname: SignedNickname::sign(&signer, &params(), "ghost", 1),
    });
    assert!(state.identities.is_empty());
}

// --- Proof-of-work ---------------------------------------------------------

#[test]
fn ground_nonce_admits_and_failing_nonce_rejects() {
    let good = valid_record(None);
    good.verify(&params(), &mock_ghostkey_check)
        .expect("valid PoW admission verifies");

    let bad = AdmissionRecord::sign(
        &key(1),
        &params(),
        AdmissionProof::Work {
            nonce: failing_nonce(),
        },
        None,
        1000,
    );
    let err = bad.verify(&params(), &mock_ghostkey_check).unwrap_err();
    assert!(err.contains("difficulty target"), "unexpected error: {err}");
}

#[test]
fn pow_challenge_binds_identity_and_canvas() {
    let nonce = valid_nonce();
    assert!(pow_nonce_meets_target(
        &params(),
        &key(1).verifying_key(),
        nonce
    ));
    // Same nonce almost surely fails for another identity or canvas; if this
    // ever flakes the fixture keys changed, not the code.
    let other_canvas = RegistryParameters { canvas_id: [8; 32] };
    assert!(
        !pow_nonce_meets_target(&params(), &key(2).verifying_key(), nonce)
            || !pow_nonce_meets_target(&other_canvas, &key(1).verifying_key(), nonce)
    );
}

#[test]
fn record_is_bound_to_canvas_id() {
    let record = valid_record(None);
    let other = RegistryParameters { canvas_id: [8; 32] };
    assert!(record.verify(&other, &mock_ghostkey_check).is_err());
}

// --- Ghost key proof plumbing ---------------------------------------------

#[test]
fn ghostkey_record_verifies_through_the_hook() {
    let record = valid_ghostkey_record();
    record
        .verify(&params(), &mock_ghostkey_check)
        .expect("mock-accepted ghost key admission verifies");
    assert_eq!(record.tier(), Tier::Ghostkey);
}

#[test]
fn ghostkey_hook_rejection_fails_verification() {
    let record = AdmissionRecord::sign(
        &key(2),
        &params(),
        AdmissionProof::Ghostkey {
            scoped_payload: vec![1, 2, 3],
            signature: vec![9; 64],
            certificate_pem: "unknown-cert".to_string(),
        },
        None,
        2000,
    );
    assert!(record.verify(&params(), &mock_ghostkey_check).is_err());
}

#[test]
fn ghostkey_field_caps_enforced() {
    for (payload, sig, pem) in [
        (vec![], vec![9; 64], "valid-cert".to_string()),
        (vec![0; 1025], vec![9; 64], "valid-cert".to_string()),
        (vec![1], vec![9; 63], "valid-cert".to_string()),
        (vec![1], vec![9; 64], "x".repeat(8193)),
    ] {
        let record = AdmissionRecord::sign(
            &key(2),
            &params(),
            AdmissionProof::Ghostkey {
                scoped_payload: payload,
                signature: sig,
                certificate_pem: pem,
            },
            None,
            2000,
        );
        assert!(record.verify(&params(), &mock_ghostkey_check).is_err());
    }
}

// --- Signature tamper tests: one per signed field -------------------------

fn assert_tampered_fails(mutate: impl FnOnce(&mut AdmissionRecord)) {
    let mut record = valid_record(Some(SignedNickname::sign(&key(1), &params(), "nick", 1)));
    record
        .verify(&params(), &mock_ghostkey_check)
        .expect("untampered record verifies");
    mutate(&mut record);
    assert!(
        record.verify(&params(), &mock_ghostkey_check).is_err(),
        "tampered record must fail"
    );
}

#[test]
fn tamper_identity_vk_fails() {
    assert_tampered_fails(|r| r.identity_vk = key(9).verifying_key());
}

#[test]
fn tamper_admitted_ts_fails() {
    assert_tampered_fails(|r| r.admitted_ts += 1);
}

#[test]
fn tamper_pow_nonce_fails() {
    assert_tampered_fails(|r| {
        r.proof = AdmissionProof::Work {
            nonce: valid_nonce() ^ 1,
        }
    });
}

#[test]
fn tamper_record_signature_fails() {
    assert_tampered_fails(|r| {
        let mut bytes = r.signature.to_bytes();
        bytes[0] ^= 0x01;
        r.signature = Signature::from_bytes(&bytes);
    });
}

#[test]
fn tamper_nickname_name_fails() {
    assert_tampered_fails(|r| r.nickname.as_mut().unwrap().name.push('x'));
}

#[test]
fn tamper_nickname_version_fails() {
    assert_tampered_fails(|r| r.nickname.as_mut().unwrap().version += 1);
}

#[test]
fn tamper_nickname_signature_fails() {
    assert_tampered_fails(|r| {
        let nick = r.nickname.as_mut().unwrap();
        let mut bytes = nick.signature.to_bytes();
        bytes[0] ^= 0x01;
        nick.signature = Signature::from_bytes(&bytes);
    });
}

#[test]
fn tamper_ghostkey_proof_fields_fails() {
    let base = valid_ghostkey_record();
    base.verify(&params(), &mock_ghostkey_check).unwrap();
    // Any proof-field change breaks the record signature before the ghost key
    // hook even runs.
    let mutations: [fn(&mut AdmissionProof); 3] = [
        |p| {
            if let AdmissionProof::Ghostkey { scoped_payload, .. } = p {
                scoped_payload.push(0);
            }
        },
        |p| {
            if let AdmissionProof::Ghostkey { signature, .. } = p {
                signature[0] ^= 1;
            }
        },
        |p| {
            if let AdmissionProof::Ghostkey {
                certificate_pem, ..
            } = p
            {
                certificate_pem.push('x');
            }
        },
    ];
    for mutate in mutations {
        let mut record = base.clone();
        mutate(&mut record.proof);
        assert!(record.verify(&params(), &mock_ghostkey_check).is_err());
    }
}

// --- Nickname validity bounds ---------------------------------------------

#[test]
fn nickname_version_zero_is_invalid() {
    let nick = SignedNickname::sign(&key(1), &params(), "zero", 0);
    assert!(nick.verify(&params(), &key(1).verifying_key()).is_err());
}

#[test]
fn nickname_length_bounds_enforced() {
    let vk = key(1).verifying_key();
    assert!(SignedNickname::sign(&key(1), &params(), "", 1)
        .verify(&params(), &vk)
        .is_err());
    assert!(SignedNickname::sign(&key(1), &params(), &"a".repeat(32), 1)
        .verify(&params(), &vk)
        .is_ok());
    assert!(SignedNickname::sign(&key(1), &params(), &"a".repeat(33), 1)
        .verify(&params(), &vk)
        .is_err());
}

#[test]
fn nickname_is_bound_to_identity_and_canvas() {
    let nick = SignedNickname::sign(&key(1), &params(), "mine", 1);
    assert!(nick.verify(&params(), &key(2).verifying_key()).is_err());
    let other = RegistryParameters { canvas_id: [8; 32] };
    assert!(nick.verify(&other, &key(1).verifying_key()).is_err());
}

// --- State-level verification ---------------------------------------------

#[test]
fn state_with_mismatched_map_key_is_invalid() {
    let mut state = RegistryState::default();
    let record = valid_record(None);
    state
        .identities
        .insert(AuthorId::from(&key(3).verifying_key()), record);
    let err = state.verify(&params(), &mock_ghostkey_check).unwrap_err();
    assert!(err.contains("mismatched identity key"), "got: {err}");
}

#[test]
fn valid_state_verifies_end_to_end() {
    let mut state = RegistryState::default();
    state.insert_record(valid_record(Some(SignedNickname::sign(
        &key(1),
        &params(),
        "nick",
        1,
    ))));
    state.insert_record(valid_ghostkey_record());
    state.verify(&params(), &mock_ghostkey_check).unwrap();
    assert_eq!(state.identities.len(), 2);
}

// --- Delta / summary size behavior ----------------------------------------

#[test]
fn delta_to_converged_peer_is_zero_bytes_against_populated_state() {
    let mut state = RegistryState::default();
    for idx in 0..10u8 {
        state.insert_record(record_from((idx, 100 + idx as u64, (idx % 3) as u64)));
    }
    let state_bytes = to_cbor(&state);
    assert!(state_bytes.len() > 2000, "state should be populated");

    let summary = state.summarize();
    let summary_bytes = to_cbor(&summary);
    assert!(
        summary_bytes.len() < state_bytes.len() / 4,
        "summary ({} B) must be much smaller than state ({} B)",
        summary_bytes.len(),
        state_bytes.len()
    );

    let delta_bytes = serialize_registry_delta(&state.delta(&summary));
    assert_eq!(
        delta_bytes.len(),
        0,
        "delta to a converged peer was {} bytes against a {} byte state",
        delta_bytes.len(),
        state_bytes.len()
    );
}

#[test]
fn delta_ships_full_record_for_unknown_identity_and_newer_nickname_for_known() {
    let mut state = RegistryState::default();
    state.insert_record(record_from((0, 100, 2)));
    state.insert_record(record_from((1, 100, 0)));

    // Requester knows author 0 at nickname version 1, has never seen author 1.
    let mut requester = BTreeMap::new();
    requester.insert(AuthorId::from(&key(1).verifying_key()), 1u64);
    let delta = state.delta(&RegistrySummary(requester)).unwrap();
    assert_eq!(delta.admissions.len(), 1);
    assert_eq!(
        AuthorId::from(&delta.admissions[0].identity_vk),
        AuthorId::from(&key(2).verifying_key())
    );
    assert_eq!(delta.nicknames.len(), 1);
    assert_eq!(delta.nicknames[0].nickname.version, 2);
}

// --- Wire-format locks: hard-coded hex, not just roundtrip ----------------

#[test]
fn challenge_and_pow_digest_format_locked() {
    let challenge = admission_challenge_bytes(&params(), &key(1).verifying_key());
    const EXPECTED_CHALLENGE_HEX: &str = "66726565706c6163653a72656769737472793a6368616c6c656e67653a763107070707070707070707070707070707070707070707070707070707070707078a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c";
    assert_eq!(hex::encode(&challenge), EXPECTED_CHALLENGE_HEX);

    let digest = pow_digest(&params(), &key(1).verifying_key(), 0);
    const EXPECTED_DIGEST_HEX: &str =
        "764ab6cd6d1b9f1e9ad5d8adc7b6e09bd1f2d1f03fe3fb57d76c67f6f6d9ee9c";
    assert_eq!(hex::encode(digest), EXPECTED_DIGEST_HEX);
}

#[test]
fn admission_signing_preimage_format_locked() {
    let work = admission_signing_bytes(
        &params(),
        &key(1).verifying_key(),
        &AdmissionProof::Work { nonce: 258 },
        1000,
    );
    const EXPECTED_WORK_HEX: &str = "66726565706c6163653a72656769737472793a61646d697373696f6e3a763107070707070707070707070707070707070707070707070707070707070707078a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5ce803000000000000000201000000000000";
    assert_eq!(hex::encode(&work), EXPECTED_WORK_HEX);

    let ghost = admission_signing_bytes(
        &params(),
        &key(1).verifying_key(),
        &AdmissionProof::Ghostkey {
            scoped_payload: vec![1, 2],
            signature: vec![3; 64],
            certificate_pem: "PEM".to_string(),
        },
        1000,
    );
    const EXPECTED_GHOSTKEY_HEX: &str = "66726565706c6163653a72656769737472793a61646d697373696f6e3a763107070707070707070707070707070707070707070707070707070707070707078a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5ce8030000000000000102000000010240000000030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030300000050454d";
    assert_eq!(hex::encode(&ghost), EXPECTED_GHOSTKEY_HEX);
}

#[test]
fn nickname_signing_preimage_format_locked() {
    let bytes = nickname_signing_bytes(&params(), &key(1).verifying_key(), 2, "nick");
    const EXPECTED_HEX: &str = "66726565706c6163653a72656769737472793a6e69636b6e616d653a763107070707070707070707070707070707070707070707070707070707070707078a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0200000000000000040000006e69636b";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
}

#[test]
fn registry_parameters_wire_format_locked() {
    let bytes = to_cbor(&params());
    const EXPECTED_HEX: &str = "a16963616e7661735f696498200707070707070707070707070707070707070707070707070707070707070707";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    let parsed: RegistryParameters = from_cbor(&bytes).unwrap();
    assert_eq!(parsed, params());
}

fn canonical_state() -> RegistryState {
    let mut state = RegistryState::default();
    state.insert_record(AdmissionRecord::sign(
        &key(1),
        &params(),
        AdmissionProof::Work { nonce: 42 },
        Some(SignedNickname::sign(&key(1), &params(), "nick", 1)),
        1000,
    ));
    state.insert_record(AdmissionRecord::sign(
        &key(2),
        &params(),
        AdmissionProof::Ghostkey {
            scoped_payload: vec![1, 2],
            signature: vec![3; 64],
            certificate_pem: "PEM".to_string(),
        },
        None,
        2000,
    ));
    state
}

#[test]
fn registry_state_wire_format_locked() {
    let state = canonical_state();
    let bytes = to_cbor(&state);
    const EXPECTED_HEX: &str = "a16a6964656e746974696573a298201881183918770e18a8187d17185f185618a31854186618c3184c187e18cc18cb188d188a189118b418ee183718a2185d18f60f185b188f18c918b31894a56b6964656e746974795f766b58208139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b3946570726f6f66a16847686f73746b6579a36e73636f7065645f7061796c6f6164820102697369676e61747572659840030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303036f63657274696669636174655f70656d6350454d686e69636b6e616d65f66b61646d69747465645f74731907d0697369676e6174757265984007183618dd18bd18ba18d218f318b118e9188418e80d0b18af18531895182e18f6181f1847182f186718e8187b0418d6181c182e186418ff18bb18da18af18e118ac189018351897182a1849188718ab181918fa18e018e0141868189418a0186618e918e8188e18ac18b618b91876183918791874186c18240d9820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185ca56b6964656e746974795f766b58208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c6570726f6f66a164576f726ba1656e6f6e6365182a686e69636b6e616d65a3646e616d65646e69636b6776657273696f6e01697369676e617475726598401866189a18c1184a18971844186b187e18ac18c41218850a18a81851184c18a518c6184918751894183e18ee04186618640e18c218ea18ce183518c0189218cb18a8188318a018b918b9181d18a9182d1899184f18e21893182918ec185318dd1844185718fd187f0e1869189618bd185e1891185e18af18790b6b61646d69747465645f74731903e8697369676e61747572659840186218e7183c18ec182c186618e21886181e188018d01889183818e3185c18321829188b186918ee18b6183718b31893188718eb1858150718331853186318bf18690b18901871188b18c70a1884184e18a5184f18d118f31400183e18b918ab18c21895183618fe18c01836181b0318e71847186d182e07";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    let parsed: RegistryState = from_cbor(&bytes).unwrap();
    assert_eq!(parsed, state);
}

#[test]
fn registry_summary_wire_format_locked() {
    let bytes = to_cbor(&canonical_state().summarize());
    const EXPECTED_HEX: &str = "a298201881183918770e18a8187d17185f185618a31854186618c3184c187e18cc18cb188d188a189118b418ee183718a2185d18f60f185b188f18c918b31894009820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185c01";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
}

#[test]
fn registry_delta_wire_format_locked() {
    let delta = canonical_state()
        .delta(&RegistrySummary(BTreeMap::new()))
        .unwrap();
    let bytes = serialize_registry_delta(&Some(delta.clone()));
    const EXPECTED_HEX: &str = "a26a61646d697373696f6e7382a56b6964656e746974795f766b58208139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b3946570726f6f66a16847686f73746b6579a36e73636f7065645f7061796c6f6164820102697369676e61747572659840030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303036f63657274696669636174655f70656d6350454d686e69636b6e616d65f66b61646d69747465645f74731907d0697369676e6174757265984007183618dd18bd18ba18d218f318b118e9188418e80d0b18af18531895182e18f6181f1847182f186718e8187b0418d6181c182e186418ff18bb18da18af18e118ac189018351897182a1849188718ab181918fa18e018e0141868189418a0186618e918e8188e18ac18b618b91876183918791874186c18240da56b6964656e746974795f766b58208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c6570726f6f66a164576f726ba1656e6f6e6365182a686e69636b6e616d65a3646e616d65646e69636b6776657273696f6e01697369676e617475726598401866189a18c1184a18971844186b187e18ac18c41218850a18a81851184c18a518c6184918751894183e18ee04186618640e18c218ea18ce183518c0189218cb18a8188318a018b918b9181d18a9182d1899184f18e21893182918ec185318dd1844185718fd187f0e1869189618bd185e1891185e18af18790b6b61646d69747465645f74731903e8697369676e61747572659840186218e7183c18ec182c186618e21886181e188018d01889183818e3185c18321829188b186918ee18b6183718b31893188718eb1858150718331853186318bf18690b18901871188b18c70a1884184e18a5184f18d118f31400183e18b918ab18c21895183618fe18c01836181b0318e71847186d182e07696e69636b6e616d657380";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    let parsed: RegistryDelta = from_cbor(&bytes).unwrap();
    assert_eq!(parsed, delta);
}
