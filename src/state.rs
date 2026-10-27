//! Review requests and their authorization transitions.

use crate::model::{
    Authorization, AuthorizationScope, HeadSha, InputRevision, ObservedDiscussion, PrKey,
    StateError,
};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::HashSet;

const SCHEMA_VERSION: u32 = 2;

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
}

// Require the field in schema 2 while allowing null to mean not yet observed.
fn deserialize_discussion<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ObservedDiscussion>, D::Error> {
    Option::<ObservedDiscussion>::deserialize(deserializer)
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
}

#[derive(Debug, Clone, Serialize)]
pub struct State {
    schema_version: u32,
    requests: Vec<ReviewRequest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawState {
    schema_version: u32,
    requests: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyReviewRequest {
    key: PrKey,
    head_sha: HeadSha,
    authorization: Authorization,
}

impl<'de> Deserialize<'de> for State {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawState::deserialize(deserializer)?;
        let requests = match raw.schema_version {
            1 => raw
                .requests
                .into_iter()
                .map(|value| {
                    let old: LegacyReviewRequest =
                        serde_json::from_value(value).map_err(de::Error::custom)?;
                    Ok(ReviewRequest {
                        key: old.key,
                        head_sha: old.head_sha,
                        authorization: old.authorization,
                        revision: InputRevision::new(1).map_err(de::Error::custom)?,
                        discussion: None,
                    })
                })
                .collect::<Result<Vec<_>, D::Error>>()?,
            2 => raw
                .requests
                .into_iter()
                .map(|value| serde_json::from_value(value).map_err(de::Error::custom))
                .collect::<Result<Vec<_>, D::Error>>()?,
            other => return Err(de::Error::custom(StateError::UnsupportedSchema(other))),
        };
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            requests,
        })
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            requests: Vec::new(),
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
        }
        Ok(())
    }

    pub fn requests(&self) -> &[ReviewRequest] {
        &self.requests
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
        if &request.head_sha != inspected_head {
            return Err(StateError::HeadMismatch {
                inspected: inspected_head.clone(),
                recorded: request.head_sha.clone(),
            });
        }
        if request.revision != inspected_revision {
            return Err(StateError::RevisionMismatch {
                inspected: inspected_revision,
                recorded: request.revision,
            });
        }
        if request.discussion.is_none() {
            return Err(StateError::DiscussionNotObserved(key.clone()));
        }
        let authorization = Authorization::from(scope);
        let changed = request.authorization != authorization;
        request.authorization = authorization;
        Ok(changed)
    }

    pub fn revoke(&mut self, key: &PrKey) -> Result<bool, StateError> {
        let request = self.request_mut(key)?;
        let changed = request.authorization != Authorization::AwaitingLead;
        request.authorization = Authorization::AwaitingLead;
        Ok(changed)
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
                "schema_version": 1,
                "requests": [{"key": raw_key, "head_sha": HEAD_A, "authorization": "awaiting_lead"}]
            });
            let error = serde_json::from_value::<State>(invalid_state).unwrap_err();
            assert!(error.to_string().contains(expected_error), "{error}");
        }
        let invalid_head = serde_json::json!({
            "schema_version": 1,
            "requests": [
                {"key": {"repository": "owner/repo", "number": 42}, "head_sha": "not-a-sha", "authorization": "awaiting_lead"}
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
            "schema_version": 1,
            "requests": [
                {
                    "key": {"repository": "Owner/Repo", "number": 42},
                    "head_sha": HEAD_A.to_ascii_uppercase(),
                    "authorization": "awaiting_lead"
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
        assert_eq!(serialized["schema_version"], 2);
        assert_eq!(serialized["requests"][0]["revision"], 1);
        assert!(serialized["requests"][0]["discussion"].is_null());
    }

    #[test]
    fn corrupt_or_future_state_is_rejected() {
        let duplicate = serde_json::json!({
            "schema_version": 1,
            "requests": [
                {"key": {"repository": "owner/repo", "number": 42}, "head_sha": HEAD_A, "authorization": "one_round"},
                {"key": {"repository": "Owner/Repo", "number": 42}, "head_sha": HEAD_B, "authorization": "pr_cycle"}
            ]
        });
        let state: State = serde_json::from_value(duplicate).unwrap();
        assert_eq!(state.validate(), Err(StateError::DuplicateRequest(key())));

        let future =
            serde_json::from_str::<State>(r#"{"schema_version":3,"requests":[]}"#).unwrap_err();
        assert!(
            future
                .to_string()
                .contains("unsupported state schema version: 3")
        );
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
    fn first_refresh_establishes_baseline_and_old_revision_cannot_authorize() {
        let mut state: State = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "requests": [{
                "key": {"repository": "owner/repo", "number": 42},
                "head_sha": HEAD_A,
                "authorization": "one_round"
            }]
        }))
        .unwrap();
        let empty = discussion(&[]);
        assert_eq!(
            state.refresh(&key(), &head(HEAD_A), &empty).unwrap(),
            RefreshOutcome::Updated {
                head_changed: false,
                discussion_changed: true,
                authorization_cleared: true,
            }
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
        );
        assert_eq!(
            state.authorize(
                &key(),
                &head(HEAD_A),
                revision(1),
                AuthorizationScope::PrCycle
            ),
            Err(StateError::RevisionMismatch {
                inspected: revision(1),
                recorded: revision(2),
            })
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
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
    fn legacy_authorization_survives_load_until_first_refresh() {
        for (old_authorization, expected_after_refresh) in [
            ("one_round", Authorization::AwaitingLead),
            ("pr_cycle", Authorization::PrCycle),
        ] {
            let raw = serde_json::json!({
                "schema_version": 1,
                "requests": [{
                    "key": {"repository": "owner/repo", "number": 42},
                    "head_sha": HEAD_A,
                    "authorization": old_authorization
                }]
            });
            let mut state: State = serde_json::from_value(raw).unwrap();
            assert_eq!(state.get(&key()).unwrap().revision(), revision(1));
            assert!(state.get(&key()).unwrap().discussion().is_none());
            state
                .refresh(&key(), &head(HEAD_A), &discussion(&[]))
                .unwrap();
            assert_eq!(
                state.get(&key()).unwrap().authorization(),
                expected_after_refresh
            );
            assert_eq!(serde_json::to_value(state).unwrap()["schema_version"], 2);
        }
    }

    #[test]
    fn malformed_v2_observations_and_revision_overflow_are_rejected() {
        let base = serde_json::json!({
            "schema_version": 2,
            "requests": [{
                "key": {"repository": "owner/repo", "number": 42},
                "head_sha": HEAD_A,
                "authorization": "one_round",
                "revision": 1,
                "discussion": {"author_id": 7, "comments": []}
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
}
