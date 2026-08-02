//! Stable-URL facade / web container (Phase 7). State is the gateway webapp
//! framing whose metadata slot carries a signed [`FacadeMetadata`] pointer;
//! every byte (including the web archive, via its blake3 in the signed
//! pointer) is covered by the owner's signature. Updates are last-writer-wins
//! on the deterministic `(version, signature)` key, so merges commute; both
//! `UpdateData::State` and `UpdateData::Delta` payloads are interpreted as a
//! full framed state (`fdev execute update` defaults to Delta).

use common::facade::{decode_facade_state, FacadeMetadata, FacadeParameters};
use freenet_stdlib::prelude::*;

#[cfg(test)]
mod tests;

pub struct Contract;

fn decode_params(parameters: &Parameters<'_>) -> Result<FacadeParameters, ContractError> {
    common::from_cbor(parameters.as_ref()).map_err(ContractError::Deser)
}

fn reject(reason: String) -> ContractError {
    ContractError::InvalidUpdateWithInfo { reason }
}

/// Decode + verify a framed state; empty bytes are the genesis state (`None`).
fn decode_verified(
    params: &FacadeParameters,
    bytes: &[u8],
) -> Result<Option<FacadeMetadata>, String> {
    if bytes.is_empty() {
        return Ok(None);
    }
    decode_facade_state(params, bytes).map(Some)
}

fn validate_state_impl(
    parameters: Parameters<'static>,
    state: State<'static>,
) -> Result<ValidateResult, ContractError> {
    let params = decode_params(&parameters)?;
    Ok(match decode_verified(&params, state.as_ref()) {
        Ok(_) => ValidateResult::Valid,
        Err(_) => ValidateResult::Invalid,
    })
}

fn update_state_impl(
    parameters: Parameters<'static>,
    state: State<'static>,
    data: Vec<UpdateData<'static>>,
) -> Result<UpdateModification<'static>, ContractError> {
    let params = decode_params(&parameters)?;
    let mut winner_bytes = state.as_ref().to_vec();
    let mut winner = decode_verified(&params, &winner_bytes).map_err(reject)?;
    for update in data {
        let incoming_bytes = match &update {
            UpdateData::State(incoming) => incoming.as_ref(),
            UpdateData::Delta(delta) => delta.as_ref(),
            UpdateData::StateAndDelta {
                state: incoming, ..
            } => incoming.as_ref(),
            _ => continue,
        };
        let incoming = decode_facade_state(&params, incoming_bytes).map_err(reject)?;
        if winner
            .as_ref()
            .is_none_or(|current| incoming.order_key() > current.order_key())
        {
            winner_bytes = incoming_bytes.to_vec();
            winner = Some(incoming);
        }
    }
    Ok(UpdateModification::valid(State::from(winner_bytes)))
}

fn version_of(params: &FacadeParameters, state: &[u8]) -> Result<u64, ContractError> {
    Ok(decode_verified(params, state)
        .map_err(ContractError::Deser)?
        .map_or(0, |meta| meta.pointer.version))
}

#[contract]
impl ContractInterface for Contract {
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        validate_state_impl(parameters, state)
    }

    fn update_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        update_state_impl(parameters, state, data)
    }

    fn summarize_state(
        parameters: Parameters<'static>,
        state: State<'static>,
    ) -> Result<StateSummary<'static>, ContractError> {
        let params = decode_params(&parameters)?;
        let version = version_of(&params, state.as_ref())?;
        Ok(StateSummary::from(common::to_cbor(&version)))
    }

    fn get_state_delta(
        parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        let params = decode_params(&parameters)?;
        let ours = version_of(&params, state.as_ref())?;
        let theirs: u64 = if summary.as_ref().is_empty() {
            0
        } else {
            common::from_cbor(summary.as_ref()).map_err(ContractError::Deser)?
        };
        // A converged (or newer) peer gets a zero-byte delta.
        if ours <= theirs {
            Ok(StateDelta::from(Vec::new()))
        } else {
            Ok(StateDelta::from(state.as_ref().to_vec()))
        }
    }
}
