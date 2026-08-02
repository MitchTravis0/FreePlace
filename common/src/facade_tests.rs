//! Phase 7 test battery for the facade pointer core: per-field tamper tests,
//! frame parsing, deterministic merge ordering, and wire-format locks.

use ed25519_dalek::SigningKey;

use crate::facade::{
    decode_facade_state, encode_facade_frame, facade_signing_bytes, parse_facade_frame,
    FacadeMetadata, FacadeParameters, FacadePointer,
};
use crate::to_cbor;

fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn params() -> FacadeParameters {
    FacadeParameters {
        owner_vk: key(1).verifying_key(),
        instance: 0,
    }
}

fn pointer() -> FacadePointer {
    FacadePointer {
        version: 1_700_000_000,
        current_app: Some([0xAB; 32]),
        prev_apps: vec![[0xCD; 32]],
        web_hash: *blake3::hash(b"loader archive").as_bytes(),
    }
}

fn signed_state(web: &[u8]) -> Vec<u8> {
    let mut pointer = pointer();
    pointer.web_hash = *blake3::hash(web).as_bytes();
    let meta = FacadeMetadata::sign(&key(1), &params(), pointer);
    encode_facade_frame(&to_cbor(&meta), web)
}

#[test]
fn valid_signed_state_decodes() {
    let meta = decode_facade_state(&params(), &signed_state(b"web bytes")).unwrap();
    assert_eq!(meta.pointer.current_app, Some([0xAB; 32]));
}

#[test]
fn tampering_any_signed_field_fails_verification() {
    let web = b"web bytes";
    let base = pointer();
    let tampered: Vec<(&str, FacadePointer)> = vec![
        (
            "version",
            FacadePointer {
                version: base.version + 1,
                ..base.clone()
            },
        ),
        (
            "current_app value",
            FacadePointer {
                current_app: Some([0xAC; 32]),
                ..base.clone()
            },
        ),
        (
            "current_app presence",
            FacadePointer {
                current_app: None,
                ..base.clone()
            },
        ),
        (
            "prev_apps",
            FacadePointer {
                prev_apps: vec![[0xCE; 32]],
                ..base.clone()
            },
        ),
        (
            "web_hash",
            FacadePointer {
                web_hash: [9; 32],
                ..base.clone()
            },
        ),
    ];
    for (field, pointer) in tampered {
        let mut meta = FacadeMetadata::sign(&key(1), &params(), base.clone());
        meta.pointer = pointer;
        assert!(
            meta.verify(&params(), web).is_err(),
            "tampered {field} must fail"
        );
    }
}

#[test]
fn signature_binds_the_parameters() {
    let web = b"web bytes";
    let mut pointer = pointer();
    pointer.web_hash = *blake3::hash(web).as_bytes();
    let meta = FacadeMetadata::sign(&key(1), &params(), pointer);
    let other_owner = FacadeParameters {
        owner_vk: key(2).verifying_key(),
        instance: 0,
    };
    let other_instance = FacadeParameters {
        owner_vk: key(1).verifying_key(),
        instance: 1,
    };
    assert!(meta.verify(&other_owner, web).is_err());
    assert!(meta.verify(&other_instance, web).is_err());
    assert!(meta.verify(&params(), web).is_ok());
}

#[test]
fn tampering_the_web_slot_fails_verification() {
    let state = signed_state(b"web bytes");
    let (meta, _) = parse_facade_frame(&state).unwrap();
    let meta: FacadeMetadata = crate::from_cbor(meta).unwrap();
    assert!(meta.verify(&params(), b"other web bytes").is_err());
}

#[test]
fn prev_apps_over_the_cap_is_rejected() {
    let web = b"web bytes";
    let mut pointer = pointer();
    pointer.web_hash = *blake3::hash(web).as_bytes();
    pointer.prev_apps = vec![[1; 32]; crate::constants::FACADE_MAX_PREV_APPS + 1];
    let meta = FacadeMetadata::sign(&key(1), &params(), pointer);
    assert!(meta.verify(&params(), web).is_err());
}

#[test]
fn frame_roundtrip_and_malformed_frames() {
    let frame = encode_facade_frame(b"meta", b"web");
    let (meta, web) = parse_facade_frame(&frame).unwrap();
    assert_eq!((meta, web), (b"meta".as_slice(), b"web".as_slice()));

    assert!(parse_facade_frame(&frame[..frame.len() - 1]).is_err());
    let mut trailing = frame.clone();
    trailing.push(0);
    assert!(parse_facade_frame(&trailing).is_err());
    assert!(parse_facade_frame(b"").is_err());
    // A meta length pointing past the end must not panic.
    let mut bad = frame;
    bad[0..8].copy_from_slice(&u64::MAX.to_be_bytes());
    assert!(parse_facade_frame(&bad).is_err());
}

#[test]
fn order_key_is_deterministic_and_commutative() {
    let newer = FacadeMetadata::sign(
        &key(1),
        &params(),
        FacadePointer {
            version: 2,
            ..pointer()
        },
    );
    let older = FacadeMetadata::sign(
        &key(1),
        &params(),
        FacadePointer {
            version: 1,
            ..pointer()
        },
    );
    let pick = |a: &FacadeMetadata, b: &FacadeMetadata| {
        if b.order_key() > a.order_key() {
            b.clone()
        } else {
            a.clone()
        }
    };
    assert_eq!(pick(&newer, &older), newer);
    assert_eq!(pick(&older, &newer), newer);

    // Same version, different signatures (different ring): still one winner
    // regardless of merge order.
    let a = FacadeMetadata::sign(&key(1), &params(), pointer());
    let b = FacadeMetadata::sign(
        &key(1),
        &params(),
        FacadePointer {
            prev_apps: vec![],
            ..pointer()
        },
    );
    assert_eq!(pick(&a, &b), pick(&b, &a));
}

#[test]
fn facade_signing_bytes_wire_format_locked() {
    let bytes = facade_signing_bytes(&params(), &pointer());
    assert_eq!(
        hex::encode(&bytes),
        "66726565706c6163653a6661636164653a706f696e7465723a76318a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c000000000000000000f1536500000000\
         01abababababababababababababababababababababababababababababababab01000000cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\
         c0afc79e2180dc2b8e62cff53f000dcc15840f82006d7c737c560781e161a617"
            .replace(char::is_whitespace, "")
    );
}
