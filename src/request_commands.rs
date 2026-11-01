//! Shared request refresh and lead authorization commands for the CLI and UI.

use crate::storage;
use review_platform::{
    AuthorizationScope, HeadSha, InputRevision, PrKey, RefreshOutcome, StateError,
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

#[derive(Debug, Error)]
pub enum AuthorizationError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("local state: {0}")]
    Storage(#[source] Box<dyn Error + Send + Sync>),
}

impl From<anyhow::Error> for AuthorizationError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error.into())
    }
}

/// Authorize only the inputs inspected by the lead, under the shared state lock.
pub fn authorize(
    state_file: &Path,
    key: &PrKey,
    head: &HeadSha,
    revision: InputRevision,
    scope: AuthorizationScope,
) -> Result<bool, AuthorizationError> {
    let _lock = storage::lock(state_file)?;
    let mut state = storage::load(state_file)?;
    let changed = state.authorize(key, head, revision, scope)?;
    if changed {
        storage::save(state_file, &state)?;
    }
    Ok(changed)
}

/// Revoke authorization without requiring the lead to match the current inputs.
pub fn revoke(state_file: &Path, key: &PrKey) -> Result<bool, AuthorizationError> {
    let _lock = storage::lock(state_file)?;
    let mut state = storage::load(state_file)?;
    let changed = state.revoke(key)?;
    if changed {
        storage::save(state_file, &state)?;
    }
    Ok(changed)
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
