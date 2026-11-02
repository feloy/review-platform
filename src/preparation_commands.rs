//! Shared preparation and model-validation orchestration with locked persistence.

use crate::storage;
use review_platform::{
    Authorization, ModelValidationError, PrKey, RefreshOutcome, ReviewRequest, StateError,
    archive::{self, ArchiveError},
    config::{
        AgentId, AgentKind, ConfigError, ModelId, PromptHash, ResolvedReviewModel, ReviewConfig,
        VmArchive,
    },
    github::{GithubClient, GithubError, PullRequest, checks::CheckSummary},
    preparation::Preparation,
    round::{self, ModelSelection, RoundError},
};
use serde::Deserialize;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrepareError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("reading review configuration {}: {source}", path.display())]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Github(#[from] GithubError),
    #[error(transparent)]
    Round(#[from] RoundError),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Validation(#[from] ModelValidationError),
    #[error("configuration or authorization changed; prepare and inspect the review again")]
    StaleRules,
    #[error("local state: {0}")]
    Storage(#[source] Box<dyn Error + Send + Sync>),
}

impl From<anyhow::Error> for PrepareError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error.into())
    }
}

/// Untrusted input naming the rules shown to the lead before model validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectedRules {
    authorization: Authorization,
    configuration_sha256: PromptHash,
}

impl InspectedRules {
    pub fn authorization(&self) -> Authorization {
        self.authorization
    }
    pub fn configuration_sha256(&self) -> &PromptHash {
        &self.configuration_sha256
    }
}

/// Read-only display details of one model resolved from trusted configuration.
pub struct PreparedModel {
    model: ModelId,
    agent: AgentId,
    runner_kind: AgentKind,
    model_provider: String,
    prompt_sha256: PromptHash,
    runner_sha256: PromptHash,
    archive: VmArchive,
}

impl PreparedModel {
    fn from_resolved(choice: &ResolvedReviewModel<'_>) -> Self {
        Self {
            model: choice.model().id().clone(),
            agent: choice.agent().id().clone(),
            runner_kind: choice.agent().runner().kind(),
            model_provider: choice.agent().runner().model_provider().to_owned(),
            prompt_sha256: choice.agent().prompt_hash().clone(),
            runner_sha256: choice.agent().runner().hash().clone(),
            archive: choice.archive().clone(),
        }
    }
    pub fn model(&self) -> &ModelId {
        &self.model
    }
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }
    pub fn runner_kind(&self) -> AgentKind {
        self.runner_kind
    }
    pub fn model_provider(&self) -> &str {
        &self.model_provider
    }
    pub fn prompt_sha256(&self) -> &PromptHash {
        &self.prompt_sha256
    }
    pub fn runner_sha256(&self) -> &PromptHash {
        &self.runner_sha256
    }
    pub fn archive(&self) -> &VmArchive {
        &self.archive
    }
}

/// An owned snapshot for display; callers cannot construct or change its evidence.
pub struct PreparedReview {
    request: ReviewRequest,
    pull: PullRequest,
    changes: RefreshOutcome,
    preparation: Preparation,
    checks: CheckSummary,
    models: Vec<PreparedModel>,
    resolved_index: usize,
    configuration_sha256: PromptHash,
}

impl PreparedReview {
    pub fn pull(&self) -> &PullRequest {
        &self.pull
    }
    pub fn changes(&self) -> RefreshOutcome {
        self.changes
    }

    pub fn request(&self) -> &ReviewRequest {
        &self.request
    }
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }
    pub fn checks(&self) -> CheckSummary {
        self.checks
    }
    pub fn models(&self) -> &[PreparedModel] {
        &self.models
    }
    pub fn resolved(&self) -> &PreparedModel {
        // Construction finds this index in the same immutable model vector.
        &self.models[self.resolved_index]
    }
    pub fn configuration_sha256(&self) -> &PromptHash {
        &self.configuration_sha256
    }
}

fn read_config(path: &Path) -> Result<ReviewConfig, PrepareError> {
    let bytes = fs::read(path).map_err(|source| PrepareError::ReadConfig {
        path: path.to_owned(),
        source,
    })?;
    Ok(ReviewConfig::from_json(&bytes, path)?)
}

fn check_rules(
    request: &ReviewRequest,
    hash: &PromptHash,
    inspected: Option<&InspectedRules>,
) -> Result<(), PrepareError> {
    if inspected.is_some_and(|rules| {
        rules.authorization() != request.authorization() || rules.configuration_sha256() != hash
    }) {
        return Err(PrepareError::StaleRules);
    }
    Ok(())
}

/// Observe GitHub inputs and prepare, optionally persisting a verified lead model choice.
/// GUI callers supply the rules displayed with the choice; the CLI inspects them directly.
pub fn prepare(
    state_file: &Path,
    config_file: &Path,
    key: &PrKey,
    selection: Option<&ModelSelection>,
    inspected_rules: Option<&InspectedRules>,
) -> Result<PreparedReview, PrepareError> {
    let _pr_lock = storage::pr_lock(state_file, key)?;
    let config = read_config(config_file)?;
    let configuration_sha256 = config.hash().clone();
    let initial = storage::load(state_file)?;
    let initial_request = initial
        .get(key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
    if let (Some(selected), Some(_)) = (selection, inspected_rules) {
        initial_request.ensure_inspected(selected.head(), selected.revision())?;
    }
    check_rules(initial_request, &configuration_sha256, inspected_rules)?;
    let prior_revision = initial_request.revision();
    let client = GithubClient::new()?;
    let observation = client.observe_pull_excluding(key, &initial.confirmed_review_ids(key))?;
    let lock = storage::lock(state_file)?;
    let mut state = storage::load(state_file)?;
    let request = state
        .get(key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
    if request.revision() != prior_revision {
        return Err(StateError::RevisionMismatch {
            inspected: prior_revision,
            recorded: request.revision(),
        }
        .into());
    }
    check_rules(request, &configuration_sha256, inspected_rules)?;
    let refreshed = round::refresh_observation(&mut state, &config, observation)?;
    let changes = refreshed.outcome();
    if refreshed.outcome() != RefreshOutcome::Unchanged {
        storage::save(state_file, &state)?;
    }
    drop(lock);
    let report = round::prepare(&client, &mut state, refreshed, selection)?;
    if read_config(config_file)?.hash() != config.hash() {
        return Err(PrepareError::StaleRules);
    }
    if let Some(selected) = selection {
        let verified = archive::verify_archive(report.resolved().archive())?;
        let _lock = storage::lock(state_file)?;
        let mut current = storage::load(state_file)?;
        check_rules(
            current
                .get(key)
                .ok_or_else(|| StateError::RequestNotFound(key.clone()))?,
            &configuration_sha256,
            inspected_rules,
        )?;
        if read_config(config_file)?.hash() != config.hash() {
            return Err(PrepareError::StaleRules);
        }
        if current.validate_model(
            report.preparation(),
            selected.head(),
            selected.revision(),
            report.resolved(),
            &verified,
        )? {
            storage::save(state_file, &current)?;
        }
        state = current;
    }
    let models: Vec<_> = config
        .review_models_for(key)?
        .iter()
        .map(PreparedModel::from_resolved)
        .collect();
    let resolved_index = models
        .iter()
        .position(|choice| choice.model() == report.resolved().model().id())
        .ok_or_else(|| ConfigError::NoReviewModels(key.repository().to_owned()))?;
    let request = state
        .get(key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
        .clone();
    // Move the library's evidence into the display snapshot without making it forgeable.
    let checks = report.checks();
    let (preparation, pull) = report.into_parts();
    Ok(PreparedReview {
        request,
        pull,
        changes,
        preparation,
        checks,
        models,
        resolved_index,
        configuration_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_platform::{AuthorizationScope, HeadSha, InputRevision, ObservedDiscussion, State};
    use std::num::NonZeroU64;

    fn fixture(directory: &Path) -> (PathBuf, PathBuf, PrKey, HeadSha, InspectedRules) {
        let state_file = directory.join("state.json");
        let config_file = directory.join("config.json");
        let key = PrKey::new("owner/repo", 15).unwrap();
        let head = HeadSha::new("a".repeat(40)).unwrap();
        let mut state = State::default();
        state.record_request(key.clone(), head.clone()).unwrap();
        state
            .refresh(
                &key,
                &head,
                &ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap(),
            )
            .unwrap();
        state
            .authorize(
                &key,
                &head,
                InputRevision::new(2).unwrap(),
                AuthorizationScope::OneRound,
            )
            .unwrap();
        storage::save(&state_file, &state).unwrap();
        let config = serde_json::to_vec(&serde_json::json!({
            "schema_version": 6,
            "execution": {"openshell": directory.join("openshell"), "gateway":"gateway", "workspace":"default", "driver":"vm", "github_provider":"github-ro"},
            "models": [{"id":"model-alpha", "coauthors":[]}],
            "agents": [{"id":"claude", "models":["model-alpha"], "prompt_file":"review.md", "runner": {
                "kind":"claude", "model_provider":"model-provider", "policy": directory.join("policy.yaml"), "policy_sha256":"b".repeat(64), "hold_command":["/bin/sleep","3600"]
            }}],
            "repositories": [{"name":"owner/repo", "review_models":["model-alpha"], "runners":[{
                "agent":"claude", "vm_archive":{"path": directory.join("reviewer.tar"), "sha256":"a".repeat(64), "config_sha256":"c".repeat(64)}
            }]}],
        })).unwrap();
        fs::write(directory.join("review.md"), "Review the pull request.\n").unwrap();
        fs::write(&config_file, &config).unwrap();
        let rules = InspectedRules {
            authorization: Authorization::OneRound,
            configuration_sha256: read_config(&config_file).unwrap().hash().clone(),
        };
        (state_file, config_file, key, head, rules)
    }

    #[test]
    fn gui_validation_rejects_stale_inputs_and_rules_before_fetching_github() {
        let directory = tempfile::tempdir().unwrap();
        let (state_file, config_file, key, head, rules) = fixture(directory.path());
        let original = fs::read(&state_file).unwrap();
        for (inspected_head, revision, head_mismatch) in [
            (HeadSha::new("b".repeat(40)).unwrap(), 2, true),
            (head.clone(), 1, false),
        ] {
            let selection = ModelSelection::new(
                ModelId::new("model-alpha").unwrap(),
                inspected_head,
                InputRevision::new(revision).unwrap(),
            );
            let error = prepare(
                &state_file,
                &config_file,
                &key,
                Some(&selection),
                Some(&rules),
            )
            .err()
            .unwrap();
            assert_eq!(
                matches!(error, PrepareError::State(StateError::HeadMismatch { .. })),
                head_mismatch
            );
            assert_eq!(
                matches!(
                    error,
                    PrepareError::State(StateError::RevisionMismatch { .. })
                ),
                !head_mismatch
            );
        }
        let selection = ModelSelection::new(
            ModelId::new("model-alpha").unwrap(),
            head,
            InputRevision::new(2).unwrap(),
        );
        for stale in [
            InspectedRules {
                authorization: Authorization::PrCycle,
                configuration_sha256: rules.configuration_sha256.clone(),
            },
            InspectedRules {
                authorization: rules.authorization,
                configuration_sha256: PromptHash::new("f".repeat(64)).unwrap(),
            },
        ] {
            let error = prepare(
                &state_file,
                &config_file,
                &key,
                Some(&selection),
                Some(&stale),
            )
            .err()
            .unwrap();
            assert!(matches!(error, PrepareError::StaleRules));
        }
        assert_eq!(fs::read(state_file).unwrap(), original);
    }

    #[test]
    fn gui_validation_rejects_prompt_edits_before_fetching_github() {
        let directory = tempfile::tempdir().unwrap();
        let (state_file, config_file, key, head, rules) = fixture(directory.path());
        let original_state = fs::read(&state_file).unwrap();
        let original_config = fs::read(&config_file).unwrap();
        fs::write(
            directory.path().join("review.md"),
            "Changed instructions.\n",
        )
        .unwrap();
        let selection = ModelSelection::new(
            ModelId::new("model-alpha").unwrap(),
            head,
            InputRevision::new(2).unwrap(),
        );
        let error = prepare(
            &state_file,
            &config_file,
            &key,
            Some(&selection),
            Some(&rules),
        )
        .err()
        .unwrap();
        assert!(matches!(error, PrepareError::StaleRules));
        assert_eq!(fs::read(&state_file).unwrap(), original_state);
        assert_eq!(fs::read(&config_file).unwrap(), original_config);
    }

    #[test]
    fn preparation_reports_configuration_and_missing_request_errors_without_state_changes() {
        let directory = tempfile::tempdir().unwrap();
        let (state_file, config_file, _, _, _) = fixture(directory.path());
        let original = fs::read(&state_file).unwrap();
        let key = PrKey::new("owner/repo", 16).unwrap();
        let error = prepare(&state_file, &config_file, &key, None, None)
            .err()
            .unwrap();
        assert!(matches!(
            error,
            PrepareError::State(StateError::RequestNotFound(_))
        ));
        fs::write(&config_file, br#"{"schema_version":999}"#).unwrap();
        let error = prepare(&state_file, &config_file, &key, None, None)
            .err()
            .unwrap();
        assert!(matches!(
            error,
            PrepareError::Config(ConfigError::UnsupportedSchema(999))
        ));
        let error = prepare(
            &state_file,
            &directory.path().join("missing.json"),
            &key,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(error, PrepareError::ReadConfig { .. }));
        assert_eq!(fs::read(state_file).unwrap(), original);
    }
}
