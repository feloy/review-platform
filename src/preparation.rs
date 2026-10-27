//! Pure readiness and model-proposal decisions for a review round.

use crate::config::{
    AgentId, CoauthorIdentity, ConfigError, ModelId, PromptHash, ResolvedReviewModel, VmArchive,
};
use crate::{Authorization, HeadSha, InputRevision, PrKey, ReviewRequest};
use std::collections::HashSet;
use std::fmt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PreparationError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("observed PR head {observed} differs from recorded head {recorded}")]
    PullHeadMismatch {
        recorded: HeadSha,
        observed: HeadSha,
    },
    #[error("CI results for head {checked} differ from recorded head {recorded}")]
    ChecksHeadMismatch { recorded: HeadSha, checked: HeadSha },
    #[error("review model {model} was resolved for {configured}, expected {expected}")]
    ReviewModelRepositoryMismatch {
        model: ModelId,
        configured: String,
        expected: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullAvailability {
    Open,
    Closed,
}

/// PR availability bound to the head in a verified GitHub snapshot.
#[derive(Clone, Copy)]
pub struct PullObservation<'a> {
    head_sha: &'a HeadSha,
    availability: PullAvailability,
}

impl<'a> PullObservation<'a> {
    pub(crate) fn new(head_sha: &'a HeadSha, availability: PullAvailability) -> Self {
        Self {
            head_sha,
            availability,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Green,
    Pending,
    Failing,
    Unknown,
    NoResults,
}

/// CI summary bound to the exact head queried by the GitHub adapter.
#[derive(Clone, Copy)]
pub struct CiObservation<'a> {
    head_sha: &'a HeadSha,
    state: CiState,
}

impl<'a> CiObservation<'a> {
    pub(crate) fn new(head_sha: &'a HeadSha, state: CiState) -> Self {
        Self { head_sha, state }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    AwaitingAuthorization,
    DiscussionNotObserved,
    PullClosed,
    ChecksPending,
    ChecksFailing,
    ChecksUnknown,
    NoChecks,
    ModelValidationRequired,
}

impl Readiness {
    pub fn permits_validation(self) -> bool {
        matches!(self, Self::Ready | Self::ModelValidationRequired)
    }
}

impl fmt::Display for Readiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Ready => "ready to run",
            Self::AwaitingAuthorization => "lead authorization is required",
            Self::DiscussionNotObserved => "author discussion has not been observed",
            Self::PullClosed => "the PR is closed",
            Self::ChecksPending => "checks for the current head are pending",
            Self::ChecksFailing => "checks for the current head are failing",
            Self::ChecksUnknown => "checks for the current head have an unknown result",
            Self::NoChecks => "no checks were reported for the current head",
            Self::ModelValidationRequired => "lead model validation is required",
        };
        f.write_str(text)
    }
}

/// Readiness and validation evidence bound to the inspected inputs and resolved choices.
#[must_use = "inspect readiness or pass this evidence to model validation"]
pub struct Preparation {
    key: PrKey,
    head_sha: HeadSha,
    revision: InputRevision,
    authorization: Authorization,
    choices: Vec<PreparedChoice>,
    proposed_model: ModelId,
    proposal_matches_coauthor: bool,
    validated_model: Option<ModelId>,
    readiness: Readiness,
}

struct PreparedChoice {
    model: ModelId,
    agent: AgentId,
    prompt_sha256: PromptHash,
    vm_archive: VmArchive,
}

impl PreparedChoice {
    fn from_resolved(choice: &ResolvedReviewModel<'_>) -> Self {
        Self {
            model: choice.model().id().clone(),
            agent: choice.agent().id().clone(),
            prompt_sha256: choice.agent().prompt_hash().clone(),
            vm_archive: choice.archive().clone(),
        }
    }

    fn matches(&self, choice: &ResolvedReviewModel<'_>) -> bool {
        self.model == *choice.model().id()
            && self.agent == *choice.agent().id()
            && self.prompt_sha256 == *choice.agent().prompt_hash()
            && self.vm_archive == *choice.archive()
    }
}

impl Preparation {
    pub(crate) fn key(&self) -> &PrKey {
        &self.key
    }

    pub(crate) fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub(crate) fn revision(&self) -> InputRevision {
        self.revision
    }

    pub(crate) fn authorization(&self) -> Authorization {
        self.authorization
    }

    pub(crate) fn allows_choice(&self, choice: &ResolvedReviewModel<'_>) -> bool {
        choice.repository() == self.key.repository()
            && self
                .choices
                .iter()
                .any(|candidate| candidate.matches(choice))
    }

    pub fn proposed_model(&self) -> &ModelId {
        &self.proposed_model
    }

    pub fn proposal_matches_coauthor(&self) -> bool {
        self.proposal_matches_coauthor
    }

    pub fn validated_model(&self) -> Option<&ModelId> {
        self.validated_model.as_ref()
    }

    pub fn readiness(&self) -> Readiness {
        self.readiness
    }
}

pub fn prepare(
    request: &ReviewRequest,
    pull: PullObservation<'_>,
    ci: CiObservation<'_>,
    review_models: &[ResolvedReviewModel<'_>],
    coauthors: &HashSet<CoauthorIdentity>,
) -> Result<Preparation, PreparationError> {
    if pull.head_sha != request.head_sha() {
        return Err(PreparationError::PullHeadMismatch {
            recorded: request.head_sha().clone(),
            observed: pull.head_sha.clone(),
        });
    }
    if ci.head_sha != request.head_sha() {
        return Err(PreparationError::ChecksHeadMismatch {
            recorded: request.head_sha().clone(),
            checked: ci.head_sha.clone(),
        });
    }
    if let Some(choice) = review_models
        .iter()
        .find(|choice| choice.repository() != request.key().repository())
    {
        return Err(PreparationError::ReviewModelRepositoryMismatch {
            model: choice.model().id().clone(),
            configured: choice.repository().to_owned(),
            expected: request.key().repository().to_owned(),
        });
    }
    let proposal = review_models
        .iter()
        .find(|candidate| !candidate.model().matches_coauthor(coauthors))
        .or_else(|| review_models.first())
        .ok_or_else(|| ConfigError::NoReviewModels(request.key().repository().to_owned()))?;
    let validated = request
        .model_validation()
        .filter(|validation| {
            validation.head_sha() == request.head_sha()
                && validation.revision() == request.revision()
        })
        .and_then(|validation| {
            review_models
                .iter()
                .find(|candidate| validation.matches(candidate))
                .map(|_| validation.model().clone())
        });
    let readiness = if request.authorization() == Authorization::AwaitingLead {
        Readiness::AwaitingAuthorization
    } else if request.discussion().is_none() {
        Readiness::DiscussionNotObserved
    } else {
        match pull.availability {
            PullAvailability::Closed => Readiness::PullClosed,
            PullAvailability::Open => match ci.state {
                CiState::Pending => Readiness::ChecksPending,
                CiState::Failing => Readiness::ChecksFailing,
                CiState::Unknown => Readiness::ChecksUnknown,
                CiState::NoResults => Readiness::NoChecks,
                CiState::Green => {
                    if validated.is_some() {
                        Readiness::Ready
                    } else {
                        Readiness::ModelValidationRequired
                    }
                }
            },
        }
    };
    Ok(Preparation {
        key: request.key().clone(),
        head_sha: request.head_sha().clone(),
        revision: request.revision(),
        authorization: request.authorization(),
        choices: review_models
            .iter()
            .map(PreparedChoice::from_resolved)
            .collect(),
        proposed_model: proposal.model().id().clone(),
        proposal_matches_coauthor: proposal.model().matches_coauthor(coauthors),
        validated_model: validated,
        readiness,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::VerifiedArchive;
    use crate::config::ReviewConfig;
    use crate::{AuthorizationScope, HeadSha, InputRevision, ObservedDiscussion, PrKey, State};
    use std::num::NonZeroU64;

    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn config_json() -> serde_json::Value {
        let archive = |name: &str| std::env::temp_dir().join(name).to_str().unwrap().to_owned();
        serde_json::json!({
            "schema_version": 2,
            "models": [
                {"id":"model-alpha", "coauthors":[{"name":"Model Alpha", "email":"model@example.com"}]},
                {"id":"model-beta", "coauthors":[{"name":"Model Beta", "email":"model@example.com"}]}
            ],
            "agents": [
                {"id":"claude", "models":["model-alpha", "model-beta"], "prompt":"Review."},
                {"id":"codex", "models":["model-beta"], "prompt":"Review carefully."}
            ],
            "repositories": [{
                "name":"owner/repo",
                "review_models":["model-alpha", "model-beta"],
                "runners":[
                    {"agent":"codex", "vm_archive":{"path":archive("codex.tar"), "sha256":"b".repeat(64)}},
                    {"agent":"claude", "vm_archive":{"path":archive("claude.tar"), "sha256":"a".repeat(64)}}
                ]
            }]
        })
    }

    fn config() -> ReviewConfig {
        ReviewConfig::from_json(&serde_json::to_vec(&config_json()).unwrap()).unwrap()
    }

    fn authorized_state() -> (State, PrKey, HeadSha, InputRevision) {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let head = HeadSha::new(HEAD).unwrap();
        let discussion = ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap();
        let mut state = State::default();
        state.record_request(key.clone(), head.clone()).unwrap();
        state.refresh(&key, &head, &discussion).unwrap();
        let revision = InputRevision::new(2).unwrap();
        state
            .authorize(&key, &head, revision, AuthorizationScope::OneRound)
            .unwrap();
        (state, key, head, revision)
    }

    fn prepare_for_head(
        request: &ReviewRequest,
        head: &HeadSha,
        pull: PullAvailability,
        ci: CiState,
        review_models: &[ResolvedReviewModel<'_>],
        coauthors: &HashSet<CoauthorIdentity>,
    ) -> Result<Preparation, PreparationError> {
        prepare(
            request,
            PullObservation::new(head, pull),
            CiObservation::new(head, ci),
            review_models,
            coauthors,
        )
    }

    #[test]
    fn proposal_avoids_coauthor_model_and_resolves_a_compatible_agent() {
        let config = config();
        let (mut state, key, head, revision) = authorized_state();
        let review_models = config.review_models_for(&key).unwrap();
        let coauthors =
            HashSet::from([CoauthorIdentity::new("Model Alpha", "model@example.com").unwrap()]);
        let result = prepare_for_head(
            state.get(&key).unwrap(),
            &head,
            PullAvailability::Open,
            CiState::Green,
            &review_models,
            &coauthors,
        )
        .unwrap();
        assert_eq!(result.proposed_model().as_str(), "model-beta");
        assert_eq!(review_models[1].agent().id().as_str(), "codex");
        assert_eq!(result.readiness(), Readiness::ModelValidationRequired);

        let verified = VerifiedArchive::for_test(review_models[0].archive());
        state
            .validate_model(&result, &head, revision, &review_models[0], &verified)
            .unwrap();
        let result = prepare_for_head(
            state.get(&key).unwrap(),
            &head,
            PullAvailability::Open,
            CiState::Green,
            &review_models,
            &coauthors,
        )
        .unwrap();
        assert_eq!(result.validated_model().unwrap().as_str(), "model-alpha");
        assert_eq!(result.readiness(), Readiness::Ready);

        let all_coauthors = HashSet::from([
            CoauthorIdentity::new("Model Alpha", "model@example.com").unwrap(),
            CoauthorIdentity::new("Model Beta", "model@example.com").unwrap(),
        ]);
        let result = prepare_for_head(
            state.get(&key).unwrap(),
            &head,
            PullAvailability::Open,
            CiState::Green,
            &review_models,
            &all_coauthors,
        )
        .unwrap();
        assert_eq!(result.proposed_model().as_str(), "model-alpha");
        assert!(result.proposal_matches_coauthor());

        let result = prepare_for_head(
            state.get(&key).unwrap(),
            &head,
            PullAvailability::Open,
            CiState::Green,
            &review_models,
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(result.proposed_model().as_str(), "model-alpha");
        assert!(!result.proposal_matches_coauthor());
    }

    #[test]
    fn preparation_rejects_pull_and_ci_results_for_other_heads() {
        let config = config();
        let (state, key, head, _) = authorized_state();
        let different_head = HeadSha::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let review_models = config.review_models_for(&key).unwrap();
        let coauthors = HashSet::new();
        assert!(matches!(
            prepare(
                state.get(&key).unwrap(),
                PullObservation::new(&different_head, PullAvailability::Open),
                CiObservation::new(&head, CiState::Green),
                &review_models,
                &coauthors,
            ),
            Err(PreparationError::PullHeadMismatch { .. })
        ));
        assert!(matches!(
            prepare(
                state.get(&key).unwrap(),
                PullObservation::new(&head, PullAvailability::Open),
                CiObservation::new(&different_head, CiState::Green),
                &review_models,
                &coauthors,
            ),
            Err(PreparationError::ChecksHeadMismatch { .. })
        ));
        let mut raw = config_json();
        let mut other = raw["repositories"][0].clone();
        other["name"] = "other/repo".into();
        raw["repositories"].as_array_mut().unwrap().push(other);
        let config = ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let other_key = PrKey::new("other/repo", 42).unwrap();
        let other_models = config.review_models_for(&other_key).unwrap();
        assert!(matches!(
            prepare(
                state.get(&key).unwrap(),
                PullObservation::new(&head, PullAvailability::Open),
                CiObservation::new(&head, CiState::Green),
                &other_models,
                &coauthors,
            ),
            Err(PreparationError::ReviewModelRepositoryMismatch { .. })
        ));
    }

    #[test]
    fn gates_and_changed_configuration_prevent_readiness() {
        let config = config();
        let (mut state, key, head, revision) = authorized_state();
        let review_models = config.review_models_for(&key).unwrap();
        let empty = HashSet::new();
        let prepared = prepare_for_head(
            state.get(&key).unwrap(),
            &head,
            PullAvailability::Open,
            CiState::Green,
            &review_models,
            &empty,
        )
        .unwrap();
        let verified = VerifiedArchive::for_test(review_models[0].archive());
        state
            .validate_model(&prepared, &head, revision, &review_models[0], &verified)
            .unwrap();
        for (pull, ci, expected) in [
            (
                PullAvailability::Closed,
                CiState::Green,
                Readiness::PullClosed,
            ),
            (
                PullAvailability::Open,
                CiState::Pending,
                Readiness::ChecksPending,
            ),
            (
                PullAvailability::Open,
                CiState::Failing,
                Readiness::ChecksFailing,
            ),
            (
                PullAvailability::Open,
                CiState::Unknown,
                Readiness::ChecksUnknown,
            ),
            (
                PullAvailability::Open,
                CiState::NoResults,
                Readiness::NoChecks,
            ),
        ] {
            assert_eq!(
                prepare_for_head(
                    state.get(&key).unwrap(),
                    &head,
                    pull,
                    ci,
                    &review_models,
                    &empty
                )
                .unwrap()
                .readiness(),
                expected
            );
        }
        let mut changed = config_json();
        changed["agents"][0]["prompt"] = "A changed prompt.".into();
        let changed = ReviewConfig::from_json(&serde_json::to_vec(&changed).unwrap()).unwrap();
        let changed_review_models = changed.review_models_for(&key).unwrap();
        assert_eq!(
            prepare_for_head(
                state.get(&key).unwrap(),
                &head,
                PullAvailability::Open,
                CiState::Green,
                &changed_review_models,
                &empty
            )
            .unwrap()
            .readiness(),
            Readiness::ModelValidationRequired
        );
        let mut changed_archive = config_json();
        changed_archive["repositories"][0]["runners"][1]["vm_archive"]["sha256"] =
            "c".repeat(64).into();
        let changed_archive =
            ReviewConfig::from_json(&serde_json::to_vec(&changed_archive).unwrap()).unwrap();
        let changed_review_models = changed_archive.review_models_for(&key).unwrap();
        assert_eq!(
            prepare_for_head(
                state.get(&key).unwrap(),
                &head,
                PullAvailability::Open,
                CiState::Green,
                &changed_review_models,
                &empty
            )
            .unwrap()
            .readiness(),
            Readiness::ModelValidationRequired
        );
        let mut changed_runner = config_json();
        changed_runner["agents"][1]["models"] = serde_json::json!(["model-alpha", "model-beta"]);
        let changed_runner =
            ReviewConfig::from_json(&serde_json::to_vec(&changed_runner).unwrap()).unwrap();
        let changed_review_models = changed_runner.review_models_for(&key).unwrap();
        assert_eq!(changed_review_models[0].agent().id().as_str(), "codex");
        assert_eq!(
            prepare_for_head(
                state.get(&key).unwrap(),
                &head,
                PullAvailability::Open,
                CiState::Green,
                &changed_review_models,
                &empty
            )
            .unwrap()
            .readiness(),
            Readiness::ModelValidationRequired
        );
        state.revoke(&key).unwrap();
        assert_eq!(
            prepare_for_head(
                state.get(&key).unwrap(),
                &head,
                PullAvailability::Open,
                CiState::Green,
                &review_models,
                &empty
            )
            .unwrap()
            .readiness(),
            Readiness::AwaitingAuthorization
        );
    }
}
