//! Registry admission core: signed admission records (PoW or ghost key proof),
//! nickname updates with monotonic-version replay protection, and the registry
//! CRDT (state, summary, delta).
//!
//! Merge semantics are two independent pointwise lattices per identity: the
//! admission core converges to the deterministic minimum of
//! `(admitted_ts, signature bytes)`, and the nickname converges to the
//! deterministic maximum of `(version, signature bytes)`. Both are pure
//! functions of the set of known updates, so any arrival order converges.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::constants::{
    MAX_GHOSTKEY_CERT_PEM_BYTES, MAX_GHOSTKEY_SCOPED_PAYLOAD_BYTES, MAX_NICKNAME_BYTES,
    POW_DIFFICULTY_BITS,
};
use crate::identity::{AuthorId, Tier};

/// Domain-separation prefix for admission record signature preimages.
const ADMISSION_SIGNING_CONTEXT: &[u8] = b"freeplace:registry:admission:v1";

/// Domain-separation prefix for nickname signature preimages.
const NICKNAME_SIGNING_CONTEXT: &[u8] = b"freeplace:registry:nickname:v1";

/// Domain-separation prefix for the admission challenge. The PoW digest is
/// `blake3(challenge || nonce)`; the ghost key path signs the same challenge
/// bytes. Binding `canvas_id` and `identity_vk` prevents cross-instance and
/// cross-identity replay.
const CHALLENGE_CONTEXT: &[u8] = b"freeplace:registry:challenge:v1";

/// Instance parameters for the registry contract. `canvas_id` is the stable
/// identity anchor shared with the tile contracts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RegistryParameters {
    pub canvas_id: [u8; 32],
}

/// The expensive-to-produce, cheap-to-verify admission proof.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum AdmissionProof {
    Work {
        nonce: u64,
    },
    Ghostkey {
        /// CBOR `ScopedPayload { requestor, payload }`; `payload` must equal
        /// the admission challenge bytes.
        scoped_payload: Vec<u8>,
        /// Ed25519 signature by the ghost key over `scoped_payload`.
        signature: Vec<u8>,
        /// PEM certificate chain back to the Freenet master key.
        certificate_pem: String,
    },
}

impl AdmissionProof {
    pub fn tier(&self) -> Tier {
        match self {
            AdmissionProof::Work { .. } => Tier::Pow,
            AdmissionProof::Ghostkey { .. } => Tier::Ghostkey,
        }
    }
}

/// A nickname signed by its identity. Replay of an older nickname is rejected
/// by the monotonic `version` counter (the version is inside the signed bytes).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SignedNickname {
    pub name: String,
    /// Strictly increasing per identity, starting at 1.
    pub version: u64,
    pub signature: Signature,
}

/// One identity's admission: proof verified once here, tier derived from the
/// proof kind, nickname editable later via [`NicknameUpdate`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AdmissionRecord {
    pub identity_vk: VerifyingKey,
    pub proof: AdmissionProof,
    /// Not covered by `signature` (it is editable later); carries its own.
    pub nickname: Option<SignedNickname>,
    pub admitted_ts: u64,
    /// By `identity_vk` over the canonical admission bytes (incl. the proof).
    pub signature: Signature,
}

/// Hook verifying a ghost key proof (certificate chain, challenge signature).
/// Lives outside `common` so contracts that only read registry state do not
/// link the RSA verification stack.
pub type GhostkeyCheck<'a> = &'a dyn Fn(
    &RegistryParameters,
    &VerifyingKey,
    &[u8], // scoped_payload
    &[u8], // signature
    &str,  // certificate_pem
) -> Result<(), String>;

/// The admission challenge both proof kinds bind to.
pub fn admission_challenge_bytes(
    params: &RegistryParameters,
    identity_vk: &VerifyingKey,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHALLENGE_CONTEXT.len() + 32 + 32);
    out.extend_from_slice(CHALLENGE_CONTEXT);
    out.extend_from_slice(&params.canvas_id);
    out.extend_from_slice(identity_vk.as_bytes());
    out
}

/// PoW digest: `blake3(challenge || nonce_le)`.
pub fn pow_digest(params: &RegistryParameters, identity_vk: &VerifyingKey, nonce: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&admission_challenge_bytes(params, identity_vk));
    hasher.update(&nonce.to_le_bytes());
    *hasher.finalize().as_bytes()
}

fn leading_zero_bits(digest: &[u8; 32]) -> u32 {
    let mut bits = 0;
    for byte in digest {
        if *byte == 0 {
            bits += 8;
        } else {
            bits += byte.leading_zeros();
            break;
        }
    }
    bits
}

/// Whether a nonce satisfies the difficulty target for this identity.
pub fn pow_nonce_meets_target(
    params: &RegistryParameters,
    identity_vk: &VerifyingKey,
    nonce: u64,
) -> bool {
    leading_zero_bits(&pow_digest(params, identity_vk, nonce)) >= POW_DIFFICULTY_BITS
}

/// Grind the smallest nonce meeting the target (test/tooling helper; the UI
/// runs the same search in a Web Worker).
pub fn find_pow_nonce(params: &RegistryParameters, identity_vk: &VerifyingKey) -> u64 {
    (0u64..)
        .find(|nonce| pow_nonce_meets_target(params, identity_vk, *nonce))
        .expect("a satisfying nonce exists below u64::MAX")
}

/// Canonical signature preimage for an admission record. Manual fixed layout:
/// context prefix, canvas id, identity key, timestamp, then the proof with a
/// tag byte and length-prefixed variable fields. The nickname is deliberately
/// excluded (separately signed, editable later).
pub fn admission_signing_bytes(
    params: &RegistryParameters,
    identity_vk: &VerifyingKey,
    proof: &AdmissionProof,
    admitted_ts: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(ADMISSION_SIGNING_CONTEXT);
    out.extend_from_slice(&params.canvas_id);
    out.extend_from_slice(identity_vk.as_bytes());
    out.extend_from_slice(&admitted_ts.to_le_bytes());
    match proof {
        AdmissionProof::Work { nonce } => {
            out.push(0);
            out.extend_from_slice(&nonce.to_le_bytes());
        }
        AdmissionProof::Ghostkey {
            scoped_payload,
            signature,
            certificate_pem,
        } => {
            out.push(1);
            for field in [
                scoped_payload.as_slice(),
                signature.as_slice(),
                certificate_pem.as_bytes(),
            ] {
                let len: u32 = field.len().try_into().expect("proof field < 4 GiB");
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(field);
            }
        }
    }
    out
}

/// Canonical signature preimage for a nickname.
pub fn nickname_signing_bytes(
    params: &RegistryParameters,
    identity_vk: &VerifyingKey,
    version: u64,
    name: &str,
) -> Vec<u8> {
    let name_len: u32 = name.len().try_into().expect("name < 4 GiB");
    let mut out = Vec::with_capacity(NICKNAME_SIGNING_CONTEXT.len() + 32 + 32 + 8 + 4 + name.len());
    out.extend_from_slice(NICKNAME_SIGNING_CONTEXT);
    out.extend_from_slice(&params.canvas_id);
    out.extend_from_slice(identity_vk.as_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(name.as_bytes());
    out
}

impl SignedNickname {
    pub fn sign(key: &SigningKey, params: &RegistryParameters, name: &str, version: u64) -> Self {
        use ed25519_dalek::Signer;
        let bytes = nickname_signing_bytes(params, &key.verifying_key(), version, name);
        SignedNickname {
            name: name.to_string(),
            version,
            signature: key.sign(&bytes),
        }
    }

    pub fn verify(
        &self,
        params: &RegistryParameters,
        identity_vk: &VerifyingKey,
    ) -> Result<(), String> {
        if self.version == 0 {
            return Err("nickname version must be >= 1".to_string());
        }
        if self.name.is_empty() || self.name.len() > MAX_NICKNAME_BYTES {
            return Err(format!(
                "nickname must be 1..={MAX_NICKNAME_BYTES} bytes, got {}",
                self.name.len()
            ));
        }
        let bytes = nickname_signing_bytes(params, identity_vk, self.version, &self.name);
        identity_vk
            .verify_strict(&bytes, &self.signature)
            .map_err(|_| "invalid nickname signature".to_string())
    }
}

impl AdmissionRecord {
    pub fn sign(
        key: &SigningKey,
        params: &RegistryParameters,
        proof: AdmissionProof,
        nickname: Option<SignedNickname>,
        admitted_ts: u64,
    ) -> Self {
        use ed25519_dalek::Signer;
        let identity_vk = key.verifying_key();
        let bytes = admission_signing_bytes(params, &identity_vk, &proof, admitted_ts);
        AdmissionRecord {
            identity_vk,
            proof,
            nickname,
            admitted_ts,
            signature: key.sign(&bytes),
        }
    }

    pub fn tier(&self) -> Tier {
        self.proof.tier()
    }

    /// Full verification: record signature, then the proof itself (the
    /// expensive part, re-checked so a malicious PUT cannot forge admissions),
    /// then the nickname if present.
    pub fn verify(
        &self,
        params: &RegistryParameters,
        ghostkey_check: GhostkeyCheck,
    ) -> Result<(), String> {
        let bytes =
            admission_signing_bytes(params, &self.identity_vk, &self.proof, self.admitted_ts);
        self.identity_vk
            .verify_strict(&bytes, &self.signature)
            .map_err(|_| "invalid admission signature".to_string())?;
        match &self.proof {
            AdmissionProof::Work { nonce } => {
                if !pow_nonce_meets_target(params, &self.identity_vk, *nonce) {
                    return Err(
                        "proof-of-work nonce does not meet the difficulty target".to_string()
                    );
                }
            }
            AdmissionProof::Ghostkey {
                scoped_payload,
                signature,
                certificate_pem,
            } => {
                if scoped_payload.is_empty()
                    || scoped_payload.len() > MAX_GHOSTKEY_SCOPED_PAYLOAD_BYTES
                {
                    return Err("ghost key scoped payload size out of bounds".to_string());
                }
                if signature.len() != 64 {
                    return Err("ghost key signature must be 64 bytes".to_string());
                }
                if certificate_pem.len() > MAX_GHOSTKEY_CERT_PEM_BYTES {
                    return Err("ghost key certificate too large".to_string());
                }
                ghostkey_check(
                    params,
                    &self.identity_vk,
                    scoped_payload,
                    signature,
                    certificate_pem,
                )?;
            }
        }
        if let Some(nickname) = &self.nickname {
            nickname.verify(params, &self.identity_vk)?;
        }
        Ok(())
    }
}

/// Deterministic total order key for the admission core (smallest wins).
fn core_key(record: &AdmissionRecord) -> (u64, [u8; 64]) {
    (record.admitted_ts, record.signature.to_bytes())
}

/// Deterministic nickname winner: highest `(version, signature bytes)`.
fn better_nickname(a: Option<SignedNickname>, b: Option<SignedNickname>) -> Option<SignedNickname> {
    match (a, b) {
        (Some(x), Some(y)) => {
            if (y.version, y.signature.to_bytes()) > (x.version, x.signature.to_bytes()) {
                Some(y)
            } else {
                Some(x)
            }
        }
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

/// A standalone nickname change for an already-admitted identity.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NicknameUpdate {
    pub identity_vk: VerifyingKey,
    pub nickname: SignedNickname,
}

/// Registry contract state: admitted identities keyed by their id.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct RegistryState {
    pub identities: BTreeMap<AuthorId, AdmissionRecord>,
}

/// Summary: per-identity nickname version (0 = no nickname). Presence of the
/// key tells the delta computation the requester already has the admission.
/// BTreeMap (never HashMap) so identical states summarize to identical bytes.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct RegistrySummary(pub BTreeMap<AuthorId, u64>);

/// Delta: admissions the requester lacks entirely, plus newer nicknames for
/// identities it already knows.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RegistryDelta {
    pub admissions: Vec<AdmissionRecord>,
    pub nicknames: Vec<NicknameUpdate>,
}

impl RegistryState {
    /// Insert one admission, resolving conflicts deterministically (core min,
    /// nickname max — see the module docs).
    pub fn insert_record(&mut self, record: AdmissionRecord) {
        let author = AuthorId::from(&record.identity_vk);
        let merged = match self.identities.get(&author) {
            None => record,
            Some(existing) => {
                let nickname = better_nickname(existing.nickname.clone(), record.nickname.clone());
                let mut core = if core_key(&record) < core_key(existing) {
                    record
                } else {
                    existing.clone()
                };
                core.nickname = nickname;
                core
            }
        };
        self.identities.insert(author, merged);
    }

    /// Apply a nickname change. Updates for identities not (yet) admitted are
    /// dropped; that divergence is transient because summary/delta
    /// reconciliation ships the admission and nickname together.
    pub fn apply_nickname(&mut self, update: &NicknameUpdate) {
        let author = AuthorId::from(&update.identity_vk);
        if let Some(entry) = self.identities.get_mut(&author) {
            entry.nickname = better_nickname(entry.nickname.take(), Some(update.nickname.clone()));
        }
    }

    /// Admission tier of an identity, `None` if not admitted. This is the
    /// lookup tiles and chat feed into the cooldown/rate filters.
    pub fn tier_of(&self, author: &AuthorId) -> Option<Tier> {
        self.identities.get(author).map(|record| record.tier())
    }

    /// Commutative merge (pinned by property tests).
    pub fn merge(&mut self, other: &RegistryState) {
        for record in other.identities.values() {
            self.insert_record(record.clone());
        }
    }

    pub fn summarize(&self) -> RegistrySummary {
        RegistrySummary(
            self.identities
                .iter()
                .map(|(author, record)| {
                    (*author, record.nickname.as_ref().map_or(0, |n| n.version))
                })
                .collect(),
        )
    }

    /// What the requester is missing. `None` means converged and MUST
    /// serialize to zero bytes.
    pub fn delta(&self, summary: &RegistrySummary) -> Option<RegistryDelta> {
        let mut admissions = Vec::new();
        let mut nicknames = Vec::new();
        for (author, record) in &self.identities {
            match summary.0.get(author) {
                None => admissions.push(record.clone()),
                Some(their_version) => {
                    if let Some(nickname) = &record.nickname {
                        if nickname.version > *their_version {
                            nicknames.push(NicknameUpdate {
                                identity_vk: record.identity_vk,
                                nickname: nickname.clone(),
                            });
                        }
                    }
                }
            }
        }
        if admissions.is_empty() && nicknames.is_empty() {
            None
        } else {
            Some(RegistryDelta {
                admissions,
                nicknames,
            })
        }
    }

    /// Merge a delta into this state (admissions before nicknames, so a
    /// nickname riding with its admission always lands).
    pub fn apply_delta(&mut self, delta: &RegistryDelta) {
        for record in &delta.admissions {
            self.insert_record(record.clone());
        }
        for update in &delta.nicknames {
            self.apply_nickname(update);
        }
    }

    /// Verify every invariant on the bytes alone: map-key binding, record
    /// signatures, proofs, nicknames. This is what keeps admission forgery out
    /// of a malicious full-state PUT.
    pub fn verify(
        &self,
        params: &RegistryParameters,
        ghostkey_check: GhostkeyCheck,
    ) -> Result<(), String> {
        for (author, record) in &self.identities {
            if *author != AuthorId::from(&record.identity_vk) {
                return Err("record stored under a mismatched identity key".to_string());
            }
            record.verify(params, ghostkey_check)?;
        }
        Ok(())
    }
}

/// Serialize a delta for the wire, collapsing "converged" to zero bytes.
pub fn serialize_registry_delta(delta: &Option<RegistryDelta>) -> Vec<u8> {
    match delta {
        Some(d) => crate::to_cbor(d),
        None => Vec::new(),
    }
}
