//! User identity primitives shared by all contracts.

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use crate::constants::{GHOSTKEY_TILE_COOLDOWN_SECS, POW_TILE_COOLDOWN_SECS};

/// A user identity: the raw ed25519 verifying-key bytes. Used as a map key
/// everywhere (BTreeMap needs Ord; lexicographic byte order is deterministic).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct AuthorId(pub [u8; 32]);

impl From<&VerifyingKey> for AuthorId {
    fn from(vk: &VerifyingKey) -> Self {
        AuthorId(vk.to_bytes())
    }
}

/// Admission tier, recorded once by the registry at admission time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Tier {
    Pow,
    Ghostkey,
}

impl Tier {
    /// Per-tile placement cooldown for this tier.
    pub fn tile_cooldown_secs(self) -> u64 {
        match self {
            Tier::Pow => POW_TILE_COOLDOWN_SECS,
            Tier::Ghostkey => GHOSTKEY_TILE_COOLDOWN_SECS,
        }
    }
}
