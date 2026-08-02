//! The single global chat room: the Phase 4 message CRDT behind the contract
//! boundary. `update_state` does cheap checks only (signature, content bounds,
//! future skew); registry admission of every author is checked in the validate
//! pass the host runs after each update, via the related-contracts mechanism.

use std::collections::BTreeMap;

use common::chat::{
    serialize_chat_delta, ChatDelta, ChatParameters, ChatState, ChatSummary, SignedMessage,
};
use common::constants::{CHAT_MAX_MESSAGES_PER_AUTHOR, CHAT_MESSAGE_CAP, MAX_FUTURE_SKEW_SECS};
use common::identity::AuthorId;
use common::registry::RegistryState;
use freenet_stdlib::prelude::*;

#[cfg(test)]
mod tests;

pub struct Contract;

fn decode_params(parameters: &Parameters<'_>) -> Result<ChatParameters, ContractError> {
    common::from_cbor(parameters.as_ref()).map_err(ContractError::Deser)
}

/// Empty bytes are the genesis state.
fn decode_state(state: &[u8]) -> Result<ChatState, String> {
    if state.is_empty() {
        Ok(ChatState::default())
    } else {
        common::from_cbor(state)
    }
}

fn host_now_ts() -> u64 {
    #[cfg(target_family = "wasm")]
    {
        freenet_stdlib::time::now().timestamp().max(0) as u64
    }
    // The native stdlib time stub is UB; tests supply `now` through the
    // *_impl functions, and this fallback only serves native rlib builds.
    #[cfg(not(target_family = "wasm"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

fn reject(reason: String) -> ContractError {
    ContractError::InvalidUpdateWithInfo { reason }
}

/// Per-message checks that need no registry: signature (which also covers
/// content bounds and cross-context binding to this chat's parameters) and
/// future skew. There is deliberately no past-skew check (self-DoS on stored
/// state).
fn verify_message(
    message: &SignedMessage,
    params: &ChatParameters,
    now: u64,
) -> Result<(), String> {
    message.verify(params)?;
    if message.ts > now + MAX_FUTURE_SKEW_SECS {
        return Err("message timestamp too far in the future".to_string());
    }
    Ok(())
}

/// Verify a stored state's structure on the bytes alone: map keys bound to
/// their messages, the ring buffer and per-author caps, and every message
/// signed and in bounds. This is what keeps a malicious full-state PUT out.
fn verify_state_structure(
    state: &ChatState,
    params: &ChatParameters,
    now: u64,
) -> Result<(), String> {
    if state.messages.len() > CHAT_MESSAGE_CAP {
        return Err("message count exceeds the ring buffer cap".to_string());
    }
    let mut per_author: BTreeMap<AuthorId, usize> = BTreeMap::new();
    for (id, message) in &state.messages {
        if *id != message.id() {
            return Err("message stored under a mismatched id key".to_string());
        }
        let count = per_author.entry(id.author).or_insert(0);
        *count += 1;
        if *count > CHAT_MAX_MESSAGES_PER_AUTHOR {
            return Err("author message count exceeds the per-author cap".to_string());
        }
        verify_message(message, params, now)?;
    }
    Ok(())
}

/// Every author in the chat must be admitted in the registry.
fn check_registry_membership(state: &ChatState, registry_bytes: &[u8]) -> Result<(), String> {
    let registry: RegistryState = if registry_bytes.is_empty() {
        RegistryState::default()
    } else {
        common::from_cbor(registry_bytes)?
    };
    for id in state.messages.keys() {
        if !registry.identities.contains_key(&id.author) {
            return Err("message author not admitted in the registry".to_string());
        }
    }
    Ok(())
}

fn validate_state_impl(
    parameters: Parameters<'static>,
    state: State<'static>,
    related: RelatedContracts<'static>,
    now: u64,
) -> Result<ValidateResult, ContractError> {
    let params = decode_params(&parameters)?;
    let Ok(state) = decode_state(state.as_ref()) else {
        return Ok(ValidateResult::Invalid);
    };
    if verify_state_structure(&state, &params, now).is_err() {
        return Ok(ValidateResult::Invalid);
    }
    if state.messages.is_empty() {
        return Ok(ValidateResult::Valid);
    }
    let registry_id = ContractInstanceId::new(params.registry);
    match related.states().find(|(id, _)| **id == registry_id) {
        // First pass: ask the host to fetch the registry.
        None => Ok(ValidateResult::RequestRelated(vec![registry_id])),
        // The related-contracts mechanism is young (plan.md risks): when the
        // host could not supply the registry state, fall back to
        // signature-only validation instead of bricking the chat.
        Some((_, None)) => Ok(ValidateResult::Valid),
        Some((_, Some(registry_bytes))) => Ok(
            match check_registry_membership(&state, registry_bytes.as_ref()) {
                Ok(()) => ValidateResult::Valid,
                Err(_) => ValidateResult::Invalid,
            },
        ),
    }
}

fn merge_state(
    current: &mut ChatState,
    params: &ChatParameters,
    bytes: &[u8],
    now: u64,
) -> Result<(), ContractError> {
    let incoming = decode_state(bytes).map_err(reject)?;
    // merge() re-inserts each message keyed by its own id, so only
    // per-message checks are needed here, not structural ones.
    for message in incoming.messages.values() {
        verify_message(message, params, now).map_err(reject)?;
    }
    current.merge(&incoming);
    Ok(())
}

fn apply_delta(
    current: &mut ChatState,
    params: &ChatParameters,
    bytes: &[u8],
    now: u64,
) -> Result<(), ContractError> {
    let delta: ChatDelta = common::from_cbor(bytes).map_err(reject)?;
    for message in &delta.messages {
        verify_message(message, params, now).map_err(reject)?;
    }
    current.apply_delta(&delta);
    Ok(())
}

fn update_state_impl(
    parameters: Parameters<'static>,
    state: State<'static>,
    data: Vec<UpdateData<'static>>,
    now: u64,
) -> Result<UpdateModification<'static>, ContractError> {
    let params = decode_params(&parameters)?;
    let mut current = decode_state(state.as_ref()).map_err(ContractError::Deser)?;
    for update in data {
        match update {
            UpdateData::State(incoming) => {
                merge_state(&mut current, &params, incoming.as_ref(), now)?;
            }
            UpdateData::Delta(delta) => {
                apply_delta(&mut current, &params, delta.as_ref(), now)?;
            }
            UpdateData::StateAndDelta {
                state: incoming,
                delta,
            } => {
                merge_state(&mut current, &params, incoming.as_ref(), now)?;
                apply_delta(&mut current, &params, delta.as_ref(), now)?;
            }
            // Related-contract updates are not subscribed to by this contract.
            _ => {}
        }
    }
    Ok(UpdateModification::valid(State::from(common::to_cbor(
        &current,
    ))))
}

#[contract]
impl ContractInterface for Contract {
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        validate_state_impl(parameters, state, related, host_now_ts())
    }

    fn update_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        update_state_impl(parameters, state, data, host_now_ts())
    }

    fn summarize_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
    ) -> Result<StateSummary<'static>, ContractError> {
        let state = decode_state(state.as_ref()).map_err(ContractError::Deser)?;
        Ok(StateSummary::from(common::to_cbor(&state.summarize())))
    }

    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        let state = decode_state(state.as_ref()).map_err(ContractError::Deser)?;
        let summary: ChatSummary = if summary.as_ref().is_empty() {
            ChatSummary::default()
        } else {
            common::from_cbor(summary.as_ref()).map_err(ContractError::Deser)?
        };
        Ok(StateDelta::from(serialize_chat_delta(
            &state.delta(&summary),
        )))
    }
}
