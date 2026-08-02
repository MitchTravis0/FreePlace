//! One 256x256 canvas tile: the Phase 1 placement CRDT behind the contract
//! boundary. `update_state` does cheap checks only (signature, bounds, future
//! skew); registry admission of every author is checked in the validate pass
//! the host runs after each update, via the related-contracts mechanism.

use common::constants::{MAX_FUTURE_SKEW_SECS, MAX_PLACEMENTS_PER_AUTHOR};
use common::identity::AuthorId;
use common::registry::RegistryState;
use common::tile::{
    serialize_delta, SignedPlacement, TileDelta, TileParameters, TileState, TileSummary,
};
use freenet_stdlib::prelude::*;

#[cfg(test)]
mod tests;

pub struct Contract;

fn decode_params(parameters: &Parameters<'_>) -> Result<TileParameters, ContractError> {
    common::from_cbor(parameters.as_ref()).map_err(ContractError::Deser)
}

/// Empty bytes are the genesis state.
fn decode_state(state: &[u8]) -> Result<TileState, String> {
    if state.is_empty() {
        Ok(TileState::default())
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

/// Per-placement checks that need no registry: signature (which also covers
/// bounds and cross-context binding to this tile's parameters) and future
/// skew. There is deliberately no past-skew check (self-DoS on stored state).
fn verify_placement(
    placement: &SignedPlacement,
    params: &TileParameters,
    now: u64,
) -> Result<(), String> {
    placement.verify(params)?;
    if placement.ts > now + MAX_FUTURE_SKEW_SECS {
        return Err("placement timestamp too far in the future".to_string());
    }
    Ok(())
}

/// Verify a stored state's structure on the bytes alone: map keys bound to
/// their placements, per-author log cap, and every placement signed and in
/// bounds. This is what keeps a malicious full-state PUT out.
fn verify_state_structure(
    state: &TileState,
    params: &TileParameters,
    now: u64,
) -> Result<(), String> {
    for (author, log) in &state.placements {
        if log.is_empty() {
            return Err("empty placement log".to_string());
        }
        if log.len() > MAX_PLACEMENTS_PER_AUTHOR {
            return Err("placement log exceeds the per-author cap".to_string());
        }
        for (ts, placement) in log {
            if *author != AuthorId::from(&placement.author) {
                return Err("placement stored under a mismatched author key".to_string());
            }
            if *ts != placement.ts {
                return Err("placement stored under a mismatched timestamp key".to_string());
            }
            verify_placement(placement, params, now)?;
        }
    }
    Ok(())
}

/// Every author in the tile must be admitted in the registry.
fn check_registry_membership(state: &TileState, registry_bytes: &[u8]) -> Result<(), String> {
    let registry: RegistryState = if registry_bytes.is_empty() {
        RegistryState::default()
    } else {
        common::from_cbor(registry_bytes)?
    };
    for author in state.placements.keys() {
        if !registry.identities.contains_key(author) {
            return Err("placement author not admitted in the registry".to_string());
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
    if state.placements.is_empty() {
        return Ok(ValidateResult::Valid);
    }
    let registry_id = ContractInstanceId::new(params.registry);
    match related.states().find(|(id, _)| **id == registry_id) {
        // First pass: ask the host to fetch the registry.
        None => Ok(ValidateResult::RequestRelated(vec![registry_id])),
        // The related-contracts mechanism is young (plan.md risks): when the
        // host could not supply the registry state, fall back to
        // signature-only validation instead of bricking the tile.
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
    current: &mut TileState,
    params: &TileParameters,
    bytes: &[u8],
    now: u64,
) -> Result<(), ContractError> {
    let incoming = decode_state(bytes).map_err(reject)?;
    // merge() re-inserts each placement keyed by its own author and timestamp,
    // so only per-placement checks are needed here, not structural ones.
    for log in incoming.placements.values() {
        for placement in log.values() {
            verify_placement(placement, params, now).map_err(reject)?;
        }
    }
    current.merge(&incoming);
    Ok(())
}

fn apply_delta(
    current: &mut TileState,
    params: &TileParameters,
    bytes: &[u8],
    now: u64,
) -> Result<(), ContractError> {
    let delta: TileDelta = common::from_cbor(bytes).map_err(reject)?;
    for placement in &delta.placements {
        verify_placement(placement, params, now).map_err(reject)?;
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
        let summary: TileSummary = if summary.as_ref().is_empty() {
            TileSummary::default()
        } else {
            common::from_cbor(summary.as_ref()).map_err(ContractError::Deser)?
        };
        Ok(StateDelta::from(serialize_delta(&state.delta(&summary))))
    }
}
