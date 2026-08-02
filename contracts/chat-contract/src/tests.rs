//! Phase 4 exit-check battery at the contract boundary: posts by admitted
//! identities are accepted, tampered and malformed states are rejected, the
//! related-contract registry check gates authors, the delta to a converged
//! peer is zero bytes, and eviction beyond the ring buffer cap is
//! byte-identical on peers that saw the messages in different orders.

use std::collections::HashMap;

use common::chat::{ChatDelta, ChatParameters, ChatState, ChatSummary, SignedMessage};
use common::constants::{CHAT_MAX_MESSAGES_PER_AUTHOR, CHAT_MESSAGE_CAP, MAX_FUTURE_SKEW_SECS};
use common::identity::AuthorId;
use common::registry::{AdmissionProof, AdmissionRecord, RegistryParameters, RegistryState};
use ed25519_dalek::SigningKey;
use freenet_stdlib::prelude::*;

use crate::{update_state_impl, validate_state_impl, Contract};

const NOW: u64 = 1_000_000;

fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn chat_params() -> ChatParameters {
    ChatParameters {
        canvas_id: [7; 32],
        registry: [42; 32],
    }
}

fn params_bytes() -> Parameters<'static> {
    Parameters::from(common::to_cbor(&chat_params()))
}

fn registry_id() -> ContractInstanceId {
    ContractInstanceId::new(chat_params().registry)
}

/// Registry with key(1)..key(20) admitted (the flood fixtures spread the
/// ring-buffer overflow across many authors, since one author's stored share
/// caps at CHAT_MAX_MESSAGES_PER_AUTHOR). The chat only checks membership,
/// so the proofs need not verify here.
fn registry_state() -> RegistryState {
    let reg_params = RegistryParameters { canvas_id: [7; 32] };
    let mut state = RegistryState::default();
    for n in 1..=20 {
        state.insert_record(AdmissionRecord::sign(
            &key(n),
            &reg_params,
            AdmissionProof::Work { nonce: 0 },
            None,
            100,
        ));
    }
    state
}

fn related_with(state: Option<State<'static>>) -> RelatedContracts<'static> {
    let mut map = HashMap::new();
    map.insert(registry_id(), state);
    RelatedContracts::from(map)
}

fn related_registry() -> RelatedContracts<'static> {
    related_with(Some(State::from(common::to_cbor(&registry_state()))))
}

fn message(author: u8, content: &str, ts: u64, seq: u32) -> SignedMessage {
    SignedMessage::sign(&key(author), &chat_params(), content, ts, seq)
}

fn delta_update(messages: Vec<SignedMessage>) -> UpdateData<'static> {
    UpdateData::Delta(StateDelta::from(common::to_cbor(&ChatDelta { messages })))
}

fn genesis() -> State<'static> {
    State::from(common::to_cbor(&ChatState::default()))
}

/// Apply each update through the contract in sequence, threading the returned
/// state bytes, exactly as the host does.
fn run_updates(
    mut state: State<'static>,
    updates: Vec<UpdateData<'static>>,
) -> Result<Vec<u8>, ContractError> {
    for update in updates {
        let modification = update_state_impl(params_bytes(), state, vec![update], NOW)?;
        state = modification.new_state.expect("update returns a state");
    }
    Ok(state.as_ref().to_vec())
}

fn validate(state_bytes: &[u8], related: RelatedContracts<'static>) -> ValidateResult {
    validate_state_impl(
        params_bytes(),
        State::from(state_bytes.to_vec()),
        related,
        NOW,
    )
    .unwrap()
}

fn state_bytes_of(state: &ChatState) -> Vec<u8> {
    common::to_cbor(state)
}

// --- Validation: genesis, two-pass related check, membership ---------------

#[test]
fn genesis_state_validates_without_registry() {
    assert!(matches!(
        validate(&[], RelatedContracts::default()),
        ValidateResult::Valid
    ));
    assert!(matches!(
        validate(
            &state_bytes_of(&ChatState::default()),
            RelatedContracts::default()
        ),
        ValidateResult::Valid
    ));
}

#[test]
fn populated_state_requests_registry_then_validates() {
    let mut state = ChatState::default();
    state.insert(message(1, "hello", 100, 0));
    let bytes = state_bytes_of(&state);

    // First pass: no related state supplied yet.
    let first = validate(&bytes, RelatedContracts::default());
    let ValidateResult::RequestRelated(requested) = first else {
        panic!("expected RequestRelated, got {first:?}");
    };
    assert_eq!(requested, vec![registry_id()]);

    // Second pass: registry supplied, author is admitted.
    assert!(matches!(
        validate(&bytes, related_registry()),
        ValidateResult::Valid
    ));
}

#[test]
fn unadmitted_author_is_invalid() {
    let mut state = ChatState::default();
    state.insert(message(99, "intruder", 100, 0));
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn missing_registry_state_falls_back_to_signature_only() {
    let mut state = ChatState::default();
    state.insert(message(99, "intruder", 100, 0));
    assert!(matches!(
        validate(&state_bytes_of(&state), related_with(None)),
        ValidateResult::Valid
    ));
}

// --- Validation: structure and tamper rejection ----------------------------

#[test]
fn tampered_message_is_invalid() {
    let mut state = ChatState::default();
    let mut m = message(1, "hello", 100, 0);
    m.content.push('!');
    state.messages.insert(m.id(), m);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn mismatched_id_key_is_invalid() {
    let m = message(1, "hello", 100, 0);
    let mut id = m.id();
    id.ts += 1;
    let mut state = ChatState::default();
    state.messages.insert(id, m);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));

    let m = message(1, "hello", 100, 0);
    let mut id = m.id();
    id.author = AuthorId::from(&key(2).verifying_key());
    let mut state = ChatState::default();
    state.messages.insert(id, m);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn over_cap_state_is_invalid() {
    let mut state = ChatState::default();
    for i in 0..=CHAT_MESSAGE_CAP as u64 {
        let m = message(1, "flood", 100 + i, 0);
        state.messages.insert(m.id(), m);
    }
    assert!(state.messages.len() > CHAT_MESSAGE_CAP);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn per_author_flood_state_is_invalid() {
    // One author holding more than the per-author cap (but under the global
    // cap) is a structural violation even when every message verifies.
    let mut state = ChatState::default();
    for i in 0..=CHAT_MAX_MESSAGES_PER_AUTHOR as u64 {
        let m = message(1, "flood", 100 + i, 0);
        state.messages.insert(m.id(), m);
    }
    assert!(state.messages.len() <= CHAT_MESSAGE_CAP);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn flood_updates_cannot_evict_other_authors() {
    // key(2) posts twice, then key(1) floods far past the per-author cap via
    // deltas: the flooder's stored share caps out and key(2)'s history stays.
    let mut updates = vec![
        delta_update(vec![message(2, "early one", 5, 0)]),
        delta_update(vec![message(2, "early two", 6, 0)]),
    ];
    for i in 0..(3 * CHAT_MAX_MESSAGES_PER_AUTHOR as u64) {
        updates.push(delta_update(vec![message(1, "spam", 100 + i, 0)]));
    }
    let bytes = run_updates(genesis(), updates).unwrap();
    let state: ChatState = common::from_cbor(&bytes).unwrap();
    let count_of = |n: u8| {
        let author = AuthorId::from(&key(n).verifying_key());
        state
            .messages
            .keys()
            .filter(|id| id.author == author)
            .count()
    };
    assert_eq!(count_of(1), CHAT_MAX_MESSAGES_PER_AUTHOR);
    assert_eq!(count_of(2), 2);
    assert!(matches!(
        validate(&bytes, related_registry()),
        ValidateResult::Valid
    ));
}

#[test]
fn far_future_message_is_invalid_in_validate() {
    let mut state = ChatState::default();
    state.insert(message(
        1,
        "from the future",
        NOW + MAX_FUTURE_SKEW_SECS + 1,
        0,
    ));
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));

    let mut state = ChatState::default();
    state.insert(message(
        1,
        "at the skew edge",
        NOW + MAX_FUTURE_SKEW_SECS,
        0,
    ));
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Valid
    ));
}

// --- Updates: cheap checks at ingress --------------------------------------

#[test]
fn valid_post_is_accepted() {
    let m = message(1, "hello world", 100, 0);
    let bytes = run_updates(genesis(), vec![delta_update(vec![m.clone()])]).unwrap();
    let parsed: ChatState = common::from_cbor(&bytes).unwrap();
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages.values().next().unwrap(), &m);
}

#[test]
fn tampered_delta_is_rejected() {
    let mut m = message(1, "hello", 100, 0);
    m.ts += 1;
    let err = run_updates(genesis(), vec![delta_update(vec![m])]).unwrap_err();
    assert!(matches!(err, ContractError::InvalidUpdateWithInfo { .. }));
}

#[test]
fn oversized_content_delta_is_rejected() {
    let m = message(1, &"x".repeat(513), 100, 0);
    let err = run_updates(genesis(), vec![delta_update(vec![m])]).unwrap_err();
    assert!(matches!(err, ContractError::InvalidUpdateWithInfo { .. }));
}

#[test]
fn far_future_delta_is_rejected() {
    let m = message(1, "hello", NOW + MAX_FUTURE_SKEW_SECS + 1, 0);
    let err = run_updates(genesis(), vec![delta_update(vec![m])]).unwrap_err();
    let ContractError::InvalidUpdateWithInfo { reason } = err else {
        panic!("expected InvalidUpdateWithInfo, got {err:?}");
    };
    assert!(reason.contains("future"), "got: {reason}");
}

#[test]
fn full_state_merge_verifies_messages() {
    let mut incoming = ChatState::default();
    incoming.insert(message(1, "hello", 100, 0));
    let merged = run_updates(
        genesis(),
        vec![UpdateData::State(State::from(state_bytes_of(&incoming)))],
    )
    .unwrap();
    let parsed: ChatState = common::from_cbor(&merged).unwrap();
    assert_eq!(parsed, incoming);

    let mut tampered = ChatState::default();
    let mut m = message(1, "hello", 100, 0);
    m.seq += 1;
    tampered.messages.insert(m.id(), m);
    assert!(run_updates(
        genesis(),
        vec![UpdateData::State(State::from(state_bytes_of(&tampered)))],
    )
    .is_err());
}

// --- Phase 4 exit check: eviction identical on out-of-order peers ----------

/// More messages than the ring buffer holds, split across 20 admitted
/// authors (each staying under the per-author cap), with interleaved
/// timestamps.
fn flood_messages() -> Vec<SignedMessage> {
    (0..CHAT_MESSAGE_CAP as u64 + 10)
        .map(|i| message((i % 20) as u8 + 1, &format!("msg {i}"), 100 + i, 0))
        .collect()
}

#[test]
fn eviction_beyond_cap_is_identical_on_out_of_order_peers() {
    let messages = flood_messages();
    let forward: Vec<UpdateData> = messages
        .iter()
        .map(|m| delta_update(vec![m.clone()]))
        .collect();
    let reverse: Vec<UpdateData> = messages
        .iter()
        .rev()
        .map(|m| delta_update(vec![m.clone()]))
        .collect();

    let peer_a = run_updates(genesis(), forward).unwrap();
    let peer_b = run_updates(genesis(), reverse).unwrap();
    assert_eq!(peer_a, peer_b, "state bytes must be identical");

    let state: ChatState = common::from_cbor(&peer_a).unwrap();
    assert_eq!(state.messages.len(), CHAT_MESSAGE_CAP);
    // The oldest 10 messages are the ones evicted, on both peers.
    assert_eq!(state.messages.keys().next().unwrap().ts, 110);

    // Both converged states pass full validation against the registry.
    assert!(matches!(
        validate(&peer_a, related_registry()),
        ValidateResult::Valid
    ));
}

#[test]
fn state_merge_and_delta_paths_converge() {
    // Peer A ingests messages as deltas; peer B receives A's full state in
    // one merge. Both must hold identical bytes.
    let messages: Vec<SignedMessage> = (0..20u64)
        .map(|i| message(1, &format!("msg {i}"), 100 + i * 5, 0))
        .collect();
    let updates: Vec<UpdateData> = messages
        .iter()
        .map(|m| delta_update(vec![m.clone()]))
        .collect();
    let peer_a = run_updates(genesis(), updates).unwrap();
    let peer_b = run_updates(
        genesis(),
        vec![UpdateData::State(State::from(peer_a.clone()))],
    )
    .unwrap();
    assert_eq!(peer_a, peer_b);
}

// --- Phase 4 exit check: delta size against a populated chat ---------------

#[test]
fn delta_to_converged_peer_is_zero_bytes_against_populated_chat() {
    let mut updates = Vec::new();
    for author in [1u8, 2] {
        for i in 0..30u64 {
            updates.push(delta_update(vec![message(
                author,
                &format!("author {author} message {i}"),
                100 + i * 10,
                0,
            )]));
        }
    }
    let state_bytes = run_updates(genesis(), updates).unwrap();
    assert!(state_bytes.len() > 2000, "chat should be populated");
    let raw = State::from(state_bytes.clone());

    let summary = Contract::summarize_state(params_bytes(), raw.clone()).unwrap();
    assert!(
        summary.as_ref().len() < state_bytes.len() / 4,
        "summary ({} B) must be much smaller than state ({} B)",
        summary.as_ref().len(),
        state_bytes.len()
    );

    let delta = Contract::get_state_delta(params_bytes(), raw.clone(), summary).unwrap();
    assert_eq!(
        delta.as_ref().len(),
        0,
        "delta to a converged peer was {} bytes against a {} byte state",
        delta.as_ref().len(),
        state_bytes.len()
    );

    // A peer with an empty summary gets every message.
    let empty_summary = StateSummary::from(common::to_cbor(&ChatSummary::default()));
    let full = Contract::get_state_delta(params_bytes(), raw, empty_summary).unwrap();
    let parsed: ChatDelta = common::from_cbor(full.as_ref()).unwrap();
    assert_eq!(parsed.messages.len(), 60);
}
