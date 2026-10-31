//! Shared local request refresh orchestration for the CLI and operator UI.

use crate::storage;
use review_platform::{
    PrKey, RefreshOutcome, StateError,
    github::{GithubClient, GithubError},
};
use std::error::Error;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RefreshError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Github(#[from] GithubError),
    #[error("local state: {0}")]
    Storage(#[source] Box<dyn Error + Send + Sync>),
}

fn storage_error(error: anyhow::Error) -> RefreshError {
    RefreshError::Storage(error.into())
}

/// Refresh one existing request using the same process locks for every caller.
pub fn refresh(state_file: &Path, key: &PrKey) -> Result<RefreshOutcome, RefreshError> {
    let _pr_lock = storage::pr_lock(state_file, key).map_err(storage_error)?;
    let state = storage::load(state_file).map_err(storage_error)?;
    let prior_revision = state
        .get(key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
        .revision();
    let client = GithubClient::new()?;
    let observation = client.observe_pull_excluding(key, &state.confirmed_review_ids(key))?;
    let _lock = storage::lock(state_file).map_err(storage_error)?;
    let mut state = storage::load(state_file).map_err(storage_error)?;
    let current_revision = state
        .get(key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
        .revision();
    if current_revision != prior_revision {
        return Err(StateError::RevisionMismatch {
            inspected: prior_revision,
            recorded: current_revision,
        }
        .into());
    }
    let outcome = state.refresh(
        observation.key(),
        observation.head_sha(),
        observation.discussion(),
    )?;
    if outcome != RefreshOutcome::Unchanged {
        storage::save(state_file, &state).map_err(storage_error)?;
    }
    Ok(outcome)
}
