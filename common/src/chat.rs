//! The global chat room CRDT core: signed messages in a capped ring buffer,
//! commutative merge, the greedy per-author rate-limit filter, and
//! summary/delta computation.
//!
//! Storage keeps well-formed messages up to two caps: the global ring buffer
//! (newest CHAT_MESSAGE_CAP) and a per-author cap (newest
//! CHAT_MAX_MESSAGES_PER_AUTHOR, so one identity cannot flood everyone else's
//! history out of the buffer). The rate limit is a derived display-time
//! filter (same pattern as the pixel cooldown), so it is a pure function of
//! the set of known messages and all peers agree on it.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::constants::{
    CHAT_MAX_MESSAGES_PER_AUTHOR, CHAT_MESSAGE_CAP, CHAT_MIN_MESSAGE_INTERVAL_SECS,
    MAX_CHAT_MESSAGE_BYTES,
};
use crate::identity::AuthorId;

/// Domain-separation prefix for message signature preimages.
const MESSAGE_SIGNING_CONTEXT: &[u8] = b"freeplace:chat:message:v1";

/// Instance parameters for the chat contract. `canvas_id` is the stable
/// identity anchor shared with the registry and tiles.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ChatParameters {
    pub canvas_id: [u8; 32],
    /// Raw `ContractInstanceId` bytes of the registry contract, so
    /// `validate_state` can request it as a related contract. Same coupling as
    /// the tiles: a registry re-key changes these bytes and re-keys the chat.
    pub registry: [u8; 32],
}

/// Total-order message key: `(ts, author, seq)`. The ring buffer keeps the
/// newest CHAT_MESSAGE_CAP messages under this order, which makes eviction
/// itself commutative.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct MessageId {
    pub ts: u64,
    pub author: AuthorId,
    pub seq: u32,
}

/// One signed chat message.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SignedMessage {
    pub content: String,
    /// Author-claimed unix timestamp, seconds.
    pub ts: u64,
    /// Author-chosen tie-breaker for several messages in the same second.
    pub seq: u32,
    pub author: VerifyingKey,
    pub signature: Signature,
}

/// Canonical signature preimage for a message. Manual fixed layout, never
/// serde output: context prefix, chat parameters, author, fixed-width fields,
/// then the length-prefixed content (cross-context binding: a message signed
/// for one canvas cannot be replayed against another).
pub fn message_signing_bytes(
    params: &ChatParameters,
    author: &VerifyingKey,
    ts: u64,
    seq: u32,
    content: &str,
) -> Vec<u8> {
    let content_len: u32 = content.len().try_into().expect("content < 4 GiB");
    let mut out = Vec::with_capacity(
        MESSAGE_SIGNING_CONTEXT.len() + 32 + 32 + 32 + 8 + 4 + 4 + content.len(),
    );
    out.extend_from_slice(MESSAGE_SIGNING_CONTEXT);
    out.extend_from_slice(&params.canvas_id);
    out.extend_from_slice(&params.registry);
    out.extend_from_slice(author.as_bytes());
    out.extend_from_slice(&ts.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&content_len.to_le_bytes());
    out.extend_from_slice(content.as_bytes());
    out
}

impl SignedMessage {
    /// Build and sign a message for the given chat instance.
    pub fn sign(
        key: &SigningKey,
        params: &ChatParameters,
        content: &str,
        ts: u64,
        seq: u32,
    ) -> Self {
        use ed25519_dalek::Signer;
        let author = key.verifying_key();
        let bytes = message_signing_bytes(params, &author, ts, seq, content);
        SignedMessage {
            content: content.to_string(),
            ts,
            seq,
            author,
            signature: key.sign(&bytes),
        }
    }

    /// Verify the signature and content bounds against this chat's parameters.
    pub fn verify(&self, params: &ChatParameters) -> Result<(), String> {
        if self.content.is_empty() || self.content.len() > MAX_CHAT_MESSAGE_BYTES {
            return Err(format!(
                "message content must be 1..={MAX_CHAT_MESSAGE_BYTES} bytes, got {}",
                self.content.len()
            ));
        }
        let bytes = message_signing_bytes(params, &self.author, self.ts, self.seq, &self.content);
        self.author
            .verify_strict(&bytes, &self.signature)
            .map_err(|_| "invalid message signature".to_string())
    }

    pub fn id(&self) -> MessageId {
        MessageId {
            ts: self.ts,
            author: AuthorId::from(&self.author),
            seq: self.seq,
        }
    }
}

/// Deterministic winner between two messages occupying the same MessageId
/// slot. Signature bytes differ for distinct payloads, so this is a total
/// order on well-formed messages.
fn slot_beats(challenger: &SignedMessage, incumbent: &SignedMessage) -> bool {
    (
        challenger.signature.to_bytes(),
        challenger.content.as_bytes(),
    ) > (incumbent.signature.to_bytes(), incumbent.content.as_bytes())
}

/// Chat contract state: the capped ring buffer of signed messages.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ChatState {
    pub messages: BTreeMap<MessageId, SignedMessage>,
}

/// Summary: per-author newest `(ts, seq)` watermark. BTreeMap (never HashMap)
/// so identical states summarize to identical bytes.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ChatSummary(pub BTreeMap<AuthorId, (u64, u32)>);

/// Delta: messages the requester is missing (newer than its watermarks).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ChatDelta {
    pub messages: Vec<SignedMessage>,
}

impl ChatState {
    /// Insert one message, resolving same-id conflicts deterministically and
    /// enforcing the per-author cap and the ring buffer cap (newest kept).
    pub fn insert(&mut self, message: SignedMessage) {
        self.insert_bounded(message, CHAT_MESSAGE_CAP, CHAT_MAX_MESSAGES_PER_AUTHOR);
    }

    /// Cap-parametric insert. Keep-N-largest (globally and per author)
    /// composes with set union, so any merge order of the same message set
    /// converges (pinned by property tests, which use small caps to keep case
    /// generation cheap). Per-author eviction runs first, then the global cap.
    pub(crate) fn insert_bounded(&mut self, message: SignedMessage, cap: usize, author_cap: usize) {
        let id = message.id();
        match self.messages.entry(id) {
            Entry::Occupied(mut existing) => {
                if slot_beats(&message, existing.get()) {
                    existing.insert(message);
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(message);
            }
        }
        let author_ids: Vec<MessageId> = self
            .messages
            .keys()
            .filter(|other| other.author == id.author)
            .copied()
            .collect();
        for evicted in &author_ids[..author_ids.len().saturating_sub(author_cap)] {
            self.messages.remove(evicted);
        }
        while self.messages.len() > cap {
            self.messages.pop_first();
        }
    }

    /// Commutative merge: per-slot deterministic resolution, then the caps.
    pub fn merge(&mut self, other: &ChatState) {
        self.merge_bounded(other, CHAT_MESSAGE_CAP, CHAT_MAX_MESSAGES_PER_AUTHOR);
    }

    pub(crate) fn merge_bounded(&mut self, other: &ChatState, cap: usize, author_cap: usize) {
        for message in other.messages.values() {
            self.insert_bounded(message.clone(), cap, author_cap);
        }
    }

    /// Per-author newest `(ts, seq)` watermarks. The map iterates in id order,
    /// so the last write per author is its maximum.
    pub fn summarize(&self) -> ChatSummary {
        let mut watermarks = BTreeMap::new();
        for id in self.messages.keys() {
            watermarks.insert(id.author, (id.ts, id.seq));
        }
        ChatSummary(watermarks)
    }

    /// Messages newer than the requester's per-author watermark (all of an
    /// author's messages if the requester has never seen that author). `None`
    /// means converged and MUST serialize to zero bytes.
    pub fn delta(&self, summary: &ChatSummary) -> Option<ChatDelta> {
        let messages: Vec<SignedMessage> = self
            .messages
            .iter()
            .filter(|(id, _)| {
                summary
                    .0
                    .get(&id.author)
                    .is_none_or(|watermark| (id.ts, id.seq) > *watermark)
            })
            .map(|(_, message)| message.clone())
            .collect();
        if messages.is_empty() {
            None
        } else {
            Some(ChatDelta { messages })
        }
    }

    /// Merge a delta's messages into this state.
    pub fn apply_delta(&mut self, delta: &ChatDelta) {
        for message in &delta.messages {
            self.insert(message.clone());
        }
    }

    /// The greedy rate-limit filter: per author, walk messages earliest-first
    /// and accept each one at least CHAT_MIN_MESSAGE_INTERVAL_SECS after the
    /// last accepted. A pure function of the message set, so all peers agree.
    /// Returns messages in display (id) order.
    pub fn valid_messages(&self) -> Vec<&SignedMessage> {
        let mut last_accepted: BTreeMap<AuthorId, u64> = BTreeMap::new();
        let mut accepted = Vec::new();
        // The map is ordered by (ts, author, seq), so each author's messages
        // arrive here in ascending timestamp order.
        for (id, message) in &self.messages {
            let ok = match last_accepted.get(&id.author) {
                None => true,
                Some(last) => id.ts - last >= CHAT_MIN_MESSAGE_INTERVAL_SECS,
            };
            if ok {
                accepted.push(message);
                last_accepted.insert(id.author, id.ts);
            }
        }
        accepted
    }
}

/// Serialize a delta for the wire, collapsing "converged" to zero bytes.
pub fn serialize_chat_delta(delta: &Option<ChatDelta>) -> Vec<u8> {
    match delta {
        Some(d) => crate::to_cbor(d),
        None => Vec::new(),
    }
}
