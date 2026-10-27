//! Reusable orchestration of review preparation across the CLI and future daemon.

use crate::archive::{self, ArchiveError};
use crate::config::{ConfigError, ModelId, ResolvedReviewModel, ReviewConfig};
use crate::github::checks::CheckSummary;
use crate::github::discussion::PullObservation;
use crate::github::{GithubClient, GithubError};
use crate::preparation::{self, Preparation, PreparationError};
use crate::{HeadSha, InputRevision, PrKey, RefreshOutcome, State, StateError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RoundError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Github(#[from] GithubError),
    #[error(transparent)]
    Preparation(#[from] PreparationError),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Validation(#[from] crate::ModelValidationError),
    #[error("recorded PR inputs changed since refresh for {0}")]
    StaleObservation(PrKey),
}

/// The lead's model choice, bound to the head and revision they inspected.
pub struct ModelSelection {
    model: ModelId,
    head: HeadSha,
    revision: InputRevision,
}

impl ModelSelection {
    pub fn new(model: ModelId, head: HeadSha, revision: InputRevision) -> Self {
        Self {
            model,
            head,
            revision,
        }
    }

    pub fn model(&self) -> &ModelId {
        &self.model
    }
}

/// A refreshed request, ready for the caller to persist before further reads.
#[must_use = "persist refreshed state before gathering more evidence"]
pub struct RefreshedRound<'a> {
    observation: PullObservation,
    models: Vec<ResolvedReviewModel<'a>>,
    outcome: RefreshOutcome,
}

impl RefreshedRound<'_> {
    pub fn outcome(&self) -> RefreshOutcome {
        self.outcome
    }
}

/// Prepared review details needed by an operator or round starter.
#[must_use = "inspect readiness before starting a review round"]
pub struct RoundPreparation<'a> {
    preparation: Preparation,
    resolved: ResolvedReviewModel<'a>,
    checks: CheckSummary,
    validation_changed: bool,
}

impl<'a> RoundPreparation<'a> {
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    pub fn resolved(&self) -> &ResolvedReviewModel<'a> {
        &self.resolved
    }

    pub fn checks(&self) -> CheckSummary {
        self.checks
    }

    pub fn validation_changed(&self) -> bool {
        self.validation_changed
    }
}

/// Refresh recorded inputs before the caller persists them and continues.
pub fn refresh<'a>(
    client: &GithubClient,
    state: &mut State,
    config: &'a ReviewConfig,
    key: &PrKey,
) -> Result<RefreshedRound<'a>, RoundError> {
    let models = config.review_models_for(key)?;
    if state.get(key).is_none() {
        return Err(StateError::RequestNotFound(key.clone()).into());
    }
    let observation = client.observe_pull(key)?;
    let outcome = state.refresh(
        observation.key(),
        observation.head_sha(),
        observation.discussion(),
    )?;
    Ok(RefreshedRound {
        observation,
        models,
        outcome,
    })
}

/// Recheck GitHub evidence, then verify the archive for the resolved choice.
pub fn prepare<'a>(
    client: &GithubClient,
    state: &mut State,
    refreshed: RefreshedRound<'a>,
    selection: Option<&ModelSelection>,
) -> Result<RoundPreparation<'a>, RoundError> {
    let RefreshedRound {
        observation,
        mut models,
        ..
    } = refreshed;
    let key = observation.key().clone();
    let request = state
        .get(&key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
    if request.head_sha() != observation.head_sha()
        || request.discussion() != Some(observation.discussion())
    {
        return Err(RoundError::StaleObservation(key.clone()));
    }
    if let Some(choice) = selection {
        request.ensure_inspected(&choice.head, choice.revision)?;
    }

    let inputs = client.preparation_inputs(observation)?;
    let request = state
        .get(&key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
    let mut preparation = preparation::prepare(
        request,
        inputs.pull_observation(),
        inputs.ci_observation(),
        &models,
        inputs.coauthors(),
    )?;
    let selected_index = selection
        .map(|choice| {
            models
                .iter()
                .position(|candidate| candidate.model().id() == &choice.model)
                .ok_or_else(|| ConfigError::ModelNotConfigured {
                    repository: key.repository().to_owned(),
                    model: choice.model.clone(),
                })
        })
        .transpose()?;
    let mut verified_index = None;
    let mut validation_changed = false;
    if let (Some(choice), Some(index)) = (selection, selected_index) {
        if preparation.readiness().permits_validation() {
            let chosen = &models[index];
            let verified = archive::verify_archive(chosen.archive())?;
            validation_changed = state.validate_model(
                &preparation,
                &choice.head,
                choice.revision,
                chosen,
                &verified,
            )?;
            verified_index = Some(index);
            let request = state
                .get(&key)
                .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
            preparation = preparation::prepare(
                request,
                inputs.pull_observation(),
                inputs.ci_observation(),
                &models,
                inputs.coauthors(),
            )?;
        }
    }
    let resolved_index = selected_index
        .or_else(|| {
            preparation.validated_model().and_then(|model| {
                models
                    .iter()
                    .position(|candidate| candidate.model().id() == model)
            })
        })
        .or_else(|| {
            models
                .iter()
                .position(|candidate| candidate.model().id() == preparation.proposed_model())
        })
        .ok_or_else(|| ConfigError::NoReviewModels(key.repository().to_owned()))?;
    if preparation.readiness().permits_validation() && verified_index != Some(resolved_index) {
        let _verified = archive::verify_archive(models[resolved_index].archive())?;
    }
    let resolved = models.remove(resolved_index);
    Ok(RoundPreparation {
        preparation,
        resolved,
        checks: inputs.checks_summary(),
        validation_changed,
    })
}
