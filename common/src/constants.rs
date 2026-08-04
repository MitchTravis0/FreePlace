//! All tunable constants in one place (plan.md: retuning re-keys contracts,
//! so the values are centralized here and go through the migration path).

/// Side length of one tile in pixels.
pub const TILE_SIZE: u16 = 256;

/// Pixels in one tile.
pub const TILE_AREA: usize = TILE_SIZE as usize * TILE_SIZE as usize;

/// The canvas is a TILES_PER_SIDE x TILES_PER_SIDE grid of tiles.
pub const TILES_PER_SIDE: u8 = 4;

/// Full canvas side length in pixels.
pub const CANVAS_SIZE: u32 = TILE_SIZE as u32 * TILES_PER_SIDE as u32;

/// Number of palette colors; valid color values are 0..PALETTE_COLORS.
/// 255 is the ceiling, not 256: 0xff is the EMPTY_PIXEL sentinel in derived
/// canvases. Indices 0..15 are the original 2017 r/place palette, so pixels
/// placed under the 16-color era keep their colors.
pub const PALETTE_COLORS: u8 = 255;

/// Sentinel in a derived canvas for "no valid placement at this coordinate".
pub const EMPTY_PIXEL: u8 = 0xff;

/// Per-author placement log cap (evicted by count, newest kept; never by age).
pub const MAX_PLACEMENTS_PER_AUTHOR: usize = 8;

/// Per-author cap on the baked layer: placements evicted from the live log are
/// baked (kept newest-first) so canvas pixels survive past the live cap, while
/// one identity's permanent footprint per tile stays bounded. Raised 256 ->
/// 1024 (2026-08-02) after the 2s cooldown retune shrank the fade horizon to
/// ~8.8 min, then 1024 -> 65536 (2026-08-03) after hardcore painters hit the
/// 1024 wall. 65536 = TILE_AREA, the usefulness ceiling: an author can never
/// have more visible pixels in a tile than the tile has coordinates, so a
/// bigger cap could only retain invisible history. Costs accepted: ~7.2 MiB
/// worst-case per author per tile, so ~7 max-footprint authors reach the
/// 50 MiB tile cap (identities are PoW-priced; the cap was already a weak
/// flood deterrent at any value), and the fade horizon becomes ~36 h of
/// nonstop single-tile painting at ghost tier (~15 d PoW).
pub const MAX_BAKED_PER_AUTHOR: usize = 65536;

/// Per-tile cooldown for PoW-admitted identities, in seconds.
pub const POW_TILE_COOLDOWN_SECS: u64 = 20;

/// Per-tile cooldown for ghost-key-admitted identities, in seconds. 2 is the
/// floor: timestamps have 1-second resolution and the per-author log is keyed
/// by ts, so anything lower collides same-second placements on the map key.
pub const GHOSTKEY_TILE_COOLDOWN_SECS: u64 = 2;

/// Placements timestamped further than this ahead of the host clock are
/// invalid. There is deliberately no past-skew check (self-DoS on stored state).
pub const MAX_FUTURE_SKEW_SECS: u64 = 5 * 60;

/// Minimum leading zero bits a PoW admission digest must have. Dev-friendly
/// value (~2^18 hashes, a moment in a browser worker); final deterrence tuning
/// happens before the real-network publish (Phase 7) and re-keys the contract
/// via the migration path.
pub const POW_DIFFICULTY_BITS: u32 = 18;

/// Maximum nickname length in bytes (UTF-8).
pub const MAX_NICKNAME_BYTES: usize = 32;

/// Chat ring buffer: only the newest CHAT_MESSAGE_CAP messages (by MessageId
/// order) are kept; eviction is deterministic so it commutes with merge.
pub const CHAT_MESSAGE_CAP: usize = 500;

/// Per-author stored message cap (newest kept, same pattern as the tile log's
/// MAX_PLACEMENTS_PER_AUTHOR). Bounds how much of the ring buffer one
/// admitted identity can occupy, so a flooder cannot evict everyone else's
/// history. 16 distinct authors can still fill the whole buffer.
pub const CHAT_MAX_MESSAGES_PER_AUTHOR: usize = 32;

/// Minimum seconds between chat messages per author, enforced by the same
/// deterministic greedy filter as the pixel cooldown (display-time, tier-free).
pub const CHAT_MIN_MESSAGE_INTERVAL_SECS: u64 = 3;

/// Maximum chat message content length in bytes (UTF-8).
pub const MAX_CHAT_MESSAGE_BYTES: usize = 512;

/// Caps on the ghost key proof fields, bounding registry state growth per
/// admission (the certificate chain is stored so validation stays permissionless).
pub const MAX_GHOSTKEY_SCOPED_PAYLOAD_BYTES: usize = 1024;
pub const MAX_GHOSTKEY_CERT_PEM_BYTES: usize = 8192;

/// Rollback ring size in the facade pointer (previous web-container ids kept).
pub const FACADE_MAX_PREV_APPS: usize = 8;
