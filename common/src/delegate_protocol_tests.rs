//! Wire-format locks for the UI <-> identity-delegate protocol. The web side
//! builds these CBOR bytes with its own encoder, so the encoding is pinned to
//! hard-coded hex — a refactor that shifts the bytes must fail here, not in
//! the browser.

use crate::delegate_protocol::{IdentityRequest, IdentityResponse};
use crate::registry::AdmissionProof;

#[test]
fn get_identity_request_wire_format_is_stable() {
    assert_eq!(
        hex::encode(crate::to_cbor(&IdentityRequest::GetIdentity)),
        "6b4765744964656e74697479"
    );
}

#[test]
fn sign_placement_request_wire_format_is_stable() {
    let request = IdentityRequest::SignPlacement {
        tile_params: vec![1, 2, 3],
        coord: 258,
        color: 5,
        ts: 1_700_000_000,
    };
    assert_eq!(
        hex::encode(crate::to_cbor(&request)),
        "a16d5369676e506c6163656d656e74a46b74696c655f706172616d738301020365636f6f726419010265636f6c6f72056274731a6553f100"
    );
}

#[test]
fn sign_admission_request_wire_format_is_stable() {
    let request = IdentityRequest::SignAdmission {
        registry_params: vec![9],
        proof: AdmissionProof::Work { nonce: 7 },
        admitted_ts: 1_700_000_000,
        nickname: Some("ab".to_string()),
    };
    assert_eq!(
        hex::encode(crate::to_cbor(&request)),
        "a16d5369676e41646d697373696f6ea46f72656769737472795f706172616d7381096570726f6f66a164576f726ba1656e6f6e6365076b61646d69747465645f74731a6553f100686e69636b6e616d65626162"
    );
}

#[test]
fn identity_response_wire_format_is_stable() {
    let response = IdentityResponse::Identity {
        verifying_key: [4u8; 32],
    };
    assert_eq!(
        hex::encode(crate::to_cbor(&response)),
        "a1684964656e74697479a16d766572696679696e675f6b657998200404040404040404040404040404040404040404040404040404040404040404"
    );
}

#[test]
fn protocol_roundtrips_through_cbor() {
    let request = IdentityRequest::SignChatMessage {
        chat_params: vec![1],
        content: "hi".to_string(),
        ts: 12,
        seq: 3,
    };
    let decoded: IdentityRequest = crate::from_cbor(&crate::to_cbor(&request)).unwrap();
    assert_eq!(decoded, request);
    let response = IdentityResponse::Error {
        message: "nope".to_string(),
    };
    let decoded: IdentityResponse = crate::from_cbor(&crate::to_cbor(&response)).unwrap();
    assert_eq!(decoded, response);
}

#[test]
fn legacy_delegate_lineage_matches_the_registry() {
    // The build-time codegen re-derives delegate_key = blake3(code_hash || params)
    // and fails the build on mismatch; this pins the emitted contents so the
    // registry file cannot silently lose or reorder entries.
    let lineage = crate::legacy::LEGACY_IDENTITY_DELEGATES;
    assert_eq!(lineage.len(), 1);
    let (delegate_key, code_hash) = lineage[0];
    assert_eq!(
        hex::encode(code_hash),
        "c8e480ce9e7d4669111c4b167beaa53356f3217e531c87103fa3180453759aa2"
    );
    assert_eq!(
        hex::encode(delegate_key),
        "b47f8ab11940e326dbd8ba14e7f28c05f213d92d31d21f5649c01c432215d504"
    );
}
