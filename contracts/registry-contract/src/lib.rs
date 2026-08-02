//! Identity admission registry. Verifies the expensive admission proof (PoW
//! target or ghost key certificate chain) when an update arrives, and again in
//! `validate_state` so a malicious full-state PUT cannot forge admissions.
//! Tiles and chat later consult the recorded `(identity, tier, nickname)` with
//! a single lookup, never a re-verification.

use common::constants::MAX_FUTURE_SKEW_SECS;
use common::registry::{
    serialize_registry_delta, GhostkeyCheck, RegistryDelta, RegistryParameters, RegistryState,
    RegistrySummary,
};
use freenet_stdlib::prelude::*;

mod ghostkey;
#[cfg(test)]
mod tests;

pub struct Contract;

// The wasm runtime provides no system entropy source and verification never
// draws any; ghostkey_lib's unused key-generation paths link getrandom, so
// give it a stub that fails instead of failing the build.
#[cfg(target_family = "wasm")]
getrandom::register_custom_getrandom!(no_entropy);
#[cfg(target_family = "wasm")]
fn no_entropy(_buf: &mut [u8]) -> Result<(), getrandom::Error> {
    Err(getrandom::Error::UNSUPPORTED)
}

fn decode_params(parameters: &Parameters<'_>) -> Result<RegistryParameters, ContractError> {
    common::from_cbor(parameters.as_ref()).map_err(ContractError::Deser)
}

/// Empty bytes are the genesis state.
fn decode_state(state: &[u8]) -> Result<RegistryState, String> {
    if state.is_empty() {
        Ok(RegistryState::default())
    } else {
        common::from_cbor(state)
    }
}

fn host_now_ts() -> u64 {
    #[cfg(target_family = "wasm")]
    {
        freenet_stdlib::time::now().timestamp().max(0) as u64
    }
    // The native stdlib time stub is UB; tests supply `now` through
    // update_state_impl, and this fallback only serves native rlib builds.
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

fn merge_state(
    current: &mut RegistryState,
    params: &RegistryParameters,
    bytes: &[u8],
    check: GhostkeyCheck,
) -> Result<(), ContractError> {
    let incoming = decode_state(bytes).map_err(reject)?;
    incoming.verify(params, check).map_err(reject)?;
    current.merge(&incoming);
    Ok(())
}

fn apply_delta(
    current: &mut RegistryState,
    params: &RegistryParameters,
    bytes: &[u8],
    check: GhostkeyCheck,
    now: u64,
) -> Result<(), ContractError> {
    let delta: RegistryDelta = common::from_cbor(bytes).map_err(reject)?;
    for record in &delta.admissions {
        record.verify(params, check).map_err(reject)?;
        // Future-skew only; no past-skew check ever (self-DoS on stored state).
        if record.admitted_ts > now + MAX_FUTURE_SKEW_SECS {
            return Err(reject(
                "admission timestamp too far in the future".to_string(),
            ));
        }
    }
    for update in &delta.nicknames {
        update
            .nickname
            .verify(params, &update.identity_vk)
            .map_err(reject)?;
    }
    current.apply_delta(&delta);
    Ok(())
}

fn validate_state_impl(
    parameters: Parameters<'static>,
    state: State<'static>,
    check: GhostkeyCheck,
) -> Result<ValidateResult, ContractError> {
    let params = decode_params(&parameters)?;
    let Ok(state) = decode_state(state.as_ref()) else {
        return Ok(ValidateResult::Invalid);
    };
    Ok(match state.verify(&params, check) {
        Ok(()) => ValidateResult::Valid,
        Err(_) => ValidateResult::Invalid,
    })
}

fn update_state_impl(
    parameters: Parameters<'static>,
    state: State<'static>,
    data: Vec<UpdateData<'static>>,
    check: GhostkeyCheck,
    now: u64,
) -> Result<UpdateModification<'static>, ContractError> {
    let params = decode_params(&parameters)?;
    let mut current = decode_state(state.as_ref()).map_err(ContractError::Deser)?;
    for update in data {
        match update {
            UpdateData::State(incoming) => {
                merge_state(&mut current, &params, incoming.as_ref(), check)?;
            }
            UpdateData::Delta(delta) => {
                apply_delta(&mut current, &params, delta.as_ref(), check, now)?;
            }
            UpdateData::StateAndDelta {
                state: incoming,
                delta,
            } => {
                merge_state(&mut current, &params, incoming.as_ref(), check)?;
                apply_delta(&mut current, &params, delta.as_ref(), check, now)?;
            }
            // Related-contract data is never requested by this contract.
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
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        validate_state_impl(parameters, state, &ghostkey::freenet_ghostkey_check)
    }

    fn update_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        update_state_impl(
            parameters,
            state,
            data,
            &ghostkey::freenet_ghostkey_check,
            host_now_ts(),
        )
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
        let summary: RegistrySummary = if summary.as_ref().is_empty() {
            RegistrySummary::default()
        } else {
            common::from_cbor(summary.as_ref()).map_err(ContractError::Deser)?
        };
        Ok(StateDelta::from(serialize_registry_delta(
            &state.delta(&summary),
        )))
    }
}
