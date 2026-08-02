//! Phase 1 exit-check battery: merge monoid properties, cooldown filter
//! determinism, delta/summary behavior, per-field tamper tests, and
//! hard-coded wire-format locks.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, SigningKey};
use proptest::prelude::*;

use crate::constants::{EMPTY_PIXEL, MAX_PLACEMENTS_PER_AUTHOR, TILE_AREA};
use crate::identity::{AuthorId, Tier};
use crate::tile::{
    placement_signing_bytes, serialize_delta, SignedPlacement, TileParameters, TileState,
};
use crate::{from_cbor, to_cbor};

fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn params() -> TileParameters {
    TileParameters {
        canvas_id: [7; 32],
        tile_x: 1,
        tile_y: 2,
        registry: [9; 32],
    }
}

/// Key 1 is PoW-admitted, key 2 ghost-key-admitted, key 3 not admitted.
fn tier_of(author: &AuthorId) -> Option<Tier> {
    if *author == AuthorId::from(&key(1).verifying_key()) {
        Some(Tier::Pow)
    } else if *author == AuthorId::from(&key(2).verifying_key()) {
        Some(Tier::Ghostkey)
    } else {
        None
    }
}

/// (author index 0..3, coord, color, ts)
type Spec = (usize, u16, u8, u64);

fn placement(spec: Spec) -> SignedPlacement {
    let (idx, coord, color, ts) = spec;
    SignedPlacement::sign(&key(idx as u8 + 1), &params(), coord, color, ts)
}

fn state_from(specs: &[Spec]) -> TileState {
    let mut state = TileState::default();
    for spec in specs {
        state.insert(placement(*spec));
    }
    state
}

fn merged(a: &TileState, b: &TileState) -> TileState {
    let mut out = a.clone();
    out.merge(b);
    out
}

/// Small caps keep proptest cases cheap while forcing constant live-log
/// eviction, baking, and baked-cap eviction (same trick as chat_tests).
const SMALL_LOG_CAP: usize = 2;
const SMALL_BAKED_CAP: usize = 3;

fn state_from_bounded(specs: &[Spec]) -> TileState {
    let mut state = TileState::default();
    for spec in specs {
        state.insert_bounded(placement(*spec), SMALL_LOG_CAP, SMALL_BAKED_CAP);
    }
    state
}

fn merged_bounded(a: &TileState, b: &TileState) -> TileState {
    let mut out = a.clone();
    out.merge_bounded(b, SMALL_LOG_CAP, SMALL_BAKED_CAP);
    out
}

fn arb_specs() -> impl Strategy<Value = Vec<Spec>> {
    prop::collection::vec((0..3usize, 0..32u16, 0..16u8, 0..48u64), 0..12)
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
        let empty = TileState::default();
        prop_assert_eq!(merged(&sa, &empty), sa.clone());
        prop_assert_eq!(merged(&empty, &sa), sa);
    }

    /// Two peers that see the same placements in any arrival order end up with
    /// byte-identical states and byte-identical derived canvases.
    #[test]
    fn cooldown_filter_is_arrival_order_independent(
        (original, shuffled) in arb_specs().prop_flat_map(|v| {
            let orig = v.clone();
            (Just(orig), Just(v).prop_shuffle())
        })
    ) {
        let peer_a = state_from(&original);
        let peer_b = state_from(&shuffled);
        prop_assert_eq!(&peer_a, &peer_b);
        prop_assert_eq!(peer_a.derive_canvas(tier_of), peer_b.derive_canvas(tier_of));
    }

    /// A converged peer (summary == our summary) always gets a zero-byte delta.
    #[test]
    fn delta_to_self_is_always_none(a in arb_specs()) {
        let sa = state_from(&a);
        let delta = sa.delta(&sa.summarize());
        prop_assert!(delta.is_none());
        prop_assert_eq!(serialize_delta(&delta).len(), 0);
    }

    /// Summary + delta reconciliation: a peer holding A reaches B exactly,
    /// when B extends A with strictly newer placements.
    #[test]
    fn delta_summary_roundtrip(
        base in arb_specs(),
        extra in prop::collection::vec((0..3usize, 0..32u16, 0..16u8, 0..8u64), 0..6)
    ) {
        let state_a = state_from(&base);
        let newest = base.iter().map(|s| s.3).max().unwrap_or(0);
        let mut state_b = state_a.clone();
        for (idx, coord, color, ts_offset) in &extra {
            state_b.insert(placement((*idx, *coord, *color, newest + 1 + ts_offset)));
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

proptest! {
    /// With eviction and baking constantly firing, merge must stay
    /// commutative and equal to building from the union of the inputs (the
    /// convergence claim for the baked layer).
    #[test]
    fn bounded_merge_is_commutative(a in arb_specs(), b in arb_specs()) {
        let (sa, sb) = (state_from_bounded(&a), state_from_bounded(&b));
        prop_assert_eq!(merged_bounded(&sa, &sb), merged_bounded(&sb, &sa));
    }

    #[test]
    fn bounded_merge_is_associative(a in arb_specs(), b in arb_specs(), c in arb_specs()) {
        let (sa, sb, sc) = (
            state_from_bounded(&a),
            state_from_bounded(&b),
            state_from_bounded(&c),
        );
        prop_assert_eq!(
            merged_bounded(&merged_bounded(&sa, &sb), &sc),
            merged_bounded(&sa, &merged_bounded(&sb, &sc))
        );
    }

    #[test]
    fn bounded_merge_equals_union_build(a in arb_specs(), b in arb_specs()) {
        let mut union = a.clone();
        union.extend(b.iter().copied());
        prop_assert_eq!(
            merged_bounded(&state_from_bounded(&a), &state_from_bounded(&b)),
            state_from_bounded(&union)
        );
    }

    #[test]
    fn bounded_states_are_arrival_order_independent(
        (original, shuffled) in arb_specs().prop_flat_map(|v| {
            let orig = v.clone();
            (Just(orig), Just(v).prop_shuffle())
        })
    ) {
        prop_assert_eq!(state_from_bounded(&original), state_from_bounded(&shuffled));
    }

    /// The converged-delta and roundtrip guarantees must survive baking too.
    #[test]
    fn bounded_delta_to_self_is_always_none(a in arb_specs()) {
        let sa = state_from_bounded(&a);
        prop_assert!(sa.delta(&sa.summarize()).is_none());
    }
}

#[test]
fn same_author_pair_inside_cooldown_converges_to_earliest() {
    // PoW cooldown is 120s; two placements 5s apart. Whatever order peers see
    // them in, only the earliest is valid on both.
    let first = placement((0, 10, 3, 100));
    let second = placement((0, 11, 4, 105));

    for order in [[first, second], [second, first]] {
        let mut state = TileState::default();
        for p in order {
            state.insert(p);
        }
        let valid = state.valid_placements(tier_of);
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0].ts, 100);
        let canvas = state.derive_canvas(tier_of);
        assert_eq!(canvas[10], 3);
        assert_eq!(canvas[11], EMPTY_PIXEL);
    }
}

#[test]
fn cooldown_boundary_is_inclusive() {
    // Ghost-key cooldown is 30s: exactly 30s later is accepted, 29s is not.
    let state = state_from(&[(1, 0, 1, 100), (1, 1, 2, 130)]);
    assert_eq!(state.valid_placements(tier_of).len(), 2);
    let state = state_from(&[(1, 0, 1, 100), (1, 1, 2, 129)]);
    assert_eq!(state.valid_placements(tier_of).len(), 1);
}

#[test]
fn unadmitted_author_contributes_nothing() {
    let state = state_from(&[(2, 5, 5, 100)]);
    assert!(state.valid_placements(tier_of).is_empty());
    assert!(state
        .derive_canvas(tier_of)
        .iter()
        .all(|&c| c == EMPTY_PIXEL));
}

#[test]
fn lww_winner_is_latest_then_author_then_signature() {
    // Different ts: later wins.
    let state = state_from(&[(0, 7, 1, 100), (1, 7, 2, 200)]);
    assert_eq!(state.derive_canvas(tier_of)[7], 2);

    // Same ts, different authors: greater author key bytes win. Both orders.
    let a = placement((0, 7, 1, 100));
    let b = placement((1, 7, 2, 100));
    let expected = if a.author.to_bytes() > b.author.to_bytes() {
        a.color
    } else {
        b.color
    };
    for order in [[a, b], [b, a]] {
        let mut state = TileState::default();
        for p in order {
            state.insert(p);
        }
        assert_eq!(state.derive_canvas(tier_of)[7], expected);
    }
}

#[test]
fn same_author_same_ts_slot_conflict_is_deterministic() {
    let a = placement((0, 1, 1, 100));
    let b = placement((0, 2, 2, 100));
    let mut ab = TileState::default();
    ab.insert(a);
    ab.insert(b);
    let mut ba = TileState::default();
    ba.insert(b);
    ba.insert(a);
    assert_eq!(ab, ba);
    assert_eq!(ab.placements.values().next().unwrap().len(), 1);
}

#[test]
fn per_author_log_caps_at_newest_k() {
    let specs: Vec<Spec> = (0..12).map(|i| (0, i as u16, 1, i as u64)).collect();
    let state = state_from(&specs);
    let log = state
        .placements
        .get(&AuthorId::from(&key(1).verifying_key()))
        .unwrap();
    assert_eq!(log.len(), MAX_PLACEMENTS_PER_AUTHOR);
    // Newest kept: timestamps 4..=11 survive, 0..=3 evicted.
    assert_eq!(*log.keys().next().unwrap(), 4);
    assert_eq!(*log.keys().next_back().unwrap(), 11);
}

/// The bug that motivated the baked layer: an author's 9th placement must not
/// erase their 1st from the derived canvas. Honest cadence (>= cooldown), so
/// every placement is cooldown-valid.
#[test]
fn pixels_survive_beyond_the_live_log_cap() {
    let count = MAX_PLACEMENTS_PER_AUTHOR + 1;
    let specs: Vec<Spec> = (0..count)
        .map(|i| (1, i as u16, 2, 100 + 30 * i as u64))
        .collect();
    let state = state_from(&specs);
    let canvas = state.derive_canvas(tier_of);
    for (i, pixel) in canvas.iter().take(count).enumerate() {
        assert_eq!(*pixel, 2, "pixel at coord {i} must survive eviction");
    }
}

/// Eviction must not launder a rapid-fire burst into visible pixels: baked
/// placements run through the same cooldown chain as live ones, so a flooder
/// still paints exactly one pixel per cooldown window.
#[test]
fn baked_burst_stays_cooldown_filtered() {
    let count = MAX_PLACEMENTS_PER_AUTHOR + 20;
    let specs: Vec<Spec> = (0..count)
        .map(|i| (0, i as u16, 1, 1000 + i as u64))
        .collect();
    let state = state_from(&specs);
    assert_eq!(state.baked.values().map(|log| log.len()).sum::<usize>(), 20);
    // All placements are 1s apart; the PoW cooldown is 120s, so only the
    // earliest retained placement is visible, baked or not.
    assert_eq!(state.valid_placements(tier_of).len(), 1);
    let canvas = state.derive_canvas(tier_of);
    assert_eq!(canvas.iter().filter(|&&c| c != EMPTY_PIXEL).count(), 1);
    assert_eq!(canvas[0], 1);
}

/// The baked layer is itself a newest-K log with deterministic eviction.
#[test]
fn baked_layer_caps_at_newest_k() {
    let specs: Vec<Spec> = (0..8)
        .map(|i| (0, i as u16, 1, 10 * (i as u64 + 1)))
        .collect();
    let state = state_from_bounded(&specs);
    let author = AuthorId::from(&key(1).verifying_key());
    assert_eq!(
        state.placements[&author]
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![70, 80]
    );
    // Six placements were displaced from the live log; the baked cap keeps
    // the newest three, deterministically.
    assert_eq!(
        state.baked[&author].keys().copied().collect::<Vec<_>>(),
        vec![40, 50, 60]
    );
}

/// A fresh peer (empty summary) rebuilds the sender's exact state, baked
/// layer included, from the delta alone.
#[test]
fn delta_reconstructs_baked_state_for_fresh_peer() {
    let specs: Vec<Spec> = (0..12)
        .map(|i| (1, i as u16, 2, 100 + 30 * i as u64))
        .collect();
    let sender = state_from(&specs);
    assert!(!sender.baked.is_empty());
    let delta = sender
        .delta(&crate::tile::TileSummary(BTreeMap::new()))
        .unwrap();
    assert_eq!(delta.placements.len(), 12);
    let mut receiver = TileState::default();
    receiver.apply_delta(&delta);
    assert_eq!(receiver, sender);
}

#[test]
fn empty_state_derives_empty_canvas() {
    let canvas = TileState::default().derive_canvas(tier_of);
    assert_eq!(canvas.len(), TILE_AREA);
    assert!(canvas.iter().all(|&c| c == EMPTY_PIXEL));
}

/// Delta to a fully-converged peer must be zero bytes, and the summary must be
/// a small fraction of the state (contract-patterns.md size test).
#[test]
fn delta_to_converged_peer_is_zero_bytes_against_populated_state() {
    let mut state = TileState::default();
    for author in 0..3usize {
        for i in 0..MAX_PLACEMENTS_PER_AUTHOR + 4 {
            state.insert(placement((
                author,
                (author * 100 + i) as u16,
                (i % 16) as u8,
                (i as u64) * 200,
            )));
        }
    }
    assert!(!state.baked.is_empty(), "state should have a baked layer");
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

    let delta_bytes = serialize_delta(&state.delta(&summary));
    assert_eq!(
        delta_bytes.len(),
        0,
        "delta to a converged peer was {} bytes against a {} byte state",
        delta_bytes.len(),
        state_bytes.len()
    );
    assert!(delta_bytes.len() < 256);
}

#[test]
fn delta_ships_whole_log_for_unknown_author() {
    let state = state_from(&[(0, 1, 1, 10), (0, 2, 2, 200)]);
    let delta = state
        .delta(&crate::tile::TileSummary(BTreeMap::new()))
        .unwrap();
    assert_eq!(delta.placements.len(), 2);
}

// --- Signature tamper tests: one per signed field -------------------------

fn assert_tampered_fails(mutate: impl FnOnce(&mut SignedPlacement)) {
    let mut p = placement((0, 123, 5, 1000));
    p.verify(&params()).expect("untampered placement verifies");
    mutate(&mut p);
    assert!(p.verify(&params()).is_err(), "tampered placement must fail");
}

#[test]
fn tamper_coord_fails() {
    assert_tampered_fails(|p| p.coord += 1);
}

#[test]
fn tamper_color_fails() {
    // Stays inside the palette so the signature check is what fails.
    assert_tampered_fails(|p| p.color = (p.color + 1) % 16);
}

#[test]
fn tamper_ts_fails() {
    assert_tampered_fails(|p| p.ts += 1);
}

#[test]
fn tamper_author_fails() {
    assert_tampered_fails(|p| p.author = key(9).verifying_key());
}

#[test]
fn tamper_signature_fails() {
    assert_tampered_fails(|p| {
        let mut bytes = p.signature.to_bytes();
        bytes[0] ^= 0x01;
        p.signature = Signature::from_bytes(&bytes);
    });
}

#[test]
fn placement_bound_to_canvas_id() {
    let p = placement((0, 123, 5, 1000));
    let mut other = params();
    other.canvas_id = [8; 32];
    assert!(p.verify(&other).is_err());
}

#[test]
fn placement_bound_to_tile_position() {
    let p = placement((0, 123, 5, 1000));
    let mut other = params();
    other.tile_x = 3;
    assert!(p.verify(&other).is_err());
    let mut other = params();
    other.tile_y = 3;
    assert!(p.verify(&other).is_err());
}

#[test]
fn placement_bound_to_registry() {
    let p = placement((0, 123, 5, 1000));
    let mut other = params();
    other.registry = [10; 32];
    assert!(p.verify(&other).is_err());
}

#[test]
fn out_of_palette_color_fails_even_correctly_signed() {
    let p = SignedPlacement::sign(&key(1), &params(), 0, 200, 1000);
    assert!(p.verify(&params()).is_err());
}

// --- Wire-format locks: hard-coded hex, not just roundtrip ----------------

fn canonical_state() -> TileState {
    state_from(&[(0, 1, 2, 10), (0, 3, 4, 20), (1, 500, 15, 30)])
}

#[test]
fn signing_preimage_format_locked() {
    let bytes = placement_signing_bytes(&params(), &key(1).verifying_key(), 258, 5, 1000);
    const EXPECTED_HEX: &str = "66726565706c6163653a74696c653a706c6163656d656e743a76310707070707070707070707070707070707070707070707070707070707070707010209090909090909090909090909090909090909090909090909090909090909098a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c020105e803000000000000";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
}

#[test]
fn tile_parameters_wire_format_locked() {
    let bytes = to_cbor(&params());
    const EXPECTED_HEX: &str = "a46963616e7661735f6964982007070707070707070707070707070707070707070707070707070707070707076674696c655f78016674696c655f790268726567697374727998200909090909090909090909090909090909090909090909090909090909090909";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    let parsed: TileParameters = from_cbor(&bytes).unwrap();
    assert_eq!(parsed, params());
}

#[test]
fn tile_state_wire_format_locked() {
    let state = canonical_state();
    let bytes = to_cbor(&state);
    const EXPECTED_HEX: &str = "a16a706c6163656d656e7473a298201881183918770e18a8187d17185f185618a31854186618c3184c187e18cc18cb188d188a189118b418ee183718a2185d18f60f185b188f18c918b31894a1181ea565636f6f72641901f465636f6c6f720f627473181e66617574686f7258208139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394697369676e6174757265984018c802187018c5187c182618de1866183718e7185218b3070e18d51894185e186c183218f4183618e0188a18e61868189d18bc18e418a4181b182c189918b3184b188618b218ac18db1898182718b418b0185b1859189e18ae18c418331882181f184514189018ce189118ef0f1863187a1518611118d5089820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185ca20aa565636f6f72640165636f6c6f72026274730a66617574686f7258208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c697369676e6174757265984018cc1872185b18bd181a18e90318b218840f18cc1618bd18f118a918761822184b189e18e0182018ee18f518d8188b18d0188c18b518c418db18c618be18fd188c189318db18c6187f18251852185b181e18a9182518861836186f182a187a1840184a186e181e18a0185c18b218c918b1181818751885183218950e14a565636f6f72640365636f6c6f72046274731466617574686f7258208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c697369676e6174757265984018a0189b182f0c1888183a18ff18800618e11842183b18f318e4184f18fb181c0e16181b1894182418181826186e182b183d1841181b18e9186e187918d11839185b18de185b188918251835186318e818d6182f18fd18a418330b18e21892186f09189d187c1872181818d01118d9182b181c15183809";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    let parsed: TileState = from_cbor(&bytes).unwrap();
    assert_eq!(parsed, state);
}

/// A state whose baked layer is populated must also have locked bytes (small
/// caps keep the fixture readable; the wire format is cap-independent). A
/// state with an empty baked layer skips the field entirely, so the
/// pre-baked-layer lock above is unchanged.
#[test]
fn tile_state_with_baked_wire_format_locked() {
    let mut state = TileState::default();
    for (i, ts) in [(1u16, 10u64), (3, 20), (5, 30)] {
        state.insert_bounded(
            SignedPlacement::sign(&key(1), &params(), i, 2, ts),
            SMALL_LOG_CAP,
            SMALL_BAKED_CAP,
        );
    }
    assert_eq!(state.baked.values().map(|log| log.len()).sum::<usize>(), 1);
    let bytes = to_cbor(&state);
    const EXPECTED_HEX: &str = "a26a706c6163656d656e7473a19820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185ca214a565636f6f72640365636f6c6f72026274731466617574686f7258208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c697369676e6174757265984018f5184418c418420018751896185d18af187b18eb182e18711873183f1845182218df181f0015186d18591869189818d3183d18981872182418c2182918e718e7189e1849183b18ca18e918f9185a1862186718ce18c418f818da184a1888184018a518ae184c10189e18771844189d18f4185b189918a118630e181ea565636f6f72640565636f6c6f7202627473181e66617574686f7258208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c697369676e6174757265984018de1892185a1218a518f91618a118cd184b18bf18f9183c18ff1833184e181918a6181a18f218c218a718fb187218a8189518ea188c1885187d18c9189418db181d18d4186518d9188c1118d1187518df18b918c918de184d18d5183218ae1829181d18cf18361718b0183b1885189c1858184d182e18a518c9046562616b6564a19820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185ca10aa565636f6f72640165636f6c6f72026274730a66617574686f7258208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c697369676e6174757265984018cc1872185b18bd181a18e90318b218840f18cc1618bd18f118a918761822184b189e18e0182018ee18f518d8188b18d0188c18b518c418db18c618be18fd188c189318db18c6187f18251852185b181e18a9182518861836186f182a187a1840184a186e181e18a0185c18b218c918b1181818751885183218950e";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    let parsed: TileState = from_cbor(&bytes).unwrap();
    assert_eq!(parsed, state);
}

#[test]
fn tile_summary_wire_format_locked() {
    let bytes = to_cbor(&canonical_state().summarize());
    const EXPECTED_HEX: &str = "a298201881183918770e18a8187d17185f185618a31854186618c3184c187e18cc18cb188d188a189118b418ee183718a2185d18f60f185b188f18c918b31894181e9820188a188818e318dd18740918f1189518fd185218db182d183c18ba185d187218ca18670918bf181d189412181b18f3187418880118b40f186f185c14";
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
}
