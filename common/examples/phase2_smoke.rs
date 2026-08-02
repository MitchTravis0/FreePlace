//! Phase 2 local-node smoke helper.
//!
//! `gen <outdir>` writes the registry publish/update fixtures (a fresh random
//! canvas_id per run, so re-runs publish a fresh contract instance).
//! `check <statefile> <expected-nickname> <expected-version>` decodes state
//! fetched from the node and asserts exactly the valid admission is present.

use std::env;
use std::fs;
use std::path::Path;
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use common::identity::{AuthorId, Tier};
use common::registry::{
    find_pow_nonce, pow_nonce_meets_target, AdmissionProof, AdmissionRecord, NicknameUpdate,
    RegistryDelta, RegistryParameters, RegistryState, SignedNickname,
};
use common::to_cbor;
use ed25519_dalek::SigningKey;

fn valid_identity() -> SigningKey {
    SigningKey::from_bytes(&[11; 32])
}

fn bad_nonce_identity() -> SigningKey {
    SigningKey::from_bytes(&[22; 32])
}

fn tampered_identity() -> SigningKey {
    SigningKey::from_bytes(&[33; 32])
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

fn admissions_delta(records: Vec<AdmissionRecord>) -> RegistryDelta {
    RegistryDelta {
        admissions: records,
        nicknames: vec![],
    }
}

fn nickname_delta(
    key: &SigningKey,
    params: &RegistryParameters,
    name: &str,
    version: u64,
) -> RegistryDelta {
    RegistryDelta {
        admissions: vec![],
        nicknames: vec![NicknameUpdate {
            identity_vk: key.verifying_key(),
            nickname: SignedNickname::sign(key, params, name, version),
        }],
    }
}

fn gen(outdir: &Path) {
    fs::create_dir_all(outdir).expect("create output dir");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let canvas_id = *blake3::hash(&nanos.to_le_bytes()).as_bytes();
    let params = RegistryParameters { canvas_id };
    let ts = now_ts();

    let write = |name: &str, bytes: Vec<u8>| {
        fs::write(outdir.join(name), bytes).expect("write fixture");
    };

    write("params.bin", to_cbor(&params));
    write("state.bin", to_cbor(&RegistryState::default()));

    let valid = valid_identity();
    let nonce = find_pow_nonce(&params, &valid.verifying_key());
    let valid_record = AdmissionRecord::sign(
        &valid,
        &params,
        AdmissionProof::Work { nonce },
        Some(SignedNickname::sign(&valid, &params, "smoke", 1)),
        ts,
    );
    write(
        "delta-valid.bin",
        to_cbor(&admissions_delta(vec![valid_record])),
    );
    write(
        "delta-nick.bin",
        to_cbor(&nickname_delta(&valid, &params, "renamed", 2)),
    );
    write(
        "delta-nick-replay.bin",
        to_cbor(&nickname_delta(&valid, &params, "smoke", 1)),
    );

    let bad = bad_nonce_identity();
    let bad_nonce = (0u64..)
        .find(|n| !pow_nonce_meets_target(&params, &bad.verifying_key(), *n))
        .unwrap();
    let bad_record = AdmissionRecord::sign(
        &bad,
        &params,
        AdmissionProof::Work { nonce: bad_nonce },
        None,
        ts,
    );
    write(
        "delta-bad-nonce.bin",
        to_cbor(&admissions_delta(vec![bad_record])),
    );

    let tampered_key = tampered_identity();
    let tampered_nonce = find_pow_nonce(&params, &tampered_key.verifying_key());
    let mut tampered = AdmissionRecord::sign(
        &tampered_key,
        &params,
        AdmissionProof::Work {
            nonce: tampered_nonce,
        },
        None,
        ts,
    );
    tampered.admitted_ts += 1;
    write(
        "delta-tampered.bin",
        to_cbor(&admissions_delta(vec![tampered])),
    );

    println!("fixtures written to {}", outdir.display());
}

fn check(statefile: &Path, expected_name: &str, expected_version: u64) {
    let bytes = fs::read(statefile).expect("read state file");
    let state: RegistryState =
        common::from_cbor(&bytes).expect("fetched state decodes as RegistryState");

    let author = AuthorId::from(&valid_identity().verifying_key());
    assert_eq!(
        state.identities.len(),
        1,
        "expected exactly the valid admission, found {} identities",
        state.identities.len()
    );
    let record = state
        .identities
        .get(&author)
        .expect("the valid identity is admitted");
    assert_eq!(record.tier(), Tier::Pow, "tier must be Pow");
    let nickname = record.nickname.as_ref().expect("nickname present");
    assert_eq!(nickname.name, expected_name);
    assert_eq!(nickname.version, expected_version);
    println!(
        "check ok: 1 identity, tier Pow, nickname {:?} v{}",
        nickname.name, nickname.version
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen") if args.len() == 3 => gen(Path::new(&args[2])),
        Some("check") if args.len() == 5 => check(
            Path::new(&args[2]),
            &args[3],
            args[4].parse().expect("version is a number"),
        ),
        _ => {
            eprintln!("usage: phase2_smoke gen <outdir> | check <statefile> <name> <version>");
            exit(2);
        }
    }
}
