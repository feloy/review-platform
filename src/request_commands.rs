//! Shared PR management, refresh, and lead authorization commands for the CLI and UI.

use crate::{review_commands, storage};
use luthiai::{
    AuthorizationScope, HeadSha, InputRevision, PrKey, RefreshOutcome, StateError,
    github::{GithubClient, GithubError, PullRequest, discussion::PullObservation},
    review::RoundId,
};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::{fs, io};
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

#[derive(Debug, Error)]
pub enum UnmanageError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("local state: {0}")]
    Storage(#[from] anyhow::Error),
    #[error("cannot remove round {round} while its runner lock is unavailable: {source}")]
    RunnerLock {
        round: RoundId,
        #[source]
        source: anyhow::Error,
    },
    #[error("could not remove saved files for round {round}; retry unmanage: {source}")]
    Artifacts {
        round: RoundId,
        #[source]
        source: io::Error,
    },
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

/// Delete a PR's local records and files after all work and recovery have finished.
pub fn unmanage(state_file: &Path, key: &PrKey) -> Result<bool, UnmanageError> {
    let _pr_lock = storage::pr_lock(state_file, key)?;
    let _lock = storage::lock(state_file)?;
    let mut state = storage::load(state_file)?;
    let removed = state.unmanage(key)?;
    let files = removed
        .iter()
        .map(|id| check_round_files(state_file, id).map(|path| (id, path)))
        .collect::<Result<Vec<_>, _>>()?;
    // Check every runner before deleting any files.
    // Delete files before forgetting their IDs so a failed removal can be retried.
    // A partial filesystem failure leaves the recorded request available for retry.
    for (id, path) in files {
        if let Some(path) = path {
            fs::remove_dir_all(&path).map_err(|source| UnmanageError::Artifacts {
                round: id.clone(),
                source,
            })?;
        }
    }
    storage::save(state_file, &state)?;
    Ok(true)
}

fn check_round_files(state_file: &Path, id: &RoundId) -> Result<Option<PathBuf>, UnmanageError> {
    let path = review_commands::round_path(state_file, id);
    let artifacts = |source| UnmanageError::Artifacts {
        round: id.clone(),
        source,
    };
    let root = path.parent().ok_or_else(|| {
        artifacts(io::Error::new(
            io::ErrorKind::InvalidInput,
            "round directory has no parent",
        ))
    })?;
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(artifacts(error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(artifacts(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rounds directory must be a regular directory",
            )));
        }
        Ok(_) => {}
    }
    let metadata = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(artifacts(error)),
        Ok(metadata) => metadata,
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let runner = storage::try_recovery_lock(&path.join("runner")).map_err(|source| {
            UnmanageError::RunnerLock {
                round: id.clone(),
                source,
            }
        })?;
        // The PR lock prevents another runner from starting; release this handle
        // before deleting its directory so Windows can remove the lock file too.
        drop(runner);
    }
    Ok(Some(path))
}

fn storage_error(error: anyhow::Error) -> RefreshError {
    RefreshError::Storage(error.into())
}

/// Refresh one existing request using the same process locks for every caller.
pub fn refresh(state_file: &Path, key: &PrKey) -> Result<RefreshOutcome, RefreshError> {
    Ok(refresh_with_details(state_file, key)?.outcome())
}

/// Display context from the same observation that refreshed the recorded inputs.
pub struct RefreshedRequest {
    observation: PullObservation,
    revision: InputRevision,
    outcome: RefreshOutcome,
}

impl RefreshedRequest {
    pub fn pull(&self) -> &PullRequest {
        self.observation.pull()
    }
    pub fn revision(&self) -> InputRevision {
        self.revision
    }
    pub fn outcome(&self) -> RefreshOutcome {
        self.outcome
    }
}

pub fn refresh_with_details(
    state_file: &Path,
    key: &PrKey,
) -> Result<RefreshedRequest, RefreshError> {
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
    Ok(RefreshedRequest {
        observation,
        revision: state
            .get(key)
            .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
            .revision(),
        outcome,
    })
}
