//! Phase 3 exit-check battery at the contract boundary: two simulated peers
//! applying the same placements in different orders reach byte-identical
//! states and derived canvases, in-cooldown placements are dropped
//! deterministically on both, the delta to a converged peer is zero bytes,
//! and the related-contract registry check accepts members and rejects
//! unknown authors and tampered or malformed states.

use std::collections::{BTreeMap, HashMap};

use common::constants::{EMPTY_PIXEL, MAX_FUTURE_SKEW_SECS, MAX_PLACEMENTS_PER_AUTHOR};
use common::identity::{AuthorId, Tier};
use common::registry::{AdmissionProof, AdmissionRecord, RegistryParameters, RegistryState};
use common::tile::{SignedPlacement, TileDelta, TileParameters, TileState, TileSummary};
use ed25519_dalek::SigningKey;
use freenet_stdlib::prelude::*;

use crate::{update_state_impl, validate_state_impl, Contract};

const NOW: u64 = 1_000_000;

fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn tile_params() -> TileParameters {
    TileParameters {
        canvas_id: [7; 32],
        tile_x: 1,
        tile_y: 2,
        registry: [42; 32],
    }
}

fn params_bytes() -> Parameters<'static> {
    Parameters::from(common::to_cbor(&tile_params()))
}

fn registry_id() -> ContractInstanceId {
    ContractInstanceId::new(tile_params().registry)
}

/// Registry with key(1) admitted as Pow and key(2) as Ghostkey. The tile only
/// checks membership and reads tiers, so the proofs need not verify here.
fn registry_state() -> RegistryState {
    let reg_params = RegistryParameters { canvas_id: [7; 32] };
    let mut state = RegistryState::default();
    state.insert_record(AdmissionRecord::sign(
        &key(1),
        &reg_params,
        AdmissionProof::Work { nonce: 0 },
        None,
        100,
    ));
    state.insert_record(AdmissionRecord::sign(
        &key(2),
        &reg_params,
        AdmissionProof::Ghostkey {
            scoped_payload: vec![1],
            signature: vec![0; 64],
            certificate_pem: String::new(),
        },
        None,
        100,
    ));
    state
}

fn tier_of(author: &AuthorId) -> Option<Tier> {
    registry_state().tier_of(author)
}

fn related_with(state: Option<State<'static>>) -> RelatedContracts<'static> {
    let mut map = HashMap::new();
    map.insert(registry_id(), state);
    RelatedContracts::from(map)
}

fn related_registry() -> RelatedContracts<'static> {
    related_with(Some(State::from(common::to_cbor(&registry_state()))))
}

fn placement(author: u8, coord: u16, color: u8, ts: u64) -> SignedPlacement {
    SignedPlacement::sign(&key(author), &tile_params(), coord, color, ts)
}

fn delta_update(placements: Vec<SignedPlacement>) -> UpdateData<'static> {
    UpdateData::Delta(StateDelta::from(common::to_cbor(&TileDelta { placements })))
}

fn genesis() -> State<'static> {
    State::from(common::to_cbor(&TileState::default()))
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

fn state_bytes_of(state: &TileState) -> Vec<u8> {
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
            &state_bytes_of(&TileState::default()),
            RelatedContracts::default()
        ),
        ValidateResult::Valid
    ));
}

#[test]
fn populated_state_requests_registry_then_validates() {
    let mut state = TileState::default();
    state.insert(placement(1, 10, 3, 100));
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
    let mut state = TileState::default();
    state.insert(placement(3, 10, 3, 100));
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn missing_registry_state_falls_back_to_signature_only() {
    let mut state = TileState::default();
    state.insert(placement(3, 10, 3, 100));
    assert!(matches!(
        validate(&state_bytes_of(&state), related_with(None)),
        ValidateResult::Valid
    ));
}

// --- Validation: structure and tamper rejection ----------------------------

#[test]
fn tampered_placement_is_invalid() {
    let mut state = TileState::default();
    let mut p = placement(1, 10, 3, 100);
    p.color = (p.color + 1) % 16;
    state.insert(p);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn mismatched_author_key_is_invalid() {
    let p = placement(1, 10, 3, 100);
    let mut state = TileState::default();
    state
        .placements
        .entry(AuthorId::from(&key(2).verifying_key()))
        .or_default()
        .insert(p.ts, p);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn mismatched_timestamp_key_is_invalid() {
    let p = placement(1, 10, 3, 100);
    let mut state = TileState::default();
    state
        .placements
        .entry(AuthorId::from(&p.author))
        .or_default()
        .insert(p.ts + 1, p);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn empty_log_is_invalid() {
    let mut state = TileState::default();
    state
        .placements
        .insert(AuthorId::from(&key(1).verifying_key()), BTreeMap::new());
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn oversized_log_is_invalid() {
    let mut log = BTreeMap::new();
    for i in 0..=MAX_PLACEMENTS_PER_AUTHOR as u64 {
        let p = placement(1, i as u16, 1, 100 + i);
        log.insert(p.ts, p);
    }
    assert!(log.len() > MAX_PLACEMENTS_PER_AUTHOR);
    let mut state = TileState::default();
    state
        .placements
        .insert(AuthorId::from(&key(1).verifying_key()), log);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn far_future_placement_is_invalid_in_validate() {
    let mut state = TileState::default();
    state.insert(placement(1, 10, 3, NOW + MAX_FUTURE_SKEW_SECS + 1));
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));

    let mut state = TileState::default();
    state.insert(placement(1, 10, 3, NOW + MAX_FUTURE_SKEW_SECS));
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Valid
    ));
}

// --- Validation: the baked layer -------------------------------------------

/// Twelve honest placements: eight live, four baked. Structure and
/// membership checks must cover both maps.
fn baked_state() -> TileState {
    let mut state = TileState::default();
    for i in 0..(MAX_PLACEMENTS_PER_AUTHOR as u64 + 4) {
        state.insert(placement(1, i as u16, 3, 100 + 130 * i));
    }
    assert!(!state.baked.is_empty());
    state
}

#[test]
fn baked_state_validates_against_registry() {
    assert!(matches!(
        validate(&state_bytes_of(&baked_state()), related_registry()),
        ValidateResult::Valid
    ));
}

#[test]
fn unadmitted_baked_author_is_invalid() {
    let mut state = baked_state();
    let foreign = placement(3, 9, 3, 50);
    state
        .baked
        .entry(AuthorId::from(&foreign.author))
        .or_default()
        .insert(foreign.ts, foreign);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn tampered_baked_placement_is_invalid() {
    let mut state = baked_state();
    let log = state.baked.values_mut().next().unwrap();
    log.values_mut().next().unwrap().color += 1;
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn mismatched_baked_keys_are_invalid() {
    let p = placement(1, 10, 3, 100);
    let mut state = baked_state();
    state
        .baked
        .entry(AuthorId::from(&key(2).verifying_key()))
        .or_default()
        .insert(p.ts, p);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));

    let mut state = baked_state();
    state
        .baked
        .entry(AuthorId::from(&p.author))
        .or_default()
        .insert(p.ts + 1, p);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn oversized_baked_log_is_invalid() {
    let mut log = BTreeMap::new();
    for i in 0..=common::constants::MAX_BAKED_PER_AUTHOR as u64 {
        let p = placement(1, i as u16, 1, 100 + i);
        log.insert(p.ts, p);
    }
    let mut state = baked_state();
    state
        .baked
        .insert(AuthorId::from(&key(1).verifying_key()), log);
    assert!(matches!(
        validate(&state_bytes_of(&state), related_registry()),
        ValidateResult::Invalid
    ));
}

#[test]
fn merge_of_state_with_tampered_baked_entry_is_rejected() {
    let mut incoming = baked_state();
    incoming
        .baked
        .values_mut()
        .next()
        .unwrap()
        .values_mut()
        .next()
        .unwrap()
        .ts += 1;
    assert!(run_updates(
        genesis(),
        vec![UpdateData::State(State::from(state_bytes_of(&incoming)))],
    )
    .is_err());
}

/// The end-to-end regression for the "my oldest pixels disappear" bug at the
/// contract boundary: placements beyond the live cap stay in state (baked)
/// and stay visible, whatever order peers saw them in.
#[test]
fn evicted_pixels_survive_through_the_update_path() {
    let placements: Vec<SignedPlacement> = (0..(MAX_PLACEMENTS_PER_AUTHOR as u64 + 4))
        .map(|i| placement(1, i as u16, 3, 100 + 130 * i))
        .collect();
    let forward: Vec<UpdateData> = placements.iter().map(|p| delta_update(vec![*p])).collect();
    let reverse: Vec<UpdateData> = placements
        .iter()
        .rev()
        .map(|p| delta_update(vec![*p]))
        .collect();
    let peer_a = run_updates(genesis(), forward).unwrap();
    let peer_b = run_updates(genesis(), reverse).unwrap();
    assert_eq!(peer_a, peer_b, "state bytes must be identical");

    let state: TileState = common::from_cbor(&peer_a).unwrap();
    assert!(!state.baked.is_empty());
    let canvas = state.derive_canvas(tier_of);
    for (i, pixel) in canvas
        .iter()
        .take(MAX_PLACEMENTS_PER_AUTHOR + 4)
        .enumerate()
    {
        assert_eq!(*pixel, 3, "pixel at coord {i} must survive eviction");
    }
    assert!(matches!(
        validate(&peer_a, related_registry()),
        ValidateResult::Valid
    ));
}

// --- Updates: cheap checks at ingress --------------------------------------

#[test]
fn tampered_delta_is_rejected() {
    let mut p = placement(1, 10, 3, 100);
    p.coord += 1;
    let err = run_updates(genesis(), vec![delta_update(vec![p])]).unwrap_err();
    assert!(matches!(err, ContractError::InvalidUpdateWithInfo { .. }));
}

#[test]
fn far_future_delta_is_rejected() {
    let p = placement(1, 10, 3, NOW + MAX_FUTURE_SKEW_SECS + 1);
    let err = run_updates(genesis(), vec![delta_update(vec![p])]).unwrap_err();
    let ContractError::InvalidUpdateWithInfo { reason } = err else {
        panic!("expected InvalidUpdateWithInfo, got {err:?}");
    };
    assert!(reason.contains("future"), "got: {reason}");
}

#[test]
fn full_state_merge_verifies_placements() {
    let mut incoming = TileState::default();
    incoming.insert(placement(1, 10, 3, 100));
    let merged = run_updates(
        genesis(),
        vec![UpdateData::State(State::from(state_bytes_of(&incoming)))],
    )
    .unwrap();
    let parsed: TileState = common::from_cbor(&merged).unwrap();
    assert_eq!(parsed, incoming);

    let mut tampered = TileState::default();
    let mut p = placement(1, 10, 3, 100);
    p.ts += 1;
    tampered
        .placements
        .entry(AuthorId::from(&p.author))
        .or_default()
        .insert(p.ts, p);
    assert!(run_updates(
        genesis(),
        vec![UpdateData::State(State::from(state_bytes_of(&tampered)))],
    )
    .is_err());
}

// --- Phase 3 exit check: order-independent convergence ---------------------

/// key(1) is Pow (20s cooldown): ts 105 lands inside the window opened at
/// ts 100 and must be dropped from the derived canvas; ts 300 is clear.
/// key(2) is Ghostkey (2s): ts 140 clears the window opened at ts 100.
fn convergence_placements() -> Vec<SignedPlacement> {
    vec![
        placement(1, 10, 3, 100),
        placement(1, 11, 4, 105),
        placement(1, 12, 5, 300),
        placement(2, 10, 7, 100),
        placement(2, 20, 9, 140),
    ]
}

#[test]
fn peers_converge_to_identical_canvases_regardless_of_update_order() {
    let placements = convergence_placements();
    let forward: Vec<UpdateData> = placements.iter().map(|p| delta_update(vec![*p])).collect();
    let reverse: Vec<UpdateData> = placements
        .iter()
        .rev()
        .map(|p| delta_update(vec![*p]))
        .collect();

    let peer_a = run_updates(genesis(), forward).unwrap();
    let peer_b = run_updates(genesis(), reverse).unwrap();
    assert_eq!(peer_a, peer_b, "state bytes must be identical");

    let state_a: TileState = common::from_cbor(&peer_a).unwrap();
    let state_b: TileState = common::from_cbor(&peer_b).unwrap();
    let canvas_a = state_a.derive_canvas(tier_of);
    let canvas_b = state_b.derive_canvas(tier_of);
    assert_eq!(canvas_a, canvas_b, "derived canvases must be identical");

    // The in-cooldown placement is dropped on both peers.
    assert_eq!(canvas_a[11], EMPTY_PIXEL);
    // Placements past their cooldown are visible.
    assert_eq!(canvas_a[12], 5);
    assert_eq!(canvas_a[20], 9);
    // The coord-10 tie (same ts, different authors) resolves by author bytes.
    let expected = if key(1).verifying_key().to_bytes() > key(2).verifying_key().to_bytes() {
        3
    } else {
        7
    };
    assert_eq!(canvas_a[10], expected);

    // Both converged states pass full validation against the registry.
    assert!(matches!(
        validate(&peer_a, related_registry()),
        ValidateResult::Valid
    ));
}

#[test]
fn state_merge_and_delta_paths_converge() {
    // Peer A ingests placements as deltas; peer B receives A's full state in
    // one merge. Both must hold identical bytes.
    let placements = convergence_placements();
    let updates: Vec<UpdateData> = placements.iter().map(|p| delta_update(vec![*p])).collect();
    let peer_a = run_updates(genesis(), updates).unwrap();
    let peer_b = run_updates(
        genesis(),
        vec![UpdateData::State(State::from(peer_a.clone()))],
    )
    .unwrap();
    assert_eq!(peer_a, peer_b);
}

// --- Phase 3 exit check: delta size against a populated tile ---------------

#[test]
fn delta_to_converged_peer_is_zero_bytes_against_populated_tile() {
    let per_author = MAX_PLACEMENTS_PER_AUTHOR as u64 + 2;
    let mut updates = Vec::new();
    for author in [1u8, 2] {
        for i in 0..per_author {
            updates.push(delta_update(vec![placement(
                author,
                (author as u16) * 100 + i as u16,
                (i % 16) as u8,
                100 + i * 200,
            )]));
        }
    }
    let state_bytes = run_updates(genesis(), updates).unwrap();
    assert!(state_bytes.len() > 2000, "tile should be populated");
    let populated: TileState = common::from_cbor(&state_bytes).unwrap();
    assert!(
        !populated.baked.is_empty(),
        "tile should have a baked layer"
    );
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

    // A peer with an empty summary gets every placement, baked included.
    let empty_summary = StateSummary::from(common::to_cbor(&TileSummary::default()));
    let full = Contract::get_state_delta(params_bytes(), raw, empty_summary).unwrap();
    let parsed: TileDelta = common::from_cbor(full.as_ref()).unwrap();
    assert_eq!(parsed.placements.len(), 2 * per_author as usize);
}
