//! Stable GitHub evidence for preparing a review round.

use super::checks::{CheckReport, CheckSummary};
use super::commits::CommitCoauthors;
use super::discussion::PullObservation as DiscussionObservation;
use super::{GithubClient, GithubError, GithubResult, PullRequest};
use crate::HeadSha;
use crate::config::CoauthorIdentity;
use crate::preparation::{CiObservation, PullObservation};
use std::collections::HashSet;

/// GitHub inputs checked against one PR head and author discussion.
pub struct PreparationInputs {
    pull: PullRequest,
    checks: CheckReport,
    coauthors: CommitCoauthors,
}

impl PreparationInputs {
    pub fn head_sha(&self) -> &HeadSha {
        self.pull.head_sha()
    }

    pub fn pull_observation(&self) -> PullObservation<'_> {
        self.pull.preparation_observation()
    }

    pub fn ci_observation(&self) -> CiObservation<'_> {
        self.checks.preparation_observation()
    }

    pub fn coauthors(&self) -> &HashSet<CoauthorIdentity> {
        self.coauthors.identities()
    }

    pub fn checks_summary(&self) -> CheckSummary {
        self.checks.summary()
    }
}

impl GithubClient {
    /// Gather exact-head checks and commit hints, then recheck the PR and discussion.
    pub fn preparation_inputs(
        &self,
        observed: DiscussionObservation,
    ) -> GithubResult<PreparationInputs> {
        let key = observed.key();
        let pull = self.pull_request(key)?;
        if pull.head_sha() != observed.head_sha() {
            return Err(GithubError::HeadChanged {
                key: key.clone(),
                expected: observed.head_sha().clone(),
                actual: pull.head_sha().clone(),
            });
        }
        if pull.author_id() != observed.author_id() {
            return Err(GithubError::AuthorChanged { key: key.clone() });
        }
        let checks = self.checks_for_pull(key, &pull)?;
        let coauthors = self.coauthors_for_pull(key, &pull)?;
        let final_observation =
            self.observe_pull_excluding(key, observed.confirmed_review_ids())?;
        if final_observation.head_sha() != observed.head_sha()
            || final_observation.discussion() != observed.discussion()
        {
            return Err(GithubError::ObservationChanged);
        }
        let final_pull = self.pull_request(key)?;
        if final_pull.head_sha() != pull.head_sha() {
            return Err(GithubError::HeadChanged {
                key: key.clone(),
                expected: pull.head_sha().clone(),
                actual: final_pull.head_sha().clone(),
            });
        }
        if final_pull.author_id() != pull.author_id() {
            return Err(GithubError::AuthorChanged { key: key.clone() });
        }
        Ok(PreparationInputs {
            pull: final_pull,
            checks,
            coauthors,
        })
    }
}
