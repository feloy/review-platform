//! Shared, read-only private draft inspection and GitHub freshness checks.

use crate::{review_commands::round_path, storage};
use review_platform::{
    PrKey, State,
    github::GithubClient,
    publication::SealedBody,
    review::{self, ArtifactError, DraftOutput, Freshness, RoundId, RoundRecord},
};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DraftError {
    #[error("round {0} does not exist")]
    NotFound(RoundId),
    #[error("round {0} belongs to another PR")]
    WrongRequest(RoundId),
    #[error("round {0} has no saved draft")]
    Unavailable(RoundId),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("saved draft differs from the original agent artifact")]
    ArtifactMismatch,
    #[error("private draft differs from the recorded publication seal")]
    SealChanged,
    #[error("local state: {0}")]
    Storage(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl From<anyhow::Error> for DraftError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error.into())
    }
}

/// Verified contents and the fixed round they belong to, with freshness at inspection.
pub struct InspectedDraft {
    record: RoundRecord,
    draft: DraftOutput,
    seal: SealedBody,
    freshness: Freshness,
}

impl InspectedDraft {
    pub fn record(&self) -> &RoundRecord {
        &self.record
    }
    pub fn draft(&self) -> &DraftOutput {
        &self.draft
    }
    pub fn seal(&self) -> &SealedBody {
        &self.seal
    }
    pub fn freshness(&self) -> Freshness {
        self.freshness
    }
    pub fn into_contents(self) -> (DraftOutput, SealedBody) {
        (self.draft, self.seal)
    }
}

/// Verify immutable draft files against each other and any existing state seal.
/// CLI decision commands may subsequently persist a missing seal under their lock.
pub fn read_verified(
    state_path: &Path,
    state: &State,
    key: &PrKey,
    id: &RoundId,
) -> Result<InspectedDraft, DraftError> {
    let record = state
        .round(id)
        .ok_or_else(|| DraftError::NotFound(id.clone()))?;
    if record.key() != key {
        return Err(DraftError::WrongRequest(id.clone()));
    }
    if !record.status().has_saved_draft() {
        return Err(DraftError::Unavailable(id.clone()));
    }
    let base = round_path(state_path, id);
    let draft = review::read_saved_draft(&base.join("draft.json"))?;
    let (agent, artifact_hash) = review::read_agent_output_with_hash(
        &base.join("output/review-output.json"),
        String::new(),
    )?;
    if draft.status() != agent.status()
        || draft.comments() != agent.comments()
        || draft.lead_advice() != agent.lead_advice()
    {
        return Err(DraftError::ArtifactMismatch);
    }
    let seal = SealedBody::new(draft.comments(), id.as_str(), artifact_hash);
    if record.sealed_body().is_some_and(|recorded| {
        recorded.body() != seal.body()
            || recorded.hash() != seal.hash()
            || recorded.artifact_hash() != seal.artifact_hash()
    }) {
        return Err(DraftError::SealChanged);
    }
    Ok(InspectedDraft {
        record: record.clone(),
        draft,
        seal,
        freshness: Freshness::Unknown,
    })
}

pub fn github_freshness(record: &RoundRecord, confirmed_ids: &[u64]) -> Freshness {
    GithubClient::new()
        .and_then(|client| {
            let observation = client.observe_pull_excluding(record.key(), confirmed_ids)?;
            let pull = client.pull_request(record.key())?;
            Ok(record.publication_freshness(
                observation.head_sha(),
                observation.discussion(),
                pull.head_sha(),
                pull.availability(),
            ))
        })
        .unwrap_or(Freshness::Unknown)
}

pub fn inspect(state_path: &Path, key: &PrKey, id: &RoundId) -> Result<InspectedDraft, DraftError> {
    inspect_with_freshness(state_path, key, id, github_freshness)
}

pub(crate) fn inspect_with_freshness(
    state_path: &Path,
    key: &PrKey,
    id: &RoundId,
    check: impl FnOnce(&RoundRecord, &[u64]) -> Freshness,
) -> Result<InspectedDraft, DraftError> {
    // State writes replace the whole file atomically. Inspection never persists state.
    let state = storage::load(state_path)?;
    let mut inspected = read_verified(state_path, &state, key, id)?;
    inspected.freshness = check(inspected.record(), &state.confirmed_review_ids(key));
    Ok(inspected)
}
