//! Review requests, authorization, and model-validation transitions.

use crate::archive::VerifiedArchive;
use crate::config::{AgentId, ModelId, PromptHash, ResolvedReviewModel, VmArchive};
use crate::model::{
    Authorization, AuthorizationScope, HeadSha, InputRevision, ObservedDiscussion, PrKey,
    StateError,
};
use crate::preparation::{Preparation, Readiness};
use crate::review::{
    FORMAT_RULES, FailureKind, Freshness, RoundId, RoundRecord, RoundStateError, RoundStatus,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use thiserror::Error;

const SCHEMA_VERSION: u32 = 5;

#[derive(Debug, Error)]
pub enum ModelValidationError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("preparation no longer matches the inspected inputs or authorization")]
    StalePreparation,
    #[error("model validation is blocked: {0}")]
    Blocked(Readiness),
    #[error("selected model {0} was not resolved in this preparation")]
    ModelNotPrepared(ModelId),
    #[error("the selected model's VM archive was not verified")]
    ArchiveNotVerified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    New,
    Unchanged,
    HeadChangedAuthorizationReset,
    HeadChangedAuthorizationPreserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    Unchanged,
    Updated {
        head_changed: bool,
        discussion_changed: bool,
        authorization_cleared: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    key: PrKey,
    head_sha: HeadSha,
    authorization: Authorization,
    revision: InputRevision,
    #[serde(deserialize_with = "deserialize_discussion")]
    discussion: Option<ObservedDiscussion>,
    #[serde(deserialize_with = "deserialize_model_validation")]
    model_validation: Option<ModelValidation>,
}

// Require the field in current state while allowing null to mean not yet observed.
fn deserialize_discussion<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ObservedDiscussion>, D::Error> {
    Option::<ObservedDiscussion>::deserialize(deserializer)
}

fn deserialize_model_validation<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ModelValidation>, D::Error> {
    Option::<ModelValidation>::deserialize(deserializer)
}

/// The lead's selected model and resolved agent, bound to inspected inputs and configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelValidation {
    model: ModelId,
    agent: AgentId,
    head_sha: HeadSha,
    revision: InputRevision,
    prompt_sha256: PromptHash,
    format_sha256: PromptHash,
    execution_sha256: PromptHash,
    runner_sha256: PromptHash,
    vm_archive: VmArchive,
}

impl ModelValidation {
    pub fn model(&self) -> &ModelId {
        &self.model
    }

    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    pub fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub fn revision(&self) -> InputRevision {
        self.revision
    }

    pub fn matches(&self, choice: &ResolvedReviewModel<'_>) -> bool {
        self.model == *choice.model().id()
            && self.agent == *choice.agent().id()
            && self.prompt_sha256 == *choice.agent().prompt_hash()
            && self.format_sha256 == PromptHash::of(FORMAT_RULES)
            && self.execution_sha256 == *choice.execution().hash()
            && self.runner_sha256 == *choice.agent().runner().hash()
            && self.vm_archive == *choice.archive()
    }

    fn matches_round(&self, round: &RoundRecord) -> bool {
        self.model == *round.model()
            && self.agent == *round.agent()
            && self.head_sha == *round.head_sha()
            && self.revision == round.revision()
            && self.prompt_sha256 == *round.prompt_hash()
            && self.format_sha256 == *round.format_hash()
            && self.execution_sha256 == *round.execution_hash()
            && self.runner_sha256 == *round.runner_hash()
            && self.vm_archive.checksum() == round.archive_hash()
            && self.vm_archive.config_checksum() == round.archive_config_hash()
    }
}

impl ReviewRequest {
    pub fn key(&self) -> &PrKey {
        &self.key
    }

    pub fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub fn authorization(&self) -> Authorization {
        self.authorization
    }

    pub fn revision(&self) -> InputRevision {
        self.revision
    }

    pub fn discussion(&self) -> Option<&ObservedDiscussion> {
        self.discussion.as_ref()
    }

    pub fn model_validation(&self) -> Option<&ModelValidation> {
        self.model_validation.as_ref()
    }

    pub fn ensure_inspected(
        &self,
        inspected_head: &HeadSha,
        inspected_revision: InputRevision,
    ) -> Result<(), StateError> {
        if &self.head_sha != inspected_head {
            return Err(StateError::HeadMismatch {
                inspected: inspected_head.clone(),
                recorded: self.head_sha.clone(),
            });
        }
        if self.revision != inspected_revision {
            return Err(StateError::RevisionMismatch {
                inspected: inspected_revision,
                recorded: self.revision,
            });
        }
        if self.discussion.is_none() {
            return Err(StateError::DiscussionNotObserved(self.key.clone()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    schema_version: u32,
    requests: Vec<ReviewRequest>,
    rounds: Vec<RoundRecord>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            requests: Vec::new(),
            rounds: Vec::new(),
        }
    }
}

impl State {
    pub fn validate(&self) -> Result<(), StateError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(StateError::UnsupportedSchema(self.schema_version));
        }
        let mut seen = HashSet::new();
        for request in &self.requests {
            if !seen.insert(&request.key) {
                return Err(StateError::DuplicateRequest(request.key.clone()));
            }
            if request.authorization == Authorization::OneRound && request.discussion.is_none() {
                return Err(StateError::DiscussionNotObserved(request.key.clone()));
            }
            if let Some(validation) = &request.model_validation {
                if request.head_sha != validation.head_sha {
                    return Err(StateError::HeadMismatch {
                        inspected: validation.head_sha.clone(),
                        recorded: request.head_sha.clone(),
                    });
                }
                if request.revision != validation.revision {
                    return Err(StateError::RevisionMismatch {
                        inspected: validation.revision,
                        recorded: request.revision,
                    });
                }
                if request.discussion.is_none() {
                    return Err(StateError::DiscussionNotObserved(request.key.clone()));
                }
                if request.authorization == Authorization::AwaitingLead {
                    return Err(StateError::AwaitingAuthorization(request.key.clone()));
                }
            }
        }
        let mut round_ids = HashSet::new();
        let mut blocking_keys = HashSet::new();
        for round in &self.rounds {
            round
                .validate()
                .map_err(|_| StateError::InvalidRound(round.id().to_string()))?;
            if !round_ids.insert(round.id()) {
                return Err(StateError::InvalidRound(round.id().to_string()));
            }
            if self.get(round.key()).is_none() {
                return Err(StateError::InvalidRound(round.id().to_string()));
            }
            if round.blocks_new_round() && !blocking_keys.insert(round.key()) {
                return Err(StateError::InvalidRound(round.id().to_string()));
            }
        }
        Ok(())
    }

    pub fn requests(&self) -> &[ReviewRequest] {
        &self.requests
    }

    pub fn rounds(&self) -> &[RoundRecord] {
        &self.rounds
    }

    pub fn round(&self, id: &RoundId) -> Option<&RoundRecord> {
        self.rounds.iter().find(|round| round.id() == id)
    }

    pub fn latest_round(&self, key: &PrKey) -> Option<&RoundRecord> {
        self.rounds.iter().rev().find(|round| round.key() == key)
    }

    pub fn begin_round(
        &mut self,
        round: RoundRecord,
        choice: &ResolvedReviewModel<'_>,
    ) -> Result<(), RoundStateError> {
        if self.round(round.id()).is_some() {
            return Err(RoundStateError::DuplicateId(round.id().clone()));
        }
        if let Some(blocking) = self
            .rounds
            .iter()
            .find(|existing| existing.key() == round.key() && existing.blocks_new_round())
        {
            return Err(RoundStateError::BlocksNewRound(blocking.id().clone()));
        }
        let request = self
            .get(round.key())
            .ok_or_else(|| StateError::RequestNotFound(round.key().clone()))?;
        request.ensure_inspected(round.head_sha(), round.revision())?;
        if request.authorization() == Authorization::AwaitingLead {
            return Err(StateError::AwaitingAuthorization(round.key().clone()).into());
        }
        if request.discussion() != Some(round.discussion())
            || !request
                .model_validation()
                .is_some_and(|validation| validation.matches(choice))
            || round.model() != choice.model().id()
            || round.agent() != choice.agent().id()
            || round.prompt_hash() != choice.agent().prompt_hash()
            || round.format_hash() != &PromptHash::of(FORMAT_RULES)
            || round.execution_hash() != choice.execution().hash()
            || round.runner_hash() != choice.agent().runner().hash()
            || round.archive_hash() != choice.archive().checksum()
            || round.archive_config_hash() != choice.archive().config_checksum()
        {
            return Err(RoundStateError::ModelNotValidated);
        }
        self.rounds.push(round);
        Ok(())
    }

    pub fn dispatch_round(&mut self, id: &RoundId, at: u64) -> Result<(), RoundStateError> {
        let index = self
            .rounds
            .iter()
            .position(|round| round.id() == id)
            .ok_or_else(|| RoundStateError::NotFound(id.clone()))?;
        let round = &self.rounds[index];
        if round.status() != RoundStatus::Starting {
            return Err(RoundStateError::WrongStatus(id.clone()));
        }
        let request = self
            .get(round.key())
            .ok_or_else(|| StateError::RequestNotFound(round.key().clone()))?;
        request.ensure_inspected(round.head_sha(), round.revision())?;
        if request.discussion() != Some(round.discussion())
            || request.authorization() == Authorization::AwaitingLead
            || !request
                .model_validation()
                .is_some_and(|validation| validation.matches_round(round))
        {
            return Err(RoundStateError::ModelNotValidated);
        }
        let key = round.key().clone();
        let request = self.request_mut(&key)?;
        match request.authorization {
            Authorization::OneRound => {
                request.authorization = Authorization::AwaitingLead;
                request.model_validation = None;
            }
            Authorization::PrCycle => {}
            Authorization::AwaitingLead => {
                return Err(StateError::AwaitingAuthorization(key).into());
            }
        }
        self.rounds[index].mark_running(at);
        Ok(())
    }

    pub fn complete_round(
        &mut self,
        id: &RoundId,
        at: u64,
        freshness: Freshness,
    ) -> Result<(), RoundStateError> {
        let round = self.round_mut(id)?;
        if round.status() != RoundStatus::Running {
            return Err(RoundStateError::WrongStatus(id.clone()));
        }
        round.mark_draft(at, freshness);
        Ok(())
    }

    pub fn mark_round_cleaned(&mut self, id: &RoundId) -> Result<(), RoundStateError> {
        let round = self.round_mut(id)?;
        round.mark_cleaned();
        Ok(())
    }

    pub fn fail_round(
        &mut self,
        id: &RoundId,
        at: u64,
        failure: FailureKind,
        interrupted: bool,
        incomplete_log: bool,
    ) -> Result<(), RoundStateError> {
        let round = self.round_mut(id)?;
        if !matches!(round.status(), RoundStatus::Starting | RoundStatus::Running) {
            return Err(RoundStateError::WrongStatus(id.clone()));
        }
        round.mark_failed(at, failure, interrupted, incomplete_log);
        Ok(())
    }

    fn round_mut(&mut self, id: &RoundId) -> Result<&mut RoundRecord, RoundStateError> {
        self.rounds
            .iter_mut()
            .find(|round| round.id() == id)
            .ok_or_else(|| RoundStateError::NotFound(id.clone()))
    }

    pub fn get(&self, key: &PrKey) -> Option<&ReviewRequest> {
        self.requests.iter().find(|request| request.key == *key)
    }

    pub fn record_request(
        &mut self,
        key: PrKey,
        head_sha: HeadSha,
    ) -> Result<RecordOutcome, StateError> {
        if let Some(request) = self.requests.iter_mut().find(|request| request.key == key) {
            if request.head_sha == head_sha {
                return Ok(RecordOutcome::Unchanged);
            }
            let next_revision = request.revision.next()?;
            request.head_sha = head_sha;
            request.revision = next_revision;
            request.discussion = None;
            request.model_validation = None;
            return Ok(match request.authorization {
                Authorization::OneRound => {
                    request.authorization = Authorization::AwaitingLead;
                    RecordOutcome::HeadChangedAuthorizationReset
                }
                Authorization::AwaitingLead | Authorization::PrCycle => {
                    RecordOutcome::HeadChangedAuthorizationPreserved
                }
            });
        }
        self.requests.push(ReviewRequest {
            key,
            head_sha,
            authorization: Authorization::AwaitingLead,
            revision: InputRevision::new(1)?,
            discussion: None,
            model_validation: None,
        });
        Ok(RecordOutcome::New)
    }

    pub fn refresh(
        &mut self,
        key: &PrKey,
        head_sha: &HeadSha,
        discussion: &ObservedDiscussion,
    ) -> Result<RefreshOutcome, StateError> {
        let request = self.request_mut(key)?;
        let head_changed = &request.head_sha != head_sha;
        let discussion_changed = request.discussion.as_ref() != Some(discussion);
        if !head_changed && !discussion_changed {
            return Ok(RefreshOutcome::Unchanged);
        }
        let next_revision = request.revision.next()?;
        let authorization_cleared = request.authorization == Authorization::OneRound;
        request.head_sha = head_sha.clone();
        request.discussion = Some(discussion.clone());
        request.revision = next_revision;
        request.model_validation = None;
        if authorization_cleared {
            request.authorization = Authorization::AwaitingLead;
        }
        Ok(RefreshOutcome::Updated {
            head_changed,
            discussion_changed,
            authorization_cleared,
        })
    }

    pub fn authorize(
        &mut self,
        key: &PrKey,
        inspected_head: &HeadSha,
        inspected_revision: InputRevision,
        scope: AuthorizationScope,
    ) -> Result<bool, StateError> {
        let request = self.request_mut(key)?;
        request.ensure_inspected(inspected_head, inspected_revision)?;
        let authorization = Authorization::from(scope);
        let changed = request.authorization != authorization;
        request.authorization = authorization;
        if changed {
            request.model_validation = None;
        }
        Ok(changed)
    }

    pub fn validate_model(
        &mut self,
        preparation: &Preparation,
        inspected_head: &HeadSha,
        inspected_revision: InputRevision,
        choice: &ResolvedReviewModel<'_>,
        archive: &VerifiedArchive<'_>,
    ) -> Result<bool, ModelValidationError> {
        let request = self.request_mut(preparation.key())?;
        request.ensure_inspected(inspected_head, inspected_revision)?;
        if preparation.head_sha() != inspected_head
            || preparation.revision() != inspected_revision
            || preparation.authorization() != request.authorization
        {
            return Err(ModelValidationError::StalePreparation);
        }
        if !preparation.readiness().permits_validation() {
            return Err(ModelValidationError::Blocked(preparation.readiness()));
        }
        if !preparation.allows_choice(choice) {
            return Err(ModelValidationError::ModelNotPrepared(
                choice.model().id().clone(),
            ));
        }
        if !archive.matches(choice.archive()) {
            return Err(ModelValidationError::ArchiveNotVerified);
        }
        let validation = ModelValidation {
            model: choice.model().id().clone(),
            agent: choice.agent().id().clone(),
            head_sha: inspected_head.clone(),
            revision: inspected_revision,
            prompt_sha256: choice.agent().prompt_hash().clone(),
            format_sha256: PromptHash::of(FORMAT_RULES),
            execution_sha256: choice.execution().hash().clone(),
            runner_sha256: choice.agent().runner().hash().clone(),
            vm_archive: choice.archive().clone(),
        };
        let changed = request.model_validation.as_ref() != Some(&validation);
        request.model_validation = Some(validation);
        Ok(changed)
    }

    pub fn revoke(&mut self, key: &PrKey) -> Result<bool, StateError> {
        let request = self.request_mut(key)?;
        let changed = request.authorization != Authorization::AwaitingLead;
        request.authorization = Authorization::AwaitingLead;
        let validation_cleared = request.model_validation.take().is_some();
        Ok(changed || validation_cleared)
    }

    fn request_mut(&mut self, key: &PrKey) -> Result<&mut ReviewRequest, StateError> {
        self.requests
            .iter_mut()
            .find(|request| request.key == *key)
            .ok_or_else(|| StateError::RequestNotFound(key.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReviewConfig;
    use crate::preparation::{CiObservation, CiState, PullAvailability, PullObservation, prepare};
    use crate::{CommentFingerprint, CommentHash, CommentSource};
    use std::num::NonZeroU64;

    const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn key() -> PrKey {
        PrKey::new("owner/repo", 42).unwrap()
    }

    fn head(value: impl Into<String>) -> HeadSha {
        HeadSha::new(value).unwrap()
    }

    fn revision(value: u64) -> InputRevision {
        InputRevision::new(value).unwrap()
    }

    fn discussion(comments: &[(CommentSource, u64, u8)]) -> ObservedDiscussion {
        let comments = comments
            .iter()
            .map(|(source, id, digest)| {
                CommentFingerprint::new(
                    *source,
                    NonZeroU64::new(*id).unwrap(),
                    CommentHash::from_digest([*digest; 32]),
                )
            })
            .collect();
        ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), comments).unwrap()
    }

    #[test]
    fn one_round_authorization_expires_when_head_changes() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A)).unwrap();
        state
            .refresh(&key(), &head(HEAD_A), &discussion(&[]))
            .unwrap();
        state
            .authorize(
                &key(),
                &head(HEAD_A),
                revision(2),
                AuthorizationScope::OneRound,
            )
            .unwrap();

        assert_eq!(
            state.record_request(key(), head(HEAD_B)).unwrap(),
            RecordOutcome::HeadChangedAuthorizationReset
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
        );
    }

    #[test]
    fn authorization_rejects_a_head_changed_since_the_lead_checked_status() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A)).unwrap();
        state
            .refresh(&key(), &head(HEAD_A), &discussion(&[]))
            .unwrap();
        let seen_head = state.get(&key()).unwrap().head_sha().clone();
        state.record_request(key(), head(HEAD_B)).unwrap();

        assert_eq!(
            state.authorize(
                &key(),
                &seen_head,
                revision(2),
                AuthorizationScope::OneRound
            ),
            Err(StateError::HeadMismatch {
                inspected: head(HEAD_A),
                recorded: head(HEAD_B),
            })
        );
        let request = state.get(&key()).unwrap();
        assert_eq!(request.head_sha(), &head(HEAD_B));
        assert_eq!(request.authorization(), Authorization::AwaitingLead);

        state
            .refresh(&key(), &head(HEAD_B), &discussion(&[]))
            .unwrap();
        assert!(
            state
                .authorize(
                    &key(),
                    &head(HEAD_B),
                    revision(4),
                    AuthorizationScope::OneRound
                )
                .unwrap()
        );
    }

    #[test]
    fn cycle_authorization_survives_head_change_and_can_be_revoked() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A)).unwrap();
        state
            .refresh(&key(), &head(HEAD_A), &discussion(&[]))
            .unwrap();
        state
            .authorize(
                &key(),
                &head(HEAD_A),
                revision(2),
                AuthorizationScope::PrCycle,
            )
            .unwrap();

        assert_eq!(
            state.record_request(key(), head(HEAD_B)).unwrap(),
            RecordOutcome::HeadChangedAuthorizationPreserved
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::PrCycle
        );
        state.revoke(&key()).unwrap();
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
        );
    }

    #[test]
    fn invalid_identifiers_fail_at_construction_and_deserialization() {
        assert_eq!(HeadSha::new("not-a-sha"), Err(StateError::InvalidHeadSha));
        assert_eq!(
            HeadSha::new("g".repeat(40)),
            Err(StateError::InvalidHeadSha)
        );
        assert_eq!(
            HeadSha::new("A".repeat(64)).unwrap().as_str(),
            "a".repeat(64)
        );
        assert!(PrKey::new("owner/repo/extra", 42).is_err());
        assert_eq!(
            PrKey::new("owner/repo", 0),
            Err(StateError::InvalidPrNumber)
        );

        for (raw_key, expected_error) in [
            (
                serde_json::json!({"repository": "owner/repo", "number": 0}),
                "PR number must be greater than zero",
            ),
            (
                serde_json::json!({"repository": "owner/repo/extra", "number": 42}),
                "repository must be a valid GitHub owner/name",
            ),
        ] {
            let invalid_state = serde_json::json!({
                "schema_version": 5,
                "rounds": [],
                "requests": [{"key": raw_key, "head_sha": HEAD_A, "authorization": "awaiting_lead", "revision": 1, "discussion": null, "model_validation": null}]
            });
            let error = serde_json::from_value::<State>(invalid_state).unwrap_err();
            assert!(error.to_string().contains(expected_error), "{error}");
        }
        let invalid_head = serde_json::json!({
            "schema_version": 5,
            "rounds": [],
            "requests": [
                {"key": {"repository": "owner/repo", "number": 42}, "head_sha": "not-a-sha", "authorization": "awaiting_lead", "revision": 1, "discussion": null, "model_validation": null}
            ]
        });
        let error = serde_json::from_value::<State>(invalid_head).unwrap_err();
        assert!(
            error.to_string().contains("head SHA must contain 40 or 64"),
            "{error}"
        );
    }

    #[test]
    fn repository_and_sha_case_cannot_create_a_second_request() {
        let mut state = State::default();
        let mixed_case_key = PrKey::new("Owner/Repo", 42).unwrap();
        let uppercase_head = head(HEAD_A.to_ascii_uppercase());
        state
            .record_request(mixed_case_key, uppercase_head.clone())
            .unwrap();
        state
            .refresh(&key(), &uppercase_head, &discussion(&[]))
            .unwrap();
        state
            .authorize(
                &key(),
                &uppercase_head,
                revision(2),
                AuthorizationScope::OneRound,
            )
            .unwrap();

        assert_eq!(
            state.record_request(key(), head(HEAD_A)).unwrap(),
            RecordOutcome::Unchanged
        );
        assert_eq!(state.requests().len(), 1);
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::OneRound
        );
    }

    #[test]
    fn state_json_keeps_its_shape_and_normalizes_identifiers() {
        let raw = serde_json::json!({
            "schema_version": 5,
            "rounds": [],
            "requests": [
                {
                    "key": {"repository": "Owner/Repo", "number": 42},
                    "head_sha": HEAD_A.to_ascii_uppercase(),
                    "authorization": "awaiting_lead",
                    "revision": 1,
                    "discussion": null,
                    "model_validation": null
                }
            ]
        });
        let state: State = serde_json::from_value(raw).unwrap();
        state.validate().unwrap();
        assert_eq!(state.get(&key()).unwrap().head_sha(), &head(HEAD_A));

        let serialized = serde_json::to_value(&state).unwrap();
        assert_eq!(serialized["requests"][0]["key"]["repository"], "owner/repo");
        assert_eq!(serialized["requests"][0]["key"]["number"], 42);
        assert_eq!(serialized["requests"][0]["head_sha"], HEAD_A);
        assert_eq!(serialized["requests"][0]["authorization"], "awaiting_lead");
        assert_eq!(serialized["schema_version"], 5);
        assert_eq!(serialized["rounds"], serde_json::json!([]));
        assert_eq!(serialized["requests"][0]["revision"], 1);
        assert!(serialized["requests"][0]["discussion"].is_null());
        assert!(serialized["requests"][0]["model_validation"].is_null());
    }

    #[test]
    fn corrupt_or_future_state_is_rejected() {
        let duplicate = serde_json::json!({
            "schema_version": 5,
            "rounds": [],
            "requests": [
                {"key": {"repository": "owner/repo", "number": 42}, "head_sha": HEAD_A, "authorization": "one_round", "revision": 1, "discussion": {"author_id": 7, "comments": []}, "model_validation": null},
                {"key": {"repository": "Owner/Repo", "number": 42}, "head_sha": HEAD_B, "authorization": "pr_cycle", "revision": 1, "discussion": null, "model_validation": null}
            ]
        });
        let state: State = serde_json::from_value(duplicate).unwrap();
        assert_eq!(state.validate(), Err(StateError::DuplicateRequest(key())));

        let future: State =
            serde_json::from_str(r#"{"schema_version":6,"requests":[],"rounds":[]}"#).unwrap();
        assert_eq!(future.validate(), Err(StateError::UnsupportedSchema(6)));
    }

    #[test]
    fn refresh_detects_head_and_author_discussion_changes_under_every_authorization() {
        let initial = discussion(&[
            (CommentSource::IssueComment, 1, 1),
            (CommentSource::ReviewComment, 2, 2),
        ]);
        let changes = [
            (head(HEAD_B), initial.clone(), true, false),
            (
                head(HEAD_A),
                discussion(&[
                    (CommentSource::IssueComment, 1, 1),
                    (CommentSource::ReviewComment, 2, 2),
                    (CommentSource::Review, 3, 3),
                ]),
                false,
                true,
            ),
            (
                head(HEAD_A),
                discussion(&[
                    (CommentSource::IssueComment, 1, 9),
                    (CommentSource::ReviewComment, 2, 2),
                ]),
                false,
                true,
            ),
            (
                head(HEAD_A),
                discussion(&[(CommentSource::IssueComment, 1, 1)]),
                false,
                true,
            ),
        ];
        for authorization in [
            Authorization::AwaitingLead,
            Authorization::OneRound,
            Authorization::PrCycle,
        ] {
            for (next_head, next_discussion, head_changed, discussion_changed) in &changes {
                let mut state = State::default();
                state.record_request(key(), head(HEAD_A)).unwrap();
                state.refresh(&key(), &head(HEAD_A), &initial).unwrap();
                match authorization {
                    Authorization::AwaitingLead => {}
                    Authorization::OneRound => {
                        state
                            .authorize(
                                &key(),
                                &head(HEAD_A),
                                revision(2),
                                AuthorizationScope::OneRound,
                            )
                            .unwrap();
                    }
                    Authorization::PrCycle => {
                        state
                            .authorize(
                                &key(),
                                &head(HEAD_A),
                                revision(2),
                                AuthorizationScope::PrCycle,
                            )
                            .unwrap();
                    }
                }
                assert_eq!(
                    state.refresh(&key(), next_head, next_discussion).unwrap(),
                    RefreshOutcome::Updated {
                        head_changed: *head_changed,
                        discussion_changed: *discussion_changed,
                        authorization_cleared: authorization == Authorization::OneRound,
                    }
                );
                let request = state.get(&key()).unwrap();
                assert_eq!(request.revision(), revision(3));
                assert_eq!(request.head_sha(), next_head);
                assert_eq!(request.discussion(), Some(next_discussion));
                assert_eq!(
                    request.authorization(),
                    if authorization == Authorization::OneRound {
                        Authorization::AwaitingLead
                    } else {
                        authorization
                    }
                );
                assert_eq!(
                    state.refresh(&key(), next_head, next_discussion).unwrap(),
                    RefreshOutcome::Unchanged
                );
                assert_eq!(state.get(&key()).unwrap().revision(), revision(3));
            }
        }
    }

    #[test]
    fn current_state_rejects_one_round_authorization_without_observed_discussion() {
        let state: State = serde_json::from_value(serde_json::json!({
            "schema_version": 5,
            "rounds": [],
            "requests": [{
                "key": {"repository": "owner/repo", "number": 42},
                "head_sha": HEAD_A,
                "authorization": "one_round",
                "revision": 1,
                "discussion": null,
                "model_validation": null
            }]
        }))
        .unwrap();
        assert_eq!(
            state.validate(),
            Err(StateError::DiscussionNotObserved(key()))
        );
    }

    #[test]
    fn new_authorization_requires_observed_discussion_after_request_or_manual_head_change() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A)).unwrap();
        let before = serde_json::to_value(&state).unwrap();
        for scope in [AuthorizationScope::OneRound, AuthorizationScope::PrCycle] {
            assert_eq!(
                state.authorize(&key(), &head(HEAD_A), revision(1), scope),
                Err(StateError::DiscussionNotObserved(key()))
            );
            assert_eq!(serde_json::to_value(&state).unwrap(), before);
        }

        state
            .refresh(&key(), &head(HEAD_A), &discussion(&[]))
            .unwrap();
        state
            .authorize(
                &key(),
                &head(HEAD_A),
                revision(2),
                AuthorizationScope::PrCycle,
            )
            .unwrap();
        state.record_request(key(), head(HEAD_B)).unwrap();
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::PrCycle
        );
        assert_eq!(
            state.authorize(
                &key(),
                &head(HEAD_B),
                revision(3),
                AuthorizationScope::OneRound
            ),
            Err(StateError::DiscussionNotObserved(key()))
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::PrCycle
        );
        state
            .refresh(&key(), &head(HEAD_B), &discussion(&[]))
            .unwrap();
        assert!(
            state
                .authorize(
                    &key(),
                    &head(HEAD_B),
                    revision(4),
                    AuthorizationScope::OneRound
                )
                .unwrap()
        );
    }

    #[test]
    fn changed_author_identity_advances_revision_and_failed_scope_switch_keeps_authorization() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A)).unwrap();
        let original = discussion(&[]);
        state.refresh(&key(), &head(HEAD_A), &original).unwrap();
        state
            .authorize(
                &key(),
                &head(HEAD_A),
                revision(2),
                AuthorizationScope::PrCycle,
            )
            .unwrap();
        let different_author =
            ObservedDiscussion::new(NonZeroU64::new(8).unwrap(), vec![]).unwrap();
        assert_eq!(
            state
                .refresh(&key(), &head(HEAD_A), &different_author)
                .unwrap(),
            RefreshOutcome::Updated {
                head_changed: false,
                discussion_changed: true,
                authorization_cleared: false,
            }
        );
        assert_eq!(
            state.authorize(
                &key(),
                &head(HEAD_A),
                revision(2),
                AuthorizationScope::OneRound
            ),
            Err(StateError::RevisionMismatch {
                inspected: revision(2),
                recorded: revision(3),
            })
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::PrCycle
        );
    }

    #[test]
    fn manual_head_change_advances_revision_and_discards_discussion() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A)).unwrap();
        state
            .refresh(&key(), &head(HEAD_A), &discussion(&[]))
            .unwrap();
        assert_eq!(
            state.record_request(key(), head(HEAD_A)).unwrap(),
            RecordOutcome::Unchanged
        );
        assert_eq!(state.get(&key()).unwrap().revision(), revision(2));
        state.record_request(key(), head(HEAD_B)).unwrap();
        let request = state.get(&key()).unwrap();
        assert_eq!(request.revision(), revision(3));
        assert!(request.discussion().is_none());
    }

    #[test]
    fn malformed_observations_and_revision_overflow_are_rejected() {
        let base = serde_json::json!({
            "schema_version": 5,
            "rounds": [],
            "requests": [{
                "key": {"repository": "owner/repo", "number": 42},
                "head_sha": HEAD_A,
                "authorization": "one_round",
                "revision": 1,
                "discussion": {"author_id": 7, "comments": []},
                "model_validation": null
            }]
        });
        for (field, replacement) in [
            ("revision", serde_json::json!(0)),
            (
                "discussion",
                serde_json::json!({"author_id": 0, "comments": []}),
            ),
            (
                "discussion",
                serde_json::json!({"author_id": 7, "comments": [{"source": "issue_comment", "id": 1, "sha256": "bad"}]}),
            ),
            (
                "discussion",
                serde_json::json!({"author_id": 7, "comments": [
                    {"source": "issue_comment", "id": 1, "sha256": "a".repeat(64)},
                    {"source": "issue_comment", "id": 1, "sha256": "b".repeat(64)}
                ]}),
            ),
        ] {
            let mut value = base.clone();
            value["requests"][0][field] = replacement;
            assert!(serde_json::from_value::<State>(value).is_err());
        }
        let mut missing_discussion = base.clone();
        missing_discussion["requests"][0]
            .as_object_mut()
            .unwrap()
            .remove("discussion");
        assert!(serde_json::from_value::<State>(missing_discussion).is_err());
        let mut value = base;
        value["requests"][0]["revision"] = serde_json::json!(u64::MAX);
        let mut state: State = serde_json::from_value(value).unwrap();
        assert_eq!(
            state.record_request(key(), head(HEAD_B)),
            Err(StateError::RevisionOverflow)
        );
        assert_eq!(
            state.refresh(&key(), &head(HEAD_B), &discussion(&[])),
            Err(StateError::RevisionOverflow)
        );
        assert_eq!(state.get(&key()).unwrap().head_sha(), &head(HEAD_A));
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::OneRound
        );
    }

    #[test]
    fn model_validation_tracks_exact_inputs_and_is_cleared_by_changes() {
        let raw = serde_json::json!({
            "schema_version":5,
            "execution": crate::config::test_execution_json(),
            "models":[{"id":"model-alpha", "coauthors":[]}],
            "agents":[
                {"id":"claude", "models":["model-alpha"], "prompt":"Review.", "runner":crate::config::test_runner_json()},
                {"id":"another-agent", "models":["model-alpha"], "prompt":"Review separately.", "runner":crate::config::test_runner_json()}
            ],
            "repositories":[{"name":"owner/repo", "review_models":["model-alpha"], "runners":[{
                "agent":"claude",
                "vm_archive":{
                    "path":std::env::temp_dir().join("reviewer.tar").to_str().unwrap(),
                    "sha256":"a".repeat(64),
                    "config_sha256":"c".repeat(64)
                }
            }]}]
        });
        let config = ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let review_models = config.review_models_for(&key()).unwrap();
        let current_head = head(HEAD_A);
        let verified = VerifiedArchive::for_test(review_models[0].archive());
        let mut state = State::default();
        state.record_request(key(), current_head.clone()).unwrap();
        state
            .refresh(&key(), &current_head, &discussion(&[]))
            .unwrap();
        let awaiting = prepare(
            state.get(&key()).unwrap(),
            PullObservation::new(&current_head, PullAvailability::Open),
            CiObservation::new(&current_head, CiState::Green),
            &review_models,
            &HashSet::new(),
        )
        .unwrap();
        assert!(matches!(
            state.validate_model(
                &awaiting,
                &current_head,
                revision(2),
                &review_models[0],
                &verified,
            ),
            Err(ModelValidationError::Blocked(
                Readiness::AwaitingAuthorization
            ))
        ));
        state
            .authorize(
                &key(),
                &current_head,
                revision(2),
                AuthorizationScope::PrCycle,
            )
            .unwrap();
        assert!(matches!(
            state.validate_model(
                &awaiting,
                &current_head,
                revision(2),
                &review_models[0],
                &verified,
            ),
            Err(ModelValidationError::StalePreparation)
        ));
        let prepared = prepare(
            state.get(&key()).unwrap(),
            PullObservation::new(&current_head, PullAvailability::Open),
            CiObservation::new(&current_head, CiState::Green),
            &review_models,
            &HashSet::new(),
        )
        .unwrap();
        let pending = prepare(
            state.get(&key()).unwrap(),
            PullObservation::new(&current_head, PullAvailability::Open),
            CiObservation::new(&current_head, CiState::Pending),
            &review_models,
            &HashSet::new(),
        )
        .unwrap();
        assert!(matches!(
            state.validate_model(
                &pending,
                &current_head,
                revision(2),
                &review_models[0],
                &verified,
            ),
            Err(ModelValidationError::Blocked(Readiness::ChecksPending))
        ));
        let other_archive = VmArchive::new(
            std::env::temp_dir().join("different.tar"),
            crate::config::ArchiveHash::new("a".repeat(64)).unwrap(),
            crate::config::ArchiveHash::new("c".repeat(64)).unwrap(),
        )
        .unwrap();
        let wrong_verification = VerifiedArchive::for_test(&other_archive);
        assert!(matches!(
            state.validate_model(
                &prepared,
                &current_head,
                revision(2),
                &review_models[0],
                &wrong_verification,
            ),
            Err(ModelValidationError::ArchiveNotVerified)
        ));
        let mut changed_config = raw.clone();
        changed_config["agents"][0]["prompt"] = "Review differently.".into();
        let changed_config =
            ReviewConfig::from_json(&serde_json::to_vec(&changed_config).unwrap()).unwrap();
        let changed_models = changed_config.review_models_for(&key()).unwrap();
        let changed_verification = VerifiedArchive::for_test(changed_models[0].archive());
        assert!(matches!(
            state.validate_model(
                &prepared,
                &current_head,
                revision(2),
                &changed_models[0],
                &changed_verification,
            ),
            Err(ModelValidationError::ModelNotPrepared(_))
        ));
        assert!(state.get(&key()).unwrap().model_validation().is_none());
        assert!(
            state
                .validate_model(
                    &prepared,
                    &current_head,
                    revision(2),
                    &review_models[0],
                    &verified,
                )
                .unwrap()
        );
        assert!(
            !state
                .validate_model(
                    &prepared,
                    &current_head,
                    revision(2),
                    &review_models[0],
                    &verified,
                )
                .unwrap()
        );
        let mut changed_runner = raw.clone();
        changed_runner["agents"][0]["runner"]["max_turns"] = 17.into();
        let changed_runner =
            ReviewConfig::from_json(&serde_json::to_vec(&changed_runner).unwrap()).unwrap();
        assert!(
            !state
                .get(&key())
                .unwrap()
                .model_validation()
                .unwrap()
                .matches(&changed_runner.review_models_for(&key()).unwrap()[0])
        );
        let mut changed_other_agent = raw.clone();
        changed_other_agent["agents"][1]["runner"]["max_turns"] = 17.into();
        let changed_other_agent =
            ReviewConfig::from_json(&serde_json::to_vec(&changed_other_agent).unwrap()).unwrap();
        assert!(
            state
                .get(&key())
                .unwrap()
                .model_validation()
                .unwrap()
                .matches(&changed_other_agent.review_models_for(&key()).unwrap()[0])
        );
        let mut changed_metadata = raw.clone();
        changed_metadata["repositories"][0]["runners"][0]["vm_archive"]["config_sha256"] =
            "d".repeat(64).into();
        let changed_metadata =
            ReviewConfig::from_json(&serde_json::to_vec(&changed_metadata).unwrap()).unwrap();
        assert!(
            !state
                .get(&key())
                .unwrap()
                .model_validation()
                .unwrap()
                .matches(&changed_metadata.review_models_for(&key()).unwrap()[0])
        );
        let mut old_format = serde_json::to_value(&state).unwrap();
        old_format["requests"][0]["model_validation"]["format_sha256"] = "b".repeat(64).into();
        let old_format: State = serde_json::from_value(old_format).unwrap();
        assert_eq!(
            prepare(
                old_format.get(&key()).unwrap(),
                PullObservation::new(&current_head, PullAvailability::Open),
                CiObservation::new(&current_head, CiState::Green),
                &review_models,
                &HashSet::new(),
            )
            .unwrap()
            .readiness(),
            Readiness::ModelValidationRequired
        );
        assert!(matches!(
            state.validate_model(
                &prepared,
                &current_head,
                revision(1),
                &review_models[0],
                &verified,
            ),
            Err(ModelValidationError::State(
                StateError::RevisionMismatch { .. }
            ))
        ));
        let mut malformed = serde_json::to_value(&state).unwrap();
        malformed["requests"][0]["model_validation"]["head_sha"] = HEAD_B.into();
        let malformed: State = serde_json::from_value(malformed).unwrap();
        assert!(matches!(
            malformed.validate(),
            Err(StateError::HeadMismatch { .. })
        ));
        let validated = serde_json::to_value(&state).unwrap();
        for (field, value) in [
            ("model", serde_json::json!("INVALID MODEL")),
            ("agent", serde_json::json!("Invalid Agent")),
            ("prompt_sha256", serde_json::json!("bad")),
        ] {
            let mut malformed = validated.clone();
            malformed["requests"][0]["model_validation"][field] = value;
            assert!(serde_json::from_value::<State>(malformed).is_err());
        }
        let mut malformed = validated;
        malformed["requests"][0]["model_validation"]["vm_archive"]["path"] = "relative.tar".into();
        assert!(serde_json::from_value::<State>(malformed).is_err());

        state
            .refresh(
                &key(),
                &head(HEAD_A),
                &discussion(&[(CommentSource::IssueComment, 1, 1)]),
            )
            .unwrap();
        assert!(state.get(&key()).unwrap().model_validation().is_none());
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::PrCycle
        );
        let prepared = prepare(
            state.get(&key()).unwrap(),
            PullObservation::new(&current_head, PullAvailability::Open),
            CiObservation::new(&current_head, CiState::Green),
            &review_models,
            &HashSet::new(),
        )
        .unwrap();
        state
            .validate_model(
                &prepared,
                &current_head,
                revision(3),
                &review_models[0],
                &verified,
            )
            .unwrap();
        state.revoke(&key()).unwrap();
        assert!(state.get(&key()).unwrap().model_validation().is_none());
    }

    #[test]
    fn rounds_consume_one_round_authorization_and_saved_drafts_block_retries() {
        let raw = serde_json::json!({
            "schema_version":5,
            "execution": crate::config::test_execution_json(),
            "models":[{"id":"model-alpha", "coauthors":[]}],
            "agents":[{"id":"claude", "models":["model-alpha"], "prompt":"Review.", "runner":crate::config::test_runner_json()}],
            "repositories":[{"name":"owner/repo", "review_models":["model-alpha"], "runners":[{
                "agent":"claude", "vm_archive":{"path":std::env::temp_dir().join("reviewer.tar"), "sha256":"a".repeat(64), "config_sha256":"c".repeat(64)}
            }]}]
        });
        let config = ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let choices = config.review_models_for(&key()).unwrap();
        let choice = &choices[0];
        let head = head(HEAD_A);
        let discussion = discussion(&[]);
        let mut state = State::default();
        state.record_request(key(), head.clone()).unwrap();
        state.refresh(&key(), &head, &discussion).unwrap();
        state
            .authorize(&key(), &head, revision(2), AuthorizationScope::OneRound)
            .unwrap();
        let prepared = prepare(
            state.get(&key()).unwrap(),
            PullObservation::new(&head, PullAvailability::Open),
            CiObservation::new(&head, CiState::Green),
            &choices,
            &HashSet::new(),
        )
        .unwrap();
        state
            .validate_model(
                &prepared,
                &head,
                revision(2),
                choice,
                &VerifiedArchive::for_test(choice.archive()),
            )
            .unwrap();
        let first = RoundRecord::new(
            RoundId::new("r-first").unwrap(),
            key(),
            head.clone(),
            revision(2),
            discussion.clone(),
            choice,
            1,
        );
        state.begin_round(first.clone(), choice).unwrap();
        assert!(matches!(
            state.begin_round(
                RoundRecord::new(
                    RoundId::new("r-second").unwrap(),
                    key(),
                    head.clone(),
                    revision(2),
                    discussion.clone(),
                    choice,
                    2
                ),
                choice
            ),
            Err(RoundStateError::BlocksNewRound(_))
        ));
        state.dispatch_round(first.id(), 3).unwrap();
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
        );
        assert!(state.get(&key()).unwrap().model_validation().is_none());
        state
            .complete_round(first.id(), 4, Freshness::Stale)
            .unwrap();
        state.mark_round_cleaned(first.id()).unwrap();
        assert!(state.validate().is_ok());
        state
            .authorize(&key(), &head, revision(2), AuthorizationScope::OneRound)
            .unwrap();
        let prepared = prepare(
            state.get(&key()).unwrap(),
            PullObservation::new(&head, PullAvailability::Open),
            CiObservation::new(&head, CiState::Green),
            &choices,
            &HashSet::new(),
        )
        .unwrap();
        state
            .validate_model(
                &prepared,
                &head,
                revision(2),
                choice,
                &VerifiedArchive::for_test(choice.archive()),
            )
            .unwrap();
        assert!(matches!(
            state.begin_round(
                RoundRecord::new(
                    RoundId::new("r-third").unwrap(),
                    key(),
                    head,
                    revision(2),
                    discussion,
                    choice,
                    5
                ),
                choice
            ),
            Err(RoundStateError::BlocksNewRound(_))
        ));
    }

    #[test]
    fn revocation_between_creation_and_dispatch_prevents_consumption() {
        let raw = serde_json::json!({
            "schema_version":5,
            "execution": crate::config::test_execution_json(),
            "models":[{"id":"model-alpha", "coauthors":[]}],
            "agents":[{"id":"claude", "models":["model-alpha"], "prompt":"Review.", "runner":crate::config::test_runner_json()}],
            "repositories":[{"name":"owner/repo", "review_models":["model-alpha"], "runners":[{
                "agent":"claude", "vm_archive":{"path":std::env::temp_dir().join("reviewer.tar"), "sha256":"a".repeat(64), "config_sha256":"c".repeat(64)}
            }]}]
        });
        let config = ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let choices = config.review_models_for(&key()).unwrap();
        let choice = &choices[0];
        let head = head(HEAD_A);
        let discussion = discussion(&[]);
        let mut state = State::default();
        state.record_request(key(), head.clone()).unwrap();
        state.refresh(&key(), &head, &discussion).unwrap();
        state
            .authorize(&key(), &head, revision(2), AuthorizationScope::OneRound)
            .unwrap();
        let prepared = prepare(
            state.get(&key()).unwrap(),
            PullObservation::new(&head, PullAvailability::Open),
            CiObservation::new(&head, CiState::Green),
            &choices,
            &HashSet::new(),
        )
        .unwrap();
        state
            .validate_model(
                &prepared,
                &head,
                revision(2),
                choice,
                &VerifiedArchive::for_test(choice.archive()),
            )
            .unwrap();
        let round = RoundRecord::new(
            RoundId::new("r-revoked").unwrap(),
            key(),
            head,
            revision(2),
            discussion,
            choice,
            1,
        );
        state.begin_round(round.clone(), choice).unwrap();
        state.revoke(&key()).unwrap();
        assert!(matches!(
            state.dispatch_round(round.id(), 2),
            Err(RoundStateError::ModelNotValidated)
        ));
        assert_eq!(
            state.round(round.id()).unwrap().status(),
            RoundStatus::Starting
        );
        state
            .fail_round(round.id(), 3, FailureKind::Dispatch, false, false)
            .unwrap();
        state.mark_round_cleaned(round.id()).unwrap();
        assert!(state.validate().is_ok());
    }
}
