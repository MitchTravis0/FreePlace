//! Phase 4 chat-core battery: merge monoid properties (including cap
//! eviction), rate-limit filter determinism, delta/summary behavior,
//! per-field tamper tests, and hard-coded wire-format locks.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, SigningKey};
use proptest::prelude::*;

use crate::chat::{
    message_signing_bytes, serialize_chat_delta, ChatParameters, ChatState, ChatSummary,
    SignedMessage,
};
use crate::constants::{CHAT_MAX_MESSAGES_PER_AUTHOR, CHAT_MESSAGE_CAP, MAX_CHAT_MESSAGE_BYTES};
use crate::identity::AuthorId;
use crate::{from_cbor, to_cbor};

/// Small caps for property tests, so both evictions (global ring buffer and
/// per-author) are exercised without signing hundreds of messages per case.
/// The eviction math is cap-generic.
const TEST_CAP: usize = 4;
const TEST_AUTHOR_CAP: usize = 2;

fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn params() -> ChatParameters {
    ChatParameters {
        canvas_id: [7; 32],
        registry: [9; 32],
    }
}

/// (author index 0..3, ts, seq, content index)
type Spec = (usize, u64, u32, usize);

fn message(spec: Spec) -> SignedMessage {
    let (idx, ts, seq, content) = spec;
    SignedMessage::sign(
        &key(idx as u8 + 1),
        &params(),
        &format!("m{content}"),
        ts,
        seq,
    )
}

fn state_from_bounded(specs: &[Spec], cap: usize, author_cap: usize) -> ChatState {
    let mut state = ChatState::default();
    for spec in specs {
        state.insert_bounded(message(*spec), cap, author_cap);
    }
    state
}

fn state_from(specs: &[Spec]) -> ChatState {
    let mut state = ChatState::default();
    for spec in specs {
        state.insert(message(*spec));
    }
    state
}

fn merged_bounded(a: &ChatState, b: &ChatState, cap: usize, author_cap: usize) -> ChatState {
    let mut out = a.clone();
    out.merge_bounded(b, cap, author_cap);
    out
}

fn arb_specs() -> impl Strategy<Value = Vec<Spec>> {
    prop::collection::vec((0..3usize, 0..48u64, 0..3u32, 0..8usize), 0..12)
}

proptest! {
    #[test]
    fn merge_is_commutative(a in arb_specs(), b in arb_specs()) {
        let (sa, sb) = (
            state_from_bounded(&a, TEST_CAP, TEST_AUTHOR_CAP),
            state_from_bounded(&b, TEST_CAP, TEST_AUTHOR_CAP),
        );
        prop_assert_eq!(
            merged_bounded(&sa, &sb, TEST_CAP, TEST_AUTHOR_CAP),
            merged_bounded(&sb, &sa, TEST_CAP, TEST_AUTHOR_CAP)
        );
    }

    #[test]
    fn merge_is_associative(a in arb_specs(), b in arb_specs(), c in arb_specs()) {
        let (sa, sb, sc) = (
            state_from_bounded(&a, TEST_CAP, TEST_AUTHOR_CAP),
            state_from_bounded(&b, TEST_CAP, TEST_AUTHOR_CAP),
            state_from_bounded(&c, TEST_CAP, TEST_AUTHOR_CAP),
        );
        prop_assert_eq!(
            merged_bounded(
                &merged_bounded(&sa, &sb, TEST_CAP, TEST_AUTHOR_CAP),
                &sc, TEST_CAP, TEST_AUTHOR_CAP
            ),
            merged_bounded(
                &sa,
                &merged_bounded(&sb, &sc, TEST_CAP, TEST_AUTHOR_CAP),
                TEST_CAP, TEST_AUTHOR_CAP
            )
        );
    }

    #[test]
    fn merge_identity(a in arb_specs()) {
        let sa = state_from_bounded(&a, TEST_CAP, TEST_AUTHOR_CAP);
        let empty = ChatState::default();
        prop_assert_eq!(merged_bounded(&sa, &empty, TEST_CAP, TEST_AUTHOR_CAP), sa.clone());
        prop_assert_eq!(merged_bounded(&empty, &sa, TEST_CAP, TEST_AUTHOR_CAP), sa);
    }

    /// Two peers that see the same messages in any arrival order end up with
    /// byte-identical states (eviction included) and identical rate-filtered
    /// views.
    #[test]
    fn eviction_and_rate_filter_are_arrival_order_independent(
        (original, shuffled) in arb_specs().prop_flat_map(|v| {
            let orig = v.clone();
            (Just(orig), Just(v).prop_shuffle())
        })
    ) {
        let peer_a = state_from_bounded(&original, TEST_CAP, TEST_AUTHOR_CAP);
        let peer_b = state_from_bounded(&shuffled, TEST_CAP, TEST_AUTHOR_CAP);
        prop_assert_eq!(&peer_a, &peer_b);
        prop_assert_eq!(peer_a.valid_messages(), peer_b.valid_messages());
    }

    /// A converged peer (summary == our summary) always gets a zero-byte delta.
    #[test]
    fn delta_to_self_is_always_none(a in arb_specs()) {
        let sa = state_from(&a);
        let delta = sa.delta(&sa.summarize());
        prop_assert!(delta.is_none());
        prop_assert_eq!(serialize_chat_delta(&delta).len(), 0);
    }

    /// Summary + delta reconciliation: a peer holding A reaches B exactly,
    /// when B extends A with strictly newer messages.
    #[test]
    fn delta_summary_roundtrip(
        base in arb_specs(),
        extra in prop::collection::vec((0..3usize, 0..8u64, 0..3u32, 0..8usize), 0..6)
    ) {
        let state_a = state_from(&base);
        let newest = base.iter().map(|s| s.1).max().unwrap_or(0);
        let mut state_b = state_a.clone();
        for (idx, ts_offset, seq, content) in &extra {
            state_b.insert(message((*idx, newest + 1 + ts_offset, *seq, *content)));
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

// --- Cap eviction ----------------------------------------------------------

#[test]
fn ring_buffer_caps_at_newest_n_with_real_cap() {
    // 16 authors each posting exactly the per-author cap: 512 candidates for
    // the 500-slot buffer, so eviction here is purely the global ring buffer.
    let count = 16 * CHAT_MAX_MESSAGES_PER_AUTHOR as u64;
    let specs: Vec<Spec> = (0..count)
        .map(|ts| ((ts % 16) as usize, ts, 0, (ts % 8) as usize))
        .collect();
    let state = state_from(&specs);
    assert_eq!(state.messages.len(), CHAT_MESSAGE_CAP);
    // Newest kept: the oldest (count - cap) timestamps are evicted.
    assert_eq!(
        state.messages.keys().next().unwrap().ts,
        count - CHAT_MESSAGE_CAP as u64
    );
    assert_eq!(state.messages.keys().next_back().unwrap().ts, count - 1);
}

#[test]
fn flooder_cannot_evict_others_history() {
    // Author index 1 posted two old messages; author index 0 then floods
    // hundreds. The flooder's stored share caps at CHAT_MAX_MESSAGES_PER_AUTHOR
    // and the victim's history survives.
    let mut state = state_from(&[(1, 5, 0, 0), (1, 6, 0, 1)]);
    for ts in 100..400u64 {
        state.insert(message((0, ts, 0, (ts % 8) as usize)));
    }
    let flooder = AuthorId::from(&key(1).verifying_key());
    let victim = AuthorId::from(&key(2).verifying_key());
    let count_of = |author: AuthorId| {
        state
            .messages
            .keys()
            .filter(|id| id.author == author)
            .count()
    };
    assert_eq!(count_of(flooder), CHAT_MAX_MESSAGES_PER_AUTHOR);
    assert_eq!(count_of(victim), 2);
}

#[test]
fn eviction_order_is_by_ts_then_author_then_seq() {
    // Four messages at cap 3: the smallest id (ts, author, seq) is evicted.
    let a1 = message((0, 100, 0, 0));
    let a2 = message((0, 100, 1, 1));
    let b1 = message((1, 100, 0, 2));
    let c1 = message((2, 200, 0, 3));
    let smaller_author = if AuthorId::from(&a1.author) < AuthorId::from(&b1.author) {
        a1.id()
    } else {
        b1.id()
    };
    let mut state = ChatState::default();
    for m in [&a1, &a2, &b1, &c1] {
        state.insert_bounded(m.clone(), 3, 3);
    }
    assert_eq!(state.messages.len(), 3);
    assert!(!state.messages.contains_key(&smaller_author));
}

// --- Rate-limit filter -----------------------------------------------------

#[test]
fn rate_limit_boundary_is_inclusive() {
    // 3s interval: exactly 3s later is accepted, 2s is not.
    let state = state_from(&[(0, 100, 0, 0), (0, 103, 0, 1)]);
    assert_eq!(state.valid_messages().len(), 2);
    let state = state_from(&[(0, 100, 0, 0), (0, 102, 0, 1)]);
    assert_eq!(state.valid_messages().len(), 1);
}

#[test]
fn same_second_burst_keeps_only_the_first() {
    let state = state_from(&[(0, 100, 0, 0), (0, 100, 1, 1), (0, 100, 2, 2)]);
    let valid = state.valid_messages();
    assert_eq!(valid.len(), 1);
    assert_eq!(valid[0].seq, 0);
}

#[test]
fn rate_limit_is_per_author() {
    // Two authors posting in the same second are both accepted.
    let state = state_from(&[(0, 100, 0, 0), (1, 100, 0, 1), (0, 101, 0, 2)]);
    assert_eq!(state.valid_messages().len(), 2);
}

#[test]
fn rate_filter_converges_to_earliest_in_any_order() {
    // Two messages 1s apart: whatever order peers see them in, only the
    // earliest is accepted on both.
    let first = message((0, 100, 0, 0));
    let second = message((0, 101, 0, 1));
    for order in [[&first, &second], [&second, &first]] {
        let mut state = ChatState::default();
        for m in order {
            state.insert(m.clone());
        }
        let valid = state.valid_messages();
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0].ts, 100);
    }
}

// --- Delta/summary sizes ---------------------------------------------------

/// Delta to a fully-converged peer must be zero bytes, and the summary must be
/// a small fraction of the state (contract-patterns.md size test).
#[test]
fn delta_to_converged_peer_is_zero_bytes_against_populated_state() {
    let mut state = ChatState::default();
    for author in 0..3usize {
        for i in 0..20u64 {
            state.insert(message((author, i * 10, 0, (i % 8) as usize)));
        }
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

    let delta_bytes = serialize_chat_delta(&state.delta(&summary));
    assert_eq!(
        delta_bytes.len(),
        0,
        "delta to a converged peer was {} bytes against a {} byte state",
        delta_bytes.len(),
        state_bytes.len()
    );
}

#[test]
fn delta_ships_all_messages_for_unknown_author() {
    let state = state_from(&[(0, 10, 0, 0), (0, 200, 0, 1)]);
    let delta = state.delta(&ChatSummary(BTreeMap::new())).unwrap();
    assert_eq!(delta.messages.len(), 2);
}

#[test]
fn delta_ships_same_second_higher_seq_messages() {
    // The watermark is (ts, seq), so a same-ts message with a higher seq is
    // still shipped to a peer whose watermark is the lower seq.
    let older = state_from(&[(0, 100, 0, 0)]);
    let newer = state_from(&[(0, 100, 0, 0), (0, 100, 1, 1)]);
    let delta = newer.delta(&older.summarize()).unwrap();
    assert_eq!(delta.messages.len(), 1);
    assert_eq!(delta.messages[0].seq, 1);
}

// --- Signature tamper tests: one per signed field -------------------------

fn assert_tampered_fails(mutate: impl FnOnce(&mut SignedMessage)) {
    let mut m = message((0, 1000, 2, 3));
    m.verify(&params()).expect("untampered message verifies");
    mutate(&mut m);
    assert!(m.verify(&params()).is_err(), "tampered message must fail");
}

#[test]
fn tamper_content_fails() {
    assert_tampered_fails(|m| m.content.push('x'));
}

#[test]
fn tamper_ts_fails() {
    assert_tampered_fails(|m| m.ts += 1);
}

#[test]
fn tamper_seq_fails() {
    assert_tampered_fails(|m| m.seq += 1);
}

#[test]
fn tamper_author_fails() {
    assert_tampered_fails(|m| m.author = key(9).verifying_key());
}

#[test]
fn tamper_signature_fails() {
    assert_tampered_fails(|m| {
        let mut bytes = m.signature.to_bytes();
        bytes[0] ^= 0x01;
        m.signature = Signature::from_bytes(&bytes);
    });
}

#[test]
fn message_bound_to_canvas_id() {
    let m = message((0, 1000, 0, 0));
    let mut other = params();
    other.canvas_id = [8; 32];
    assert!(m.verify(&other).is_err());
}

#[test]
fn message_bound_to_registry() {
    let m = message((0, 1000, 0, 0));
    let mut other = params();
    other.registry = [10; 32];
    assert!(m.verify(&other).is_err());
}

#[test]
fn empty_content_fails_even_correctly_signed() {
    let m = SignedMessage::sign(&key(1), &params(), "", 1000, 0);
    assert!(m.verify(&params()).is_err());
}

#[test]
fn oversized_content_fails_even_correctly_signed() {
    let long = "x".repeat(MAX_CHAT_MESSAGE_BYTES + 1);
    let m = SignedMessage::sign(&key(1), &params(), &long, 1000, 0);
    assert!(m.verify(&params()).is_err());
    let max = "x".repeat(MAX_CHAT_MESSAGE_BYTES);
    let m = SignedMessage::sign(&key(1), &params(), &max, 1000, 0);
    assert!(m.verify(&params()).is_ok());
}

// --- Wire-format locks: hard-coded hex, not just roundtrip ----------------

fn canonical_state() -> ChatState {
    state_from(&[(0, 10, 0, 1), (0, 20, 1, 2), (1, 30, 0, 3)])
}

#[test]
fn signing_preimage_format_locked() {
    let bytes = message_signing_bytes(&params(), &key(1).verifying_key(), 1000, 2, "hi");
    const EXPECTED_HEX: &str = "66726565706c6163653a636861743a6d6573736167653a7631070707070707070707070707070707070707070707070707070707070707070709090909090909090909090909090909090909090909090909090909090909098a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5ce80300000000000002000000020000006869";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
}

#[test]
fn chat_parameters_wire_format_locked() {
    let bytes = to_cbor(&params());
    const EXPECTED_HEX: &str = "a26963616e7661735f69649820070707070707070707070707070707070707070707070707070707070707070768726567697374727998200909090909090909090909090909090909090909090909090909090909090909";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    let parsed: ChatParameters = from_cbor(&bytes).unwrap();
    assert_eq!(parsed, params());
}

#[test]
fn chat_state_wire_format_locked() {
    let state = canonical_state();
    let bytes = to_cbor(&state);
    const EXPECTED_HEX: &str = "a1686d65737361676573a3a36274730a66617574686f729820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185c6373657100a567636f6e74656e74626d316274730a637365710066617574686f7258208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c697369676e617475726598401848182d187d1876188218d618811832188518410418e118e6183418b90a186f091837184c181d187418971846181f1888183318e6188418a618c4183c188518ca183c186a189b185b18cf182d1878187f183b188518771882187518eb18bf1824185c189418af05188b18e718180618ab184618a50d18a70ca36274731466617574686f729820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185c6373657101a567636f6e74656e74626d3262747314637365710166617574686f7258208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c697369676e617475726598400f188618d4182a187e18181876185f18cc18af18471835181e18b318ef187a18dc18ec18ca18561860186f18ce187e18d6189018b70f18ef18a50918b40f18cc18ba18ca18c3188f18f907188e184f03185a18691847189413184f187b18c218ba18bd18d418c118c70618631866181f18ba15183104a3627473181e66617574686f7298201881183918770e18a8187d17185f185618a31854186618c3184c187e18cc18cb188d188a189118b418ee183718a2185d18f60f185b188f18c918b318946373657100a567636f6e74656e74626d33627473181e637365710066617574686f7258208139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394697369676e61747572659840188418ea18a2188c184e0318e5183f18f618ce183f18781887181a1851188a189b18c11853121839182f189018ce18bf18eb18c118401872186d188a183618a31866131857184b18a8188a18a918bc183518611875183018bc18ec183b18fc18c11872090f18a418b21831181818c418e718c4182c184d184703";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    let parsed: ChatState = from_cbor(&bytes).unwrap();
    assert_eq!(parsed, state);
}

#[test]
fn chat_summary_wire_format_locked() {
    let bytes = to_cbor(&canonical_state().summarize());
    const EXPECTED_HEX: &str = "a298201881183918770e18a8187d17185f185618a31854186618c3184c187e18cc18cb188d188a189118b418ee183718a2185d18f60f185b188f18c918b3189482181e009820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185c821401";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
}
