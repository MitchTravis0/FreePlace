//! The canvas tile CRDT core: signed placements, commutative merge, the greedy
//! cooldown filter, LWW canvas derivation, and summary/delta computation.
//!
//! The rendered canvas is derived, never stored. Because the cooldown filter is
//! a pure function of the set of known placements, two peers that have seen the
//! same placements derive the same canvas regardless of arrival order.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::constants::{
    EMPTY_PIXEL, MAX_BAKED_PER_AUTHOR, MAX_PLACEMENTS_PER_AUTHOR, PALETTE_COLORS, TILE_AREA,
};
use crate::identity::{AuthorId, Tier};

/// Domain-separation prefix for placement signature preimages.
const PLACEMENT_SIGNING_CONTEXT: &[u8] = b"freeplace:tile:placement:v1";

/// Instance parameters: `(canvas_id, tile_x, tile_y)` distinguish the 16 tile
/// contracts. Fixed at contract creation; part of every signature preimage
/// (cross-context binding: a placement signed for one tile cannot be replayed
/// against another).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TileParameters {
    pub canvas_id: [u8; 32],
    pub tile_x: u8,
    pub tile_y: u8,
    /// Raw `ContractInstanceId` bytes of the registry contract, so
    /// `validate_state` can request it as a related contract (an instance id
    /// cannot be derived from `canvas_id` alone). Note the coupling: a
    /// registry re-key changes these bytes and therefore re-keys every tile.
    pub registry: [u8; 32],
}

/// One signed pixel placement.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SignedPlacement {
    /// Local coordinate within the tile: `y * TILE_SIZE + x`, 0..65536.
    pub coord: u16,
    /// Palette index, 0..PALETTE_COLORS.
    pub color: u8,
    /// Author-claimed unix timestamp, seconds.
    pub ts: u64,
    pub author: VerifyingKey,
    pub signature: Signature,
}

/// Canonical signature preimage for a placement. Manual fixed layout, never
/// serde output: context prefix, tile parameters, author, then the fields.
/// All fields are fixed-length, so no length prefixes are needed.
pub fn placement_signing_bytes(
    params: &TileParameters,
    author: &VerifyingKey,
    coord: u16,
    color: u8,
    ts: u64,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(PLACEMENT_SIGNING_CONTEXT.len() + 32 + 2 + 32 + 32 + 2 + 1 + 8);
    out.extend_from_slice(PLACEMENT_SIGNING_CONTEXT);
    out.extend_from_slice(&params.canvas_id);
    out.push(params.tile_x);
    out.push(params.tile_y);
    out.extend_from_slice(&params.registry);
    out.extend_from_slice(author.as_bytes());
    out.extend_from_slice(&coord.to_le_bytes());
    out.push(color);
    out.extend_from_slice(&ts.to_le_bytes());
    out
}

impl SignedPlacement {
    /// Build and sign a placement for the given tile.
    pub fn sign(key: &SigningKey, params: &TileParameters, coord: u16, color: u8, ts: u64) -> Self {
        use ed25519_dalek::Signer;
        let author = key.verifying_key();
        let bytes = placement_signing_bytes(params, &author, coord, color, ts);
        SignedPlacement {
            coord,
            color,
            ts,
            author,
            signature: key.sign(&bytes),
        }
    }

    /// Verify the signature and field bounds against this tile's parameters.
    pub fn verify(&self, params: &TileParameters) -> Result<(), String> {
        if self.color >= PALETTE_COLORS {
            return Err(format!("color {} out of palette range", self.color));
        }
        let bytes = placement_signing_bytes(params, &self.author, self.coord, self.color, self.ts);
        self.author
            .verify_strict(&bytes, &self.signature)
            .map_err(|_| "invalid placement signature".to_string())
    }

    fn author_id(&self) -> AuthorId {
        AuthorId::from(&self.author)
    }
}

/// Deterministic winner between two placements from the same author with the
/// same timestamp (a single log slot). Signature bytes differ for distinct
/// payloads, so this is a total order on well-formed placements.
fn slot_winner(a: SignedPlacement, b: SignedPlacement) -> SignedPlacement {
    let ka = (a.signature.to_bytes(), a.coord, a.color);
    let kb = (b.signature.to_bytes(), b.coord, b.color);
    if ka >= kb {
        a
    } else {
        b
    }
}

/// One author's ts-keyed placement log (the shape of both state maps).
pub type PlacementLog = BTreeMap<u64, SignedPlacement>;

/// Tile contract state: per-author recent placement logs, keyed by timestamp,
/// capped at the newest MAX_PLACEMENTS_PER_AUTHOR entries per author, plus the
/// baked layer: placements displaced from the live log by that cap, kept as a
/// second per-author log capped at MAX_BAKED_PER_AUTHOR (newest kept). Baking
/// is what stops canvas pixels from vanishing when their author keeps placing.
/// `baked` is skipped when empty so pre-baked-layer states and genesis bytes
/// are unchanged.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TileState {
    pub placements: BTreeMap<AuthorId, PlacementLog>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub baked: BTreeMap<AuthorId, PlacementLog>,
}

/// Summary: per-author newest-timestamp watermark. BTreeMap (never HashMap) so
/// identical states summarize to identical bytes.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TileSummary(pub BTreeMap<AuthorId, u64>);

/// Delta: placements the requester is missing (newer than its watermarks).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TileDelta {
    pub placements: Vec<SignedPlacement>,
}

/// Insert into a ts-keyed log with deterministic same-slot resolution and a
/// newest-K cap, returning what the cap displaced. Keep-K-largest composes
/// with set union, so any merge order of the same placement sets converges.
fn log_insert(
    log: &mut PlacementLog,
    placement: SignedPlacement,
    cap: usize,
) -> Vec<SignedPlacement> {
    log.entry(placement.ts)
        .and_modify(|existing| *existing = slot_winner(*existing, placement))
        .or_insert(placement);
    let mut displaced = Vec::new();
    while log.len() > cap {
        let oldest = *log.keys().next().expect("log is non-empty");
        displaced.push(log.remove(&oldest).expect("oldest key exists"));
    }
    displaced
}

impl TileState {
    /// Insert one placement, resolving same-timestamp conflicts
    /// deterministically and enforcing the per-author cap (newest kept).
    /// Placements the cap displaces are baked rather than dropped.
    pub fn insert(&mut self, placement: SignedPlacement) {
        self.insert_bounded(placement, MAX_PLACEMENTS_PER_AUTHOR, MAX_BAKED_PER_AUTHOR);
    }

    /// Cap-parametric insert (property tests use small caps to keep case
    /// generation cheap, mirroring chat's insert_bounded).
    pub(crate) fn insert_bounded(
        &mut self,
        placement: SignedPlacement,
        log_cap: usize,
        baked_cap: usize,
    ) {
        let author = placement.author_id();
        let log = self.placements.entry(author).or_default();
        for evicted in log_insert(log, placement, log_cap) {
            self.bake_bounded(evicted, baked_cap);
        }
    }

    /// Bake one placement displaced from a live log. The baked layer is itself
    /// a per-author newest-K log (same convergence argument as the live log);
    /// what its cap displaces is gone for good. Keyed by the placement's own
    /// author and ts, never by a caller-supplied key.
    fn bake_bounded(&mut self, placement: SignedPlacement, baked_cap: usize) {
        let log = self.baked.entry(placement.author_id()).or_default();
        log_insert(log, placement, baked_cap);
    }

    /// Commutative merge: per-slot deterministic resolution, then the caps.
    /// The other peer's baked entries go straight to our baked layer (they
    /// were displaced from a live log somewhere, which is a property of the
    /// placement set, not of the arrival order).
    pub fn merge(&mut self, other: &TileState) {
        self.merge_bounded(other, MAX_PLACEMENTS_PER_AUTHOR, MAX_BAKED_PER_AUTHOR);
    }

    pub(crate) fn merge_bounded(&mut self, other: &TileState, log_cap: usize, baked_cap: usize) {
        for log in other.placements.values() {
            for placement in log.values() {
                self.insert_bounded(*placement, log_cap, baked_cap);
            }
        }
        for log in other.baked.values() {
            for placement in log.values() {
                self.bake_bounded(*placement, baked_cap);
            }
        }
    }

    /// Per-author newest-timestamp watermarks, over the live and baked logs
    /// (baked entries are older than the live log in every honestly reachable
    /// state, so covering them changes nothing there; it keeps the converged
    /// delta at zero bytes even for adversarially crafted shapes).
    pub fn summarize(&self) -> TileSummary {
        let mut watermarks: BTreeMap<AuthorId, u64> = BTreeMap::new();
        for (author, log) in self.placements.iter().chain(self.baked.iter()) {
            if let Some(ts) = log.keys().next_back() {
                let watermark = watermarks.entry(*author).or_insert(*ts);
                *watermark = (*watermark).max(*ts);
            }
        }
        TileSummary(watermarks)
    }

    /// Live and baked placements newer than the requester's per-author
    /// watermark (all of an author's placements if the requester has never
    /// seen that author, baked included, so a fresh peer reconstructs the full
    /// state from the delta alone). `None` means converged and MUST serialize
    /// to zero bytes.
    pub fn delta(&self, summary: &TileSummary) -> Option<TileDelta> {
        let placements: Vec<SignedPlacement> = self
            .placements
            .iter()
            .chain(self.baked.iter())
            .flat_map(|(author, log)| {
                let watermark = summary.0.get(author).copied();
                log.values()
                    .filter(move |p| watermark.is_none_or(|w| p.ts > w))
                    .copied()
            })
            .collect();
        if placements.is_empty() {
            None
        } else {
            Some(TileDelta { placements })
        }
    }

    /// Merge a delta's placements into this state.
    pub fn apply_delta(&mut self, delta: &TileDelta) {
        for placement in &delta.placements {
            self.insert(*placement);
        }
    }

    /// The greedy cooldown filter: per author, walk placements earliest-first
    /// and accept each one at least the author's tier cooldown after the last
    /// accepted. One chain runs over baked then live entries (baked timestamps
    /// precede live ones), so eviction cannot launder a rapid-fire burst into
    /// permanently visible pixels. A pure function of the placement set, so
    /// all peers agree. Authors with no tier (not admitted) contribute
    /// nothing.
    pub fn valid_placements(
        &self,
        tier_of: impl Fn(&AuthorId) -> Option<Tier>,
    ) -> Vec<&SignedPlacement> {
        let mut accepted = Vec::new();
        let authors: std::collections::BTreeSet<&AuthorId> =
            self.placements.keys().chain(self.baked.keys()).collect();
        for author in authors {
            let Some(tier) = tier_of(author) else {
                continue;
            };
            let cooldown = tier.tile_cooldown_secs();
            let mut entries: Vec<(&u64, &SignedPlacement)> = self
                .baked
                .get(author)
                .into_iter()
                .flatten()
                .chain(self.placements.get(author).into_iter().flatten())
                .collect();
            entries.sort_by_key(|(ts, _)| **ts);
            let mut last_accepted: Option<u64> = None;
            for (ts, placement) in entries {
                let ok = match last_accepted {
                    None => true,
                    Some(last) => *ts >= last.saturating_add(cooldown),
                };
                if ok {
                    accepted.push(placement);
                    last_accepted = Some(*ts);
                }
            }
        }
        accepted
    }

    /// Derive the rendered tile: last-writer-wins per coordinate among valid
    /// placements (tiebreak: ts, then author key bytes, then signature bytes).
    /// EMPTY_PIXEL marks coordinates with no valid placement.
    pub fn derive_canvas(&self, tier_of: impl Fn(&AuthorId) -> Option<Tier>) -> Vec<u8> {
        let mut winners: BTreeMap<u16, &SignedPlacement> = BTreeMap::new();
        for placement in self.valid_placements(tier_of) {
            winners
                .entry(placement.coord)
                .and_modify(|current| {
                    let cur = (
                        current.ts,
                        current.author.to_bytes(),
                        current.signature.to_bytes(),
                    );
                    let new = (
                        placement.ts,
                        placement.author.to_bytes(),
                        placement.signature.to_bytes(),
                    );
                    if new > cur {
                        *current = placement;
                    }
                })
                .or_insert(placement);
        }
        let mut canvas = vec![EMPTY_PIXEL; TILE_AREA];
        for (coord, placement) in winners {
            canvas[coord as usize] = placement.color;
        }
        canvas
    }
}

/// Serialize a delta for the wire, collapsing "converged" to zero bytes.
pub fn serialize_delta(delta: &Option<TileDelta>) -> Vec<u8> {
    match delta {
        Some(d) => crate::to_cbor(d),
        None => Vec::new(),
    }
}
